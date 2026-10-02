//! What this C library refuses. What each waits on is
//! `issues/build/libc-refuses-what-toyos-cannot-yet-answer.md`'s.

use core::ptr;

use crate::errno::{self, ENOSYS};

fn refuse() -> i32 {
    errno::set(ENOSYS);
    -1
}

/// A process never replaces its image: a program starts as a new process.
#[no_mangle]
pub unsafe extern "C" fn execv(_path: *const u8, _argv: *const *const u8) -> i32 {
    refuse()
}

/// As [`execv`].
#[no_mangle]
pub unsafe extern "C" fn execve(_path: *const u8, _argv: *const *const u8, _envp: *const *const u8) -> i32 {
    refuse()
}

/// ToyOS has no POSIX session.
#[no_mangle]
pub extern "C" fn setsid() -> i32 {
    refuse()
}

/// As [`setsid`].
#[no_mangle]
pub extern "C" fn getsid(_pid: i32) -> i32 {
    refuse()
}

/// No host name is published to a process.
#[no_mangle]
pub unsafe extern "C" fn gethostname(_name: *mut u8, _len: usize) -> i32 {
    refuse()
}

/// No release, version or node name is published to a process.
#[no_mangle]
pub unsafe extern "C" fn uname(_buf: *mut u8) -> i32 {
    refuse()
}

/// No filesystem ToyOS mounts keeps a second name for a file.
#[no_mangle]
pub unsafe extern "C" fn link(_existing: *const u8, _new: *const u8) -> i32 {
    refuse()
}

/// `SYS_SYMLINK` displaces whatever holds the name, where POSIX refuses one
/// that exists, and a question asked first races its answer.
#[no_mangle]
pub unsafe extern "C" fn symlink(_target: *const u8, _link: *const u8) -> i32 {
    refuse()
}

/// A file has no owner.
#[no_mangle]
pub extern "C" fn fchown(_fd: i32, _owner: u32, _group: u32) -> i32 {
    refuse()
}

/// A file has no mode bits to set.
#[no_mangle]
pub unsafe extern "C" fn chmod(_path: *const u8, _mode: u32) -> i32 {
    refuse()
}

/// As [`chmod`].
#[no_mangle]
pub extern "C" fn fchmod(_fd: i32, _mode: u32) -> i32 {
    refuse()
}

/// No call answers a filesystem's size or free space.
#[no_mangle]
pub unsafe extern "C" fn statvfs(_path: *const u8, _buf: *mut u8) -> i32 {
    refuse()
}

/// As [`statvfs`].
#[no_mangle]
pub unsafe extern "C" fn fstatvfs(_fd: i32, _buf: *mut u8) -> i32 {
    refuse()
}

/// No call answers a process's limits.
#[no_mangle]
pub unsafe extern "C" fn getrlimit(_resource: i32, _rlp: *mut u8) -> i32 {
    refuse()
}

/// As [`getrlimit`].
#[no_mangle]
pub unsafe extern "C" fn setrlimit(_resource: i32, _rlp: *const u8) -> i32 {
    refuse()
}

/// ToyOS keeps no user database: a user is the part of the tree a session was
/// handed. POSIX's `_r` lookups answer their error, `result` null.
#[no_mangle]
pub unsafe extern "C" fn getpwnam_r(
    _name: *const u8,
    _pwd: *mut u8,
    _buf: *mut u8,
    _buflen: usize,
    result: *mut *mut u8,
) -> i32 {
    unsafe { *result = ptr::null_mut() };
    ENOSYS
}

/// As [`getpwnam_r`].
#[no_mangle]
pub unsafe extern "C" fn getpwuid_r(_uid: u32, _pwd: *mut u8, _buf: *mut u8, _buflen: usize, result: *mut *mut u8) -> i32 {
    unsafe { *result = ptr::null_mut() };
    ENOSYS
}

/// No call answers which pages of a range are mapped, and no mapping is of a
/// file (`mmap`).
#[no_mangle]
pub unsafe extern "C" fn msync(_addr: *mut u8, _len: usize, _flags: i32) -> i32 {
    refuse()
}

/// A mapping's protection is fixed when it is made.
#[no_mangle]
pub unsafe extern "C" fn mprotect(_addr: *mut u8, _len: usize, _prot: i32) -> i32 {
    refuse()
}

/// The kernel resolves a path by its own rules, not POSIX's (`..` read off
/// the text, only the last name's link followed, a relative link read
/// against its mount), and no call answers where one leads.
#[no_mangle]
pub unsafe extern "C" fn realpath(_path: *const u8, _resolved: *mut u8) -> *mut u8 {
    errno::set(ENOSYS);
    ptr::null_mut()
}
