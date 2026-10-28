use core::cell::SyncUnsafeCell;

use toyos_abi::syscall::{self, VmAt, VmProt};

use crate::alloc::Layout;
use crate::ptr;
use crate::sys::sync::Mutex;

#[path = "../../libc/src/mapped.rs"]
mod mapped;

/// dlmalloc's memory: the process's own private pages, committed whole and
/// populated as they are touched, and given back in part when it trims.
struct ToyOsAllocator;

unsafe impl dlmalloc::Allocator for ToyOsAllocator {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        // SAFETY: placed anywhere, so it replaces nothing.
        match unsafe { syscall::vm_reserve(size, VmProt::ReadWrite, VmAt::ANYWHERE) } {
            Ok(ptr) => (ptr, size, 0),
            Err(_) => (ptr::null_mut(), 0, 0),
        }
    }

    fn remap(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize, _can_move: bool) -> *mut u8 {
        ptr::null_mut()
    }

    fn free_part(&self, ptr: *mut u8, oldsize: usize, newsize: usize) -> bool {
        // SAFETY: dlmalloc gives up the tail of a segment it no longer uses.
        unsafe { syscall::vm_release(ptr.wrapping_add(newsize), oldsize - newsize).is_ok() }
    }

    fn free(&self, ptr: *mut u8, size: usize) -> bool {
        // SAFETY: dlmalloc gives up a whole segment it no longer uses.
        unsafe { syscall::vm_release(ptr, size).is_ok() }
    }

    fn can_release_part(&self, _flags: u32) -> bool {
        true
    }

    fn allocates_zeros(&self) -> bool {
        true
    }

    fn page_size(&self) -> usize {
        syscall::PAGE_SIZE
    }
}

struct SyncDlmalloc(dlmalloc::Dlmalloc<ToyOsAllocator>);
unsafe impl Sync for SyncDlmalloc {}

static DLMALLOC: SyncUnsafeCell<SyncDlmalloc> =
    SyncUnsafeCell::new(SyncDlmalloc(dlmalloc::Dlmalloc::new_with_allocator(ToyOsAllocator)));

/// std's futex mutex: a contended allocation sleeps rather than spinning
/// through the holder's slice, which may be inside the `mmap` dlmalloc makes.
static LOCK: Mutex = Mutex::new();

struct DropLock;

fn lock() -> DropLock {
    LOCK.lock();
    DropLock
}

impl Drop for DropLock {
    fn drop(&mut self) {
        // SAFETY: a `DropLock` exists only while `lock` holds `LOCK`.
        unsafe { LOCK.unlock() }
    }
}

#[inline]
pub unsafe fn alloc(layout: Layout) -> *mut u8 {
    if mapped::holds(layout) {
        return mapped::alloc(layout);
    }
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.malloc(layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn alloc_zeroed(layout: Layout) -> *mut u8 {
    if mapped::holds(layout) {
        return mapped::alloc(layout);
    }
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.calloc(layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn dealloc(ptr: *mut u8, layout: Layout) {
    if mapped::holds(layout) {
        return unsafe { mapped::dealloc(ptr, layout) };
    }
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.free(ptr, layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    let small = || {
        let _lock = lock();
        unsafe { (*DLMALLOC.get()).0.realloc(ptr, layout.size(), layout.align(), new_size) }
    };
    unsafe { mapped::realloc(&crate::alloc::System, ptr, layout, new_size, small) }
}
