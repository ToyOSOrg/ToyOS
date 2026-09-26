//! `toyos_ssh` — what the harness does to a guest over the cable.
//!
//! One exchange per run, driven by positional arguments and answered on stdout
//! in one line per fact. **The exit status separates two different things**:
//! `0` means the exchange happened and the line above says what the guest
//! answered; `1` means this program could not complete it, and the line says
//! why. A test that conflated them would read a client bug as a guest verdict.
//!
//! ```text
//! toyos_ssh keygen  <private> <public>           → ok
//! toyos_ssh auth    <host> <port> <key>          → signed <yes|no>, then
//!                                                  authenticated
//!                                                | refused offering <methods>
//!                                                | asked to sign
//! toyos_ssh exec    <host> <port> <key> <out> <err> <command…>
//!                                                → exit <n> | no-exit-status
//! toyos_ssh feed    <host> <port> <key> <out> <err> <stdin> <command…>
//!                                                → env <refused|accepted>, exit <n>
//! toyos_ssh pipe    <host> <port> <key> <out> <err> <stdin> <command…>
//!                                                → exit <n> | no-exit-status
//! toyos_ssh abandon <host> <port> <key> <command…>       → ok <bytes>
//! toyos_ssh fire    <host> <port> <key> <command…>
//!                                                → accepted | refused | closed | silent
//! toyos_ssh put     <host> <port> <key> <local> <remote> → ok <bytes>
//! toyos_ssh get     <host> <port> <key> <remote> <local> → ok <bytes>
//! toyos_ssh list    <host> <port> <key> <remote> → entry <name> <size>…, ok <n>
//! toyos_ssh swap    <host> <port> <key> <service> <binary> <sha256>
//!                                                → accepted <path> | refused <why>
//!                                                | unasked <why> | unanswered <what>
//!                                                  (`swap <service> <sha256> <length>`
//!                                                  over `exec`; after `accepted`, the
//!                                                  program's input is held until this
//!                                                  one's stdin closes)
//! ```
//!
//! A program's stdout and stderr go to files rather than to this process's own,
//! because they are the bytes under test: a byte-exact comparison cannot be
//! made against a stream something else may also have written to.
//!
//! The guest's host key is accepted unseen, and no argument here could change
//! that: the machine mints a fresh identity every boot, so there is nothing to
//! pin it against. What is under test is the *guest's* judgement of this
//! client's key, and `auth` is the arm that asks for it to be refused.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use russh::Signer;
use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

/// The whole of one run's wall clock. A guest under TCG on a loaded host is
/// slow, and nothing here is a measurement — this is a liveness guard, so that
/// a stalled exchange ends in a named line rather than in the suite's own stall
/// verdict, which says only that a test did not finish.
const BOUND: Duration = Duration::from_secs(120);

/// The user this client authenticates as. The guest has no user model, so the
/// name is a string its daemon prints and nothing keys on.
const USER: &str = "root";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => return fail(&format!("no tokio runtime: {e}")),
    };
    match runtime.block_on(async { tokio::time::timeout(BOUND, run(&args)).await }) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(why)) => fail(&why),
        Err(_) => fail(&format!("nothing answered in {}s", BOUND.as_secs())),
    }
}

fn fail(why: &str) -> ExitCode {
    println!("error {why}");
    ExitCode::FAILURE
}

async fn run(args: &[String]) -> Result<(), String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["keygen", private, public] => keygen(private, public),
        ["auth", host, port, key] => auth(host, port, key).await,
        ["exec", host, port, key, out, err, command @ ..] => {
            exec(host, port, key, out, err, &command.join(" "), None).await
        }
        ["feed", host, port, key, out, err, stdin, command @ ..] => {
            exec(host, port, key, out, err, &command.join(" "), Some((stdin, true))).await
        }
        ["pipe", host, port, key, out, err, stdin, command @ ..] => {
            exec(host, port, key, out, err, &command.join(" "), Some((stdin, false))).await
        }
        ["abandon", host, port, key, command @ ..] => {
            abandon(host, port, key, &command.join(" ")).await
        }
        ["fire", host, port, key, command @ ..] => fire(host, port, key, &command.join(" ")).await,
        ["put", host, port, key, local, remote] => put(host, port, key, local, remote).await,
        ["get", host, port, key, remote, local] => get(host, port, key, remote, local).await,
        ["list", host, port, key, remote] => list(host, port, key, remote).await,
        ["swap", host, port, key, service, binary, digest] => {
            swap(host, port, key, service, binary, digest).await
        }
        _ => Err(format!("not a command this client has: {words:?}")),
    }
}

