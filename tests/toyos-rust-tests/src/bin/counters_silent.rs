//! A CPU that answers no round is read stale, and the read returns: the
//! round's bound ends the wait a silent CPU would otherwise hold.
//!
//! **A guest on the test kernel**, because a CPU silent past the bound is what
//! a span with its interrupts closed makes, or a host that has not run a vCPU,
//! and no guest can make either on demand. `SYS_DEBUG`'s `COUNTERS_DEAF` has
//! one CPU answer no round until `COUNTERS_HEAR`; the reader's wait, its bound
//! and its stale record are the shipped paths. Nothing is timed: a read that
//! waited without its bound reaches the harness's hang ceiling.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall::{self, debug_action, SyscallError};

fn read(cap: &SysCap) -> Vec<Record> {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    assert_eq!(cap.counters(&mut raw), Ok(raw.len()), "a read of every cpu");
    raw.iter().map(|r| Record::decode(r).expect("the kernel wrote a record that decodes")).collect()
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    let cpus = syscall::cpu_count() as u64;
    assert!(cpus >= 2, "one cpu has no other to silence");
    assert_eq!(
        syscall::debug_with(debug_action::COUNTERS_DEAF, cpus),
        SyscallError::InvalidArgument.to_u64(),
        "a cpu past the machine's was staged silent",
    );

    let silent = (cpus - 1) as usize;
    assert_eq!(syscall::debug_with(debug_action::COUNTERS_DEAF, silent as u64), 0);
    let held = read(&cap);
    let record = held[silent];
    assert!(record.stale, "cpu{silent} answers no round and was read fresh: {record:?}");
    // Stale is the last block it published, its bring-up's at least, and not nothing.
    assert!(record.get(Counter::Stamp).is_some(), "cpu{silent}'s last block was not read: {record:?}");

    assert_eq!(syscall::debug(debug_action::COUNTERS_HEAR), 0);
    let heard = loop {
        let records = read(&cap);
        if !records[silent].stale {
            break records[silent];
        }
    };
    assert!(heard.get(Counter::Stamp) > record.get(Counter::Stamp), "cpu{silent} answered with an old stamp");
    assert_eq!(heard.hardware_id, record.hardware_id, "cpu{silent}'s stale record named another cpu");
    println!("counters_silent: cpu{silent} was read stale while silent, and answered once heard");
}
