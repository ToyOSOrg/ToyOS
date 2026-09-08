//! The machine's SSH server: a shell, a command, and files both ways.
//!
//! Two rules run through every path below. **Nothing this daemon starts
//! outlives the connection that asked for it**: the thread feeding a program
//! its input, the task carrying its output and the program itself all end when
//! the session does, so a program that neither writes nor exits is ended by
//! the same event that ends the client's connection. And **nothing is answered
//! before it is whole**: a partial SFTP packet is buffered, never acted on.

mod command;
mod sftp;

use std::fs;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::ssh_key::authorized_keys::AuthorizedKeys;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::sync::{mpsc, watch};

/// Where this machine keeps its SSH identity and the keys it trusts. `/home`
/// is the only mount that is both persistent and writable by userland; where
/// it is a tmpfs the identity lasts one boot, which the fingerprint printed at
/// every start is what makes visible.
const SSH_DIR: &str = "/home/root/.ssh";
const HOST_KEY: &str = "/home/root/.ssh/host_ed25519";

/// The two files that name who may log in; a key in either authorizes.
///
/// **The second is why a freshly flashed machine can be reached at all.** A
/// bench boot mints an identity into a `/home` that may be a tmpfs and starts
/// with nothing in it, so a key that has to be *installed* before the first
/// login is a key nobody can install. Neither file is protected from anything
/// else on the machine — see
/// `issues/isolation/sshd-authorized-keys-unprotected.md`.
const AUTHORIZED_KEYS: [&str; 2] =
    ["/home/root/.ssh/authorized_keys", "/system/etc/ssh_authorized_keys"];

/// How long a program may take none of the input a client is sending before
/// this daemon stops offering it and the program runs on with a closed stdin.
///
/// The only wall clock in this file. It cannot be an event: the handler
/// offering the bytes is the connection's own task, so a program that reads
/// nothing while a client keeps sending would wedge that connection with
/// nothing left able to notice.
const INPUT_STALL: Duration = Duration::from_secs(30);

/// The program ran and this daemon cannot say how it ended.
const EXIT_LOST: u32 = 254;

/// This daemon would not run what was asked. It is the shell's own convention
/// for a command that could not be executed, and the reason is on stderr; a
/// program that ran, however it ended, never answers this.
const EXIT_REFUSED: u32 = 127;

/// The channel's stderr, in the protocol's numbering.
const EXTENDED_STDERR: u32 = 1;

/// The machine's identity, minted once and kept.
///
/// A file that exists but does not parse is refused, never replaced. Minting
/// over it would change the identity every client has pinned without anyone
/// asking, which is the one event a host key exists to make noisy.
fn host_key() -> Result<PrivateKey, String> {
    match fs::read(HOST_KEY) {
        Ok(pem) => PrivateKey::from_openssh(&pem).map_err(|e| {
            format!(
                "{HOST_KEY} is not an OpenSSH private key ({e}); refusing to \
                 replace it — move it aside to mint a new identity"
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => mint_host_key(),
        Err(e) => Err(format!("cannot read {HOST_KEY}: {e}")),
    }
}

fn mint_host_key() -> Result<PrivateKey, String> {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .map_err(|e| format!("cannot generate a host key: {e}"))?;
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| format!("cannot encode the host key: {e}"))?;
    fs::create_dir_all(SSH_DIR).map_err(|e| format!("cannot create {SSH_DIR}: {e}"))?;
    fs::write(HOST_KEY, pem.as_bytes()).map_err(|e| format!("cannot write {HOST_KEY}: {e}"))?;
    println!("sshd: minted a new host identity at {HOST_KEY}");
    Ok(key)
}

/// Does `text` — the contents of an `authorized_keys` file — name `offered`?
///
/// Keys are compared as key *data*, so a differing comment is still the same
/// key and an unusual-but-valid encoding still matches; the parse is
/// `ssh-key`'s, which is also what verified the signature.
///
/// **An entry carrying config options authorizes nothing.** Options restrict
/// what a key may do (`command="…"`, `from="…"`, `restrict`), none of them are
/// implemented here, and honouring the key while dropping its restrictions
/// would grant strictly more than the file says.
fn authorizes(text: &str, offered: &PublicKey) -> bool {
    AuthorizedKeys::new(text)
        .filter_map(Result::ok)
        .filter(|entry| entry.config_opts().is_empty())
        .any(|entry| entry.public_key().key_data() == offered.key_data())
}

/// Read fresh on every attempt, so a key added to a file takes effect without a
/// restart — there is nothing here to send a reload signal to. An unreadable
/// file names nobody, so every failure answers "not authorized".
fn is_authorized(key: &PublicKey) -> bool {
    AUTHORIZED_KEYS
        .iter()
        .any(|path| fs::read_to_string(path).is_ok_and(|text| authorizes(&text, key)))
}

/// What the files name, said once at startup so a key that will never work is
/// visible before somebody tries it. `Err` means nobody can authenticate.
fn authorized_key_count() -> Result<usize, String> {
    let mut total = 0;
    for path in AUTHORIZED_KEYS {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                println!("sshd: cannot read {path} ({e})");
                continue;
            }
        };
        let (mut usable, mut restricted, mut unreadable) = (0, 0, 0);
        for entry in AuthorizedKeys::new(&text) {
            match entry {
                Ok(entry) if entry.config_opts().is_empty() => usable += 1,
                Ok(_) => restricted += 1,
                Err(_) => unreadable += 1,
            }
        }
        if restricted > 0 {
            println!(
                "sshd: {restricted} entr(ies) in {path} carry options, which are not \
                 implemented — those keys authorize nothing"
            );
        }
        if unreadable > 0 {
            println!("sshd: {unreadable} line(s) in {path} are not public keys, ignored");
        }
        println!("sshd: {usable} key(s) authorized by {path}");
        total += usable;
    }
    if total == 0 {
        return Err(format!(
            "no file names a usable key ({}); put a public key in one of them and start again",
            AUTHORIZED_KEYS.join(" or ")
        ));
    }
    Ok(total)
}

