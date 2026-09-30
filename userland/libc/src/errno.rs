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

pub(crate) const EPERM: i32 = 1;
pub(crate) const ENOENT: i32 = 2;
pub(crate) const ESRCH: i32 = 3;
pub(crate) const EIO: i32 = 5;
pub(crate) const EBADF: i32 = 9;
pub(crate) const ECHILD: i32 = 10;
pub(crate) const EAGAIN: i32 = 11;
pub(crate) const ENOMEM: i32 = 12;
pub(crate) const EACCES: i32 = 13;
pub(crate) const EBUSY: i32 = 16;
pub(crate) const EEXIST: i32 = 17;
pub(crate) const ENODEV: i32 = 19;
pub(crate) const EINVAL: i32 = 22;
pub(crate) const EPIPE: i32 = 32;
pub(crate) const ERANGE: i32 = 34;
pub(crate) const EDEADLK: i32 = 35;
pub(crate) const ENOSYS: i32 = 38;
pub(crate) const EOVERFLOW: i32 = 75;
pub(crate) const EILSEQ: i32 = 84;
pub(crate) const EOPNOTSUPP: i32 = 95;
pub(crate) const EAFNOSUPPORT: i32 = 97;
pub(crate) const EADDRINUSE: i32 = 98;
pub(crate) const ECONNRESET: i32 = 104;
pub(crate) const ENOTCONN: i32 = 107;
pub(crate) const ETIMEDOUT: i32 = 110;
pub(crate) const ECONNREFUSED: i32 = 111;
