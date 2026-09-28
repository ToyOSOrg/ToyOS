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

/// Set the calling thread's `errno`.
pub(crate) fn set(code: i32) {
    ERRNO.set(code);
}
