//! The machine's SSH server: a shell, a command, and files both ways.
//!
//! **This is the bench's answer path.** The ThinkPad the kernel is certified on
//! has no channel out but a cable, so the three things a test harness needs of
//! a machine — run this program, take this file, give me that one — are what
//! this daemon serves, over one authenticated connection and nothing else.
//!
//! Two rules run through every path below. **Nothing waits without a bound**:
//! a client that stops reading, a program that stops taking its input, a
//! process that closes its output and does not exit — each has a named ceiling
//! and a refusal that says which one expired, because the alternative is a
//! channel that a harness on the other side of a network cannot tell from a
//! slow one. And **nothing is answered before it is whole**: a partial SFTP
//! packet is buffered, never acted on.

mod command;
mod sftp;

use std::fs;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::keys::ssh_key::authorized_keys::AuthorizedKeys;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::sync::mpsc;

/// Where this machine keeps its SSH identity and the keys it trusts.
///
/// `/home` is the only mount that is both persistent and writable by userland:
/// `/boot` is `KernelOnly` because a process that can write it can make the
/// machine unbootable, `/tmp` is a tmpfs, and `/log` is the diagnostic
/// partition — it is FAT32 by design so that it can be read on another
/// machine, which is the last place a private key should be. On a machine
/// whose disk the kernel would not adopt, `/home` is itself a tmpfs and the
/// identity lasts one boot; the fingerprint is printed every start so that is
/// visible rather than silent.
///
/// There is no user model and no file permissions, so the host key is readable
/// by every process on the machine. That is a property of the system, not of
/// this daemon — see `issues/`.
const SSH_DIR: &str = "/home/root/.ssh";
const HOST_KEY: &str = "/home/root/.ssh/host_ed25519";

/// The two files that name who may log in, in the order a person would look.
///
/// **The second is why a freshly flashed machine can be reached at all.** A
/// bench boot mints an identity into a `/home` that may be a tmpfs and starts
/// with nothing in it, so a key that has to be *installed* before the first
/// login is a key nobody can install. `/system/etc/ssh_authorized_keys` is put
/// on the image at build time and is read-only from here; the writable file
/// under `/home` stays what a person adds a key to afterwards.
///
/// Neither file is protected from anything else on the machine — see
/// `issues/isolation/sshd-authorized-keys-unprotected.md`. Adding the image
/// file does not widen that: `/system` is the read-only root, so the new one is
/// the *less* reachable of the two.
const AUTHORIZED_KEYS: [&str; 2] =
    ["/home/root/.ssh/authorized_keys", "/system/etc/ssh_authorized_keys"];

/// How long a program on a channel may take none of the input a client is
/// sending before this daemon stops offering it.
///
/// The program keeps running with a closed stdin, which is what a program that
/// stopped reading has already decided it wants; what does not happen is the
/// session hanging on it.
const INPUT_STALL: Duration = Duration::from_secs(30);

/// How long a process that has closed both its output streams may take to
/// exit before this daemon ends it and says the status is lost.
///
/// A process's pipes are closed by its own teardown, so reaching this bound at
/// all means the child kept running with its output gone: nothing more will
/// arrive on the channel, and the client is owed an answer.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// How long an SFTP session may go without a request.
const SFTP_IDLE: Duration = Duration::from_secs(300);

/// How long a connection may carry no traffic at all before russh drops it.
/// Stated rather than inherited, so the bound is one this daemon owns.
const SESSION_IDLE: Duration = Duration::from_secs(600);

/// This daemon would not run what was asked. It is the shell's own convention
/// for a command that could not be executed, and the reason is on stderr.
const EXIT_REFUSED: u32 = 127;

