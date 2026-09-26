//! Deferred preemption: per-CPU `preempt_count` and `need_resched` words in
//! the architecture's per-CPU block, touched only through `arch::percpu`'s
//! accessors. `enable()`
//! calls `scheduler::do_preempt()` when the count drops to zero and
//! `need_resched` is set; every accessor no-ops before `PERCPU_READY`.

use core::sync::atomic::Ordering;

use crate::arch::percpu;

// Before `percpu::init_bsp` the per-CPU block is not there to read — corruption, not a fault.
#[inline]
fn percpu_ready() -> bool {
    crate::log::PERCPU_READY.load(Ordering::Relaxed)
}

#[inline]
pub fn count() -> u32 {
    if !percpu_ready() { return 0; }
    percpu::preempt_count()
}

/// Sets the raw preempt-depth word; `Hw::switch` uses this to swap in the incoming context's saved depth.
#[inline]
pub fn set_count(v: u32) {
    if !percpu_ready() { return; }
    percpu::set_preempt_count(v);
}

#[inline]
pub fn need_resched() -> bool {
    if !percpu_ready() { return false; }
    percpu::resched_owed()
}

#[inline]
pub fn set_need_resched() {
    if !percpu_ready() { return; }
    percpu::set_resched_owed(true);
}

#[inline]
pub fn clear_need_resched() {
    if !percpu_ready() { return; }
    percpu::set_resched_owed(false);
}

#[inline]
pub fn disable() {
    if !percpu_ready() { return; }
    percpu::preempt_count_up();
}

/// Drops the count without polling `need_resched`, for a caller about to reschedule anyway (see `sched::driver::pass_block`).
#[inline]
pub fn enable_no_resched() {
    if !percpu_ready() { return; }
    // The request stays set; the imminent reschedule serves it.
    percpu::preempt_count_down();
}

#[inline]
pub fn enable() {
    if !percpu_ready() { return; }
    percpu::preempt_count_down();
    // `do_preempt` clears `need_resched` itself; clearing here would drop a request racing a nested schedule.
    if count() == 0 && need_resched() && !faulting() {
        crate::scheduler::do_preempt();
    }
}

// A `fault_state` stuck non-zero costs this CPU its preemption for the rest of the boot.
#[inline]
fn faulting() -> bool {
    if !percpu_ready() { return false; }
    percpu::faulting()
}
