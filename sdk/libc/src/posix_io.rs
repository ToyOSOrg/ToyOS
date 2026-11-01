// POSIX file I/O: a path or a descriptor is `toyos::files`', which routes it to
// the kernel or to the file server behind one of the process's directory
// capabilities.

#![allow(non_camel_case_types)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use toyos_abi::{RawHandle, Rights};
use toyos_abi::syscall::{self, OpenFlags, SeekFrom};

use toyos::files::{self, At};

use crate::errcode::{EBADF, EINTR, EINVAL};
use crate::signal::Call;
use crate::time::Timespec;

pub(crate) use crate::atreq::{O_APPEND, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use crate::atreq::{read_write, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW};

const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

// stat file type bits
const S_IFREG: u32 = 0o100000;
const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

// Helper: set errno from toyos-abi error

pub(crate) fn set_errno(e: toyos_abi::syscall::SyscallError) -> i32 {
    crate::errno::set(crate::errcode::of(e));
    -1
}

/// Why a call on a descriptor was not made: an `errno`.
pub(crate) type Errno = i32;

/// `-1`, with `errno` set to `e`.
pub(crate) fn refuse(e: Errno) -> i32 {
    crate::errno::set(e);
    -1
}

/// The table's refusal as `errno`.
pub(crate) fn fd_errno(e: toyos::fd::Error) -> Errno {
    match e {
        toyos::fd::Error::BadFd => EBADF,
        toyos::fd::Error::Full => crate::errcode::EMFILE,
    }
}

/// A kernel handle this process just got, as the lowest free descriptor,
/// inherited unless `cloexec`.
pub(crate) fn install(handle: toyos::OwnedHandle, cloexec: bool) -> Result<i32, Errno> {
    toyos::fd::install(toyos::fd::Object::handle(handle), !cloexec, 0).map_err(fd_errno)
}

/// A `write` to a descriptor. Stdout and stderr go through the SDK's stream
/// sinks, which are a program's log ring when its descriptor names one
/// (`toyos::log::stdio`); every other descriptor is its handle.
pub fn write_fd(raw: i32, buf: &[u8]) -> Result<usize, Errno> {
    use toyos::log::stdio::{self, Stream};
    // A closed stdout is `EBADF`'s, where the stream sink drops its bytes.
    toyos::fd::inheritable(raw).map_err(fd_errno)?;
    match raw {
        1 => stdio::write(Stream::Out, buf).map_err(crate::errcode::of_files),
        2 => stdio::write(Stream::Err, buf).map_err(crate::errcode::of_files),
        other => files::write(other, buf).map_err(crate::errcode::of_files),
    }
}

/// `-1`, with `errno` set for `toyos::files`' refusal `e`.
pub(crate) fn refuse_files(e: files::Error) -> i32 {
    refuse(crate::errcode::of_files(e))
}

pub fn c_str_to_bytes(s: *const u8) -> &'static [u8] {
    unsafe {
        let len = super::string::strlen(s);
        core::slice::from_raw_parts(s, len)
    }
}

// File descriptor operations

