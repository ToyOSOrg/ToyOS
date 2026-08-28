//! DMA memory, as a bounds-checked view rather than a raw pointer; [`DmaPool`] owns the pages, and [`Dma`] is the only safe way to touch them.

use alloc::vec::Vec;
use core::marker::PhantomData;
use core::ptr::{copy_nonoverlapping, read_unaligned, read_volatile, write_bytes,
                write_unaligned, write_volatile};

use super::pmm::PhysPage;
use super::DirectMap;

mod sealed {
    pub trait Sealed {}
}

/// Sealed: exactly two disciplines exist, [`Volatile`] and [`Unaligned`].
pub trait Discipline: sealed::Sealed {}

/// The discipline for memory that races the device.
pub enum Volatile {}
/// The discipline for memory the protocol has fenced.
pub enum Unaligned {}

impl sealed::Sealed for Volatile {}
impl sealed::Sealed for Unaligned {}
impl Discipline for Volatile {}
impl Discipline for Unaligned {}

/// A bounds-checked, `Copy` view of DMA memory, scoped to its [`DmaPool`]'s lifetime.
pub struct Dma<'pool, D: Discipline = Volatile> {
    base: *mut u8,
    size: usize,
    pool: PhantomData<&'pool DmaPool>,
    how: PhantomData<D>,
}

impl<D: Discipline> Clone for Dma<'_, D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D: Discipline> Copy for Dma<'_, D> {}

// SAFETY: `base` is a direct-mapped address, valid identically from any CPU; `Dma` is `Copy` with no lock, so sharing it shares only that address and length, never exclusive access to the memory it names.
unsafe impl<D: Discipline> Send for Dma<'_, D> {}
// SAFETY: see the `Send` impl above.
unsafe impl<D: Discipline> Sync for Dma<'_, D> {}

impl<'pool, D: Discipline> Dma<'pool, D> {
    #[inline]
    fn new(base: *mut u8, size: usize) -> Self {
        Self { base, size, pool: PhantomData, how: PhantomData }
    }

    /// How many bytes this view covers.
    #[inline]
    pub fn size(self) -> usize {
        self.size
    }

    /// The physical address of the first byte.
    #[inline]
    pub fn phys(self) -> u64 {
        DirectMap::phys_of(self.base)
    }

    /// The `size` bytes at `offset`, refused if not wholly inside `self`.
    #[inline]
    pub fn subview(self, offset: usize, size: usize) -> Self {
        self.check(offset, size);
        // SAFETY: `check` refused anything but `offset + size <= self.size`.
        Self::new(unsafe { self.base.add(offset) }, size)
    }

    /// Clear the whole view.
    #[inline]
    pub fn zero(self) {
        // SAFETY: exactly `self.size` bytes from `self.base`, the region this view covers.
        unsafe { write_bytes(self.base, 0, self.size) }
    }

    /// Copy `src` into the view at `offset`.
    #[inline]
    pub fn copy_from(self, offset: usize, src: &[u8]) {
        self.check(offset, src.len());
        // SAFETY: `check` bounded the destination for `src.len()` bytes; `src` and `self` cannot overlap since the heap is never allocated from DMA pages.
        unsafe { copy_nonoverlapping(src.as_ptr(), self.base.add(offset), src.len()) }
    }

    /// Copy `dst.len()` bytes at `offset` into `dst`, never a borrow into DMA memory.
    #[inline]
    pub fn copy_to(self, offset: usize, dst: &mut [u8]) {
        self.check(offset, dst.len());
        // SAFETY: `check` bounded the source for `dst.len()` bytes; the two cannot overlap, as `copy_from` explains.
        unsafe { copy_nonoverlapping(self.base.add(offset), dst.as_mut_ptr(), dst.len()) }
    }

    // Panics, not `Result`: every offset/length here is the driver's own arithmetic, never a device-chosen number (those arrive as `Untrusted` and are bounded before becoming an offset).
    #[inline]
    fn check(self, offset: usize, len: usize) {
        if let Err(why) = toyos_dma::within(offset, len, self.size) {
            refuse(why, self.base);
        }
    }
}

