// POSIX file I/O — thin wrappers around toyos-abi syscalls.

#![allow(non_camel_case_types)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, OpenFlags, SeekFrom};

use crate::errno::{EACCES, EAGAIN, EEXIST, EINVAL, EIO, ENOENT, EPIPE};
use crate::fdreq::CloseOnExec;
use crate::pthread::Lock;
use crate::time::Timespec;

// Constants (matching POSIX / Linux values)

const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0x40;
const O_TRUNC: i32 = 0x200;
const O_APPEND: i32 = 0x400;

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

fn set_errno(e: toyos_abi::syscall::SyscallError) -> i32 {
    use toyos_abi::syscall::SyscallError;
    let code = match e {
        SyscallError::NotFound => ENOENT,
        SyscallError::PermissionDenied => EACCES,
        SyscallError::AlreadyExists => EEXIST,
        SyscallError::InvalidArgument => EINVAL,
        SyscallError::WouldBlock => EAGAIN,
        // A write whose reader is gone, and not `ENOENT`, which says the file does not exist.
        SyscallError::Gone => EPIPE,
        SyscallError::Io => EIO,
        _ => EINVAL,
    };
    crate::errno::set(code);
    -1
}

fn fd(raw: i32) -> RawHandle { RawHandle(raw as u32) }