struct SshServer;

impl Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, peer_addr: Option<std::net::SocketAddr>) -> SshSession {
        SshSession {
            peer: peer_addr.map_or_else(|| "an unnamed peer".to_string(), |a| a.to_string()),
            channel: None,
            input: None,
            alive: watch::channel(()).0,
            is_pty: false,
        }
    }
}

struct SshSession {
    /// Who this is, for the console. A diagnostic and never an authority.
    peer: String,
    /// The open session channel, until a request takes it.
    channel: Option<Channel<Msg>>,
    /// Where channel data goes: a program's stdin, or the SFTP request stream.
    /// `None` once whatever was reading it is gone.
    input: Option<mpsc::Sender<Vec<u8>>>,
    /// Held for as long as this connection is. Every task started for it waits
    /// on a subscription, so dropping this — which is what the end of the
    /// session does — is what ends them and kills the program they serve.
    alive: watch::Sender<()>,
    is_pty: bool,
}

/// Which of a child's two output streams a chunk came off.
#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

impl SshSession {
    /// The channel this request is for, or a console line saying why there is
    /// none. A second request on one channel is a client bug, not a state this
    /// daemon carries.
    fn take_channel(&mut self, what: &str) -> Option<Channel<Msg>> {
        match self.channel.take() {
            Some(channel) => Some(channel),
            None => {
                println!("sshd: {}: a {what} on a channel that is already running", self.peer);
                None
            }
        }
    }

