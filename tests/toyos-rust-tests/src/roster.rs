//! This process's threads, as the kernel's roster publishes them: the one
//! place a process can ask whether a thread of its own is parked.
//!
//! Each binary includes this file whole and uses its own part of it.
#![allow(dead_code)]

use toyos::syscap::SysCap;
use toyos_abi::syscall;

/// `sched::payload::SCHED_RUNNING` — the state column `ps` prints.
pub const RUNNING: u8 = 0;

/// `sched::payload::SCHED_BLOCKED`.
pub const BLOCKED: u8 = 2;

/// `SCHED_UNKNOWN`, which `sys_sysinfo` also answers for a thread whose entry
/// is a zombie. A live thread's scheduler record is installed under the same
/// table lock that inserts its entry, so a thread of ours reading this has
/// exited and nothing else.
pub const ZOMBIE: u8 = 3;

/// This process's threads as the kernel publishes them: `(is a child thread,
/// scheduler state)`.
pub fn my_threads(cap: &SysCap) -> Vec<(bool, u8)> {
    threads_of(cap, syscall::getpid().raw())
}

/// Process `pid`'s threads, as [`my_threads`] reads this one's.
///
/// Even one's own threads arrive in the machine-wide roster, which is
/// `Rights::ROSTER` on a `SysCap` — there is no narrower question in the ABI,
/// and `tests/testcases` names `roster` on the test-runner row for this.
pub fn threads_of(cap: &SysCap, pid: u32) -> Vec<(bool, u8)> {
    const HEADER: usize = toyos::system::SYSINFO_HEADER_SIZE;
    const ENTRY: usize = toyos::system::SYSINFO_ENTRY_SIZE;
    const ENTRIES: usize = 1024;
    let mut buf = vec![0u8; HEADER + ENTRY * ENTRIES];
    let n = cap.roster(&mut buf);
    assert!((HEADER..=buf.len()).contains(&n), "sysinfo answered {n}");
    // Past the buffer the roster is cut short, and a thread cut off is one never found.
    let live = toyos_abi::syscall::SysinfoHeader::decode(buf[..HEADER].try_into().unwrap()).entries;
    assert!(live as usize <= ENTRIES, "the machine has {live} threads and the roster holds {ENTRIES}");
    (HEADER..)
        .step_by(ENTRY)
        .take_while(|pos| pos + ENTRY <= n)
        .filter(|&pos| u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) == pid)
        .map(|pos| (buf[pos + 9] != 0, buf[pos + 8]))
        .collect()
}

/// Poll until `cond` holds, with no deadline: a state the kernel never reaches
/// is a hang the harness ceiling reds.
pub fn await_true(cond: impl Fn() -> bool) {
    while !cond() {}
}