/// A key pair for one test, written where the caller asked.
///
/// The public half is one `authorized_keys` line, which is what the harness
/// stages into the image; the private half is what every other command below
/// authenticates with.
fn keygen(private: &str, public: &str) -> Result<(), String> {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .map_err(|e| format!("minting an ed25519 key: {e}"))?;
    let pem = key.to_openssh(LineEnding::LF).map_err(|e| format!("encoding the key: {e}"))?;
    write(private, pem.as_bytes())?;
    let line = key.public_key().to_openssh().map_err(|e| format!("encoding the public key: {e}"))?;
    write(public, format!("{line}\n").as_bytes())?;
    // The fingerprint the guest's daemon prints, so a console assertion can
    // name the key that was accepted or refused instead of trusting a count.
    println!("ok {}", key.public_key().fingerprint(HashAlg::Sha256));
    Ok(())
}

/// A signer that refuses to sign and remembers being asked.
///
/// **Being asked is the finding.** `authenticate_publickey` sends the key as a
/// probe with no signature and signs only under `USERAUTH_PK_OK`, so a guest
/// that turns an unauthorized key away at the offer never reaches this; a
/// guest that answers `PK_OK` to a stranger does, and there is nothing to do
/// with that request but record it.
struct NeverSigns {
    asked: bool,
}

/// The one thing that can go wrong here, and the trait's required conversion.
enum NoSignature {
    Asked,
    Send(russh::SendError),
}

impl From<russh::SendError> for NoSignature {
    fn from(e: russh::SendError) -> Self {
        NoSignature::Send(e)
    }
}

impl Signer for NeverSigns {
    type Error = NoSignature;

    #[allow(clippy::manual_async_fn)]
    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        _hash_alg: Option<HashAlg>,
        _to_sign: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        async move {
            self.asked = true;
            Err(NoSignature::Asked)
        }
    }
}

/// Offer the key and report which way it went, and whether the guest asked for
/// a signature before deciding. Both answers are `Ok` here: whether a key
/// *should* have been accepted is the caller's question, and this program's
/// failures are the ones that stopped it from asking.
async fn auth(host: &str, port: &str, key: &str) -> Result<(), String> {
    let (mut session, key) = start(host, port, key).await?;
    let mut signer = NeverSigns { asked: false };
    let outcome = session
        .authenticate_publickey_with(USER, key.public_key().clone(), None, &mut signer)
        .await;
    println!("signed {}", if signer.asked { "yes" } else { "no" });
    // The methods the server still offers after turning this key away are what
    // says a password could never have been guessed at: a server that answered
    // `password` here would be offering a credential.
    match outcome {
        Ok(AuthResult::Success) => println!("authenticated"),
        Ok(AuthResult::Failure { remaining_methods, .. }) => {
            let mut offered: Vec<String> =
                remaining_methods.iter().map(String::from).collect();
            offered.sort();
            println!("refused offering {}", offered.join(","));
        }
        Err(NoSignature::Asked) => println!("asked to sign"),
        Err(NoSignature::Send(e)) => return Err(format!("offering a key: {e}")),
    }
    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(())
}

/// Run a command and collect what came back on each of the channel's two
/// streams. With `stdin`, the file's bytes are sent to the program first, and
/// where it asks, an environment request is made before the exec so the caller
/// learns what the guest answers one with.
async fn exec(
    host: &str,
    port: &str,
    key: &str,
    out: &str,
    err: &str,
    command: &str,
    stdin: Option<(&str, bool)>,
) -> Result<(), String> {
    let session = connect(host, port, key).await?;
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("opening a session channel: {e}"))?;
    let feed = match stdin {
        Some((path, _)) => Some(std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?),
        None => None,
    };
    if stdin.is_some_and(|(_, probe)| probe) {
        channel
            .set_env(true, "TOYOS_SSH_PROBE", "1")
            .await
            .map_err(|e| format!("asking to set an environment variable: {e}"))?;
        match channel.wait().await {
            Some(ChannelMsg::Failure) => println!("env refused"),
            Some(ChannelMsg::Success) => println!("env accepted"),
            other => return Err(format!("an env request was answered {other:?}")),
        }
    }
    channel.exec(true, command).await.map_err(|e| format!("asking for {command:?}: {e}"))?;
    // **The input is sent beside the reading, never before it**: a program
    // that refuses its input and exits closes the channel with most of it
    // unsent, and a send waiting on a window that closed channel will never
    // open again is a client that never reads the status it was answered.
    // A program reading stdin sees its end when the writer shuts down: right
    // away, or after the bytes.
    let mut writer = channel.make_writer();
    let feeding = tokio::spawn(async move {
        if let Some(bytes) = feed {
            writer.write_all(&bytes).await?;
        }
        writer.shutdown().await
    });

    let (mut stdout, mut stderr, mut status) = (Vec::new(), Vec::new(), None);
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    // Whatever of the input the program never took is nobody's now.
    feeding.abort();
    write(out, &stdout)?;
    write(err, &stderr)?;
    match status {
        Some(status) => println!("exit {status}"),
        // Said rather than guessed: a channel that closed without one is the
        // finding, and a harness that saw `exit 0` here would report a pass.
        None => println!("no-exit-status"),
    }
    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(())
}

