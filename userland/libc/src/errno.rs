//! `errno`, one per thread: `include/errno.h` names it `(*__errno_location())`,
//! and every thread's slot is its own copy of the TLS template the kernel lays
//! out when it makes the thread.

use core::cell::Cell;

#[thread_local]
static ERRNO: Cell<i32> = Cell::new(0);

/// The calling thread's `errno`.
#[no_mangle]
pub extern "C" fn __errno_location() -> *mut i32 {
    ERRNO.as_ptr()
}

pub(crate) fn set(code: i32) {
    ERRNO.set(code);
}

// The codes this library's newer modules answer with, as `include/errno.h`
// numbers them.
pub(crate) const EPERM: i32 = 1;
pub(crate) const ENOENT: i32 = 2;
pub(crate) const ESRCH: i32 = 3;
pub(crate) const EIO: i32 = 5;
pub(crate) const EAGAIN: i32 = 11;
#[cfg(not(feature = "std-runtime"))]
pub(crate) const ENOMEM: i32 = 12;
pub(crate) const EBUSY: i32 = 16;
pub(crate) const EINVAL: i32 = 22;
pub(crate) const ERANGE: i32 = 34;
pub(crate) const EDEADLK: i32 = 35;
pub(crate) const EILSEQ: i32 = 84;
pub(crate) const ETIMEDOUT: i32 = 110;