    /// Run `argv` on this channel: its stdin is channel data, its stdout is
    /// channel data back, its stderr is the channel's extended data, and its
    /// exit status is the channel's `exit-status`.
    ///
    /// `translate_newlines` is the terminal's business, not the protocol's:
    /// there is no PTY layer on this system to turn a program's `\n` into the
    /// `\r\n` a terminal needs, so the one path that has a terminal on the far
    /// end does it here. Every other path is byte-exact.
    fn run(&mut self, argv: Vec<String>, translate_newlines: bool) {
        let Some(channel) = self.take_channel("program request") else { return };
        let (_, out) = channel.split();

        let mut child = match Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                // A spawn that failed must never look like a program that ran
                // and said nothing.
                let why = format!("sshd: cannot run {}: {e}\r\n", argv[0]);
                println!("sshd: {}: cannot run {}: {e}", self.peer, argv[0]);
                tokio::spawn(async move {
                    out.extended_data(EXTENDED_STDERR, why.as_bytes()).await.ok();
                    out.exit_status(EXIT_REFUSED).await.ok();
                    out.eof().await.ok();
                    out.close().await.ok();
                });
                return;
            }
        };

        // stdin: a thread, because a write to a pipe the child is not reading
        // blocks.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (input, mut input_rx) = mpsc::channel::<Vec<u8>>(16);
        self.input = Some(input);
        std::thread::spawn(move || {
            while let Some(chunk) = input_rx.blocking_recv() {
                if stdin.write_all(&chunk).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        });

        // stdout and stderr: one thread each onto one queue, so the order the
        // two arrived in is the order they go out in.
        let (tx, mut rx) = mpsc::channel::<(Stream, Vec<u8>)>(64);
        pump(child.stdout.take().expect("stdout was piped"), Stream::Out, tx.clone());
        pump(child.stderr.take().expect("stderr was piped"), Stream::Err, tx.clone());
        drop(tx);

        let name = argv[0].clone();
        let peer = self.peer.clone();
        let mut gone = self.alive.subscribe();
        tokio::spawn(async move {
            let mut abandoned = false;
            loop {
                let (stream, data) = tokio::select! {
                    got = rx.recv() => match got {
                        Some(got) => got,
                        // Both pipes are at EOF, which the child's own
                        // teardown is what does.
                        None => break,
                    },
                    _ = gone.changed() => {
                        abandoned = true;
                        break;
                    }
                };
                let data = if translate_newlines { crlf(&data) } else { data };
                let sent = match stream {
                    Stream::Out => out.data(&data[..]).await,
                    Stream::Err => out.extended_data(EXTENDED_STDERR, &data[..]).await,
                };
                if sent.is_err() {
                    break;
                }
            }
            let status = if abandoned {
                end(&mut child, &name, &peer)
            } else {
                reap(&mut child, &name, &peer, &mut gone).await
            };
            out.exit_status(status).await.ok();
            out.eof().await.ok();
            out.close().await.ok();
        });
    }

    /// Serve the `sftp` subsystem on this channel.
    fn run_sftp(&mut self) {
        let Some(channel) = self.take_channel("subsystem request") else { return };
        let (_, out) = channel.split();
        let (input, mut input_rx) = mpsc::channel::<Vec<u8>>(16);
        self.input = Some(input);
        let peer = self.peer.clone();

        tokio::spawn(async move {
            let mut server = sftp::Server::new();
            let mut buf: Vec<u8> = Vec::new();
            let status = loop {
                let packet = match sftp::next_packet(&mut buf) {
                    Ok(packet) => packet,
                    Err(fatal) => break fatal.say(&peer),
                };
                let Some(packet) = packet else {
                    // Nothing whole to act on. The sender is this session's, so
                    // the end of the connection is what ends this wait.
                    match input_rx.recv().await {
                        Some(chunk) => buf.extend_from_slice(&chunk),
                        None => break 0,
                    }
                    continue;
                };
                // The filesystem work is blocking; the server goes with it
                // and comes back.
                let done = tokio::task::spawn_blocking(move || {
                    let reply = server.request(&packet);
                    (server, reply)
                })
                .await;
                let Ok((returned, reply)) = done else {
                    println!("sshd: sftp for {peer}: the request task did not finish");
                    break EXIT_LOST;
                };
                server = returned;
                match reply {
                    Ok(bytes) => {
                        if out.data(&bytes[..]).await.is_err() {
                            break 0;
                        }
                    }
                    Err(fatal) => break fatal.say(&peer),
                }
            };
            out.exit_status(status).await.ok();
            out.eof().await.ok();
            out.close().await.ok();
        });
    }

    /// End this channel with `why` on its stderr and [`EXIT_REFUSED`], and say
    /// the same on the console. The client gets an answer either way.
    fn refuse(&mut self, why: &str) {
        println!("sshd: {}: refused an exec: {why}", self.peer);
        let Some(channel) = self.take_channel("refused exec") else { return };
        let (_, out) = channel.split();
        let message = format!("sshd: {why}\r\n");
        tokio::spawn(async move {
            out.extended_data(EXTENDED_STDERR, message.as_bytes()).await.ok();
            out.exit_status(EXIT_REFUSED).await.ok();
            out.eof().await.ok();
            out.close().await.ok();
        });
    }
}

/// What became of a chunk of channel data offered to whatever is reading this
/// channel's input.
#[derive(Debug, PartialEq, Eq)]
enum Offered {
    Taken,
    /// Whatever was reading is gone; there is nowhere for the bytes to go.
    Gone,
    /// Nothing took them within `bound`, so the input is closed and the
    /// program runs on without it.
    Stalled,
}