/// The program ran and this daemon cannot say how it ended.
const EXIT_LOST: u32 = 254;

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
    /// end does it here. Every other path is byte-exact, which is what makes
    /// running a binary over `exec` mean anything.
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
                // The named refusal, on the stream a client reads diagnostics
                // off and in an exit status it can branch on. A spawn that
                // failed must never look like a program that ran and said
                // nothing.
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
        // blocks, and this runtime has one thread for every session on it.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (input, mut input_rx) = mpsc::channel::<Vec<u8>>(16);
        self.input = Some(input);
        std::thread::spawn(move || {
            while let Some(chunk) = input_rx.blocking_recv() {
                if stdin.write_all(&chunk).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
            // Dropping it closes the pipe, which is the child's EOF.
        });

        // stdout and stderr: one thread each onto one queue, so the order the
        // two arrived in is the order they go out in.
        let (tx, mut rx) = mpsc::channel::<(Stream, Vec<u8>)>(64);
        pump(child.stdout.take().expect("stdout was piped"), Stream::Out, tx.clone());
        pump(child.stderr.take().expect("stderr was piped"), Stream::Err, tx.clone());
        drop(tx);

        let name = argv[0].clone();
        let peer = self.peer.clone();
        tokio::spawn(async move {
            while let Some((stream, data)) = rx.recv().await {
                let data = if translate_newlines { crlf(&data) } else { data };
                let sent = match stream {
                    Stream::Out => out.data(&data[..]).await,
                    Stream::Err => out.extended_data(EXTENDED_STDERR, &data[..]).await,
                };
                if sent.is_err() {
                    break;
                }
            }
            let status = reap(&mut child, &name, &peer).await;
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
                    Err(sftp::Fatal(why)) => {
                        println!("sshd: sftp for {peer}: {why}");
                        break EXIT_REFUSED;
                    }
                };
                let Some(packet) = packet else {
                    // Nothing whole to act on: wait for more, bounded.
                    match tokio::time::timeout(SFTP_IDLE, input_rx.recv()).await {
                        Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
                        Ok(None) => break 0,
                        Err(_) => {
                            println!(
                                "sshd: sftp for {peer}: no request in {}s, closing",
                                SFTP_IDLE.as_secs()
                            );
                            break EXIT_REFUSED;
                        }
                    }
                    continue;
                };
                // The filesystem work is blocking and this runtime has one
                // thread; the server goes with it and comes back.
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
                    Err(sftp::Fatal(why)) => {
                        println!("sshd: sftp for {peer}: {why}");
                        break EXIT_REFUSED;
                    }
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

/// One of a child's output pipes onto the queue both of them share, on a thread
/// because the read is blocking and this runtime has one thread. The queue is
/// what keeps the order the two streams arrived in.
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

/// The child's exit status, or a named refusal in place of one.
///
/// Reached only after both of the child's output pipes have closed, which its
/// own teardown is what does — so the loop below almost always ends on its
/// first question. What it exists for is the child that closed them and kept
/// running: nothing further can arrive on the channel, so the client is
/// answered and the process this daemon started is ended.
async fn reap(child: &mut Child, name: &str, peer: &str) -> u32 {
    let deadline = Instant::now() + EXIT_WAIT;
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
        if Instant::now() >= deadline {
            println!(
                "sshd: {peer}: {name} closed its output and has not exited within {}s; ending it",
                EXIT_WAIT.as_secs()
            );
            let _ = child.kill();
            return EXIT_LOST;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_millis(25));
    }
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

    /// The offer, before the client has proved it holds the key. Refusing here
    /// is what stops a client signing for a key that could never be accepted,
    /// and it is where an unauthorized key gets named — a client that takes
    /// this answer never reaches `auth_publickey`.
    ///
    /// `russh`'s default for this one is `Accept`; every other auth callback it
    /// defaults to `Reject`, which is why `auth_password` is simply absent.
    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if is_authorized(key) {
            return Ok(Auth::Accept);
        }
        println!(
            "sshd: refused {user}: {} is authorized by no file",
            key.fingerprint(HashAlg::Sha256)
        );
        Ok(Auth::reject())
    }

    /// After russh has verified the signature. Checked again rather than
    /// trusting the offer above to have filtered: a client is free to sign
    /// without asking first, and that path must reach the same files.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let fingerprint = key.fingerprint(HashAlg::Sha256);
        if is_authorized(key) {
            println!("sshd: {user} authenticated with {fingerprint}");
            return Ok(Auth::Accept);
        }
        println!("sshd: refused {user}: {fingerprint} is authorized by no file");
        Ok(Auth::reject())
    }

    async fn data(
        &mut self,
        _channel_id: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(input) = self.input.as_ref() else { return Ok(()) };
        match tokio::time::timeout(INPUT_STALL, input.send(data.to_vec())).await {
            Ok(Ok(())) => {}
            // Whatever was reading this is gone; there is nowhere for the
            // client's bytes to go and nothing to say about it.
            Ok(Err(_)) => self.input = None,
            Err(_) => {
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
        // Every bind goes through netd, which exits on a machine with no NIC.
        // sshd has nothing to offer without one, so it says so and leaves
        // instead of dumping a tokio backtrace across the boot.
        //
        // Only for that error, though. `NetdNotFound` — no netd registered the
        // service name — is the one that means what the message says, and std
        // maps it to NotConnected. `AddrInUse`, a netd that died mid-request
        // (`NetError::Io`) and a pipe failure all arrive here too, and on a
        // laptop with a live link every one of them would have exited 0 with a
        // line blaming the hardware. Nothing supervises init's children, so
        // the message is the entire diagnostic.
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
            inactivity_timeout: Some(SESSION_IDLE),
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

/// Which keys a file authorizes. Host tests — `cargo test --target "$(rustc
/// -vV | sed -n 's/^host: //p')"` from this directory; `userland/.cargo/config.toml`
/// cross-compiles to ToyOS otherwise. Real keys, and `ssh-key`'s own parser,
/// so what is under test is the decision and not a re-encoding of it.
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

    /// The image file is read beside the writable one, and neither is
    /// preferred: a key in either authorizes, which is the whole of what the
    /// image file adds.
    #[test]
    fn the_image_file_is_read_beside_the_home_one() {
        assert!(AUTHORIZED_KEYS.contains(&"/system/etc/ssh_authorized_keys"));
        assert!(AUTHORIZED_KEYS.contains(&"/home/root/.ssh/authorized_keys"));
    }
}
