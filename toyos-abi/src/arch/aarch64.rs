use crate::{tcb, Tid};

/// Where a thread finds its own [`Tid`]: the kernel writes it into the thread
/// control block at `TP + TCB_TID` before the thread's first instruction. The
/// word is the thread's own memory, so what it says is the thread's word about
/// itself and nothing more.
pub const TCB_TID: usize = tcb::AARCH64_TID;

/// The calling thread's id, read off its control block without a syscall.
#[inline]
pub fn current_tid() -> Tid {
    let tp: u64;
    // SAFETY: a read of `TPIDR_EL0`, the thread pointer, into a register.
    unsafe { core::arch::asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack)) };
    // SAFETY: `TP + TCB_TID` lies inside the psABI's TCB at TP, below the
    // first TLS block (`tcb::AARCH64_BYTES`).
    Tid(unsafe { core::ptr::read_volatile((tp as usize + TCB_TID) as *const u32) })
}

/// The counter the clock page's two words describe: the generic timer's
/// virtual count, which EL0 may read, read after every earlier instruction, as
/// the x86-64 one is.
#[inline]
pub fn counter() -> u64 {
    let now: u64;
    // SAFETY: an `isb` and a read of `CNTVCT_EL0` into a register; neither
    // touches memory.
    unsafe { core::arch::asm!("isb", "mrs {now}, cntvct_el0", now = out(reg) now, options(nomem, nostack)) };
    now
}