/// Hand `chunk` to whatever is reading this channel's input, or say why not.
///
/// The queue is bounded, so this waits when a program is not keeping up — and
/// a program that takes nothing at all must not hold the wait open, because
/// the caller is the connection's own task and a wait here is that whole
/// connection.
async fn offer(input: &mpsc::Sender<Vec<u8>>, chunk: Vec<u8>, bound: Duration) -> Offered {
    match tokio::time::timeout(bound, input.send(chunk)).await {
        Ok(Ok(())) => Offered::Taken,
        Ok(Err(_)) => Offered::Gone,
        Err(_) => Offered::Stalled,
    }
}

/// One of a child's output pipes onto the queue both of them share, on a thread
/// because the read is blocking and this runtime has one thread for every
/// session on it. The queue keeps the order the two streams arrived in.
fn pump<R: Read + Send + 'static>(
    mut pipe: R,
    stream: Stream,
    tx: mpsc::Sender<(Stream, Vec<u8>)>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send((stream, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// `\n` to `\r\n`, for the one channel that has a terminal on the far end.
fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in data {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// The child's exit status, waited for until it exits or the connection that
/// asked for it goes.
///
/// Reached after both of the child's output pipes have closed, which its own
/// teardown is what does — so the loop below almost always ends on its first
/// question. What it exists for is the child that closed them and kept
/// running: nothing further can arrive on the channel, and the client is still
/// there to be answered whenever it does end.
async fn reap(child: &mut Child, name: &str, peer: &str, gone: &mut watch::Receiver<()>) -> u32 {
    let mut backoff = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.code() {
                    Some(code @ 0..=255) => code as u32,
                    other => {
                        println!(
                            "sshd: {peer}: {name} ended as {other:?}, which is not an exit \
                             status this protocol carries"
                        );
                        EXIT_LOST
                    }
                };
            }
            Ok(None) => {}
            Err(e) => {
                println!("sshd: {peer}: cannot wait for {name}: {e}");
                return EXIT_LOST;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = gone.changed() => return end(child, name, peer),
        }
        backoff = (backoff * 2).min(Duration::from_millis(25));
    }
}

/// End a program whose connection is gone. Nothing it writes can reach anyone
/// and nobody is left to read its status, so it is killed rather than left
/// running on a machine whose only way to see it is another login.
///
/// The line is printed after the wait, so it says the process is *gone* rather
/// than that a kill was asked for.
fn end(child: &mut Child, name: &str, peer: &str) -> u32 {
    match child.kill().and_then(|()| child.wait()) {
        Ok(_) => println!("sshd: {peer}: the connection is gone; ended {name}"),
        Err(e) => println!("sshd: {peer}: the connection is gone and {name} would not end: {e}"),
    }
    EXIT_LOST
}

impl russh::server::Handler for SshSession {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channel = Some(channel);
        Ok(true)
    }

    /// The offer: a probe carrying a public key and no signature, which every
    /// client sends before it signs.
    ///
    /// **Refusing here is what makes this daemon fail closed at the earliest
    /// point the protocol has.** `russh`'s default answers `USERAUTH_PK_OK` to
    /// every offer, which tells a stranger its key would be taken and asks it
    /// to sign; a key no file names is turned away before that.
    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if is_authorized(key) {
            return Ok(Auth::Accept);
        }
        println!(
            "sshd: refused {user}: {} is authorized by no file, and was not asked to sign",
            key.fingerprint(HashAlg::Sha256)
        );
        Ok(Auth::reject())
    }

    /// After russh has verified the signature. Checked again rather than
    /// trusting the offer above to have filtered: a client is free to sign
    /// without asking first, and that path must reach the same files.
    /// `russh` defaults every other auth callback to `Reject`, which is why
    /// `auth_password` is simply absent.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let fingerprint = key.fingerprint(HashAlg::Sha256);
        if is_authorized(key) {
            println!("sshd: {user} authenticated with {fingerprint}");
            return Ok(Auth::Accept);
        }
        println!("sshd: refused {user}: {fingerprint} signed, and is authorized by no file");
        Ok(Auth::reject())
    }

    async fn data(
        &mut self,
        _channel_id: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(input) = self.input.as_ref() else { return Ok(()) };
        match offer(input, data.to_vec(), INPUT_STALL).await {
            Offered::Taken => {}
            Offered::Gone => self.input = None,
            Offered::Stalled => {
                println!(
                    "sshd: {}: the program on this channel took none of {} bytes within {}s; \
                     closing its input",
                    self.peer,
                    data.len(),
                    INPUT_STALL.as_secs()
                );
                self.input = None;
            }
        }
        Ok(())
    }

    /// The client is done sending. Dropping the sender closes the pipe, which
    /// is the EOF a program reading stdin to the end is waiting for.
    async fn channel_eof(
        &mut self,
        _channel_id: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.input = None;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _channel_id: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.input = None;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel_id)?;
        let translate = self.is_pty;
        self.run(vec!["/system/bin/shell".to_string()], translate);
        Ok(())
    }

    /// Run the named program. **Not through a shell**: the request line is one
    /// string because SSH has no argument vector, `command::split` is the whole
    /// grammar this daemon reads it with, and a client that wants a pipe asks
    /// for `shell -c` by name.
    ///
    /// A line this daemon will not run is refused on the channel's stderr with
    /// [`EXIT_REFUSED`], never by leaving the channel open.
    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel_id)?;
        let Ok(line) = std::str::from_utf8(data) else {
            self.refuse("the command is not UTF-8");
            return Ok(());
        };
        let argv = match command::split(line) {
            Ok(argv) => argv,
            Err(why) => {
                self.refuse(&why);
                return Ok(());
            }
        };
        let program = match command::resolve(&argv[0]) {
            Ok(program) => program,
            Err(why) => {
                self.refuse(&why);
                return Ok(());
            }
        };
        let argv = std::iter::once(program).chain(argv.into_iter().skip(1)).collect();
        self.run(argv, false);
        Ok(())
    }

    /// The one subsystem this daemon serves. Everything else is refused by
    /// name, which is what stops a client waiting on a channel that will never
    /// answer.
    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            session.channel_success(channel_id)?;
            self.run_sftp();
            return Ok(());
        }
        println!("sshd: {}: refused the {name:?} subsystem; this daemon serves sftp", self.peer);
        session.channel_failure(channel_id)?;
        Ok(())
    }

    /// Refused rather than ignored: russh's default leaves a `want_reply`
    /// request unanswered, and a client that sent one waits for the answer.
    /// There is no environment to set — a program here inherits this daemon's.
    async fn env_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        _value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        println!("sshd: {}: refused to set {name:?}; this daemon sets no environment", self.peer);
        session.channel_failure(channel_id)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.is_pty = true;
        session.channel_success(channel)?;
        Ok(())
    }
}

