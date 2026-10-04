//! `SYS_TRACE_READ`: the diary is the scheduler's own account of what it did,
//! read on a cursor the reader owns, and reading it is a right's.
//!
//! **A guest, because the subject is the scheduler's own records and the
//! syscall boundary**: which events the core emits, from which CPU, in what
//! order, and what the boundary refuses. No type or host test reaches either.
//! Nothing here is timed: a stamp is compared only with another.
//!
//! - A spawned child parked on a pipe until its parent writes: each pick of it
//!   that follows its park follows a wake of it, in the order read and on the
//!   clock.
//! - Two cursors at one position: one read to the end leaves the other where it
//!   was, and the other then reads every record the first did, unchanged.
//! - Every record decodes, and each CPU's come numbered without a gap and
//!   never earlier than the one before.
//! - The refusals: a capability without `TRACE` is refused and its buffer and
//!   cursor left as they were; a buffer of no records or short of a CPU, and a
//!   cursor ahead of a ring, are refused; a handle nobody holds ends the caller.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::handle::{Rights, HANDLE_INVALID};
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::trace::{TraceCursor, TraceRecord};
use toyos_trace::{Entry, Event, Thread};

const SELF_PATH: &str = "/system/bin/test_rs_trace_read";

/// `process::HANDLE_FAULT_EXIT_CODE`.
const HANDLE_FAULT: i32 = 139;

/// Records asked for at once.
const BATCH: usize = 512;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("waiter") => {
            let mut byte = [0u8];
            let read = std::io::stdin().read(&mut byte).expect("read the byte that wakes this child");
            return assert_eq!(read, 1, "the parent closed the pipe instead of writing");
        }
        Some("unheld") => {
            println!("reached unheld");
            std::io::stdout().flush().expect("flush the marker");
            let refused = syscall::trace_read(HANDLE_INVALID, &mut TraceCursor::new(), &mut [TraceRecord::EMPTY; 8]);
            panic!("a handle nobody holds was answered {refused:?} instead of ending the caller");
        }
        _ => {}
    }
    let mut first = TraceCursor::new();
    read_to_now(&mut first);
    let mut second = first;

    let mut child = Command::new(SELF_PATH).arg("waiter").stdin(Stdio::piped()).spawn().expect("spawn the waiter");
    let task = Thread { pid: child.id(), tid: 0 };
    assert_ne!(task.pid, 0, "the waiter ended before its pid was asked");
    // The wake under test is the write's, so it is made only once the diary
    // says the child parked on the pipe; a read writes no record, so this
    // wait cannot lap the ring `second` stands in.
    let mut read_first = Vec::new();
    let mut lost_first = 0;
    while !read_first.iter().any(|r| is(r, task, Event::Park)) {
        let (more, lost) = read_to_now(&mut first);
        read_first.extend(more);
        lost_first += lost;
    }
    child.stdin.take().expect("the waiter's stdin").write_all(&[1]).expect("wake the waiter");
    assert!(child.wait().expect("wait the waiter").success(), "the waiter failed");

    let (more, lost) = read_to_now(&mut first);
    read_first.extend(more);
    lost_first += lost;
    let start = second;
    let (read_second, lost_second) = read_to_now(&mut second);
    {
        let mut by: BTreeMap<(u16, String, u32), u64> = BTreeMap::new();
        let mut span: BTreeMap<u16, (u64, u64)> = BTreeMap::new();
        for r in &read_second {
            let k = match Entry::decode(r) {
                Ok(e) => match e.event {
                    Event::Mark { index } => format!("awake{index:#x}"),
                    ev => format!("{ev:?}").split([' ', '{']).next().unwrap().to_string(),
                },
                Err(_) => format!("bad{}", r.kind),
            };
            *by.entry((r.cpu, k, r.pid)).or_default() += 1;
            let s = span.entry(r.cpu).or_insert((u64::MAX, 0));
            s.0 = s.0.min(r.stamp);
            s.1 = s.1.max(r.stamp);
        }
        for cpu in 0..syscall::cpu_count() as usize {
            eprintln!("DIAG cpu{cpu} wrote {} stamps {:?}", second.0.next[cpu] - start.0.next[cpu], span.get(&(cpu as u16)));
        }
        for ((cpu, k, pid), n) in &by {
            eprintln!("DIAG cpu{cpu} {k} pid{pid} {n}");
        }
        eprintln!("DIAG test pid {} child pid {}", std::process::id(), task.pid);
        eprintln!("DIAG lost {lost_first} {lost_second}");
    }
    assert_eq!((lost_first, lost_second), (0, 0), "a quiet machine lapped a ring inside one spawn");
    let second_by_place: BTreeMap<(u16, u64), &TraceRecord> =
        read_second.iter().map(|r| ((r.cpu, r.seq), r)).collect();
    for r in &read_first {
        assert_eq!(second_by_place.get(&(r.cpu, r.seq)), Some(&r), "the second cursor did not read {r:?} as the first did");
    }

    let entries: Vec<Entry> =
        read_second.iter().map(|r| Entry::decode(r).unwrap_or_else(|why| panic!("{r:?}: {why}"))).collect();
    each_cpu_in_order(&entries);
    woken_before_picked(&entries, task);

    rights(&first);
    unheld();
    println!("trace_read: a wake precedes its pick, a cursor is its reader's own, and the diary is TRACE's");
}

