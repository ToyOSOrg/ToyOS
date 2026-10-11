//! The park class nothing else exercises: waiting for a child, and joining a
//! thread, under volume.
//!
//! The one park site collapses P7 (child exit) and P8 (thread exit) onto
//! parks on the process object and on the thread's own watch, and **no other
//! gate reaches either**: `blocking_read_stress` is pipes, `cancel_while_parked`
//! and `killed_holder_releases` are disk and VFS. The tree's existing coverage is
//! ordering rather than volume — `process_lifecycle` has one arm on the wake
//! and `std_threading` joins four threads.
//!
//! **The verdict is a count of collected exit codes**; a lost publish parks
//! this process, and the harness's ceiling turns that into a red.
//!
//! **A child parks until the parent releases it, and that is what makes the
//! parent's wait a park.** A child on its own schedule has published its exit
//! before the wait asks, and the wait then reads a value.
//!
//! Every child is this binary with an argument, so what the parent waits for is
//! a real process exit and not a stub.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Children spawned, held on their own stdin, and released together.
const CHILDREN: u32 = 24;

/// Threads joined in a fan-in. Each exits on its own schedule, so the joiner
/// parks on some of them and not on others, and the count is what says every
/// one of those parks ended.
const THREADS: u32 = 24;

fn main() {
    if let Some(code) = std::env::args().nth(1) {
        // The child half: park in `read` until the parent drops the write end,
        // then exit with the code the parent chose.
        let mut byte = [0u8; 1];
        let _ = std::io::stdin().read(&mut byte);
        std::process::exit(code.parse::<i32>().expect("the parent passes an integer"));
    }

    let exe = std::env::current_exe().expect("current_exe failed");

    let mut children = Vec::new();
    let mut held = Vec::new();
    for i in 0..CHILDREN {
        let mut child = Command::new(&exe)
            .arg((i % 100).to_string())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn child {i}: {e}"));
        held.push(child.stdin.take().expect("the spawn was asked for a pipe"));
        children.push((i, child));
    }

    // The premise, asserted rather than left to timing: nothing has released a
    // child yet, so every wait below finds one running and parks.
    for (i, child) in &mut children {
        assert!(
            child.try_wait().expect("try_wait for a child this process spawned").is_none(),
            "child {i} exited before it was released, so its wait would have read a value",
        );
    }

    drop(held);

    let mut collected = 0u32;
    for (i, mut child) in children {
        let status = child.wait().unwrap_or_else(|e| panic!("wait for child {i}: {e}"));
        assert_eq!(
            status.code(),
            Some((i % 100) as i32),
            "child {i} answered with another process's code",
        );
        collected += 1;
    }

    // The thread half: each thread returns its own number, and the join is the
    // park.
    let joins: Vec<_> = (0..THREADS)
        .map(|i| {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(u64::from(i % 3)));
                i
            })
        })
        .collect();
    let mut joined = 0u32;
    for (i, handle) in joins.into_iter().enumerate() {
        let got = handle.join().unwrap_or_else(|_| panic!("join thread {i}"));
        assert_eq!(got, i as u32, "thread {i} answered for another one");
        joined += 1;
    }

    assert_eq!(collected, CHILDREN, "only {collected} of {CHILDREN} exits were collected");
    assert_eq!(joined, THREADS, "only {joined} of {THREADS} threads were joined");
    println!("exit_wait_storm: {collected} exits collected and {joined} threads joined");
}