/// Start a program, wait for its first output so it is certainly running, and
/// then drop the connection without reading the rest — what a harness whose
/// client died looks like from the guest's side.
async fn abandon(host: &str, port: &str, key: &str, command: &str) -> Result<(), String> {
    let session = connect(host, port, key).await?;
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("opening a session channel: {e}"))?;
    channel.exec(true, command).await.map_err(|e| format!("asking for {command:?}: {e}"))?;
    let mut seen = 0;
    while let Some(message) = channel.wait().await {
        if let ChannelMsg::Data { data } = message {
            seen = data.len();
            break;
        }
    }
    if seen == 0 {
        return Err(format!("{command:?} produced nothing, so it may never have run"));
    }
    println!("ok {seen}");
    // Dropped rather than disconnected: the guest is owed no goodbye, and a
    // client that died would send none.
    drop(session);
    Ok(())
}

/// How long `fire` waits for the guest's answer to its request.
const FIRE_ANSWER: Duration = Duration::from_secs(20);

/// Ask for a program and report the guest's answer to the request, without
/// collecting the program's status: **a command that ends the machine never
/// exits**, so its status is not a thing any client can collect. An accepted
/// request's connection is held until the guest drops it, or [`FIRE_ANSWER`].
///
/// `closed` and `silent` are answers too — a machine that went down under the
/// request drops the connection with or without a goodbye, and what the caller
/// makes of that is the caller's, against its own evidence that it went down.
async fn fire(host: &str, port: &str, key: &str, command: &str) -> Result<(), String> {
    let session = connect(host, port, key).await?;
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("opening a session channel: {e}"))?;
    channel.exec(true, command).await.map_err(|e| format!("asking for {command:?}: {e}"))?;
    let answer = tokio::time::timeout(FIRE_ANSWER, async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return "accepted",
                Some(ChannelMsg::Failure) => return "refused",
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => return "closed",
                Some(_) => {}
            }
        }
    })
    .await
    .unwrap_or("silent");
    // **Held until the machine drops it**, or the bound: sshd ends a program
    // whose connection is gone, so a client that left the moment the request
    // was accepted could end the very `reboot` it asked for before it reached
    // its syscall.
    if answer == "accepted" {
        let _ = tokio::time::timeout(FIRE_ANSWER, async {
            while !matches!(channel.wait().await, Some(ChannelMsg::Close) | None) {}
        })
        .await;
    }
    println!("{answer}");
    // Dropped rather than disconnected: the machine this was fired at may
    // already be gone, and a goodbye to it is one more wait on nothing.
    drop(session);
    Ok(())
}

/// How long `swap` waits for the guest's answer once the binary is sent.
const SWAP_ANSWER: Duration = Duration::from_secs(60);

/// Send `binary` as `service`'s replacement, naming `digest` for it, and
/// report the guest's answer.
///
/// An ordinary `exec` of `swap <service> <sha256> <length>` with the binary on
/// its input. `digest` is the caller's and is sent as given — never computed
/// here — so a caller can name the wrong one and see it refused. **The input
/// is not closed after the binary**: the program answers, then waits for its
/// input to close — this program's stdin closing is the caller's go — because
/// the service being swapped may be the one carrying this connection.
/// `unanswered` is an answer too: whether it went is the machine's log's to
/// say.
async fn swap(
    host: &str,
    port: &str,
    key: &str,
    service: &str,
    binary: &str,
    digest: &str,
) -> Result<(), String> {
    let body = std::fs::read(binary).map_err(|e| format!("reading {binary}: {e}"))?;
    if toyos_swap::parse_hex(digest).is_none() {
        return Err(format!("{digest:?} is not a SHA-256 in lowercase hex"));
    }
    let session = connect(host, port, key).await?;
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("opening a session channel: {e}"))?;
    let command = format!("swap {service} {digest} {}", body.len());
    channel.exec(true, command.as_str()).await.map_err(|e| format!("asking for {command:?}: {e}"))?;
    channel.data(&body[..]).await.map_err(|e| format!("sending the binary: {e}"))?;
    let (mut stdout, mut status, mut line) = (Vec::new(), None, None);
    let answered = tokio::time::timeout(SWAP_ANSWER, async {
        // Until the answer's newline, or the channel's end for a program that
        // exited without one.
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    stdout.extend_from_slice(&data);
                    if let Some(end) = stdout.iter().position(|&b| b == b'\n') {
                        line = Some(String::from_utf8_lossy(&stdout[..end]).to_string());
                        return;
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Eof | ChannelMsg::Close => return,
                _ => {}
            }
        }
    })
    .await;
    match (answered, &line) {
        (Ok(()), Some(said)) if said.starts_with("accepted ") => {
            println!("{said}");
            // **The program's input closing is the go**: it stops waiting and
            // exits, and init stops the old service when it hangs up. The
            // caller — whose own connections that service may carry too — says
            // when, by closing this program's stdin.
            tokio::task::spawn_blocking(|| std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new()))
                .await
                .map_err(|e| format!("waiting for the go: {e}"))?
                .map_err(|e| format!("reading the go: {e}"))?;
            let _ = channel.eof().await;
        }
        (Ok(()), Some(said)) if said.starts_with("refused ") || said.starts_with("unasked ") => println!("{said}"),
        (Ok(()), _) => println!(
            "unanswered the channel ended after {:?}, status {status:?}",
            String::from_utf8_lossy(&stdout)
        ),
        (Err(_), _) => println!(
            "unanswered nothing in {}s after {:?}",
            SWAP_ANSWER.as_secs(),
            String::from_utf8_lossy(&stdout)
        ),
    }
    // Dropped rather than disconnected: the connection may be gone with the
    // service that carried it.
    drop(session);
    Ok(())
}

