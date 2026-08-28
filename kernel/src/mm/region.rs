//! `KernelSlice` is `Copy`, carries no lifetime, and can outlive its `Allocation`; tracked at `issues/design-debt/kernelslice-outlives-its-allocation.md`.

/// A kernel allocation that can vouch for its own extent.
/// # Safety
/// `size()` must be no more than what `ptr()` really owns, for as long as `self` is alive.
pub unsafe trait Allocation {
    /// First byte of the allocation, addressable by the kernel.
    fn ptr(&self) -> *mut u8;
    /// How many bytes were allocated.
    fn size(&self) -> usize;
}

/// Bounds-checked view into a contiguous kernel memory region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KernelSlice {
    base: *mut u8,
    size: usize,
}

// SAFETY: every access through `base` is its own `unsafe fn`, so sharing `(base, size)` across threads claims nothing about aliasing.
unsafe impl Send for KernelSlice {}
// SAFETY: see the `Send` impl above — same reasoning.
unsafe impl Sync for KernelSlice {}

impl KernelSlice {
    /// The whole of an allocation, sized by the allocation — the only constructor.
    pub fn whole(alloc: &impl Allocation) -> Self {
        Self { base: alloc.ptr(), size: alloc.size() }
    }

    pub fn size(&self) -> usize { self.size }
    pub fn base(&self) -> *mut u8 { self.base }

    /// Physical address of the base via the direct map.
    pub fn phys(&self) -> u64 {
        super::DirectMap::phys_of(self.base)
    }

    pub fn subslice(&self, offset: usize, size: usize) -> KernelSlice {
        assert!(offset + size <= self.size,
            "KernelSlice OOB: offset={:#x} size={:#x} total={:#x}", offset, size, self.size);
        KernelSlice {
            // SAFETY: `offset + size <= self.size` was just asserted, so the result stays inside the allocation `self` covers.
            base: unsafe { self.base.add(offset) },
            size,
        }
    }

    fn check(&self, offset: usize, len: usize) {
        assert!(offset + len <= self.size,
            "KernelSlice OOB: offset={:#x} len={} size={:#x}", offset, len, self.size);
    }

    /// # Safety: nothing may concurrently write `size_of::<T>()` bytes at `offset` while this read runs.
    pub unsafe fn read<T>(&self, offset: usize) -> T {
        self.check(offset, core::mem::size_of::<T>());
        core::ptr::read_unaligned(self.base.add(offset) as *const T)
    }

    /// # Safety: same as `read`, plus nothing else may concurrently read or write this range.
    pub unsafe fn write<T: Copy>(&self, offset: usize, value: T) {
        self.check(offset, core::mem::size_of::<T>());
        core::ptr::write_unaligned(self.base.add(offset) as *mut T, value);
    }

    /// # Safety: the returned slice must not alias a live `&mut` for as long as it is held.
    pub unsafe fn as_slice(&self) -> &[u8] {
        core::slice::from_raw_parts(self.base, self.size)
    }

    /// # Safety: same as `write`, for `src.len()` bytes at `offset`.
    pub unsafe fn copy_from(&self, offset: usize, src: &[u8]) {
        self.check(offset, src.len());
        core::ptr::copy_nonoverlapping(src.as_ptr(), self.base.add(offset), src.len());
    }

    /// # Safety: same as `write`, for the whole range.
    pub unsafe fn zero(&self) {
        core::ptr::write_bytes(self.base, 0, self.size);
    }
}
