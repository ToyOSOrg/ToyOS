//! AArch64.

/// Complete this CPU's stores to the scanout: DSB ST waits until every store
/// before it has completed.
pub(crate) fn drain_stores() {
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}