async fn put(host: &str, port: &str, key: &str, local: &str, remote: &str) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| format!("reading {local}: {e}"))?;
    let (session, sftp) = sftp(host, port, key).await?;
    // `create` and not `write`: this crate's `write` opens with `WRITE` alone,
    // which the draft says is an open of a file that already exists — a guest
    // that answered it with a new file would be the lenient one.
    let mut file =
        sftp.create(remote).await.map_err(|e| format!("creating {remote} on the guest: {e}"))?;
    file.write_all(&bytes).await.map_err(|e| format!("writing {remote} on the guest: {e}"))?;
    file.shutdown().await.map_err(|e| format!("closing {remote} on the guest: {e}"))?;
    println!("ok {}", bytes.len());
    finish(sftp, session).await;
    Ok(())
}

async fn get(host: &str, port: &str, key: &str, remote: &str, local: &str) -> Result<(), String> {
    let (session, sftp) = sftp(host, port, key).await?;
    let bytes =
        sftp.read(remote).await.map_err(|e| format!("reading {remote} off the guest: {e}"))?;
    write(local, &bytes)?;
    println!("ok {}", bytes.len());
    finish(sftp, session).await;
    Ok(())
}

async fn list(host: &str, port: &str, key: &str, remote: &str) -> Result<(), String> {
    let (session, sftp) = sftp(host, port, key).await?;
    let entries =
        sftp.read_dir(remote).await.map_err(|e| format!("listing {remote} on the guest: {e}"))?;
    let mut names: Vec<String> = entries
        .map(|entry| format!("entry {} {}", entry.file_name(), entry.metadata().size.unwrap_or(0)))
        .collect();
    names.sort();
    for name in &names {
        println!("{name}");
    }
    println!("ok {}", names.len());
    finish(sftp, session).await;
    Ok(())
}

/// The guest's host key is accepted unseen — see this program's own note.
struct Trusting;

impl client::Handler for Trusting {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// A connected but unauthenticated session, and the key to authenticate it
/// with. Split out because `auth` reports the refusal that every other command
/// treats as a failure.
async fn start(host: &str, port: &str, key: &str) -> Result<(Handle<Trusting>, PrivateKey), String> {
    let port: u16 = port.parse().map_err(|_| format!("{port:?} is not a port number"))?;
    let pem = std::fs::read(key).map_err(|e| format!("reading the key {key}: {e}"))?;
    let key = PrivateKey::from_openssh(&pem)
        .map_err(|e| format!("{key} is not an OpenSSH private key: {e}"))?;
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(BOUND),
        ..Default::default()
    });
    let session = client::connect(config, (host, port), Trusting)
        .await
        .map_err(|e| format!("connecting to {host}:{port}: {e}"))?;
    Ok((session, key))
}

async fn connect(host: &str, port: &str, key: &str) -> Result<Handle<Trusting>, String> {
    let (mut session, key) = start(host, port, key).await?;
    let outcome = session
        .authenticate_publickey(USER, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .map_err(|e| format!("authenticating: {e}"))?;
    if !outcome.success() {
        return Err("the guest refused this key".to_string());
    }
    Ok(session)
}

async fn sftp(host: &str, port: &str, key: &str) -> Result<(Handle<Trusting>, SftpSession), String> {
    let session = connect(host, port, key).await?;
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("opening a session channel: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("asking for the sftp subsystem: {e}"))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("starting an sftp session: {e}"))?;
    Ok((session, sftp))
}

async fn finish(sftp: SftpSession, session: Handle<Trusting>) {
    let _ = sftp.close().await;
    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
}

fn write(path: &str, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, bytes).map_err(|e| format!("writing {path}: {e}"))
}
