//! The orderings a device's view of memory needs, as x86-64 gives them.
//!
//! Every function here is the contract both architectures implement; on x86-64
//! each is a compiler barrier and no instruction. TSO keeps a CPU's
//! write-back stores in program order and its loads in program order (SDM
//! Vol. 3A §9.2.2), and a store to an uncacheable register is not reordered
//! with an older store — so what a device sees follows program order once the
//! compiler is held to it. Write-combining memory is the exception, and its
//! one user orders it with its own `sfence`.

use core::sync::atomic::{compiler_fence, Ordering};

/// Every store to memory this CPU made before this call is visible to a
/// device's DMA reads before any store it makes after it — a descriptor before
/// the index that publishes it.
#[inline(always)]
pub fn dma_wmb() {
    compiler_fence(Ordering::Release);
}

/// Every load of device-written memory this CPU makes after this call sees at
/// least what the loads before it saw — a completion's index before the entry
/// it counts.
#[inline(always)]
pub fn dma_rmb() {
    compiler_fence(Ordering::Acquire);
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

/// Every store this CPU made to the scanout reaches the display before this
/// returns: the scanout is write-combining, and its stores can sit in a buffer
/// with nothing to evict them. `SFENCE` (SDM Vol. 3A §11.3.1) is the only way
/// to drain one.
#[inline(always)]
pub fn scanout_flush() {
    // SAFETY: `SFENCE` touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}
