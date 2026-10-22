use core::cell::SyncUnsafeCell;

use toyos_abi::syscall::{self, MmapFlags, MmapProt};

use crate::alloc::Layout;
use crate::ptr;
use crate::sys::sync::Mutex;

/// dlmalloc backing allocator that provides memory via mmap/munmap syscalls.
struct ToyOsAllocator;

unsafe impl dlmalloc::Allocator for ToyOsAllocator {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let ptr = unsafe {
            syscall::mmap(
                ptr::null_mut(),
                size,
                MmapProt::READ | MmapProt::WRITE,
                MmapFlags::ANONYMOUS,
            )
        };
        if ptr.is_null() { (ptr::null_mut(), 0, 0) } else { (ptr, size, 0) }
    }

    fn remap(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize, _can_move: bool) -> *mut u8 {
        // No mremap equivalent
        ptr::null_mut()
    }

    fn free_part(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize) -> bool {
        false
    }

    fn free(&self, ptr: *mut u8, size: usize) -> bool {
        unsafe { syscall::munmap(ptr, size).is_ok() }
    }

    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }

    fn allocates_zeros(&self) -> bool {
        true // mmap returns zeroed pages
    }

    fn page_size(&self) -> usize {
        0x1000
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
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.malloc(layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn alloc_zeroed(layout: Layout) -> *mut u8 {
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.calloc(layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn dealloc(ptr: *mut u8, layout: Layout) {
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.free(ptr, layout.size(), layout.align()) }
}

#[inline]
pub unsafe fn realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    let _lock = lock();
    unsafe { (*DLMALLOC.get()).0.realloc(ptr, layout.size(), layout.align(), new_size) }
}