/// Whether `record` is `event` happening to `task`.
fn is(record: &TraceRecord, task: Thread, event: Event) -> bool {
    Entry::decode(record).is_ok_and(|e| e.event == event && e.thread == Some(task))
}

/// The estate's capability, taken once: taking an endowment is a swap.
fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability"))
}

/// Every record from `cursor` on, until a read leaves its buffer short, and
/// the records lost on the way.
fn read_to_now(cursor: &mut TraceCursor) -> (Vec<TraceRecord>, u64) {
    let mut out = Vec::new();
    let mut lost = 0;
    let mut raw = vec![TraceRecord::EMPTY; BATCH];
    loop {
        let n = cap().trace(cursor, &mut raw).expect("the estate's capability reads the diary");
        lost += cursor.lost();
        out.extend_from_slice(&raw[..n]);
        if n < raw.len() {
            return (out, lost);
        }
    }
}

/// Each CPU's records come in its ring's order, without a gap, and never
/// stamped before the one ahead of them.
fn each_cpu_in_order(entries: &[Entry]) {
    let mut last: BTreeMap<u16, &Entry> = BTreeMap::new();
    for e in entries {
        if let Some(before) = last.insert(e.cpu, e) {
            assert_eq!(e.seq, before.seq + 1, "cpu{}'s ring skipped from {before:?} to {e:?}", e.cpu);
            assert!(e.stamp >= before.stamp, "cpu{} stamped {e:?} before {before:?}", e.cpu);
        }
    }
}

/// Every pick of `task` after it parked comes after a wake of it, in the
/// order read and on the clock; and it was woken and picked at least once.
fn woken_before_picked(entries: &[Entry], task: Thread) {
    let mut parked = false;
    let mut woken: Option<&Entry> = None;
    let mut picked_after_a_wake = 0;
    for e in entries.iter().filter(|e| e.event.names_its_task() && e.thread == Some(task)) {
        match e.event {
            Event::Park => parked = true,
            Event::Wake => {
                parked = false;
                woken = Some(e);
            }
            Event::Pick => {
                assert!(!parked, "{task:?} was picked at {e:?} while parked, with no wake between");
                if let Some(wake) = woken.take() {
                    assert!(e.stamp >= wake.stamp, "{task:?} was picked at {e:?}, stamped before its wake {wake:?}");
                    picked_after_a_wake += 1;
                }
            }
            _ => {}
        }
    }
    assert!(picked_after_a_wake > 0, "{task:?} slept and was never read woken and then picked");
}

fn rights(at: &TraceCursor) {
    let none = cap().narrowed(Rights::TRANSFER).expect("a cap carrying less");
    let mut cursor = *at;
    let mut buf = vec![TraceRecord::EMPTY; BATCH];
    assert_eq!(none.trace(&mut cursor, &mut buf), Err(SyscallError::PermissionDenied), "a cap without TRACE read the diary");
    // The demand is above every write, so a refusal leaves both as they were.
    assert!(buf.iter().all(|r| *r == TraceRecord::EMPTY), "a refused read wrote into the caller's buffer");
    assert_eq!(cursor, *at, "a refused read moved the caller's cursor");

    assert_eq!(cap().trace(&mut cursor, &mut []), Err(SyscallError::InvalidArgument), "a buffer of no records");
    let cpus = syscall::cpu_count() as usize;
    if cpus > 1 {
        let mut short = vec![TraceRecord::EMPTY; cpus - 1];
        assert_eq!(cap().trace(&mut cursor, &mut short), Err(SyscallError::InvalidArgument), "a buffer short of a cpu");
    }
    let mut ahead = *at;
    ahead.0.next[0] = u64::MAX;
    assert_eq!(cap().trace(&mut ahead, &mut buf), Err(SyscallError::InvalidArgument), "a cursor ahead of cpu0's ring");
}

/// In a child of its own, since the kernel's answer is to end the caller.
fn unheld() {
    let out = Command::new(SELF_PATH)
        .arg("unheld")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the unheld child")
        .wait_with_output()
        .expect("wait the unheld child");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "reached unheld", "the child never reached its call");
    assert_eq!(out.status.code(), Some(HANDLE_FAULT), "SYS_TRACE_READ took a handle nobody holds");
}
