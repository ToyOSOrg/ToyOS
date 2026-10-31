//! The generic timer is one system counter every CPU reads, at the rate
//! `CNTFRQ_EL0` states, which the boot keeps (`boot::clock`): there is no
//! second timer here to hold it to.

/// No reference to sample against.
pub fn check(_now: u64) {}

/// Now as a log line's time, for a process: the generic timer's clock, which
/// nothing here demotes.
pub fn stamp_now() -> u64 {
    crate::clock::stamp_at(super::cpu::counter())
}
