//! `/system/bin/swap <service> <sha256> <length>`: a running service's binary
//! replaced by the one on standard input, handed to `/system/bin/init`, which
//! alone can swap it.
//!
//! An ordinary program: `ssh <machine> swap netd <sha256> <length> < netd`
//! runs it over a plain `exec` channel, and a local shell runs it the same
//! way. It holds the one connector to init's [`toyos_swap::PORT`] the build
//! lets any program hold, so what it can do is ask; every decision is init's
//! ([`toyos_swap`]'s header is the order).
//!
//! **The length is not optional, because an input's end says nothing.** A
//! connection that drops mid-upload ends this program's input exactly as the
//! end of the file does, so a binary read to the end of its input is whatever
//! arrived. The input is exactly `<length>` bytes, an input that ends sooner
//! is refused by name before init hears of it, and the digest is the caller's
//! end-to-end word on the bytes.
//!
//! After the answer this program waits for the input to close — the caller's
//! proof that it has the answer, which matters when the service being swapped
//! is the one carrying the caller's connection — at most
//! [`toyos_swap::ANSWER_MS`], and then lets init go.
//!
//! The answer is one line on standard output: `accepted <path>` with exit
//! status 0; `refused <why>`, init's refusal, with 1; or `unasked <why>`, this
//! program's own before init heard of the ask — so a caller knows init will
//! say nothing about it — with 1.

use std::io::{Read, Write};
use std::time::Duration;

use toyos::endow::Endowments;
use toyos::namespace::Namespace;
use toyos_swap::{Refusal, Request};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (answer, held) = run(&args);
    let line = match &answer {
        Answer::Accepted(path) => format!("accepted {path}"),
        Answer::Refused(why) => format!("refused {why}"),
        Answer::Unasked(why) => format!("unasked {why}"),
    };
    println!("{line}");
    std::io::stdout().flush().expect("the answer reaches standard output");
    if let Some(init) = held {
        go(init);
    }
    std::process::exit(if matches!(answer, Answer::Accepted(_)) { 0 } else { 1 });
}

/// What this program answers.
enum Answer {
    /// init verified and installed the binary; the path it runs from.
    Accepted(String),
    /// init refused, and said so in its own log.
    Refused(String),
    /// This program refused before init heard of the ask.
    Unasked(String),
}

/// What the caller said about the binary, and the binary.
struct Asked {
    service: String,
    digest: toyos_swap::Digest,
    body: Vec<u8>,
}

/// The answer, and the connection to init held until the go where init
/// accepted.
fn run(args: &[String]) -> (Answer, Option<toyos::ipc::Connection>) {
    match take(args) {
        Ok(asked) => ask(&asked),
        Err(why) => (Answer::Unasked(why.to_string()), None),
    }
}

/// The request out of the argument vector and exactly the input's promised
/// bytes.
fn take(args: &[String]) -> Result<Asked, Refusal> {
    let [service, digest, len] = args else {
        return Err(Refusal::Malformed("usage: swap <service> <sha256> <length>".into()));
    };
    let digest = toyos_swap::parse_hex(digest)
        .ok_or_else(|| Refusal::Malformed(format!("{digest:?} is not a SHA-256 in lowercase hex")))?;
    let len: u64 = len.parse().map_err(|_| Refusal::Malformed(format!("{len:?} is not a length")))?;
    if !toyos_swap::is_service_name(service) {
        return Err(Refusal::Malformed(format!("{service:?} is not a service name")));
    }
    if len > toyos_swap::MAX_BINARY_BYTES {
        return Err(Refusal::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    std::io::stdin().lock().read_exact(&mut body).map_err(|e| {
        Refusal::Malformed(format!("the input ended before the {len} bytes it promised: {e}"))
    })?;
    Ok(Asked { service: service.clone(), digest, body })
}

/// Stage the binary and ask init: its answer, and the connection this program
/// holds until the go where init accepted.
fn ask(asked: &Asked) -> (Answer, Option<toyos::ipc::Connection>) {
    let unasked = |why: String| (Answer::Unasked(why), None);
    let Some(held) = Endowments::get().take::<Namespace>(toyos_swap::LABEL) else {
        return unasked(Refusal::NoAuthority.to_string());
    };
    let init = match held.open(toyos_swap::PORT) {
        Ok(conn) => conn,
        Err(e) => return unasked(format!("init's swap port did not answer: {e:?}")),
    };
    let staged = toyos_swap::staged_path(&asked.service, u64::from(std::process::id()));
    let written = std::fs::create_dir_all(toyos_swap::STAGING).and_then(|()| std::fs::write(&staged, &asked.body));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&staged);
        return unasked(format!("{staged} could not be written: {e}"));
    }
    let request = Request { service: asked.service.clone(), staged, digest: asked.digest };
    if let Err(e) = init.send_bytes(toyos_swap::MSG_SWAP, &request.encode()) {
        let _ = std::fs::remove_file(&request.staged);
        return unasked(format!("init did not take the request: {e:?}"));
    }
    let answer = match init.recv_header() {
        Ok(answer) => answer,
        Err(e) => return (Answer::Refused(format!("init did not answer: {e:?}")), None),
    };
    let mut text = vec![0u8; answer.len() as usize];
    let said = match init.recv_bytes(&answer, &mut text) {
        Ok(n) => String::from_utf8_lossy(&text[..n]).into_owned(),
        Err(e) => return (Answer::Refused(format!("init's answer did not arrive whole: {e:?}")), None),
    };
    match answer.msg_type {
        toyos_swap::MSG_ACCEPTED => (Answer::Accepted(said), Some(init)),
        toyos_swap::MSG_REFUSED => (Answer::Refused(said), None),
        other => (Answer::Refused(format!("init answered message {other}, which is no swap answer")), None),
    }
}

/// Wait for the caller's go — its input closing — and let init go by
/// dropping the connection: **this program hanging up on init is what tells
/// it to stop the old service**.
fn go(init: toyos::ipc::Connection) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut rest = Vec::new();
        let _ = tx.send(std::io::stdin().read_to_end(&mut rest).map(|_| rest.len()));
    });
    match rx.recv_timeout(Duration::from_millis(toyos_swap::ANSWER_MS)) {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => eprintln!("swap: {n} bytes arrived after the binary; the swap goes regardless"),
        Ok(Err(e)) => eprintln!("swap: the input would not read to its end ({e}); the swap goes"),
        Err(_) => eprintln!(
            "swap: the input was not closed within {} ms of the answer; the swap goes",
            toyos_swap::ANSWER_MS
        ),
    }
    drop(init);
}
