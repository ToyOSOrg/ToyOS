//! A user thread's timing protection: Arm's `PSTATE.DIT` (FEAT_DIT), which
//! EL0 writes itself. Every exception from EL0 saves it in `SPSR_EL1.DIT`, in
//! the thread's `trap::Frame`, and the return puts it back, so it follows the
//! thread across every switch and migration with no work here; a thread's
//! first entry to EL0 is with `SPSR_EL1` zero, DIT off. [`request`] reads and
//! writes the same bit in the caller's frame.

use super::{control_regs, percpu};

/// `SPSR_EL1.DIT` for an exception taken from AArch64.
const SPSR_DIT: u64 = 1 << 24;

/// Nothing: the frame carries the state.
#[derive(Clone, Copy)]
pub struct Thread;

impl Thread {
    pub const START: Self = Self;
}

/// The calling thread's `PSTATE.DIT` before this call, and with `set` that
/// value put in its frame: `None` on a machine without FEAT_DIT.
pub fn request(set: Option<bool>) -> Option<bool> {
    if !control_regs::dit() {
        return None;
    }
    // SAFETY: the frame of the syscall this call runs in, which does not
    // block, on this thread's kernel stack, and touched by nothing else
    // until the syscall returns into it.
    let frame = unsafe { &mut *percpu::syscall_frame() };
    let live = frame.spsr & SPSR_DIT != 0;
    match set {
        Some(true) => frame.spsr |= SPSR_DIT,
        Some(false) => frame.spsr &= !SPSR_DIT,
        None => {}
    }
    Some(live)
}
