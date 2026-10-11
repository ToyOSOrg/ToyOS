//! x86-64.

/// Drain this CPU's stores out of the write-combining fill buffers.
pub fn drain_stores() {
    // SAFETY: `sfence` has no operands and no memory it can misuse; the
    // target is x86-64, where the instruction always exists.
    unsafe { std::arch::x86_64::_mm_sfence() };
}
