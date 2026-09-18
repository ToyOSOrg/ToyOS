//! The load `dump-in-blocking-pass` files Ctrl+Alt+D inside: a victim, not a test.
//!
//! Two loads at once, one for each pass a Ring 3 program can drive that may not
//! serve the request: a pipe ping-pong with a child parks in blocking passes, and
//! a spawner's threads exit from a syscall while it waits for each. Every such
//! pass leaves a task it has just woken behind it and none of these tasks keeps
//! the CPU for a quantum, so on one CPU no idle check and no tick comes: a request
//! left pending there is served by nothing but the pass that the one who left it
//! owes.
//!
//! Nothing here asserts: the counts are the kernel's, and
//! `tests/common/faults.rs` holds the verdict.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Many times the pass the actuator stages at, so both requests are filed and
/// reported with the loads still running.
const ROUND_TRIPS: u32 = 1024;
const EXITS: u32 = 256;

fn echo() -> ! {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(0) | Err(_) => std::process::exit(0),
            Ok(_) => {}
        }
        if stdout.write_all(&byte).is_err() || stdout.flush().is_err() {
            std::process::exit(0);
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("echo") {
        echo();
    }

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(&exe)
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the echo half");
    let mut to_child = child.stdin.take().expect("piped stdin");
    let mut from_child = child.stdout.take().expect("piped stdout");

    let spawner = thread::spawn(|| {
        for _ in 0..EXITS {
            thread::spawn(|| {}).join().expect("join an exiting thread");
        }
    });

    let mut byte = [0u8; 1];
    for _ in 0..ROUND_TRIPS {
        to_child.write_all(&[0x5a]).expect("write to the echo half");
        to_child.flush().expect("flush to the echo half");
        from_child.read_exact(&mut byte).expect("read from the echo half");
    }

    drop(to_child);
    child.wait().expect("wait for the echo half");
    spawner.join().expect("join the spawner");
    println!("dump-stage-load: {ROUND_TRIPS} round trips, {EXITS} exits");
}