// Out of line and cold: inlined, the panic keeps LLVM from inlining the volatile accessors themselves.
#[cold]
#[inline(never)]
fn refuse(why: toyos_dma::Refused, base: *mut u8) -> ! {
    panic!("DMA: {why}, in the region at {base:p}");
}

impl<'pool> Dma<'pool, Volatile> {
    /// Read the `T` at `offset` with a volatile load; bounded and aligned for `T`.
    #[inline]
    pub fn read<T: Copy>(self, offset: usize) -> T {
        // SAFETY: `at` bounded and aligned the pointer for `size_of::<T>()`; volatile because the device may write concurrently, so the load may not be elided or reordered.
        unsafe { read_volatile(self.at::<T>(offset) as *const T) }
    }

    /// Write `value` to the `T` at `offset` with a volatile store.
    #[inline]
    pub fn write<T: Copy>(self, offset: usize, value: T) {
        // SAFETY: `at` bounded and aligned the pointer for `size_of::<T>()`; volatile because the device may read concurrently, so the store may not be elided or reordered.
        unsafe { write_volatile(self.at::<T>(offset), value) }
    }

    /// Switches to the unaligned discipline; one-way, since nothing here needs both over the same memory.
    #[inline]
    pub fn unaligned(self) -> Dma<'pool, Unaligned> {
        Dma::new(self.base, self.size)
    }

    #[inline]
    fn at<T>(self, offset: usize) -> *mut T {
        self.check(offset, core::mem::size_of::<T>());
        if let Err(why) =
            toyos_dma::aligned(self.base as usize, offset, core::mem::align_of::<T>())
        {
            refuse_unaligned(why, core::any::type_name::<T>());
        }
        // SAFETY: `check` above refused anything but `offset + size_of::<T>() <= self.size`.
        unsafe { self.base.add(offset) as *mut T }
    }
}

#[cold]
#[inline(never)]
fn refuse_unaligned(why: toyos_dma::Refused, what: &str) -> ! {
    panic!("DMA: {why}, and a volatile access of {what} needs it");
}

impl Dma<'_, Unaligned> {
    /// Read the `T` at `offset`, whatever it is aligned to.
    #[inline]
    pub fn read<T: Copy>(self, offset: usize) -> T {
        self.check(offset, core::mem::size_of::<T>());
        // SAFETY: `check` refused anything but `size_of::<T>()` bytes; not volatile, since this discipline is for a structure the device has finished writing.
        unsafe { read_unaligned(self.base.add(offset) as *const T) }
    }

    /// Write `value` to the `T` at `offset`, whatever it is aligned to.
    #[inline]
    pub fn write<T: Copy>(self, offset: usize, value: T) {
        self.check(offset, core::mem::size_of::<T>());
        // SAFETY: `check` refused anything but `size_of::<T>()` bytes; not volatile, since this discipline is for a structure written before the device is told where it is.
        unsafe { write_unaligned(self.base.add(offset) as *mut T, value) }
    }
}

/// Contiguous DMA memory backed by 2 MiB physical pages; dropping it frees them.
pub struct DmaPool {
    pages: Vec<PhysPage>,
    base: DirectMap,
    size: usize,
}

impl DmaPool {
    /// Take enough contiguous 2 MiB pages to cover `size` bytes.
    pub fn alloc(size: usize) -> Self {
        let pages_2m = size.div_ceil(super::PAGE_2M as usize);
        let pages = super::pmm::alloc_contiguous(pages_2m, super::pmm::Category::Dma)
            .expect("DmaPool: out of physical memory");
        let base = pages[0].direct_map();
        Self { pages, base, size: pages_2m * super::PAGE_2M as usize }
    }

    /// The whole pool as a view, borrowed so it cannot outlive `self`.
    pub fn view(&self) -> Dma<'_> {
        Dma::new(self.base.as_mut_ptr(), self.size)
    }

    /// Consumes the pool and leaks its pages for a `'static` view; never freed, deliberately.
    pub fn leak(self) -> Dma<'static> {
        let Self { pages, base, size } = self;
        for page in pages {
            core::mem::forget(page);
        }
        Dma::new(base.as_mut_ptr(), size)
    }
}
