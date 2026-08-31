use core::mem::ManuallyDrop;

/// A value whose page-table entries are cleared but may still be reachable through another CPU's TLB until this drops.
///
/// This must never be dropped while holding a lock a target CPU could be spinning on with `IF` clear, since shootdown blocks on every other CPU.
#[must_use = "the pages are still reachable from another CPU until this is dropped"]
pub struct Unmapped<T>(ManuallyDrop<T>);

impl<T> Unmapped<T> {
    /// Wraps `value` once its page-table entries are cleared.
    pub fn new(value: T) -> Self {
        Self(ManuallyDrop::new(value))
    }

}

impl<T> Drop for Unmapped<T> {
    fn drop(&mut self) {
        crate::arch::tlb::shootdown(crate::arch::tlb::Origin::Unmap);
        // SAFETY: the wrapped value is never taken before this drop.
        unsafe { ManuallyDrop::drop(&mut self.0) };
    }
}