fn main() {
    println!("sshd: starting...");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async {
        // A machine with no netd has nothing for this daemon to offer, and
        // `NetdNotFound` is the only error that means that — std maps it to
        // NotConnected. Every other bind failure panics rather than exiting 0
        // with a line blaming hardware that is fine.
        let listener = match tokio::net::TcpListener::bind("0.0.0.0:22").await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
                println!("sshd: no network on this machine, exiting");
                return;
            }
            Err(e) => panic!("sshd: cannot bind 0.0.0.0:22: {e}"),
        };

        // Identity and trust are settled after the bind, so that a machine with
        // no NIC still reports the network as the reason it is leaving rather
        // than minting a key it will never present.
        let host_key = match host_key() {
            Ok(key) => key,
            Err(why) => {
                println!("sshd: {why}, exiting");
                return;
            }
        };
        println!(
            "sshd: host identity {}",
            host_key.public_key().fingerprint(HashAlg::Sha256)
        );

        // A daemon that can authenticate nobody is an open port, not a service.
        match authorized_key_count() {
            Ok(count) => println!("sshd: {count} key(s) authorized in total"),
            Err(why) => {
                println!("sshd: {why}, exiting");
                return;
            }
        }

        let config = Arc::new(russh::server::Config {
            // Public keys and nothing else: password and keyboard-interactive
            // are never offered, so there is no credential for a client to
            // guess. `russh`'s default is every method it implements.
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            auth_rejection_time: std::time::Duration::from_secs(1),
            nodelay: true,
            keys: vec![host_key],
            ..Default::default()
        });

        println!("sshd: listening on port 22");
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    println!("sshd: connection from {}", addr);
                    let config = config.clone();
                    let handler = SshServer.new_client(Some(addr));
                    tokio::spawn(async move {
                        match russh::server::run_stream(config, stream, handler).await {
                            Ok(session) => {
                                if let Err(e) = session.await {
                                    println!("sshd: session error: {:?}", e);
                                }
                            }
                            Err(e) => {
                                println!("sshd: run_stream error: {:?}", e);
                            }
                        }
                    });
                }
                Err(e) => {
                    println!("sshd: accept error: {:?}", e);
                }
            }
        }
    });
}

