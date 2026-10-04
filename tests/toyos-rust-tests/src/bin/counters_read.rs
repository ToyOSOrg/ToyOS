//! `SYS_COUNTERS`: every online CPU answers a read for itself, and what it
//! answers is a right's.
//!
//! **A guest, because the subject is the kick and the syscall.** A read is a
//! round of IPIs, each answered in the kick handler of the CPU it reached,
//! and the refusals are the syscall boundary's; no host test reaches either.
//! Nothing here is timed: the only clock is the harness's hang ceiling, which
//! a read waiting without its bound reaches.
//!
//! - The count: an empty buffer is answered the CPU count, which
//!   `SYS_CPU_COUNT` says too.
//! - A round every CPU answered, read until there is one: each CPU's record
//!   is in its place, carries its stamp, and names a hardware id no other
//!   does — a reader that filled every block itself would name its own on
//!   each. The next round: every stamp moved on, and every CPU but at most
//!   the reader's own took a kick in between, which a kick counted as anything
//!   else would not show.
//! - The rights: on `COUNTERS` alone the counters that time programs are
//!   absent and the rest as before; without it, and on a handle that is no
//!   capability, `PermissionDenied`; a buffer short of a CPU
//!   `ResourceExhausted`, one longer than any machine `InvalidArgument`; and
//!   a handle nobody holds ends the caller.
//! - Many readers at once, more than there are CPUs: every read returns.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::handle::{Rights, HANDLE_INVALID};
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_counters_read";

/// `process::HANDLE_FAULT_EXIT_CODE`.
const HANDLE_FAULT: i32 = 139;

/// Reads each of the many readers makes.
const READS: usize = 50;

/// What the harness waits for on a guest whose console is all it reads.
const SAID: &str = "counters_read: every cpu answered for itself, and each counter is a right's";

fn main() {
    if std::env::args().nth(1).as_deref() == Some("unheld") {
        println!("reached unheld");
        std::io::stdout().flush().expect("flush the marker");
        let refused = syscall::counters(HANDLE_INVALID, &mut [RawRecord::EMPTY]);
        panic!("a handle nobody holds was answered {refused:?} instead of ending the caller");
    }
    let cpus = syscall::cpu_count() as usize;
    assert_eq!(cap().counters(&mut []), Ok(cpus), "the empty buffer's count");

    let first = answered(cap());
    let second = loop {
        let next = answered(cap());
        // Equal stamps are the same round, joined; a later one is the next.
        if next.iter().zip(&first).any(|(n, f)| n.get(Counter::Stamp) != f.get(Counter::Stamp)) {
            break next;
        }
    };
    for (cpu, (f, s)) in first.iter().zip(&second).enumerate() {
        assert!(s.get(Counter::Stamp) > f.get(Counter::Stamp), "cpu{cpu}'s stamp did not move: {f:?} then {s:?}");
        assert_eq!(f.hardware_id, s.hardware_id, "cpu{cpu} named two hardware ids");
    }
    let kicked = first.iter().zip(&second).filter(|(f, s)| s.get(Counter::Kicks) > f.get(Counter::Kicks)).count();
    assert!(kicked + 1 >= cpus, "{kicked} of {cpus} cpus took a kick between two rounds: {first:?} then {second:?}");

    rights(&second);
    many_readers(cpus);
    unheld();
    println!("{SAID}");
}

/// The estate's capability, taken once: taking an endowment is a swap.
fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability"))
}

fn read(cap: &SysCap) -> Result<Vec<Record>, SyscallError> {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw)?;
    assert_eq!(n, raw.len(), "a read answered {n} of {} cpus", raw.len());
    Ok(raw.iter().map(|r| Record::decode(r).expect("the kernel wrote a record that decodes")).collect())
}

/// The first read in which every CPU answered, held to what one must say.
fn answered(cap: &SysCap) -> Vec<Record> {
    loop {
        let records = read(cap).expect("the estate's capability reads the counters");
        if records.iter().any(|r| r.stale) {
            continue;
        }
        for (cpu, r) in records.iter().enumerate() {
            assert_eq!(r.cpu as usize, cpu, "record {cpu} names cpu{}", r.cpu);
            assert!(r.get(Counter::Stamp).is_some(), "cpu{cpu} answered with no stamp: {r:?}");
            assert!(r.get(Counter::Kicks).is_some(), "the estate holds TRACE and cpu{cpu}'s kicks are absent");
            let ids = records.iter().filter(|o| o.hardware_id == r.hardware_id).count();
            assert_eq!(ids, 1, "{ids} cpus named hardware id {}: {records:?}", r.hardware_id);
        }
        return records;
    }
}

fn rights(full: &[Record]) {
    let counters_only = cap().narrowed(Rights::TRANSFER.union(Rights::COUNTERS)).expect("a cap carrying COUNTERS");
    let narrow = read(&counters_only).expect("COUNTERS alone reads the counters");
    for (r, f) in narrow.iter().zip(full) {
        for counter in Counter::ALL {
            let answered = r.get(counter).is_some();
            if counter.needs().contains(Rights::TRACE) {
                assert!(!answered, "COUNTERS alone was answered cpu{}'s {}", r.cpu, counter.name());
            } else if !r.stale {
                assert_eq!(answered, f.get(counter).is_some(), "cpu{}'s {} on COUNTERS alone", r.cpu, counter.name());
            }
        }
    }

    let none = cap().narrowed(Rights::TRANSFER).expect("a cap carrying less");
    assert_eq!(none.counters(&mut []), Err(SyscallError::PermissionDenied), "a cap without COUNTERS counted the cpus");
    assert_eq!(read(&none).err(), Some(SyscallError::PermissionDenied), "a cap without COUNTERS read the counters");
    let mut buf = vec![RawRecord::EMPTY; full.len()];
    assert_eq!(
        syscall::counters(RawHandle(1), &mut buf),
        Err(SyscallError::PermissionDenied),
        "a console was taken as a capability",
    );
    // The demand is above every write, so a refusal leaves the buffer as it was.
    assert!(buf.iter().all(|r| *r == RawRecord::EMPTY), "a refused read wrote into the caller's buffer");

    if full.len() > 1 {
        let mut short = vec![RawRecord::EMPTY; full.len() - 1];
        assert_eq!(cap().counters(&mut short), Err(SyscallError::ResourceExhausted), "a buffer short of a cpu");
    }
    let mut wide = vec![RawRecord::EMPTY; 4096];
    assert_eq!(cap().counters(&mut wide), Err(SyscallError::InvalidArgument), "a buffer longer than any machine");
}

/// Twice as many readers as CPUs, each reading over and over: rounds overlap
/// and join, and every read returns.
fn many_readers(cpus: usize) {
    std::thread::scope(|s| {
        for _ in 0..2 * cpus {
            s.spawn(|| {
                for _ in 0..READS {
                    assert_eq!(read(cap()).map(|r| r.len()), Ok(cpus), "a concurrent read");
                }
            });
        }
    });
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
    assert_eq!(out.status.code(), Some(HANDLE_FAULT), "SYS_COUNTERS took a handle nobody holds");
}