#[no_mangle]
pub unsafe extern "C" fn open(path: *const u8, flags: i32, mode: u32) -> i32 {
    unsafe { openat(AT_FDCWD, path, flags, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn openat(dirfd: i32, path: *const u8, flags: i32, _mode: u32) -> i32 {
    let path = c_str_to_bytes(path);
    let how = match crate::atreq::how(flags) {
        Ok(how) => how,
        Err(e) => return refuse(e),
    };
    if path == DEV_NULL {
        return match toyos::null(null_rights(how.read, how.write)) {
            Ok(handle) => install(handle, how.cloexec).unwrap_or_else(refuse),
            Err(e) => set_errno(e),
        };
    }
    match crate::atreq::at(dirfd, path) {
        Ok(at) => files::open(at, path, how).unwrap_or_else(refuse_files),
        Err(e) => refuse(e),
    }
}

/// A spawn's `addopen` file, as a handle of this process's for the child.
/// A file a file server holds is refused: no kernel handle reaches it.
pub(crate) fn open_handle(path: &[u8], flags: i32, _mode: u32) -> Result<RawHandle, Errno> {
    let (readable, writable) = read_write(flags);
    if path == DEV_NULL {
        return syscall::null(null_rights(readable, writable)).map_err(crate::errcode::of);
    }
    if files::is_served(path) {
        return Err(crate::errcode::EOPNOTSUPP);
    }

    let mut oflags = OpenFlags(0);
    if readable { oflags |= OpenFlags::READ; }
    if writable { oflags |= OpenFlags::WRITE; }
    if flags & O_CREAT != 0 { oflags |= OpenFlags::CREATE; }
    if flags & O_CREAT != 0 && flags & O_EXCL != 0 { oflags |= OpenFlags::EXCLUSIVE; }
    if flags & O_TRUNC != 0 { oflags |= OpenFlags::TRUNCATE; }
    if flags & O_APPEND != 0 { oflags |= OpenFlags::APPEND; }
    syscall::open(path, oflags).map_err(crate::errcode::of)
}

/// POSIX's null device, which libc answers with a kernel null handle: the
/// name is libc's, and no filesystem holds it.
const DEV_NULL: &[u8] = b"/dev/null";

/// What a null handle opened for `readable` and `writable` may do: that, and
/// what every descriptor may (`dup`, a spawn's stdio, `poll`).
fn null_rights(readable: bool, writable: bool) -> Rights {
    let mut rights = Rights::DUP.union(Rights::TRANSFER).union(Rights::WAIT);
    if readable { rights = rights.union(Rights::READ); }
    if writable { rights = rights.union(Rights::WRITE); }
    rights
}

#[no_mangle]
pub unsafe extern "C" fn creat(path: *const u8, mode: u32) -> i32 {
    unsafe { open(path, O_WRONLY | O_CREAT | O_TRUNC, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn close(raw_fd: i32) -> i32 {
    // SAFETY: C's `close`: the caller gives up `raw_fd`.
    match unsafe { toyos::fd::close(raw_fd) } {
        Ok(()) => 0,
        Err(e) => refuse(fd_errno(e)),
    }
}

/// A handler that interrupts a read of a kernel object restarts it if it was
/// installed with `SA_RESTART` and answers `EINTR` if not. A descriptor no
/// watch can be armed on, a console's, and a file a server serves, which
/// answers without waiting on any event, read as they block, and nothing
/// interrupts them.
#[no_mangle]
pub unsafe extern "C" fn read(raw_fd: i32, buf: *mut u8, count: usize) -> isize {
    if buf.is_null() || count == 0 { return 0; }
    let slice = core::slice::from_raw_parts_mut(buf, count);
    if let Some(n) = crate::socket::read_if_stream(raw_fd, slice) {
        return n;
    }
    let blocking = |slice: &mut [u8]| match files::read(raw_fd, slice) {
        Ok(n) => n as isize,
        Err(e) => refuse_files(e) as isize,
    };
    let Some(mut call) = Call::enter(None, false, false) else { return blocking(slice) };
    // Held for the call, so a `close` on another thread releases nothing it waits on.
    let Ok(held) = toyos::fd::hold(raw_fd) else { return blocking(slice) };
    let Some(handle) = held.object().kernel_handle().filter(|_| held.object().tag() != toyos::fd::STREAM) else {
        drop(call);
        return blocking(slice);
    };
    loop {
        if let Some(delivery) = call.take() {
            if !call.interrupted(delivery) {
                return refuse(EINTR) as isize;
            }
            continue;
        }
        match syscall::read_nonblock(handle.raw(), slice) {
            Ok(n) => return n as isize,
            Err(syscall::SyscallError::WouldBlock) => {}
            Err(e) => return set_errno(e) as isize,
        }
        let poller = toyos::poller::Poller::new(2);
        poller.watch(&handle, toyos::poller::READABLE, 0);
        poller.watch(&call.wake(), toyos::poller::READABLE, 1);
        let mut refused = false;
        poller.wait_answers(1, u64::MAX, |token, answer| {
            refused |= token == 0 && answer == Err(syscall::SyscallError::NotSupported);
        });
        if refused {
            drop(call);
            return blocking(slice);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn write(raw_fd: i32, buf: *const u8, count: usize) -> isize {
    if buf.is_null() || count == 0 { return 0; }
    let slice = core::slice::from_raw_parts(buf, count);
    if let Some(n) = crate::socket::write_if_stream(raw_fd, slice) {
        return n;
    }
    match write_fd(raw_fd, slice) {
        Ok(n) => n as isize,
        Err(e) => refuse(e) as isize,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lseek(raw_fd: i32, offset: i64, whence: i32) -> i64 {
    let pos = match whence {
        SEEK_SET => SeekFrom::Start(offset as u64),
        SEEK_CUR => SeekFrom::Current(offset),
        SEEK_END => SeekFrom::End(offset),
        _ => { crate::errno::set(EINVAL); return -1; }
    };
    match files::seek(raw_fd, pos) {
        Ok(n) => n as i64,
        Err(e) => refuse_files(e) as i64,
    }
}

#[no_mangle]
pub unsafe extern "C" fn fstat(raw_fd: i32, buf: *mut Stat) -> i32 {
    unsafe { answered(files::fstat(raw_fd), buf) }
}

/// `buf` filled from `answer`, or `errno` from its refusal.
unsafe fn answered(answer: Result<files::Stat, files::Error>, buf: *mut Stat) -> i32 {
    let st = match answer {
        Ok(st) => st,
        Err(e) => return refuse_files(e),
    };
    if !buf.is_null() {
        // SAFETY: C's caller hands one `struct stat` at `buf`.
        unsafe { ptr::write_bytes(buf, 0, 1) };
        let s = unsafe { &mut *buf };
        s.st_dev = st.volume;
        s.st_ino = st.node;
        s.st_nlink = st.nlink;
        s.st_size = st.size as i64;
        let time = |nanos: u64| Timespec { tv_sec: (nanos / NANOS_PER_SEC) as i64, tv_nsec: (nanos % NANOS_PER_SEC) as i64 };
        s.st_atim = time(st.atime);
        s.st_mtim = time(st.mtime);
        s.st_ctim = time(st.ctime);
        s.st_mode = match st.file_type {
            syscall::FileType::File => S_IFREG | 0o644,
            syscall::FileType::Directory => S_IFDIR | 0o755,
            syscall::FileType::Symlink => S_IFLNK | 0o777,
            syscall::FileType::Pipe => S_IFIFO | 0o644,
            syscall::FileType::Tty => S_IFCHR | 0o644,
            syscall::FileType::Serial => S_IFCHR | 0o644,
            syscall::FileType::Null => S_IFCHR | 0o666,
            _ => 0o644,
        };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn ftruncate(raw_fd: i32, length: i64) -> i32 {
    let Ok(length) = u64::try_from(length) else { return refuse(EINVAL) };
    files::truncate(raw_fd, length).map_or_else(refuse_files, |()| 0)
}

/// [`ftruncate`] of `path` opened for writing.
#[no_mangle]
pub unsafe extern "C" fn truncate(path: *const u8, length: i64) -> i32 {
    let fd = unsafe { open(path, O_WRONLY, 0) };
    if fd < 0 {
        return -1;
    }
    let truncated = unsafe { ftruncate(fd, length) };
    unsafe { close(fd) };
    truncated
}

/// `_PC_PATH_MAX`, `unistd.h`'s.
const PC_PATH_MAX: i32 = 4;

/// The longest path the kernel resolves, for every path; no other limit is
/// answered.
#[no_mangle]
pub unsafe extern "C" fn pathconf(_path: *const u8, name: i32) -> i64 {
    match name {
        PC_PATH_MAX => syscall::MAX_PATH as i64,
        _ => {
            crate::errno::set(EINVAL);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn fsync(raw_fd: i32) -> i32 {
    files::fsync(raw_fd).map_or_else(refuse_files, |()| 0)
}

/// POSIX's `dup`: the lowest free descriptor on the same object, not
/// close-on-exec.
#[no_mangle]
pub unsafe extern "C" fn dup(raw_fd: i32) -> i32 {
    toyos::fd::dup(raw_fd, 0, true).unwrap_or_else(|e| refuse(fd_errno(e)))
}

/// POSIX's `dup2`: `new_fd` on `old_fd`'s object, not close-on-exec, after
/// closing what it named.
#[no_mangle]
pub unsafe extern "C" fn dup2(old_fd: i32, new_fd: i32) -> i32 {
    // SAFETY: C's `dup2`: the caller gives up what `new_fd` named.
    match unsafe { toyos::fd::dup2(old_fd, new_fd, true) } {
        Ok(fd) => {
            // The descriptor names something else now, so the stream is asked again.
            match fd {
                1 => toyos::log::stdio::forget(toyos::log::stdio::Stream::Out),
                2 => toyos::log::stdio::forget(toyos::log::stdio::Stream::Err),
                _ => {}
            }
            fd
        }
        Err(e) => refuse(fd_errno(e)),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pipe(pipefd: *mut i32) -> i32 {
    let (read, write) = match toyos::pipe_pair() {
        Ok(ends) => ends,
        Err(e) => return set_errno(e),
    };
    let read = match install(read.into(), false) {
        Ok(fd) => fd,
        Err(e) => return refuse(e),
    };
    let write = match install(write.into(), false) {
        Ok(fd) => fd,
        Err(e) => {
            // SAFETY: the descriptor installed just above, named nowhere else.
            let _ = unsafe { toyos::fd::close(read) };
            return refuse(e);
        }
    };
    *pipefd = read;
    *pipefd.add(1) = write;
    0
}

// Path operations

/// `path` from `dirfd`, as the `*at` calls name it, with `f`'s answer as
/// theirs: 0, or `-1` and `errno`.
fn on_at(dirfd: i32, path: *const u8, f: impl FnOnce(At, &[u8]) -> Result<(), files::Error>) -> i32 {
    let path = c_str_to_bytes(path);
    match crate::atreq::at(dirfd, path) {
        Ok(at) => f(at, path).map_or_else(refuse_files, |()| 0),
        Err(e) => refuse(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const u8) -> i32 {
    unsafe { unlinkat(AT_FDCWD, path, 0) }
}

#[no_mangle]
pub unsafe extern "C" fn rmdir(path: *const u8) -> i32 {
    unsafe { unlinkat(AT_FDCWD, path, AT_REMOVEDIR) }
}

#[no_mangle]
pub unsafe extern "C" fn unlinkat(dirfd: i32, path: *const u8, flags: i32) -> i32 {
    match crate::atreq::only(flags, AT_REMOVEDIR) {
        Ok(flags) => on_at(dirfd, path, |at, path| files::unlink(at, path, flags & AT_REMOVEDIR != 0)),
        Err(e) => refuse(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn mkdirat(dirfd: i32, path: *const u8, _mode: u32) -> i32 {
    on_at(dirfd, path, files::mkdir)
}

#[no_mangle]
pub unsafe extern "C" fn renameat(from_dirfd: i32, from: *const u8, to_dirfd: i32, to: *const u8) -> i32 {
    let (from, to) = (c_str_to_bytes(from), c_str_to_bytes(to));
    match (crate::atreq::at(from_dirfd, from), crate::atreq::at(to_dirfd, to)) {
        (Ok(a), Ok(b)) => files::rename((a, from), (b, to)).map_or_else(refuse_files, |()| 0),
        (Err(e), _) | (_, Err(e)) => refuse(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn link(existing: *const u8, new: *const u8) -> i32 {
    unsafe { linkat(AT_FDCWD, existing, AT_FDCWD, new, 0) }
}

/// The empty path from a descriptor, with `AT_EMPTY_PATH`, names what it
/// names: an unnamed file given its name. Without `AT_SYMLINK_FOLLOW` a
/// symlink is linked itself.
#[no_mangle]
pub unsafe extern "C" fn linkat(from_dirfd: i32, from: *const u8, to_dirfd: i32, to: *const u8, flags: i32) -> i32 {
    let (from, to) = (c_str_to_bytes(from), c_str_to_bytes(to));
    let flags = match crate::atreq::only(flags, AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) {
        Ok(flags) => flags,
        Err(e) => return refuse(e),
    };
    if let Err(e) = crate::atreq::empty_path(from, flags) {
        return refuse(e);
    }
    match (crate::atreq::at(from_dirfd, from), crate::atreq::at(to_dirfd, to)) {
        (Ok(a), Ok(b)) => {
            files::link((a, from), flags & AT_SYMLINK_FOLLOW != 0, (b, to)).map_or_else(refuse_files, |()| 0)
        }
        (Err(e), _) | (_, Err(e)) => refuse(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn getcwd(buf: *mut u8, size: usize) -> *mut u8 {
    if buf.is_null() || size == 0 { return ptr::null_mut(); }
    let slice = core::slice::from_raw_parts_mut(buf, size);
    let n = syscall::getcwd(slice);
    if n == 0 || n >= size {
        return ptr::null_mut();
    }
    *buf.add(n) = 0; // null-terminate
    buf
}

#[no_mangle]
pub unsafe extern "C" fn chdir(path: *const u8) -> i32 {
    files::chdir(c_str_to_bytes(path)).map_or_else(refuse_files, |()| 0)
}

#[no_mangle]
pub unsafe extern "C" fn stat(path: *const u8, buf: *mut Stat) -> i32 {
    unsafe { fstatat(AT_FDCWD, path, buf, 0) }
}

/// `stat` that does not follow a final symbolic link.
#[no_mangle]
pub unsafe extern "C" fn lstat(path: *const u8, buf: *mut Stat) -> i32 {
    unsafe { fstatat(AT_FDCWD, path, buf, AT_SYMLINK_NOFOLLOW) }
}

/// The empty path from a descriptor names what it names, with
/// `AT_EMPTY_PATH`.
#[no_mangle]
pub unsafe extern "C" fn fstatat(dirfd: i32, path: *const u8, buf: *mut Stat, flags: i32) -> i32 {
    let path = c_str_to_bytes(path);
    let flags = match crate::atreq::only(flags, AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) {
        Ok(flags) => flags,
        Err(e) => return refuse(e),
    };
    if let Err(e) = crate::atreq::empty_path(path, flags) {
        return refuse(e);
    }
    match crate::atreq::at(dirfd, path) {
        Ok(at) => unsafe { answered(files::stat(at, path, flags & AT_SYMLINK_NOFOLLOW == 0), buf) },
        Err(e) => refuse(e),
    }
}

/// `SYS_READLINK` answers `NotFound` for a path that is no link as for one
/// that names nothing, and POSIX tells them apart: `EINVAL` for the first. A
/// file server tells them apart itself.
#[no_mangle]
pub unsafe extern "C" fn readlink(path: *const u8, buf: *mut u8, size: usize) -> isize {
    unsafe { readlinkat(AT_FDCWD, path, buf, size) }
}

#[no_mangle]
pub unsafe extern "C" fn readlinkat(dirfd: i32, path: *const u8, buf: *mut u8, size: usize) -> isize {
    let Some(len) = crate::linkreq::target_len(size) else {
        crate::errno::set(EINVAL);
        return -1;
    };
    let at = match crate::atreq::at(dirfd, c_str_to_bytes(path)) {
        Ok(at) => at,
        Err(e) => return refuse(e) as isize,
    };
    match files::read_link(at, c_str_to_bytes(path)) {
        Ok(target) => {
            let n = target.len().min(len);
            // SAFETY: C's caller hands `size` bytes at `buf`, and `len` is at most it.
            unsafe { ptr::copy_nonoverlapping(target.as_ptr(), buf, n) };
            n as isize
        }
        Err(files::Error::Kernel(syscall::SyscallError::NotFound)) => {
            // `stat` sets `errno` for a path that names nothing.
            if unsafe { fstatat(dirfd, path, ptr::null_mut(), AT_SYMLINK_NOFOLLOW) } == 0 {
                crate::errno::set(EINVAL);
            }
            -1
        }
        Err(e) => refuse_files(e) as isize,
    }
}

/// A file server refuses a name that exists, as POSIX has it. The kernel's
/// `SYS_SYMLINK` displaces whatever holds the name, and a question asked first
/// races its answer, so a link the kernel would hold is refused `ENOSYS`.
#[no_mangle]
pub unsafe extern "C" fn symlink(target: *const u8, link: *const u8) -> i32 {
    unsafe { symlinkat(target, AT_FDCWD, link) }
}

#[no_mangle]
pub unsafe extern "C" fn symlinkat(target: *const u8, dirfd: i32, link: *const u8) -> i32 {
    let (target, link) = (c_str_to_bytes(target), c_str_to_bytes(link));
    let at = match crate::atreq::at(dirfd, link) {
        Ok(at) => at,
        Err(e) => return refuse(e),
    };
    match files::is_served_at(at, link) {
        Ok(true) => files::symlink(target, (at, link)).map_or_else(refuse_files, |()| 0),
        Ok(false) => refuse(crate::errcode::ENOSYS),
        Err(e) => refuse_files(e),
    }
}

struct TheServers;

impl crate::resolve::Served for TheServers {
    fn holds(&mut self, at: &[u8]) -> bool {
        files::is_served(at)
    }

    fn name(&mut self, at: &[u8]) -> Result<crate::resolve::Name, i32> {
        let st = files::stat(At::Cwd, at, false).map_err(crate::errcode::of_files)?;
        match st.file_type {
            syscall::FileType::Symlink => {
                files::read_link(At::Cwd, at).map(crate::resolve::Name::Link).map_err(crate::errcode::of_files)
            }
            _ => Ok(crate::resolve::Name::Other),
        }
    }
}

struct TheKernel;

impl crate::resolve::Kernel for TheKernel {
    fn read_link(&mut self, at: &[u8], target: &mut [u8]) -> Result<usize, syscall::SyscallError> {
        syscall::readlink(at, target)
    }

    fn open(&mut self, at: &[u8]) -> Result<(), syscall::SyscallError> {
        // SAFETY: a handle just opened, closed once.
        syscall::open(at, OpenFlags::READ).map(|h| unsafe { syscall::close(h) })
    }

    fn is_dir(&mut self, at: &[u8]) -> bool {
        syscall::stat(at, true).is_ok_and(|st| st.file_type == syscall::FileType::Directory)
    }
}

/// POSIX's `realpath` (`resolve::resolve_path`): POSIX's resolution where a
/// file server holds the names, the kernel's rules where it does, so the
/// answer is the file `open` of `path` reaches, in a block of `malloc`'s when
/// `resolved` is null.
#[no_mangle]
pub unsafe extern "C" fn realpath(path: *const u8, resolved: *mut u8) -> *mut u8 {
    use syscall::MAX_PATH;
    if path.is_null() {
        crate::errno::set(EINVAL);
        return ptr::null_mut();
    }
    let mut cwd = [0u8; MAX_PATH];
    let n = syscall::getcwd(&mut cwd);
    assert!(n != 0 && n <= MAX_PATH, "SYS_GETCWD: a working directory past MAX_PATH");
    let resolved_at = crate::resolve::resolve_path(&cwd[..n], c_str_to_bytes(path), &mut TheServers, &mut TheKernel);
    let at = match resolved_at {
        Ok(at) => at,
        Err(code) => {
            crate::errno::set(code);
            return ptr::null_mut();
        }
    };
    let out = if resolved.is_null() { unsafe { crate::memory::malloc(at.len() + 1) } } else { resolved };
    if !out.is_null() {
        // SAFETY: `PATH_MAX` bytes at the caller's `resolved`, as POSIX has it,
        // or `at.len() + 1` of `malloc`'s; `resolve` answers under `MAX_PATH`, which is it.
        unsafe {
            ptr::copy_nonoverlapping(at.as_ptr(), out, at.len());
            *out.add(at.len()) = 0;
        }
    }
    out
}

/// ToyOS has no permissions model: whether `path` names anything.
#[no_mangle]
pub unsafe extern "C" fn access(path: *const u8, _mode: i32) -> i32 {
    unsafe { stat(path, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn isatty(raw_fd: i32) -> i32 {
    match files::fstat(raw_fd) {
        Ok(st) if st.file_type == syscall::FileType::Tty => 1,
        answer => {
            let bad = matches!(answer, Err(files::Error::Fd(toyos::fd::Error::BadFd)));
            crate::errno::set(if bad { EBADF } else { crate::errcode::ENOTTY });
            0
        }
    }
}

static mut UMASK_VAL: u32 = 0o022;

#[no_mangle]
pub unsafe extern "C" fn umask(mask: u32) -> u32 {
    let old = unsafe { UMASK_VAL };
    unsafe { UMASK_VAL = mask & 0o777; }
    old
}

/// Whether `path` names a file, and not a directory.
pub(crate) fn is_file(path: &[u8]) -> bool {
    files::stat(At::Cwd, path, true).is_ok_and(|st| st.file_type == syscall::FileType::File)
}

/// `arg` is the register C's variadic third argument arrives in (`fdreq`).
#[no_mangle]
pub unsafe extern "C" fn fcntl(raw_fd: i32, cmd: i32, arg: u64) -> i32 {
    use crate::fdreq::{Command, FD_CLOEXEC};
    let answer = match crate::fdreq::command(cmd, arg) {
        Command::DupAtLeast { floor, cloexec } => toyos::fd::dup(raw_fd, floor, !cloexec).map_err(fd_errno),
        Command::GetFd => toyos::fd::inheritable(raw_fd).map(|inherited| if inherited { 0 } else { FD_CLOEXEC }).map_err(fd_errno),
        Command::SetFd(cloexec) => toyos::fd::set_inheritable(raw_fd, !cloexec).map(|()| 0).map_err(fd_errno),
        Command::Invalid => Err(EINVAL),
        Command::Unsupported => Err(crate::errcode::ENOSYS),
    };
    answer.unwrap_or_else(refuse)
}

// Directory operations

/// An open directory: its descriptor, the batch of its listing read last, and
/// where the next batch starts.
pub struct DIR {
    fd: i32,
    entries: Vec<(u64, u8, Box<[u8]>)>,
    at: usize,
    /// `None` once the last batch was read.
    cursor: Option<u64>,
}

#[repr(C)]
pub struct dirent {
    pub d_ino: u64,
    pub d_type: u8,
    pub d_name: [u8; crate::listing::D_NAME],
}

const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

#[no_mangle]
pub unsafe extern "C" fn opendir(path: *const u8) -> *mut DIR {
    let fd = unsafe { openat(AT_FDCWD, path, O_RDONLY | crate::atreq::O_DIRECTORY | crate::atreq::O_CLOEXEC, 0) };
    if fd < 0 {
        return ptr::null_mut();
    }
    let dir = unsafe { fdopendir(fd) };
    if dir.is_null() {
        // SAFETY: the descriptor `open` answered above, which no `DIR` took.
        let _ = unsafe { toyos::fd::close(fd) };
    }
    dir
}

/// The directory `fd` names, read through it from here on, and closed with
/// it.
#[no_mangle]
pub unsafe extern "C" fn fdopendir(fd: i32) -> *mut DIR {
    match files::fstat(fd) {
        Ok(st) if st.file_type == syscall::FileType::Directory => {
            Box::into_raw(Box::new(DIR { fd, entries: Vec::new(), at: 0, cursor: Some(0) }))
        }
        Ok(_) => {
            crate::errno::set(crate::errcode::ENOTDIR);
            ptr::null_mut()
        }
        Err(e) => {
            refuse_files(e);
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn dirfd(dir: *mut DIR) -> i32 {
    match unsafe { dir.as_ref() } {
        Some(dir) => dir.fd,
        None => refuse(EINVAL),
    }
}

/// The next batch of `dir`'s listing, each entry's node and `d_type` beside
/// its name.
fn next_batch(dir: &mut DIR, cursor: u64) -> Result<(), i32> {
    let (listing, next) = files::read_dir(dir.fd, cursor).map_err(crate::errcode::of_files)?;
    let mut entries = Vec::new();
    toyos::fs::for_each_entry(&listing, |entry| {
        let d_type = match entry.kind {
            toyos::fs::KIND_DIR => DT_DIR,
            toyos::fs::KIND_FILE => DT_REG,
            toyos::fs::KIND_SYMLINK => DT_LNK,
            _ => DT_UNKNOWN,
        };
        entries.push((entry.node, d_type, Box::from(entry.name.as_bytes())));
    })
    .map_err(|e| crate::errcode::of_files(files::Error::Served(e)))?;
    (dir.entries, dir.at, dir.cursor) = (entries, 0, next);
    Ok(())
}

/// The end of the listing is null with `errno` left as it was; a refused
/// batch is null with `errno` set.
#[no_mangle]
pub unsafe extern "C" fn readdir(dir: *mut DIR) -> *mut dirent {
    let Some(d) = (unsafe { dir.as_mut() }) else { return ptr::null_mut() };
    while d.at == d.entries.len() {
        let Some(cursor) = d.cursor else { return ptr::null_mut() };
        if let Err(e) = next_batch(d, cursor) {
            d.cursor = None;
            crate::errno::set(e);
            return ptr::null_mut();
        }
    }
    let (d_ino, d_type, name) = &d.entries[d.at];
    d.at += 1;
    let Some(d_name) = crate::listing::d_name(name) else {
        crate::errno::set(crate::errcode::EOVERFLOW);
        return ptr::null_mut();
    };

    // Use a static buffer for the dirent (not thread-safe, matching POSIX convention)
    static mut DIRENT_BUF: dirent = dirent { d_ino: 0, d_type: 0, d_name: [0; crate::listing::D_NAME] };
    let ent = &raw mut DIRENT_BUF;
    unsafe { ent.write(dirent { d_ino: *d_ino, d_type: *d_type, d_name }) };
    ent
}

#[no_mangle]
pub unsafe extern "C" fn closedir(dir: *mut DIR) -> i32 {
    if dir.is_null() { return -1; }
    // SAFETY: a `DIR` `opendir` or `fdopendir` answered, closed once.
    let dir = unsafe { Box::from_raw(dir) };
    // SAFETY: the `DIR` owns its descriptor, and goes with it.
    unsafe { toyos::fd::close(dir.fd) }.map_or_else(|e| refuse(fd_errno(e)), |()| 0)
}

// Positions and times

/// Never moves the descriptor's offset.
#[no_mangle]
pub unsafe extern "C" fn pread(raw_fd: i32, buf: *mut u8, count: usize, offset: i64) -> isize {
    let Ok(offset) = u64::try_from(offset) else { return refuse(EINVAL) as isize };
    if count == 0 { return 0; }
    // SAFETY: C's caller hands `count` writable bytes at `buf`.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, count) };
    files::pread(raw_fd, slice, offset).map_or_else(|e| refuse_files(e) as isize, |n| n as isize)
}

/// Never moves the descriptor's offset, and writes at `offset` whether or
/// not it appends, as POSIX has it.
#[no_mangle]
pub unsafe extern "C" fn pwrite(raw_fd: i32, buf: *const u8, count: usize, offset: i64) -> isize {
    let Ok(offset) = u64::try_from(offset) else { return refuse(EINVAL) as isize };
    if count == 0 { return 0; }
    // SAFETY: C's caller hands `count` bytes at `buf`.
    let slice = unsafe { core::slice::from_raw_parts(buf, count) };
    files::pwrite(raw_fd, slice, offset).map_or_else(|e| refuse_files(e) as isize, |n| n as isize)
}

/// The two `(tv_sec, tv_nsec)` or `(tv_sec, tv_usec)` pairs at `times`,
/// none where it is null.
///
/// # Safety
/// `times` is null or names two `{long, long}` pairs.
unsafe fn pairs(times: *const [i64; 2]) -> Option<[(i64, i64); 2]> {
    // SAFETY: the caller's.
    unsafe { times.as_ref().map(|_| [((*times)[0], (*times)[1]), ((*times.add(1))[0], (*times.add(1))[1])]) }
}

#[no_mangle]
pub unsafe extern "C" fn utimensat(dirfd: i32, path: *const u8, times: *const [i64; 2], flags: i32) -> i32 {
    let checked = crate::atreq::only(flags, AT_SYMLINK_NOFOLLOW)
        .and_then(|flags| crate::atreq::empty_path(c_str_to_bytes(path), flags))
        .and_then(|()| crate::atreq::times(unsafe { pairs(times) }));
    let change = match checked {
        Ok(change) => change,
        Err(e) => return refuse(e),
    };
    on_at(dirfd, path, |at, path| files::set_times(at, path, flags & AT_SYMLINK_NOFOLLOW == 0, &change))
}

#[no_mangle]
pub unsafe extern "C" fn futimens(fd: i32, times: *const [i64; 2]) -> i32 {
    match crate::atreq::times(unsafe { pairs(times) }) {
        Ok(change) => files::set_times(At::Fd(fd), b"", false, &change).map_or_else(refuse_files, |()| 0),
        Err(e) => refuse(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn utimes(path: *const u8, times: *const [i64; 2]) -> i32 {
    match crate::atreq::utimes(unsafe { pairs(times) }) {
        Ok(change) => on_at(AT_FDCWD, path, |at, path| files::set_times(at, path, true, &change)),
        Err(e) => refuse(e),
    }
}

// struct stat

#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u64,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atim: Timespec,
    pub st_mtim: Timespec,
    pub st_ctim: Timespec,
}

const NANOS_PER_SEC: u64 = 1_000_000_000;

// mmap, munmap, mprotect, madvise: the process's own private memory.

const MAP_FAILED: *mut u8 = usize::MAX as *mut u8;

/// The protection `prot` names, or the errno it is refused with.
fn vm_prot(prot: i32) -> Result<syscall::VmProt, i32> {
    use crate::memreq::Access;
    match crate::memreq::access(prot) {
        None => Err(EINVAL),
        // POSIX's ENOTSUP, which is EOPNOTSUPP here.
        Some(Access::Exec) => Err(crate::errcode::EOPNOTSUPP),
        Some(Access::None) => Ok(syscall::VmProt::None),
        Some(Access::Read) => Ok(syscall::VmProt::Read),
        Some(Access::ReadWrite) => Ok(syscall::VmProt::ReadWrite),
    }
}

/// POSIX's errno for a refusal of the pages of a range: `ENOMEM` for pages
/// the process does not hold, `EACCES` for a protection a shared mapping was
/// not made with.
fn range_errno(e: syscall::SyscallError) -> i32 {
    match e {
        syscall::SyscallError::PermissionDenied => crate::errcode::EACCES,
        syscall::SyscallError::InvalidArgument => EINVAL,
        _ => crate::errcode::ENOMEM,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mmap(
    addr: *mut u8, len: usize, prot: i32, flags: i32, _fd: i32, _offset: i64,
) -> *mut u8 {
    use crate::memreq::{self, MapRefusal};

    if let Some(refusal) = memreq::mmap_refusal(addr.addr(), len, prot, flags) {
        crate::errno::set(match refusal {
            MapRefusal::Invalid => EINVAL,
            MapRefusal::File => crate::errcode::ENODEV,
            MapRefusal::Exec => crate::errcode::EOPNOTSUPP,
        });
        return MAP_FAILED;
    }
    let prot = match vm_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => {
            crate::errno::set(errno);
            return MAP_FAILED;
        }
    };
    // POSIX takes a null address with `MAP_FIXED` as the address zero, which
    // no process holds; anything else without it is only a hint.
    let at = if flags & memreq::MAP_FIXED != 0 { syscall::VmAt::Fixed(addr) } else { syscall::VmAt::ANYWHERE };
    // SAFETY: a `MAP_FIXED` caller gives up what those pages held, as POSIX says.
    match unsafe { syscall::vm_reserve(len, prot, at) } {
        Ok(ptr) => ptr,
        Err(e) => {
            crate::errno::set(match e {
                syscall::SyscallError::InvalidArgument => EINVAL,
                _ => crate::errcode::ENOMEM,
            });
            MAP_FAILED
        }
    }
}

/// Advice changes no access's meaning, so taking none of it is POSIX's.
#[no_mangle]
pub unsafe extern "C" fn posix_madvise(_addr: *mut u8, _len: usize, advice: i32) -> i32 {
    if crate::memreq::is_advice(advice) { 0 } else { EINVAL }
}

/// Linux's `madvise`, which POSIX does not define: its hints are taken and
/// ignored, and `MADV_DONTNEED` discards the range, which then reads zeros.
#[no_mangle]
pub unsafe extern "C" fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32 {
    use crate::memreq::Advice;
    let done = match crate::memreq::madvise(addr.addr(), advice) {
        None => Err(EINVAL),
        Some(Advice::Hint) => Ok(()),
        Some(Advice::Discard) => match crate::memreq::whole_pages(len) {
            None => Err(EINVAL),
            // SAFETY: the caller's, as Linux's `madvise` leaves it.
            Some(len) => unsafe { syscall::vm_discard(addr, len) }.map_err(range_errno),
        },
    };
    match done {
        Ok(()) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// POSIX's `mprotect`: every whole page `[addr, addr + len)` reaches into
/// takes `prot`, or none does.
#[no_mangle]
pub unsafe extern "C" fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32 {
    let done = vm_prot(prot).and_then(|prot| match crate::memreq::whole_pages(len) {
        None => Err(EINVAL),
        // SAFETY: the caller's, as POSIX's `mprotect` leaves it.
        Some(len) => unsafe { syscall::vm_protect(addr, len, prot) }.map_err(range_errno),
    });
    match done {
        Ok(()) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// Unmaps every whole page `[addr, addr + len)` reaches into
/// (`memreq::whole_pages`), as POSIX does: of any mappings, and none at all is
/// no refusal. `EINVAL` for an `addr` off a page or a `len` of zero, as POSIX
/// says, and `ENOMEM`, as Linux answers past its map count, where the space
/// holds as many ranges as it may and the call would cut one in two.
#[no_mangle]
pub unsafe extern "C" fn munmap(addr: *mut u8, len: usize) -> i32 {
    let unmapped = match crate::memreq::whole_pages(len) {
        None => Err(EINVAL),
        // SAFETY: the caller's, as C's `munmap` leaves it.
        Some(len) => unsafe { syscall::vm_release(addr, len) }.map_err(|e| match e {
            syscall::SyscallError::ResourceExhausted => crate::errcode::ENOMEM,
            _ => EINVAL,
        }),
    };
    match unmapped {
        Ok(()) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

// poll

#[repr(C)]
pub struct pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// What [`wait_fds`] came to.
struct Waited {
    /// Entries ready, their `revents` written.
    ready: i32,
    /// Whether the wake event beside them was readable.
    woken: bool,
}

/// The most entries and doorbells one poll watches: a poller's handles, one of
/// them the wake event a handler interrupts it by.
const MAX_POLLED: usize = toyos::poller::Poller::MAX_HANDLES as usize - 1;

/// `poll`'s wait over the `n` entries at `fds` until `deadline`, in the
/// monotonic clock's nanoseconds, watching `wake` beside them where given: it
/// answers once an entry is ready, `wake` is readable, or the deadline passes.
///
/// A stream socket is asked its rings first, so a level-triggered `poll` of
/// one that is ready answers at once; a direction that is not is counted in,
/// drained, armed and looked at again (`toyos::net::stream::watch`), and its
/// doorbell watched for reading: the connection for `POLLIN`, the bell for
/// `POLLOUT`. Every other descriptor is watched as it is, a listener's and a
/// datagram socket's by its holder's bell.
///
/// # Safety
/// `fds` points at `n` entries.
unsafe fn wait_fds(fds: *mut pollfd, n: usize, deadline: Option<u64>, wake: Option<toyos::BorrowedHandle<'static>>) -> Result<Waited, i32> {
    use toyos::net::stream::{self, Poll, Way};

    const POLLIN: i16 = 1;
    const POLLOUT: i16 = 4;
    const POLLNVAL: i16 = 0x20;

    if n == 0 && wake.is_none() {
        let left = deadline.map_or(u64::MAX, |at| at.saturating_sub(toyos_abi::clock::nanos_since_boot()));
        syscall::nanosleep(left);
        return Ok(Waited { ready: 0, woken: false });
    }
    let entries: alloc::vec::Vec<(i32, i16)> = (0..n).map(|i| ((*fds.add(i)).fd, (*fds.add(i)).events)).collect();
    // Every open descriptor held until `poll` returns, and each one's handles
    // taken from its held object once: a `close` on another thread meanwhile
    // releases nothing this call watches or drains, and a number it reuses
    // is never looked at.
    let held: alloc::vec::Vec<Option<toyos::fd::Hold>> = entries.iter().map(|&(fd, _)| toyos::fd::hold(fd).ok()).collect();
    let objects: alloc::vec::Vec<Option<toyos::fd::Lent<'_>>> = held.iter().map(|held| held.as_ref().map(toyos::fd::Hold::object)).collect();
    let streams: alloc::vec::Vec<Option<toyos::BorrowedHandle<'_>>> =
        objects.iter().map(|object| object.as_ref().and_then(crate::socket::stream_of)).collect();
    let ways = |events: i16| [(POLLIN, Way::Receive), (POLLOUT, Way::Send)].into_iter().filter(move |(flag, _)| events & flag != 0);
    let doorbells: usize = entries.iter().zip(&streams).filter(|(_, stream)| stream.is_some()).map(|((_, events), _)| ways(*events).count()).sum();
    // A C caller asking to watch more than a poller can is not a bug in this
    // library, so it gets POSIX's own answer for an nfds it cannot serve.
    if n + doorbells > MAX_POLLED {
        return Err(EINVAL);
    }

    // The descriptors that are no stream, each as the kernel handle it is
    // watched by, and that handle's number, which `pollreq` plans by.
    let watched: alloc::vec::Vec<Option<toyos::BorrowedHandle<'_>>> = objects
        .iter()
        .zip(&streams)
        .map(|(object, stream)| match stream {
            Some(_) => None,
            None => object.as_ref().and_then(|object| crate::socket::watched_of(object).or_else(|| object.kernel_handle())),
        })
        .collect();
    let others: alloc::vec::Vec<(i32, i16)> = entries
        .iter()
        .zip(&watched)
        .map(|(&(_, events), handle)| handle.map_or((-1, 0), |handle| (handle.raw().0 as i32, events)))
        .collect();
    // A descriptor that is not open answers `POLLNVAL`; a negative one is
    // ignored, as POSIX has it.
    let closed: alloc::vec::Vec<bool> = entries.iter().zip(&held).map(|(&(fd, _), held)| fd >= 0 && held.is_none()).collect();
    loop {
        let mut revents: alloc::vec::Vec<i16> = closed.iter().map(|&closed| if closed { POLLNVAL } else { 0 }).collect();
        let mut watching = alloc::vec::Vec::new();
        for (i, &(_, events)) in entries.iter().enumerate() {
            let Some(handle) = streams[i] else { continue };
            for (flag, way) in ways(events) {
                match stream::watch(handle, way) {
                    Ok(Poll::Ready) | Err(_) => revents[i] |= flag,
                    Ok(Poll::Watching(w)) => watching.push((i, flag, stream::doorbell(handle, way), w)),
                }
            }
        }
        let ready_now = revents.iter().any(|&r| r != 0);
        let left = match deadline {
            _ if ready_now => 0,
            None => u64::MAX,
            Some(at) => at.saturating_sub(toyos_abi::clock::nanos_since_boot()),
        };
        let wake_token = (n + watching.len()) as u64;
        let poller = toyos::poller::Poller::new((n + watching.len()) as u32 + 1);
        for (entry, _, interest) in crate::pollreq::watches(&others) {
            if let Some(handle) = watched[entry] {
                poller.watch(&handle, interest, entry as u64);
            }
        }
        for (k, (_, _, doorbell, _)) in watching.iter().enumerate() {
            if let Ok(doorbell) = doorbell {
                poller.watch(doorbell, toyos::poller::READABLE, (n + k) as u64);
            }
        }
        if let Some(wake) = wake {
            poller.watch(&wake, toyos::poller::READABLE, wake_token);
        }
        let mut answered = alloc::vec![false; n + watching.len()];
        let mut woken = false;
        poller.wait(if ready_now { 0 } else { 1 }, left, |token| {
            if token == wake_token {
                woken = true;
            } else if let Some(slot) = answered.get_mut(token as usize) {
                *slot = true;
            }
        });
        for (i, flag, _, w) in watching {
            if stream::unwatch(w) {
                revents[i] |= flag;
            }
        }
        for i in 0..n {
            if others[i].0 >= 0 && answered[crate::pollreq::watch_of(&others, i)] {
                revents[i] = entries[i].1;
            }
        }
        let ready = revents.iter().filter(|&&r| r != 0).count();
        let expired = deadline.is_some_and(|at| toyos_abi::clock::nanos_since_boot() >= at);
        if ready > 0 || expired || woken {
            for (i, r) in revents.into_iter().enumerate() {
                (*fds.add(i)).revents = r;
            }
            return Ok(Waited { ready: ready as i32, woken });
        }
    }
}

/// `deadline` from now in `timeout` milliseconds, none for a negative one.
fn after_ms(timeout: i32) -> Option<u64> {
    (timeout >= 0).then(|| toyos_abi::clock::nanos_since_boot().saturating_add(timeout as u64 * 1_000_000))
}

/// `poll`'s and `ppoll`'s work, `mask` the thread's while it waits where
/// given: the ready count, 0 past the deadline, or `EINTR` once a handler ran,
/// which no `SA_RESTART` restarts.
///
/// # Safety
/// `fds` points at `n` entries.
unsafe fn poll_until(fds: *mut pollfd, n: usize, deadline: Option<u64>, mask: Option<u64>) -> i32 {
    let mut call = Call::enter(mask, false, mask.is_some());
    loop {
        if let Some(call) = call.as_mut() {
            if let Some(delivery) = call.take() {
                call.interrupted(delivery);
                return refuse(EINTR);
            }
        }
        match unsafe { wait_fds(fds, n, deadline, call.as_ref().map(Call::wake)) } {
            Ok(Waited { ready, woken: false }) => return ready,
            Ok(Waited { ready, .. }) if ready > 0 => return ready,
            Ok(_) => {}
            Err(errno) => return refuse(errno),
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn poll(fds: *mut pollfd, nfds: u32, timeout: i32) -> i32 {
    unsafe { poll_until(fds, nfds as usize, after_ms(timeout), None) }
}

/// `poll` with `sigmask`, when not null, the thread's mask while it waits: a
/// signal it unblocks is taken there, and once it returns the thread's own
/// mask is back.
#[no_mangle]
pub unsafe extern "C" fn ppoll(fds: *mut pollfd, nfds: u32, timeout: *const Timespec, sigmask: *const u64) -> i32 {
    let deadline = match unsafe { timeout.as_ref() } {
        None => None,
        Some(ts) => match crate::time::nanos_of(ts) {
            Some(nanos) => Some(toyos_abi::clock::nanos_since_boot().saturating_add(nanos)),
            None => return refuse(EINVAL),
        },
    };
    let mask = unsafe { sigmask.as_ref() }.copied().unwrap_or_else(crate::signal::mask);
    unsafe { poll_until(fds, nfds as usize, deadline, Some(mask)) }
}

/// Wait for a handler to run: `-1` and `EINTR` once one has.
#[no_mangle]
pub unsafe extern "C" fn pause() -> i32 {
    unsafe { poll_until(ptr::null_mut(), 0, None, Some(crate::signal::mask())) }
}

/// [`pause`], with `mask` the thread's while it waits.
#[no_mangle]
pub unsafe extern "C" fn sigsuspend(mask: *const u64) -> i32 {
    unsafe { poll_until(ptr::null_mut(), 0, None, Some(*mask)) }
}
