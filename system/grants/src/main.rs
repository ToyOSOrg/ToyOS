//! `/system/bin/grants`: which folder of the session's home an installed
//! package is granted, asked of `/system/bin/supervisor`, which alone keeps
//! them (`toyos_manifest::grants`).
//!
//! - `grants list`: every grant, one line each: package, access, folder.
//! - `grants add <package> <folder> [read-only]`: grant the package that
//!   folder, read-write unless asked for less; its next launch starts in it.
//! - `grants revoke <package>`: take it away; its next launch holds no folder,
//!   and one running keeps it until it ends.
//!
//! It holds the one `grants` connector the build lets a program hold, minted
//! with its row and session, so what it can do is ask: the supervisor answers
//! a login session alone, and judges every folder itself. The answer is one
//! line on standard output, the supervisor's text, with exit status 0 where it
//! was done and 1 where it was refused.

use std::io::Write;

use toyos::endow::Endowments;
use toyos::namespace::Namespace;
use toyos_manifest::grants::{self, Access, Folder, Request};

const USAGE: &str = "usage: grants list | add <package> <folder> [read-only] | revoke <package>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (done, text) = match request(&args) {
        Ok(request) => ask(&request),
        Err(why) => (false, why.to_string()),
    };
    print!("{text}");
    if !text.ends_with('\n') && !text.is_empty() {
        println!();
    }
    std::io::stdout().flush().expect("the answer reaches standard output");
    std::process::exit(if done { 0 } else { 1 });
}

fn request(args: &[String]) -> Result<Request, &'static str> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let add = |package: &str, path: &str, access| Request::Add {
        package: package.to_string(),
        folder: Folder { path: path.to_string(), access },
    };
    match args[..] {
        ["list"] => Ok(Request::List),
        ["add", package, path] => Ok(add(package, path, Access::ReadWrite)),
        ["add", package, path, "read-only"] => Ok(add(package, path, Access::ReadOnly)),
        ["revoke", package] => Ok(Request::Revoke { package: package.to_string() }),
        _ => Err(USAGE),
    }
}

/// The supervisor's answer: whether it was done, and its text.
fn ask(request: &Request) -> (bool, String) {
    let Some(held) = Endowments::get().take::<Namespace>(grants::PORT) else {
        return (false, "this program holds no grants connector".to_string());
    };
    let supervisor = match held.open(grants::PORT) {
        Ok(conn) => conn,
        Err(e) => return (false, format!("the supervisor's grants port did not answer: {e:?}")),
    };
    let (msg_type, payload) = request.encode();
    if let Err(e) = supervisor.send_bytes(msg_type, &payload) {
        return (false, format!("the supervisor did not take the request: {e:?}"));
    }
    let answer = match supervisor.recv_header() {
        Ok(answer) => answer,
        Err(e) => return (false, format!("the supervisor did not answer: {e:?}")),
    };
    let mut text = vec![0u8; answer.len() as usize];
    let said = match supervisor.recv_bytes(&answer, &mut text) {
        Ok(n) => String::from_utf8_lossy(&text[..n]).into_owned(),
        Err(e) => return (false, format!("the supervisor's answer did not arrive whole: {e:?}")),
    };
    match answer.msg_type {
        grants::MSG_DONE => (true, said),
        grants::MSG_REFUSED => (false, said),
        other => (false, format!("the supervisor answered message {other}, which is no grants answer")),
    }
}