/// A `write` to a descriptor. Stdout and stderr go through the SDK's stream
/// sinks, which are a program's log ring when its slot holds one
/// (`toyos::log::stdio`); every other descriptor is its handle.
pub fn write_fd(raw: i32, buf: &[u8]) -> Result<usize, syscall::SyscallError> {
    use toyos::log::stdio::{self, Stream};
    match raw {
        1 => stdio::write(Stream::Out, buf),
        2 => stdio::write(Stream::Err, buf),
        other => syscall::write(fd(other), buf),
    }
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
    let path_bytes = c_str_to_bytes(path);
    let mut oflags = OpenFlags(0);

    let access = flags & 3;
    if access == O_RDONLY || access == O_RDWR { oflags |= OpenFlags::READ; }
    if access == O_WRONLY || access == O_RDWR { oflags |= OpenFlags::WRITE; }
    if flags & O_CREAT != 0 { oflags |= OpenFlags::CREATE; }
    if flags & O_TRUNC != 0 { oflags |= OpenFlags::TRUNCATE; }
    if flags & O_APPEND != 0 { oflags |= OpenFlags::APPEND; }

    match syscall::open(path_bytes, oflags) {
        Ok(f) => {
            CLOEXEC.lock().opened(f.0 as i32, flags);
            f.0 as i32
        }
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn creat(path: *const u8, mode: u32) -> i32 {
    unsafe { open(path, O_WRONLY | O_CREAT | O_TRUNC, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn close(raw_fd: i32) -> i32 {
    CLOEXEC.lock().cleared(raw_fd);
    syscall::close(fd(raw_fd));
    0
}

#[no_mangle]
pub unsafe extern "C" fn read(raw_fd: i32, buf: *mut u8, count: usize) -> isize {
    if buf.is_null() || count == 0 { return 0; }
    let slice = core::slice::from_raw_parts_mut(buf, count);
    match syscall::read(fd(raw_fd), slice) {
        Ok(n) => n as isize,
        Err(e) => { set_errno(e); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn write(raw_fd: i32, buf: *const u8, count: usize) -> isize {
    if buf.is_null() || count == 0 { return 0; }
    let slice = core::slice::from_raw_parts(buf, count);
    match write_fd(raw_fd, slice) {
        Ok(n) => n as isize,
        Err(e) => { set_errno(e); -1 }
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
    match syscall::seek(fd(raw_fd), pos) {
        Ok(n) => n as i64,
        Err(e) => { set_errno(e); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn fstat(raw_fd: i32, buf: *mut Stat) -> i32 {
    match syscall::fstat(fd(raw_fd)) {
        Ok(st) => {
            if !buf.is_null() {
                ptr::write_bytes(buf, 0, 1);
                let s = &mut *buf;
                s.st_size = st.size as i64;
                s.st_mtim = Timespec {
                    tv_sec: (st.mtime / NANOS_PER_SEC) as i64,
                    tv_nsec: (st.mtime % NANOS_PER_SEC) as i64,
                };
                s.st_mode = match st.file_type {
                    syscall::FileType::File => S_IFREG | 0o644,
                    syscall::FileType::Pipe => S_IFIFO | 0o644,
                    syscall::FileType::Tty => S_IFCHR | 0o644,
                    syscall::FileType::Keyboard => S_IFCHR | 0o444,
                    syscall::FileType::Serial => S_IFCHR | 0o644,
                    _ => 0o644,
                };
            }
            0
        }
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn ftruncate(raw_fd: i32, length: i64) -> i32 {
    match syscall::ftruncate(fd(raw_fd), length as u64) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn fsync(raw_fd: i32) -> i32 {
    match syscall::fsync(fd(raw_fd)) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dup(raw_fd: i32) -> i32 {
    match syscall::dup(fd(raw_fd)) {
        Ok(f) => f.0 as i32,
        Err(e) => set_errno(e),
    }
}

/// **This does not honour POSIX's "returns `newfd`".** `newfd` is a slot, and
/// the handle the kernel hands back carries that slot's generation, so
/// `dup2(x, 1)` answers `1` only while slot 1 has never been closed. Nothing in
/// the tree redirects by closing a slot first, and this layer is where the
/// bookkeeping to fake it would go if something ever does.
#[no_mangle]
pub unsafe extern "C" fn dup2(old_fd: i32, new_fd: i32) -> i32 {
    if old_fd == new_fd { return new_fd; }
    let Ok(slot) = u16::try_from(new_fd) else {
        return set_errno(syscall::SyscallError::InvalidArgument);
    };
    match syscall::dup2(fd(old_fd), slot) {
        Ok(f) => {
            CLOEXEC.lock().cleared(f.0 as i32);
            // The slot holds something else now, so the stream is asked again.
            match slot {
                1 => toyos::log::stdio::forget(toyos::log::stdio::Stream::Out),
                2 => toyos::log::stdio::forget(toyos::log::stdio::Stream::Err),
                _ => {}
            }
            f.0 as i32
        }
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pipe(pipefd: *mut i32) -> i32 {
    let fds = match syscall::pipe() {
        Ok(fds) => fds,
        Err(e) => return set_errno(e),
    };
    *pipefd = fds.read.0 as i32;
    *pipefd.add(1) = fds.write.0 as i32;
    0
}

// Path operations

#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const u8) -> i32 {
    match syscall::delete(c_str_to_bytes(path)) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rmdir(path: *const u8) -> i32 {
    match syscall::rmdir(c_str_to_bytes(path)) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
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
    match syscall::chdir(c_str_to_bytes(path)) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }
}

/// Whether `path` names a directory, asked through `readdir`.
///
/// `open` refuses every directory, so this is the only way to tell. The buffer
/// is deliberately tiny — only whether the kernel accepted the path matters,
/// and a listing that does not fit is reported rather than written, so a
/// directory too large to list is still a yes.
unsafe fn is_dir(path_bytes: &[u8]) -> bool {
    let mut probe = [0u8; 1];
    matches!(
        syscall::readdir(path_bytes, &mut probe),
        Ok(_) | Err(syscall::SyscallError::ResourceExhausted)
    )
}

/// Fill `buf` with what a directory looks like to `stat`: a mode and nothing
/// else, because `readdir` answers existence and the kernel keeps no size or
/// mtime for one.
unsafe fn stat_a_directory(buf: *mut Stat) -> i32 {
    if !buf.is_null() {
        ptr::write_bytes(buf, 0, 1);
        (*buf).st_mode = S_IFDIR | 0o755;
    }
    0
}

// stat by path: open + fstat + close, or readdir for what open refuses
#[no_mangle]
pub unsafe extern "C" fn stat(path: *const u8, buf: *mut Stat) -> i32 {
    stat_impl(path, buf)
}

/// `stat` that does not follow a final symbolic link.
///
/// `SYS_READLINK` is what distinguishes one, and it is asked first: it succeeds
/// on a link and on nothing else, so a success is both the answer and the
/// target length `st_size` reports. Everything else is [`stat`].
#[no_mangle]
pub unsafe extern "C" fn lstat(path: *const u8, buf: *mut Stat) -> i32 {
    let path_bytes = c_str_to_bytes(path);
    let mut target = [0u8; 4096];
    if let Ok(n) = syscall::readlink(path_bytes, &mut target) {
        if !buf.is_null() {
            ptr::write_bytes(buf, 0, 1);
            let s = &mut *buf;
            s.st_mode = S_IFLNK | 0o777;
            s.st_size = n as i64;
        }
        return 0;
    }
    stat_impl(path, buf)
}

/// `SYS_READLINK` answers `NotFound` for a path that is no link as for one
/// that names nothing, and POSIX tells them apart: `EINVAL` for the first.
#[no_mangle]
pub unsafe extern "C" fn readlink(path: *const u8, buf: *mut u8, size: usize) -> isize {
    let Some(len) = crate::linkreq::target_len(size) else {
        crate::errno::set(EINVAL);
        return -1;
    };
    // SAFETY: C's caller hands `size` bytes at `buf`.
    let target = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match syscall::readlink(c_str_to_bytes(path), target) {
        Ok(n) => n as isize,
        Err(syscall::SyscallError::NotFound) => {
            // `stat_impl` sets `errno` for a path that names nothing.
            if unsafe { stat_impl(path, ptr::null_mut()) } == 0 {
                crate::errno::set(EINVAL);
            }
            -1
        }
        Err(e) => set_errno(e) as isize,
    }
}

unsafe fn stat_impl(path: *const u8, buf: *mut Stat) -> i32 {
    let path_bytes = c_str_to_bytes(path);
    // Try opening read-only
    match syscall::open(path_bytes, OpenFlags::READ) {
        Ok(f) => {
            let result = fstat(f.0 as i32, buf);
            syscall::close(f);
            result
        }
        Err(_) if is_dir(path_bytes) => stat_a_directory(buf),
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn access(path: *const u8, _mode: i32) -> i32 {
    // ToyOS has no permissions model; just check existence
    let path_bytes = c_str_to_bytes(path);
    match syscall::open(path_bytes, OpenFlags::READ) {
        Ok(f) => { syscall::close(f); 0 }
        Err(_) if is_dir(path_bytes) => 0,
        Err(e) => set_errno(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn isatty(raw_fd: i32) -> i32 {
    match syscall::fstat(fd(raw_fd)) {
        Ok(st) => (st.file_type == syscall::FileType::Tty || st.file_type == syscall::FileType::Keyboard) as i32,
        Err(_) => 0,
    }
}

static mut UMASK_VAL: u32 = 0o022;

#[no_mangle]
pub unsafe extern "C" fn umask(mask: u32) -> u32 {
    let old = unsafe { UMASK_VAL };
    unsafe { UMASK_VAL = mask & 0o777; }
    old
}

static CLOEXEC: Lock<CloseOnExec> = Lock::new(CloseOnExec::new());

/// `arg` is the register C's variadic third argument arrives in (`fdreq`).
#[no_mangle]
pub unsafe extern "C" fn fcntl(raw_fd: i32, cmd: i32, arg: u64) -> i32 {
    use crate::fdreq::Command;
    let refused = match crate::fdreq::command(cmd, arg) {
        Command::DupAtLeast(floor) => {
            let duplicate = crate::fdreq::dup_at_least(
                floor,
                || syscall::dup(fd(raw_fd)).map(|h| h.0),
                |below| syscall::close(RawHandle(below)),
            );
            return match duplicate {
                Ok(n) => n as i32,
                Err(e) => set_errno(e),
            };
        }
        Command::GetFd => return CLOEXEC.lock().flags(raw_fd),
        Command::SetFd(cloexec) => {
            CLOEXEC.lock().set(raw_fd, cloexec);
            return 0;
        }
        Command::Invalid => EINVAL,
        Command::Unsupported => crate::errno::ENOSYS,
    };
    crate::errno::set(refused);
    -1
}

// Directory operations

/// An open directory: its whole listing, and where the next `readdir` starts.
pub struct DIR {
    listing: Vec<u8>,
    at: usize,
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

#[no_mangle]
pub unsafe extern "C" fn opendir(path: *const u8) -> *mut DIR {
    let path_bytes = c_str_to_bytes(path);
    match crate::listing::whole(|buf| syscall::readdir(path_bytes, buf)) {
        Ok(listing) => Box::into_raw(Box::new(DIR { listing, at: 0 })),
        Err(e) => {
            set_errno(e);
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn readdir(dir: *mut DIR) -> *mut dirent {
    if dir.is_null() { return ptr::null_mut(); }
    let d = &mut *dir;
    let start = d.at;
    let Some(entry) = syscall::dirent(&d.listing, &mut d.at) else { return ptr::null_mut() };
    let Some(d_name) = crate::listing::d_name(entry.name) else {
        crate::errno::set(crate::errno::EOVERFLOW);
        return ptr::null_mut();
    };

    // Use a static buffer for the dirent (not thread-safe, matching POSIX convention)
    static mut DIRENT_BUF: dirent = dirent { d_ino: 0, d_type: 0, d_name: [0; crate::listing::D_NAME] };
    let ent = &raw mut DIRENT_BUF;
    ent.write(dirent { d_ino: (start + 1) as u64, d_type: if entry.is_dir { DT_DIR } else { DT_UNKNOWN }, d_name });
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
            MapRefusal::File => crate::errno::ENODEV,
            // POSIX's ENOTSUP, which is EOPNOTSUPP here.
            MapRefusal::Exec => crate::errno::EOPNOTSUPP,
        });
        return MAP_FAILED;
    }

    let mut mp = MmapProt::NONE;
    if prot & memreq::PROT_READ != 0 { mp = mp | MmapProt::READ; }
    if prot & memreq::PROT_WRITE != 0 { mp = mp | MmapProt::WRITE; }

    let mut mf = MmapFlags::PRIVATE | MmapFlags::ANONYMOUS;
    if flags & memreq::MAP_FIXED != 0 { mf = mf | MmapFlags::FIXED; }

    let ptr = unsafe { syscall::mmap(addr, len, mp, mf) };
    if ptr.is_null() {
        // The kernel's refusal does not say which it is: ENOMEM is POSIX's
        // for a place or size the address space does not allow.
        crate::errno::set(crate::errno::ENOMEM);
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

/// Linux's `madvise` (`memreq::madvise_refusal`).
#[no_mangle]
pub unsafe extern "C" fn madvise(addr: *mut u8, _len: usize, advice: i32) -> i32 {
    use crate::memreq::AdviceRefusal;
    match crate::memreq::madvise_refusal(addr.addr(), advice) {
        None => 0,
        Some(refusal) => {
            crate::errno::set(match refusal {
                AdviceRefusal::Invalid => EINVAL,
                AdviceRefusal::Discard => crate::errno::ENOSYS,
            });
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn munmap(addr: *mut u8, len: usize) -> i32 {
    // SAFETY: caller is responsible for addr/len matching a previous mmap
    match unsafe { syscall::munmap(addr, len) } {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }
}

// poll

#[repr(C)]
pub struct pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;

#[no_mangle]
pub unsafe extern "C" fn poll(fds: *mut pollfd, nfds: u32, timeout: i32) -> i32 {
    if nfds == 0 {
        if timeout > 0 {
            syscall::nanosleep(timeout as u64 * 1_000_000);
        }
        return 0;
    }

    // A poller watches at most `MAX_HANDLES` fds and now says so by panicking
    // rather than by quietly handing back a smaller ring. A C caller asking to
    // watch more is not a bug in this library, so it gets POSIX's own answer
    // for an nfds it cannot serve.
    if nfds > toyos::poller::Poller::MAX_HANDLES {
        crate::errno::set(EINVAL);
        return -1;
    }

    let timeout_ns = if timeout < 0 { None } else { Some(timeout as u64 * 1_000_000) };

    let n = nfds as usize;
    let poller = toyos::poller::Poller::new(n as u32);
    for i in 0..n {
        let pfd = &*fds.add(i);
        let mut flags = 0u32;
        if pfd.events & POLLIN != 0 { flags |= toyos::poller::READABLE; }
        if pfd.events & POLLOUT != 0 { flags |= toyos::poller::WRITABLE; }
        poller.watch_raw(toyos_abi::RawHandle(pfd.fd as u32), flags, i as u64);
    }

    let mut ready_set = alloc::vec![false; n];
    poller.wait(1, timeout_ns.unwrap_or(u64::MAX), |token| {
        if (token as usize) < n { ready_set[token as usize] = true; }
    });
    let mut ready = 0i32;
    for i in 0..n {
        let pfd = &mut *fds.add(i);
        pfd.revents = 0;
        if ready_set[i] {
            pfd.revents = pfd.events;
            ready += 1;
        }
    }
    ready
}