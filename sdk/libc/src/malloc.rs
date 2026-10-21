//! C's allocator: `malloc`, `calloc`, `realloc`, `free` and `aligned_alloc`.
//!
//! One source, and one copy in every userland program: libc compiles it into
//! a C program, and std (`sdk/std/sys/pal`) into every program that links std,
//! beside which libc compiles none and calls std's. The includer supplies
//! `HEAP`, the `GlobalAlloc` blocks come from, and `refused`, told why a
//! request got none.
//!
//! A block of `size` bytes at alignment `align` is the allocation's last
//! `size` bytes of `align + size`, with its size and alignment in the two words
//! in front of it: `free` and `realloc` hand the heap back the layout it gave.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

/// The least alignment of a block, and the room its header takes.
const MIN_ALIGN: usize = 16;

/// Why a request got no block.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No room, or a size and header no `Layout` holds: `ENOMEM`.
    NoMemory,
    /// An alignment that is no power of two: `EINVAL`.
    Alignment,
}

/// The allocation behind a block of `size` bytes at `align`, if a `Layout`
/// can describe it.
fn layout(size: usize, align: usize) -> Option<Layout> {
    Layout::from_size_align(align.checked_add(size)?, align).ok()
}

/// # Safety
/// `block` is a live block of this allocator.
unsafe fn header(block: *mut u8) -> (usize, usize) {
    unsafe { ((block.sub(16) as *const usize).read(), (block.sub(8) as *const usize).read()) }
}

/// # Safety
/// `raw` is an allocation of at least `align` bytes, `align` at least `MIN_ALIGN`.
unsafe fn block_in(raw: *mut u8, size: usize, align: usize) -> *mut u8 {
    unsafe {
        let block = raw.add(align);
        (block.sub(16) as *mut usize).write(size);
        (block.sub(8) as *mut usize).write(align);
        block
    }
}

/// `size` bytes aligned to `align`, a power of two.
unsafe fn allocate(heap: &impl GlobalAlloc, size: usize, align: usize, zeroed: bool) -> Result<*mut u8, Refusal> {
    let align = align.max(MIN_ALIGN);
    let layout = layout(size, align).ok_or(Refusal::NoMemory)?;
    let raw = unsafe { if zeroed { heap.alloc_zeroed(layout) } else { heap.alloc(layout) } };
    if raw.is_null() {
        return Err(Refusal::NoMemory);
    }
    Ok(unsafe { block_in(raw, size, align) })
}

/// Null for no bytes.
pub(crate) unsafe fn malloc(heap: &impl GlobalAlloc, size: usize) -> Result<*mut u8, Refusal> {
    if size == 0 {
        return Ok(ptr::null_mut());
    }
    unsafe { allocate(heap, size, MIN_ALIGN, false) }
}

pub(crate) unsafe fn aligned_alloc(heap: &impl GlobalAlloc, align: usize, size: usize) -> Result<*mut u8, Refusal> {
    if !align.is_power_of_two() {
        return Err(Refusal::Alignment);
    }
    unsafe { allocate(heap, size, align, false) }
}

/// Null for no bytes; refused for a product a `usize` cannot hold.
pub(crate) unsafe fn calloc(heap: &impl GlobalAlloc, count: usize, size: usize) -> Result<*mut u8, Refusal> {
    let total = count.checked_mul(size).ok_or(Refusal::NoMemory)?;
    if total == 0 {
        return Ok(ptr::null_mut());
    }
    unsafe { allocate(heap, total, MIN_ALIGN, true) }
}

/// # Safety
/// `block` is null or a live block of this allocator from `heap`.
pub(crate) unsafe fn free(heap: &impl GlobalAlloc, block: *mut u8) {
    if block.is_null() {
        return;
    }
    let (size, align) = unsafe { header(block) };
    let layout = layout(size, align).expect("a block's header is the layout it was allocated with");
    unsafe { heap.dealloc(block.sub(align), layout) };
}

/// The block moved or grown to `new_size` bytes at its own alignment, which
/// `GlobalAlloc::realloc` keeps; a refused block is left as it was.
///
/// # Safety
/// `block` is null or a live block of this allocator from `heap`.
pub(crate) unsafe fn realloc(heap: &impl GlobalAlloc, block: *mut u8, new_size: usize) -> Result<*mut u8, Refusal> {
    if block.is_null() {
        return unsafe { malloc(heap, new_size) };
    }
    if new_size == 0 {
        unsafe { free(heap, block) };
        return Ok(ptr::null_mut());
    }
    let (size, align) = unsafe { header(block) };
    let new = layout(new_size, align).ok_or(Refusal::NoMemory)?;
    let old = layout(size, align).expect("a block's header is the layout it was allocated with");
    let raw = unsafe { heap.realloc(block.sub(align), old, new.size()) };
    if raw.is_null() {
        return Err(Refusal::NoMemory);
    }
    Ok(unsafe { block_in(raw, new_size, align) })
}

/// The C symbols, over the includer's heap. Not in a host test, whose own
/// allocator would call them.
#[cfg(not(test))]
mod exports {
    use super::super::{refused, HEAP};

    fn answer(result: Result<*mut u8, super::Refusal>) -> *mut u8 {
        result.unwrap_or_else(|why| {
            refused(why);
            core::ptr::null_mut()
        })
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
        answer(unsafe { super::malloc(&HEAP, size) })
    }

    /// C11's, which takes any size: a block need not be a whole number of
    /// alignments.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut u8 {
        answer(unsafe { super::aligned_alloc(&HEAP, align, size) })
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut u8 {
        answer(unsafe { super::calloc(&HEAP, count, size) })
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn free(block: *mut u8) {
        unsafe { super::free(&HEAP, block) }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn realloc(block: *mut u8, new_size: usize) -> *mut u8 {
        answer(unsafe { super::realloc(&HEAP, block, new_size) })
    }
}
