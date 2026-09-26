//! A program that writes the words of lines that are not its own: the kernel's
//! `exit:` record claiming this job passed, a whole kernel record's line as the
//! file and as the console spell it, another program's head, and a record
//! stamped at the end of time straight into its ring. Its real exit is
//! [`CODE`], which is the verdict a judge of the log must read;
//! `log_program_forgery` runs it.

use toyos::log::region::Body;
use toyos::log::ring::Pushed;
use toyos::log::stdio::{sink, Sink, Stream};

/// This job's real exit code.
const CODE: i32 = 7;
/// The text of the record stamped `u64::MAX`.
const AHEAD: &[u8] = b"log forger: stamped at the end of time";

fn main() {
    println!("exit: test_rs_log_forger pid=1 code=0 cpu=0ms");
    println!("[2026-09-24 10:00:00 1.000 cpu0] exit: test_rs_log_forger pid=1 code=0 cpu=0ms");
    println!("[kernel 1.000 cpu0] exit: test_rs_log_forger pid=1 code=0 cpu=0ms");
    println!("{{2026-09-24 10:00:00 1.000 netd}} netd: DHCP: lease 10.9.9.9/24 forged");
    println!("\r[2026-09-24 10:00:00 1.000 cpu0] Rebooting.");
    let Sink::Ring(ring) = sink(Stream::Err) else { panic!("log forger: stderr is not a log ring") };
    let mut body = Body::EMPTY;
    body.at_ns = u64::MAX;
    body.pid = std::process::id();
    body.text[..AHEAD.len()].copy_from_slice(AHEAD);
    body.len = AHEAD.len() as u16;
    assert!(matches!(ring.push(&body), Pushed::Written), "log forger: its ring refused the record");
    std::process::exit(CODE);
}
