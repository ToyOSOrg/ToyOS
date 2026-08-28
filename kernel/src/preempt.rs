//! Deferred preemption: per-CPU `preempt_count` and `need_resched` words in
//! `arch::percpu::PerCpu`, touched only through `arch::percpu::gs`. `enable()`
//! calls `scheduler::do_preempt()` when the count drops to zero and
//! `need_resched` is set; every accessor no-ops before `PERCPU_READY`.

use core::sync::atomic::Ordering;

use crate::arch::percpu::{gs, OFF_FAULT_STATE, OFF_NEED_RESCHED, OFF_PREEMPT_COUNT};

// Before `percpu::init_bsp`, `gs:[N]` reads low identity-mapped memory — corruption, not a fault.
#[inline]
fn percpu_ready() -> bool {
    crate::log::PERCPU_READY.load(Ordering::Relaxed)
}

#[inline]
pub fn count() -> u32 {
    if !percpu_ready() { return 0; }
    gs::read_u32::<OFF_PREEMPT_COUNT>()
}

/// Sets the raw preempt-depth word; `Hw::switch` uses this to swap in the incoming context's saved depth.
#[inline]
pub fn set_count(v: u32) {
    if !percpu_ready() { return; }
    gs::write_u32::<OFF_PREEMPT_COUNT>(v);
}

#[inline]
pub fn need_resched() -> bool {
    if !percpu_ready() { return false; }
    gs::read_u8::<OFF_NEED_RESCHED>() != 0
}

#[inline]
pub fn set_need_resched() {
    if !percpu_ready() { return; }
    gs::write_u8_imm::<OFF_NEED_RESCHED, 1>();
}

#[inline]
pub fn clear_need_resched() {
    if !percpu_ready() { return; }
    gs::write_u8_imm::<OFF_NEED_RESCHED, 0>();
}

#[inline]
pub fn disable() {
    if !percpu_ready() { return; }
    gs::lock_inc_u32::<OFF_PREEMPT_COUNT>();
}

/// Drops the count without polling `need_resched`, for a caller about to reschedule anyway (see `waitq`'s park).
#[inline]
pub fn enable_no_resched() {
    if !percpu_ready() { return; }
    // The request stays set; the imminent reschedule serves it.
    gs::lock_dec_u32::<OFF_PREEMPT_COUNT>();
}

#[inline]
pub fn enable() {
    if !percpu_ready() { return; }
    gs::lock_dec_u32::<OFF_PREEMPT_COUNT>();
    // `do_preempt` clears `need_resched` itself; clearing here would drop a request racing a nested schedule.
    if count() == 0 && need_resched() && !faulting() {
        crate::scheduler::do_preempt();
    }
}

// A `fault_state` stuck non-zero costs this CPU its preemption for the rest of the boot.
#[inline]
fn faulting() -> bool {
    if !percpu_ready() { return false; }
    gs::read_u8::<OFF_FAULT_STATE>() != 0
}
