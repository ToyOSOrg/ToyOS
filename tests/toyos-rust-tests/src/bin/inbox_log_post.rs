//! A watch on the kernel's log is answered by the post a record makes.
//!
//! What the log holds for a reader is the reader's cursor's to say, which the
//! kernel does not hold, so no look can ask it: the post is the answer. A
//! reader arms its watch, a process ends and the kernel records it, and the
//! wait hands the reader its token; the record is then there to read. A watch
//! the post does not answer leaves the wait parked for good, and the harness's
//! deadline is what says so.

use std::process::Command;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record};
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;

const SELF_PATH: &str = "/system/bin/test_rs_inbox_log_post";

const LOG: u64 = 7;

fn main() {
    if std::env::args().nth(1).is_some() {
        return;
    }
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let poller = Poller::new(1);
    let mut tail = LogTail::new();
    let mut ended: Option<u32> = None;
    loop {
        // Armed before the log is read, as every reader of an edge arms. An
        // answer here is some other record's post: the watch is spent, and
        // the round begins again.
        poller.watch(&cap, READABLE, LOG);
        let mut spent = false;
        poller.wait(0, 0, |_| spent = true);
        if recorded(&mut tail, &cap, ended) {
            break;
        }
        if spent {
            continue;
        }
        // Only with a watch armed and unanswered: the record is the post it
        // waits for.
        let pid = *ended.get_or_insert_with(end_a_process);
        let mut tokens = Vec::new();
        poller.wait(1, u64::MAX, |token| tokens.push(token));
        assert_eq!(tokens, [LOG], "the wait after pid {pid}'s end was recorded");
    }
    println!("inbox_log_post: a record's post answered the log's watch");
}

/// Run a process to its end, which the kernel records, and answer its pid.
fn end_a_process() -> u32 {
    let mut child = Command::new(SELF_PATH).arg("end").spawn().expect("spawn a process to end");
    let pid = child.id();
    assert!(child.wait().expect("wait for it").success(), "the process that only ends");
    pid
}

/// Read every record the tail has not seen; whether one of them is the end of
/// `pid`.
fn recorded(tail: &mut LogTail, cap: &SysCap, pid: Option<u32>) -> bool {
    let end = pid.map(|pid| format!(" pid={pid} code=0 "));
    let mut records = vec![Record::EMPTY; 64];
    let mut found = false;
    loop {
        let read = tail.read(cap, &mut records).expect("read the kernel's records");
        if read.is_empty() {
            return found;
        }
        if let Some(end) = &end {
            found |= read.iter().any(|r| r.message().starts_with("exit: ") && r.message().contains(end));
        }
    }
}
