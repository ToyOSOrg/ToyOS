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

pub(crate) const ENOMEM: i32 = 12;
pub(crate) const ENODEV: i32 = 19;
pub(crate) const EINVAL: i32 = 22;
pub(crate) const ENOSYS: i32 = 38;
pub(crate) const EOVERFLOW: i32 = 75;
pub(crate) const EOPNOTSUPP: i32 = 95;
