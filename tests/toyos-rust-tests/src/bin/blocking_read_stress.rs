//! The lost-wake canary: a pipe ping-pong across processes, counted.
//!
//! Every round trip here is two parks and two posts on the completion core —
//! the reader blocks in `sys_read` on an empty pipe, the writer's post wakes
//! it, and the same happens back the other way. **The verdict is the count of
//! round trips**: a dropped completion parks this process, and the harness's
//! ceiling turns that into a red.
//!
//! The echo half is this same binary with an argument, so the two ends are one
//! file and the child's own parks are the same parks the parent's are.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Round trips. Enough that a wake lost at some rate shows up.
const ROUNDS: u32 = 500;

fn echo() -> ! {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            // The parent closed its end: the conversation is over.
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

    let exe = std::env::current_exe().expect("current_exe failed");
    let mut child = Command::new(&exe)
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the echo half");
    let mut to_child = child.stdin.take().expect("piped stdin");
    let mut from_child = child.stdout.take().expect("piped stdout");

    let mut completed = 0u32;
    for round in 0..ROUNDS {
        let sent = [(round % 251) as u8; 1];
        to_child.write_all(&sent).expect("write to the echo half");
        to_child.flush().expect("flush to the echo half");
        let mut got = [0u8; 1];
        from_child
            .read_exact(&mut got)
            .unwrap_or_else(|e| panic!("round {round} of {ROUNDS} never came back: {e}"));
        assert_eq!(got, sent, "round {round} came back as another byte");
        completed += 1;
    }

    drop(to_child);
    let status = child.wait().expect("wait for the echo half");
    assert!(status.success(), "the echo half exited with {status}");

    assert_eq!(
        completed, ROUNDS,
        "only {completed} of {ROUNDS} round trips completed",
    );
    println!("blocking_read_stress: {completed} round trips");
}
