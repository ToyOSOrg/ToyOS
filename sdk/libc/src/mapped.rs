//! Blocks of [`THRESHOLD`] bytes or more, each a mapping of its own beside
//! the heap that holds every smaller one: such a block grows by `vm_remap`,
//! in place or by moving its pages, and is copied only when it is aligned past
//! a page and cannot grow in place; its memory goes back at its free. One
//! source for std's allocator (`sdk/std/sys/alloc.rs`) and libc's.
//!
//! Which a block is follows from its layout alone, which every call about it
//! carries, so nothing is recorded.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use toyos_abi::syscall::{self, VmAt, VmProt, PAGE_SIZE};

/// dlmalloc's `DEFAULT_MMAP_THRESHOLD`.
const THRESHOLD: usize = 256 * 1024;

/// Whether a block of `layout` is a mapping of its own.
pub(crate) fn holds(layout: Layout) -> bool {
    layout.size() >= THRESHOLD
}

/// A mapping for `layout`, of zeros, or null where none is granted.
pub(crate) fn alloc(layout: Layout) -> *mut u8 {
    let at = VmAt::Anywhere { align: layout.align().max(PAGE_SIZE) };
    // SAFETY: placed anywhere, so it replaces nothing.
    unsafe { syscall::vm_reserve(layout.size(), VmProt::ReadWrite, at) }.unwrap_or(ptr::null_mut())
}

/// # Safety
/// `block` is a mapping [`alloc`] or [`realloc`] answered for `layout`, which
/// nothing reaches after.
pub(crate) unsafe fn dealloc(block: *mut u8, layout: Layout) {
    // A refusal leaves the block mapped and unreachable
    // (`issues/a-refused-release-of-a-large-block-leaks-it.md`).
    let _ = unsafe { syscall::vm_release(block, layout.size()) };
}

/// `GlobalAlloc::realloc` for `heap`, whose blocks this module routes:
/// `small` resizes a block that stays under [`THRESHOLD`], a mapping stays a
/// mapping by `vm_remap`, and any other block moves by a copy. A mapping
/// aligned past a page may only grow in place, since a move keeps only a
/// page's alignment.
///
/// # Safety
/// As `GlobalAlloc::realloc`.
pub(crate) unsafe fn realloc(
    heap: &impl GlobalAlloc,
    block: *mut u8,
    layout: Layout,
    new_size: usize,
    small: impl FnOnce() -> *mut u8,
) -> *mut u8 {
    // SAFETY: `GlobalAlloc::realloc` requires `new_size` at this alignment to be a layout.
    let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
    match (holds(layout), holds(new)) {
        (false, false) => return small(),
        (true, true) => {
            let may_move = layout.align() <= PAGE_SIZE;
            match unsafe { syscall::vm_remap(block, layout.size(), new_size, may_move) } {
                Ok(at) => return at,
                Err(_) if may_move => return ptr::null_mut(),
                Err(_) => {}
            }
        }
        _ => {}
    }
    let moved = unsafe { heap.alloc(new) };
    if !moved.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(block, moved, layout.size().min(new_size));
            heap.dealloc(block, layout);
        }
    }
    moved
}
