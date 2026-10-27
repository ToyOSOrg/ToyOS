//! Blocks of [`THRESHOLD`] bytes or more, each a mapping of its own beside
//! the heap that holds every smaller one, as dlmalloc's C original makes them
//! and its Rust port does not: such a block grows by `vm_remap`, in place or
//! by moving its pages, never by copying them, and its memory goes back at its
//! free. One source for std's allocator (`sdk/std/sys/alloc.rs`) and libc's.
//!
//! Which a block is follows from its layout alone, which every call about it
//! carries, so nothing is recorded.

use core::alloc::Layout;
use core::ptr;

use toyos_abi::syscall::{self, VmAt, VmProt, PAGE_SIZE};

/// dlmalloc's `DEFAULT_MMAP_THRESHOLD`.
const THRESHOLD: usize = 256 * 1024;

/// Whether a block of `layout` is a mapping of its own.
pub(crate) fn holds(layout: Layout) -> bool {
    layout.size() >= THRESHOLD
}

/// Whether [`resize`] can take a mapped block of `layout` to `new_size`: a
/// move keeps only a page's alignment.
pub(crate) fn resizes(layout: Layout, new_size: usize) -> bool {
    holds(layout) && new_size >= THRESHOLD && layout.align() <= PAGE_SIZE
}

/// A mapping for `layout`, of zeros, or null where none is granted.
pub(crate) fn alloc(layout: Layout) -> *mut u8 {
    let at = VmAt::Anywhere { align: layout.align().max(PAGE_SIZE) };
    // SAFETY: placed anywhere, so it replaces nothing.
    unsafe { syscall::vm_reserve(layout.size(), VmProt::ReadWrite, at) }.unwrap_or(ptr::null_mut())
}

/// # Safety
/// `block` is a mapping [`alloc`] or [`resize`] answered for `layout`, which
/// nothing reaches after.
pub(crate) unsafe fn dealloc(block: *mut u8, layout: Layout) {
    // Refused only at the space's bound on ranges, and then the block stays.
    let _ = unsafe { syscall::vm_release(block, layout.size()) };
}

/// The mapping `block` of `layout` resized to `new_size` bytes, its contents
/// kept, where [`resizes`] says it can be; null, and the block as it was,
/// where the kernel refuses.
///
/// # Safety
/// As [`dealloc`], once this answers a block.
pub(crate) unsafe fn resize(block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    debug_assert!(resizes(layout, new_size));
    unsafe { syscall::vm_remap(block, layout.size(), new_size, true) }.unwrap_or(ptr::null_mut())
}