/// Which keys a file authorizes, and what becomes of a program's input. Host
/// tests — `cargo test --target "$(rustc -vV | sed -n 's/^host: //p')"` from
/// this directory; `userland/.cargo/config.toml` cross-compiles to ToyOS
/// otherwise. Real keys, and `ssh-key`'s own parser, so what is under test is
/// the decision and not a re-encoding of it.
#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    /// One `authorized_keys` line for a key, as `ssh-keygen` would write it.
    fn line(key: &PrivateKey) -> String {
        key.public_key().to_openssh().unwrap()
    }

    #[test]
    fn a_listed_key_is_authorized() {
        let mine = key();
        assert!(authorizes(&line(&mine), mine.public_key()));
    }

    #[test]
    fn an_unlisted_key_is_not_authorized() {
        let (mine, stranger) = (key(), key());
        assert!(!authorizes(&line(&mine), stranger.public_key()));
    }

    #[test]
    fn a_file_that_names_nobody_authorizes_nobody() {
        let stranger = key();
        for text in ["", "\n", "   \n\t\n", "# just a comment\n", "garbage\n"] {
            assert!(
                !authorizes(text, stranger.public_key()),
                "{text:?} authorized a key",
            );
        }
    }

    /// The load-bearing refusal: the key is listed, but under restrictions this
    /// daemon does not implement. Granting it would grant more than the file
    /// says, so it is granted nothing.
    #[test]
    fn a_key_listed_with_options_authorizes_nothing() {
        let mine = key();
        for options in [
            "command=\"/system/bin/shell -c ls\"",
            "no-pty",
            "restrict",
            "from=\"10.0.0.1\",no-agent-forwarding",
        ] {
            let text = format!("{options} {}\n", line(&mine));
            assert!(
                !authorizes(&text, mine.public_key()),
                "{options:?} let the key through unrestricted",
            );
        }
    }

    /// Keys are compared as key data, so the comment is not part of identity.
    #[test]
    fn the_comment_is_not_part_of_the_key() {
        let mine = key();
        let text = format!("{} jan@some-other-laptop\n", line(&mine));
        assert!(authorizes(&text, mine.public_key()));
    }

    #[test]
    fn a_key_is_found_among_several_and_blank_lines() {
        let (first, mine, last) = (key(), key(), key());
        let text = format!(
            "# my keys\n{}\n\n{}\n   \n{}\n",
            line(&first),
            line(&mine),
            line(&last),
        );
        for k in [&first, &mine, &last] {
            assert!(authorizes(&text, k.public_key()), "a listed key was missed");
        }
        assert!(!authorizes(&text, key().public_key()));
    }

    /// A line that is not a key must not disarm the keys around it.
    #[test]
    fn an_unparseable_line_does_not_disarm_the_rest() {
        let mine = key();
        let text = format!("not-a-key at all\n{}\n", line(&mine));
        assert!(authorizes(&text, mine.public_key()));
    }

    /// The bound this daemon spends on a program's input, against a queue
    /// nothing is draining. Wrapped in a ceiling of its own so that an `offer`
    /// which lost its bound fails here instead of hanging the suite.
    #[tokio::test]
    async fn input_nothing_takes_is_given_up_on() {
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(1);
        tx.send(b"fills the queue".to_vec()).await.expect("the first chunk fits");
        let bound = Duration::from_millis(20);
        let verdict = tokio::time::timeout(Duration::from_secs(5), offer(&tx, vec![0], bound))
            .await
            .expect("offer answered within its own bound");
        assert_eq!(verdict, Offered::Stalled);
    }

    #[tokio::test]
    async fn input_a_program_is_reading_is_taken() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1);
        assert_eq!(offer(&tx, b"hello".to_vec(), INPUT_STALL).await, Offered::Taken);
        assert_eq!(rx.recv().await.as_deref(), Some(&b"hello"[..]));
    }

    /// A program that has gone is not a stall: there is nowhere for the bytes
    /// to go, and the client is owed the answer now rather than in 30 seconds.
    #[tokio::test]
    async fn input_for_a_program_that_is_gone_is_not_a_stall() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        drop(rx);
        let verdict = tokio::time::timeout(Duration::from_secs(5), offer(&tx, vec![0], INPUT_STALL))
            .await
            .expect("offer answered at once");
        assert_eq!(verdict, Offered::Gone);
    }
}
