//! `/system/bin/logout`: the login sessions open, asked of the supervisor.
//!
//! - `logout`: every session, one line each: its number, `desktop` or
//!   `remote`, `active` or `ending`, and what an ending one waits for.
//! - `logout <session>`: sign it out: ask its apps to quit, and wait for them.
//! - `logout <session> --force`: sign it out, ending whatever still runs.
//!
//! It holds the one `session-admin` connector the build lets a program hold,
//! minted with its row and session, so it is reached with `launch logout`;
//! the supervisor answers a login session's alone. The machine has one user,
//! so that is every session's.
//! The answer is the supervisor's text on standard output, with exit status 0
//! where it was done and 1 where it was refused.

use std::io::Write;

use toyos::endow::Endowments;
use toyos::namespace::Namespace;
use toyos_session::{self as wire, SignOut, SignOutSession};

const USAGE: &str = "usage: logout [<session> [--force]]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (done, text) = match request(&args) {
        Ok((msg, payload)) => ask(msg, &payload),
        Err(why) => (false, why.to_string()),
    };
    print!("{text}");
    if !text.ends_with('\n') && !text.is_empty() {
        println!();
    }
    std::io::stdout().flush().expect("the answer reaches standard output");
    std::process::exit(if done { 0 } else { 1 });
}

fn request(args: &[String]) -> Result<(u32, Vec<u8>), &'static str> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let sign_out = |share: &str, force| {
        let share = share.parse().map_err(|_| USAGE)?;
        Ok((wire::MSG_SIGN_OUT_SESSION, SignOutSession { share, sign_out: SignOut { force } }.encode().to_vec()))
    };
    match args[..] {
        [] => Ok((wire::MSG_LIST, Vec::new())),
        [share] => sign_out(share, false),
        [share, "--force"] => sign_out(share, true),
        _ => Err(USAGE),
    }
}

/// The supervisor's answer: whether it was done, and its text.
fn ask(msg: u32, payload: &[u8]) -> (bool, String) {
    let Some(held) = Endowments::get().take::<Namespace>(wire::ADMIN_PORT) else {
        return (false, "logout: this process holds no `session-admin` connector; run it with `launch logout`".into());
    };
    let supervisor = match held.open(wire::ADMIN_PORT) {
        Ok(conn) => conn,
        Err(e) => return (false, format!("logout: the supervisor's port refused: {e:?}")),
    };
    if let Err(e) = supervisor.send_bytes(msg, payload) {
        return (false, format!("logout: the request was not sent: {e:?}"));
    }
    let mut rx = Box::new(toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new());
    let answer = match rx.recv(&supervisor) {
        Ok(answer) => answer,
        Err(e) => return (false, format!("logout: the supervisor did not answer: {e:?}")),
    };
    let said = String::from_utf8_lossy(rx.payload(answer.payload_len)).into_owned();
    match answer.msg_type {
        wire::MSG_SESSIONS | wire::MSG_DONE => (true, said),
        wire::MSG_REFUSED => (false, said),
        other => (false, format!("logout: the supervisor answered message {other}, which is no answer of its port")),
    }
}
