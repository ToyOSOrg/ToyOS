//! x86-64.

/// Drain this CPU's stores to a write-combining scanout: SFENCE empties the
/// WC buffers (SDM Vol. 3A §11.3.1), where the tail of a blit otherwise waits
/// for something unrelated to evict it.
pub(crate) fn drain_stores() {
    unsafe { core::arch::x86_64::_mm_sfence() };
}
