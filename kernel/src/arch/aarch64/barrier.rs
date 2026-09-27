//! The orderings a device's view of memory needs, as AArch64 gives them.
//!
//! The same contract x86-64's `barrier` implements, and here every function is
//! an instruction: the Arm memory model is weakly ordered, and a device sits
//! outside the inner-shareable domain `fence` orders (`DMB ISH`). The outer
//! shareable `DMB OSH*` forms are what a DMA master observes (Arm ARM K.a,
//! B2.3.10 and D8.2.2); `DSB` is needed only where completion, not order, is
//! the point.

/// Every store to memory this CPU made before this call is visible to a
/// device's DMA reads before any store it makes after it — a descriptor before
/// the index that publishes it. `DMB OSHST`.
#[inline(always)]
pub fn dma_wmb() {
    // SAFETY: a barrier; reads and writes nothing.
    unsafe { core::arch::asm!("dmb oshst", options(nostack, preserves_flags)) };
}

/// Every load of device-written memory this CPU makes after this call sees at
/// least what the loads before it saw — a completion's index before the entry
/// it counts. `DMB OSHLD`.
#[inline(always)]
pub fn dma_rmb() {
    // SAFETY: a barrier; reads and writes nothing.
    unsafe { core::arch::asm!("dmb oshld", options(nostack, preserves_flags)) };
}

/// What an MMIO store is preceded by: [`dma_wmb`], so a register write that
/// starts a device's work comes after the memory that work reads.
#[inline(always)]
pub fn before_mmio_write() {
    dma_wmb();
}

/// What an MMIO load is followed by: [`dma_rmb`], so memory read after a
/// status register is at least as new as the status.
#[inline(always)]
pub fn after_mmio_read() {
    dma_rmb();
}

/// Every store this CPU made to the scanout has completed before this returns:
/// the scanout is Normal non-cacheable, whose stores may be gathered and held,
/// and `DSB ST` waits for every earlier store to complete.
#[inline(always)]
pub fn scanout_flush() {
    // SAFETY: a barrier; reads and writes nothing.
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}
