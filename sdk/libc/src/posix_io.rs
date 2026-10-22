// POSIX file I/O: a path or a descriptor is `toyos::files`', which routes it to
// the kernel or to the file server behind one of the process's directory
// capabilities.

#![allow(non_camel_case_types)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use toyos_abi::{RawHandle, Rights};
use toyos_abi::syscall::{self, OpenFlags, SeekFrom};

use toyos::files::{self, Open};

use crate::errcode::{EBADF, EINTR, EINVAL};
use crate::signal::Call;
use crate::time::Timespec;

// The values `<fcntl.h>` declares.

pub(crate) const O_RDONLY: i32 = 0;
pub(crate) const O_WRONLY: i32 = 1;
pub(crate) const O_RDWR: i32 = 2;
pub(crate) const O_CREAT: i32 = 0x40;
pub(crate) const O_EXCL: i32 = 0x80;
pub(crate) const O_TRUNC: i32 = 0x200;
pub(crate) const O_APPEND: i32 = 0x400;
pub(crate) const O_CLOEXEC: i32 = 0x80000;

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
pub(crate) fn install(handle: RawHandle, cloexec: bool) -> Result<i32, Errno> {
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
pub unsafe extern "C" fn open(path: *const u8, flags: i32, _mode: u32) -> i32 {
    let path = c_str_to_bytes(path);
    let cloexec = flags & O_CLOEXEC != 0;
    let (readable, writable) = read_write(flags);
    if path == DEV_NULL {
        return match syscall::null(null_rights(readable, writable)) {
            Ok(handle) => install(handle, cloexec).unwrap_or_else(refuse),
            Err(e) => set_errno(e),
        };
    }
    let how = Open {
        read: readable,
        write: writable,
        append: flags & O_APPEND != 0,
        create: flags & O_CREAT != 0 && flags & O_EXCL == 0,
        create_new: flags & O_CREAT != 0 && flags & O_EXCL != 0,
        truncate: flags & O_TRUNC != 0,
        cloexec,
    };
    files::open(path, how).unwrap_or_else(refuse_files)
}

/// Whether `open`'s `flags` ask to read and to write.
fn read_write(flags: i32) -> (bool, bool) {
    let access = flags & 3;
    (access == O_RDONLY || access == O_RDWR, access == O_WRONLY || access == O_RDWR)
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
    match toyos::fd::close(raw_fd) {
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
    let Ok((_held, object)) = toyos::fd::hold(raw_fd) else { return blocking(slice) };
    if object.release.is_some() || object.handle == toyos_abi::HANDLE_INVALID {
        drop(call);
        return blocking(slice);
    }
    loop {
        if let Some(delivery) = call.take() {
            if !call.interrupted(delivery) {
                return refuse(EINTR) as isize;
            }
            continue;
        }
        match syscall::read_nonblock(object.handle, slice) {
            Ok(n) => return n as isize,
            Err(syscall::SyscallError::WouldBlock) => {}
            Err(e) => return set_errno(e) as isize,
        }
        let poller = toyos::poller::Poller::new(2);
        poller.watch_raw(object.handle, toyos::poller::READABLE, 0);
        poller.watch_raw(call.wake(), toyos::poller::READABLE, 1);
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
unsafe fn answered(answer: Result<syscall::Stat, files::Error>, buf: *mut Stat) -> i32 {
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
        // Neither the kernel's `/tmp` nor the file server has hard links.
        s.st_nlink = 1;
        s.st_size = st.size as i64;
        s.st_mtim = Timespec {
            tv_sec: (st.mtime / NANOS_PER_SEC) as i64,
            tv_nsec: (st.mtime % NANOS_PER_SEC) as i64,
        };
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
    match toyos::fd::dup2(old_fd, new_fd, true) {
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
    let ends = match syscall::pipe() {
        Ok(ends) => ends,
        Err(e) => return set_errno(e),
    };
    let read = match install(ends.read, false) {
        Ok(fd) => fd,
        Err(e) => {
            syscall::close(ends.write);
            return refuse(e);
        }
    };
    let write = match install(ends.write, false) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = toyos::fd::close(read);
            return refuse(e);
        }
    };
    *pipefd = read;
    *pipefd.add(1) = write;
    0
}

// Path operations

#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const u8) -> i32 {
    files::unlink(c_str_to_bytes(path)).map_or_else(refuse_files, |()| 0)
}

#[no_mangle]
pub unsafe extern "C" fn rmdir(path: *const u8) -> i32 {
    files::rmdir(c_str_to_bytes(path)).map_or_else(refuse_files, |()| 0)
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
    unsafe { answered(files::stat(c_str_to_bytes(path), true), buf) }
}

/// `stat` that does not follow a final symbolic link.
#[no_mangle]
pub unsafe extern "C" fn lstat(path: *const u8, buf: *mut Stat) -> i32 {
    unsafe { answered(files::stat(c_str_to_bytes(path), false), buf) }
}

/// `SYS_READLINK` answers `NotFound` for a path that is no link as for one
/// that names nothing, and POSIX tells them apart: `EINVAL` for the first. A
/// file server tells them apart itself.
#[no_mangle]
pub unsafe extern "C" fn readlink(path: *const u8, buf: *mut u8, size: usize) -> isize {
    let Some(len) = crate::linkreq::target_len(size) else {
        crate::errno::set(EINVAL);
        return -1;
    };
    match files::read_link(c_str_to_bytes(path)) {
        Ok(target) => {
            let n = target.len().min(len);
            // SAFETY: C's caller hands `size` bytes at `buf`, and `len` is at most it.
            unsafe { ptr::copy_nonoverlapping(target.as_ptr(), buf, n) };
            n as isize
        }
        Err(files::Error::Kernel(syscall::SyscallError::NotFound)) => {
            // `stat` sets `errno` for a path that names nothing.
            if unsafe { stat(path, ptr::null_mut()) } == 0 {
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
    let link = c_str_to_bytes(link);
    if !files::is_served(link) {
        return refuse(crate::errcode::ENOSYS);
    }
    files::symlink(c_str_to_bytes(target), link).map_or_else(refuse_files, |()| 0)
}

struct TheServers;

impl crate::resolve::Served for TheServers {
    fn holds(&mut self, at: &[u8]) -> bool {
        files::is_served(at)
    }

    fn name(&mut self, at: &[u8]) -> Result<crate::resolve::Name, i32> {
        let st = files::stat(at, false).map_err(crate::errcode::of_files)?;
        match st.file_type {
            syscall::FileType::Symlink => files::read_link(at).map(crate::resolve::Name::Link).map_err(crate::errcode::of_files),
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
        syscall::open(at, OpenFlags::READ).map(syscall::close)
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
    files::stat(path, true).is_ok_and(|st| st.file_type == syscall::FileType::File)
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

/// An open directory: its whole listing, and where the next `readdir` starts.
pub struct DIR {
    listing: Listing,
    at: usize,
}

/// A listing as its server answered it: the kernel's, or a file server's
/// entries, `d_type` and name.
enum Listing {
    Kernel(Vec<u8>),
    Served(Vec<(u8, Box<[u8]>)>),
}

#[repr(C)]
pub struct dirent {
    pub d_ino: u64,
    pub d_type: u8,
    pub d_name: [u8; crate::listing::D_NAME],
}

/// `d_type`s: the listing tells a directory from everything else, and no more.
const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

#[no_mangle]
pub unsafe extern "C" fn opendir(path: *const u8) -> *mut DIR {
    let path = c_str_to_bytes(path);
    let listing = match files::is_served(path) {
        false => crate::listing::whole(|buf| syscall::readdir(path, buf)).map(Listing::Kernel).map_err(crate::errcode::of),
        true => served_listing(path),
    };
    match listing {
        Ok(listing) => Box::into_raw(Box::new(DIR { listing, at: 0 })),
        Err(e) => {
            crate::errno::set(e);
            ptr::null_mut()
        }
    }
}

/// A file server's listing of `path`, each entry's `d_type` beside its name.
fn served_listing(path: &[u8]) -> Result<Listing, Errno> {
    let listing = files::read_dir(path).map_err(crate::errcode::of_files)?;
    let mut entries = Vec::new();
    toyos::fs::for_each_entry(&listing, |kind, _, name| {
        let d_type = match kind {
            toyos::fs::KIND_DIR => DT_DIR,
            toyos::fs::KIND_FILE => DT_REG,
            toyos::fs::KIND_SYMLINK => DT_LNK,
            _ => DT_UNKNOWN,
        };
        entries.push((d_type, Box::from(name.as_bytes())));
    })
    .map_err(|e| crate::errcode::of_files(files::Error::Served(e)))?;
    Ok(Listing::Served(entries))
}

#[no_mangle]
pub unsafe extern "C" fn readdir(dir: *mut DIR) -> *mut dirent {
    if dir.is_null() { return ptr::null_mut(); }
    let d = &mut *dir;
    let (d_ino, d_type, name) = match &d.listing {
        Listing::Kernel(listing) => {
            let Some(entry) = syscall::dirent(listing, &mut d.at) else { return ptr::null_mut() };
            (entry.node, if entry.is_dir { DT_DIR } else { DT_UNKNOWN }, entry.name)
        }
        Listing::Served(entries) => {
            let Some((d_type, name)) = entries.get(d.at) else { return ptr::null_mut() };
            d.at += 1;
            // A file server answers no node in a listing.
            (0, *d_type, &name[..])
        }
    };
    let Some(d_name) = crate::listing::d_name(name) else {
        crate::errno::set(crate::errcode::EOVERFLOW);
        return ptr::null_mut();
    };

    // Use a static buffer for the dirent (not thread-safe, matching POSIX convention)
    static mut DIRENT_BUF: dirent = dirent { d_ino: 0, d_type: 0, d_name: [0; crate::listing::D_NAME] };
    let ent = &raw mut DIRENT_BUF;
    ent.write(dirent { d_ino, d_type, d_name });
    ent
}

#[no_mangle]
pub unsafe extern "C" fn closedir(dir: *mut DIR) -> i32 {
    if dir.is_null() { return -1; }
    // SAFETY: a `DIR` `opendir` answered, closed once.
    drop(unsafe { Box::from_raw(dir) });
    0
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

// mmap/munmap (real implementations using toyos-abi)

#[no_mangle]
pub unsafe extern "C" fn mmap(
    addr: *mut u8, len: usize, prot: i32, flags: i32, _fd: i32, _offset: i64,
) -> *mut u8 {
    use crate::memreq::{self, MapRefusal};
    use toyos_abi::syscall::{MmapProt, MmapFlags};

    const MAP_FAILED: *mut u8 = usize::MAX as *mut u8;
    if let Some(refusal) = memreq::mmap_refusal(addr.addr(), len, prot, flags) {
        crate::errno::set(match refusal {
            MapRefusal::Invalid => EINVAL,
            MapRefusal::File => crate::errcode::ENODEV,
            // POSIX's ENOTSUP, which is EOPNOTSUPP here.
            MapRefusal::Exec => crate::errcode::EOPNOTSUPP,
        });
        return MAP_FAILED;
    }

    let mut mp = MmapProt::NONE;
    if prot & memreq::PROT_READ != 0 { mp = mp | MmapProt::READ; }
    if prot & memreq::PROT_WRITE != 0 { mp = mp | MmapProt::WRITE; }

    let mut mf = MmapFlags::PRIVATE | MmapFlags::ANONYMOUS;
    if flags & memreq::MAP_FIXED != 0 { mf = mf | MmapFlags::FIXED; }

    // Asked for in whole pages, the length `munmap` names it by; more pages
    // than a `usize` counts is the null the kernel answers a size it cannot map.
    let ptr = memreq::whole_pages(len)
        .map_or(core::ptr::null_mut(), |len| unsafe { syscall::mmap(addr, len, mp, mf) });
    if ptr.is_null() {
        // The kernel's refusal does not say which it is: ENOMEM is POSIX's
        // for a place or size the address space does not allow.
        crate::errno::set(crate::errcode::ENOMEM);
        MAP_FAILED
    } else {
        ptr
    }
}

/// Advice changes no access's meaning, so taking none of it is POSIX's.
#[no_mangle]
pub unsafe extern "C" fn posix_madvise(_addr: *mut u8, _len: usize, advice: i32) -> i32 {
    if crate::memreq::is_advice(advice) { 0 } else { EINVAL }
}

/// Linux's `madvise`, which POSIX does not define (`memreq::madvise_refusal`).
#[no_mangle]
pub unsafe extern "C" fn madvise(addr: *mut u8, _len: usize, advice: i32) -> i32 {
    use crate::memreq::AdviceRefusal;
    match crate::memreq::madvise_refusal(addr.addr(), advice) {
        None => 0,
        Some(refusal) => {
            crate::errno::set(match refusal {
                AdviceRefusal::Invalid => EINVAL,
                AdviceRefusal::Discard => crate::errcode::ENOSYS,
            });
            -1
        }
    }
}

/// Unmaps the whole mapping at `addr` or nothing. `len` names it by the pages
/// it reaches into (`memreq::whole_pages`), as POSIX counts: every page of
/// the mapping, or the kernel refuses. A `len` that names some of a mapping's
/// pages, which POSIX would unmap, is refused with the rest
/// (`issues/libc-munmap-refuses-part-of-a-mapping.md`). `EINVAL` is the one
/// error POSIX gives `munmap`, so it is every refusal's.
#[no_mangle]
pub unsafe extern "C" fn munmap(addr: *mut u8, len: usize) -> i32 {
    let unmapped = crate::memreq::whole_pages(len)
        // SAFETY: the caller's, as C's `munmap` leaves it.
        .is_some_and(|len| unsafe { syscall::munmap(addr, len) }.is_ok());
    if unmapped {
        0
    } else {
        crate::errno::set(EINVAL);
        -1
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
    /// Whether the wake pipe beside them was readable.
    woken: bool,
}

/// The most entries and doorbells one poll watches: a poller's handles, one of
/// them the wake pipe a handler interrupts it by.
const MAX_POLLED: usize = toyos::poller::Poller::MAX_HANDLES as usize - 1;

/// `poll`'s wait over the `n` entries at `fds` until `deadline`, in the
/// monotonic clock's nanoseconds, watching `wake` beside them where given: it
/// answers once an entry is ready, `wake` is readable, or the deadline passes.
///
/// A stream socket is asked its rings first, so a level-triggered `poll` of
/// one that is ready answers at once; a direction that is not is counted in,
/// drained, armed and looked at again (`toyos::net::stream::watch`), and its
/// doorbell watched for reading: the connection for `POLLIN`, the bell for
/// `POLLOUT`. Every other descriptor, a listener's wake pipe and a datagram
/// socket's receive pipe for a socket's, is watched as it is.
///
/// # Safety
/// `fds` points at `n` entries.
unsafe fn wait_fds(fds: *mut pollfd, n: usize, deadline: Option<u64>, wake: Option<RawHandle>) -> Result<Waited, i32> {
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
    let held: alloc::vec::Vec<Option<(toyos::fd::Hold, toyos::fd::Object)>> =
        entries.iter().map(|&(fd, _)| toyos::fd::hold(fd).ok()).collect();
    let objects = || held.iter().map(|held| held.as_ref().map(|(_, object)| object));
    let streams: alloc::vec::Vec<Option<RawHandle>> = objects().map(|object| object.and_then(crate::socket::stream_of)).collect();
    let ways = |events: i16| [(POLLIN, Way::Receive), (POLLOUT, Way::Send)].into_iter().filter(move |(flag, _)| events & flag != 0);
    let doorbells: usize = entries.iter().zip(&streams).filter(|(_, stream)| stream.is_some()).map(|((_, events), _)| ways(*events).count()).sum();
    // A C caller asking to watch more than a poller can is not a bug in this
    // library, so it gets POSIX's own answer for an nfds it cannot serve.
    if n + doorbells > MAX_POLLED {
        return Err(EINVAL);
    }

    // The descriptors that are no stream, each as the kernel handle it is watched by.
    let others: alloc::vec::Vec<(i32, i16)> = entries
        .iter()
        .zip(objects())
        .zip(&streams)
        .map(|((&(_, events), object), stream)| match stream {
            Some(_) => (-1, 0),
            None => {
                let handle = object.and_then(|object| {
                    crate::socket::watched_of(object).or_else(|| object.kernel_handle())
                });
                (handle.map_or(-1, |handle| handle.0 as i32), events)
            }
        })
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
        for (entry, fd, interest) in crate::pollreq::watches(&others) {
            if fd >= 0 {
                poller.watch_raw(toyos_abi::RawHandle(fd as u32), interest, entry as u64);
            }
        }
        for (k, (_, _, doorbell, _)) in watching.iter().enumerate() {
            if let Ok(doorbell) = doorbell {
                poller.watch_raw(*doorbell, toyos::poller::READABLE, (n + k) as u64);
            }
        }
        if let Some(wake) = wake {
            poller.watch_raw(wake, toyos::poller::READABLE, wake_token);
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
