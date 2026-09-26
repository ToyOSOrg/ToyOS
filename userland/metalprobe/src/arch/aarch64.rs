//! AArch64.

/// Complete this CPU's stores: DSB ST waits until every store before it has
/// completed.
pub fn drain_stores() {
    // SAFETY: a barrier with no operands and no memory it can misuse.
    unsafe { std::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}
