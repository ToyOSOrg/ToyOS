//! A thread killed inside any wait the kernel has for it leaves, and its
//! process ends.
//!
//! One child per wait, each killed after it says it is about to park: a futex,
//! a poll ring, a process's end, a thread's end and a sleep. A wait the kill
//! cannot end keeps the child's last thread in its process for ever, and
//! `wait` below never returns; the harness's deadline is what says so.
//! `kill_while_blocked` holds the pipe, connection and accept waits, and
//! `mutual_kill` a kill inside a kill.

use std::io::{Read, Write};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use toyos::endow::Endowments;
use toyos::poller::Poller;
use toyos::process::Process;
use toyos::AsHandle;
use toyos_abi::syscall;
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_kill_ends_every_wait";

/// The label the process-wait child finds the process it waits on under.
const WAITED: &str = "waited";

/// `process::KILLED_EXIT_CODE`.
const KILLED: i32 = 137;

// `sleep` runs before `process-wait` and `thread-join`: both of those arms also
// sleep underneath (the waited process's `nanosleep`, the joined thread's
// `std::thread::sleep`), so a mutation that breaks sleep would otherwise surface
// under one of their names instead of its own.
const WAITS: [&str; 5] = ["futex", "poll", "sleep", "process-wait", "thread-join"];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(role) => child(role),
        None => test(),
    }
}

fn test() {
    for role in WAITS {
        // The process the process-wait child waits on, which nothing ends but this.
        let mut waited = (role == "process-wait").then(|| spawn("sleep", None));
        let endow = waited.as_ref().map(|w| {
            let dup = syscall::dup(RawHandle(w.as_raw_handle())).expect("a handle to endow");
            (WAITED.to_string(), dup.0)
        });
        let mut child = spawn(role, endow);
        println!("  {role}: killing");
        child.kill().expect("kill the parked child");
        let code = child.wait().expect("wait for the killed child").code();
        assert_eq!(code, Some(KILLED), "a child killed in its {role} wait ended with {code:?}");
        if let Some(mut waited) = waited.take() {
            waited.kill().expect("kill the waited process");
            waited.wait().expect("wait for the waited process");
        }
        println!("  {role}: a kill ended it");
    }
    println!("kill_ends_every_wait: every wait a kill reached, it ended");
}

/// Spawn a child in `role` and read its marker: it is about to park.
fn spawn(role: &str, endow: Option<(String, u32)>) -> Child {
    let mut command = Command::new(SELF_PATH);
    command.arg(role).stdout(Stdio::piped());
    if let Some((label, handle)) = endow {
        command.endow(&label, handle);
    }
    let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn {role}: {e}"));
    let out = child.stdout.as_mut().expect("child stdout");
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while out.read(&mut byte).expect("read the child's marker") == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    assert_eq!(String::from_utf8_lossy(&line), format!("parked in {role}"), "{role} never reached its wait");
    child
}

fn child(role: &str) -> ! {
    match role {
        "futex" => {
            static WORD: AtomicU32 = AtomicU32::new(0);
            say(role);
            // SAFETY: `WORD` is a live, aligned `u32` for the whole program.
            unsafe { syscall::futex_wait(WORD.as_ptr(), 0, None) };
        }
        "poll" => {
            let poller = Poller::new(1);
            say(role);
            poller.wait(1, u64::MAX, |_| {});
        }
        "process-wait" => {
            let waited: Process = Endowments::get().take(WAITED).expect("the parent endowed a process");
            say(role);
            let _ = syscall::process_wait(waited.as_handle());
        }
        "thread-join" => {
            let parked = std::thread::spawn(|| loop {
                std::thread::sleep(Duration::from_secs(3600));
            });
            say(role);
            let _ = parked.join();
        }
        "sleep" => {
            say(role);
            syscall::nanosleep(u64::MAX);
        }
        other => panic!("unknown role {other:?}"),
    }
    panic!("{role} came back from a wait nothing ends");
}

fn say(role: &str) {
    let mut out = std::io::stdout();
    out.write_all(format!("parked in {role}\n").as_bytes()).expect("say");
    out.flush().expect("flush");
}
