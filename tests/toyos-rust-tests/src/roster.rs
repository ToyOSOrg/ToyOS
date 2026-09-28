//! The kernel's thread roster as the tests that read it decode it:
//! `Rights::ROSTER` on the system capability test-runner endows every binary,
//! the one question in the ABI that says whether a thread is parked.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

/// `sched::payload::SCHED_BLOCKED` — the state column `ps` prints.
pub const BLOCKED: u8 = 2;

/// One thread of the roster.
pub struct Entry {
    pub pid: u32,
    /// A child thread, not its process's main one.
    pub is_thread: bool,
    pub state: u8,
}

/// The estate's system capability, taken once: a second `take` of the label
/// finds `HANDLE_INVALID`.
pub fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| {
        Endowments::get()
            .take(SYSCAP_LABEL)
            .expect("test-runner endows every binary it spawns a system capability")
    })
}

/// Every thread on the machine.
pub fn roster() -> Vec<Entry> {
    const HEADER: usize = toyos::system::SYSINFO_HEADER_SIZE;
    const ENTRY: usize = toyos::system::SYSINFO_ENTRY_SIZE;
    let mut buf = vec![0u8; HEADER + ENTRY * 256];
    let n = cap().roster(&mut buf);
    assert!((HEADER..=buf.len()).contains(&n), "sysinfo answered {n}");
    (HEADER..)
        .step_by(ENTRY)
        .take_while(|pos| pos + ENTRY <= n)
        .map(|pos| Entry {
            pid: u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()),
            is_thread: buf[pos + 9] != 0,
            state: buf[pos + 8],
        })
        .collect()
}

/// Whether `pid`'s main thread is parked.
pub fn main_thread_blocked(pid: u32) -> bool {
    roster().iter().any(|e| e.pid == pid && !e.is_thread && e.state == BLOCKED)
}

/// Poll until `cond` holds. The bound is a hang guard and not a timing
/// assumption: the `sysinfo` call inside `cond` is the loop's preemption point
/// (`thread::yield_now` is a spin hint on this platform).
pub fn await_true(what: &str, cond: impl Fn() -> bool) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < give_up, "{what}");
    }
}
