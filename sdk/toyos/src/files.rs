//! Files by path, and the descriptors that name open ones, wherever they are
//! served: the kernel serves `/tmp`, and every other directory a
//! process may reach is a capability in its namespace, `fs:<directory>`,
//! served by a file server ([`crate::fs`]). A program or a library is read
//! here into an image the kernel pages it from ([`read_image`]).
//!
//! **A path goes to the capability with the longest matching prefix**, at
//! most [`MAX_CAPABILITY_DEPTH`] components deep, relative to it; a path under
//! no capability the process holds is the kernel's. An absolute link a server
//! meets is followed here, in this process's own table, at most [`MAX_LINKS`]
//! times. A file server that restarts is survived: its capability is connected
//! again ([`crate::fs::Dir`]).
//!
//! **One copy of this crate keeps this state**, the copy that owns the
//! descriptor table ([`crate::fd`]), and every copy reaches it through
//! `toyos_fs_*`: std and libc see one set of connections and one table. A
//! file a server holds open is a descriptor naming an object of this module's
//! ([`crate::fd::SERVED_FILE`]); a kernel file is one naming its handle.

use alloc::string::String;
use alloc::vec::Vec;

use toyos_abi::syscall::{SeekFrom, Stat, SyscallError};

use crate::fd::{self, Fd};
use crate::fs;
use crate::process::{Image, ImageRefused};
use crate::OwnedHandle;

/// The deepest directory a capability names, in components.
pub const MAX_CAPABILITY_DEPTH: usize = 4;

/// The most absolute links one path's resolution follows.
pub const MAX_LINKS: usize = 40;

/// What an open asks for.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Open {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub create: bool,
    pub create_new: bool,
    pub truncate: bool,
    /// The descriptor is not inherited (`toyos::fd`'s flag).
    pub cloexec: bool,
}

/// Why a call was not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The kernel's refusal, of a path or a handle it serves.
    Kernel(SyscallError),
    /// A file server's refusal, or its client end's ([`crate::fs::Error`]).
    Served(fs::Error),
    /// The descriptor table's.
    Fd(fd::Error),
    /// A path that is not UTF-8, which neither server takes.
    NotUtf8,
    /// More absolute links on the way than are followed.
    Loop,
    /// A rename between two servers, or between a server and the kernel.
    CrossesServers,
    /// A descriptor that names no file, but an object of a library's own.
    NotAFile,
}

// The C interface's words: a negative answer is an `Error`.
const BAD_FD: i64 = -1;
const FULL: i64 = -2;
const NOT_UTF8: i64 = -3;
const LOOP: i64 = -4;
const CROSSES: i64 = -5;
const NOT_A_FILE: i64 = -6;
const KERNEL: i64 = 0x100;
const SERVED: i64 = 0x200;

#[cfg(any(feature = "fd-table", test))]
fn encode(e: Error) -> i64 {
    match e {
        Error::Fd(fd::Error::BadFd) => BAD_FD,
        Error::Fd(fd::Error::Full) => FULL,
        Error::NotUtf8 => NOT_UTF8,
        Error::Loop => LOOP,
        Error::CrossesServers => CROSSES,
        Error::NotAFile => NOT_A_FILE,
        Error::Kernel(e) => -(KERNEL + (u64::MAX - e.to_u64()) as i64),
        Error::Served(e) => -(SERVED + i64::from(e.byte())),
    }
}

fn decode(word: i64) -> Result<u64, Error> {
    match word {
        0.. => Ok(word as u64),
        BAD_FD => Err(Error::Fd(fd::Error::BadFd)),
        FULL => Err(Error::Fd(fd::Error::Full)),
        NOT_UTF8 => Err(Error::NotUtf8),
        LOOP => Err(Error::Loop),
        CROSSES => Err(Error::CrossesServers),
        NOT_A_FILE => Err(Error::NotAFile),
        _ => {
            let code = -word;
            let kernel = (KERNEL..SERVED).contains(&code).then(|| SyscallError::from_u64(u64::MAX - (code - KERNEL) as u64)).flatten();
            let served = (SERVED..SERVED + 256).contains(&code).then(|| fs::Error::from_byte((code - SERVED) as u8)).flatten();
            match (kernel, served) {
                (Some(e), _) => Err(Error::Kernel(e)),
                (_, Some(e)) => Err(Error::Served(e)),
                _ => panic!("toyos::files: an answer {word} no error encodes"),
            }
        }
    }
}

const SEEK_START: u32 = 0;
const SEEK_CURRENT: u32 = 1;
const SEEK_END: u32 = 2;

extern "C" {
    fn toyos_fs_open(path: *const u8, len: usize, how: Open) -> i64;
    fn toyos_fs_stat(path: *const u8, len: usize, follow: bool, out: *mut Stat) -> i64;
    fn toyos_fs_fstat(fd: Fd, out: *mut Stat) -> i64;
    fn toyos_fs_read(fd: Fd, buf: *mut u8, len: usize) -> i64;
    fn toyos_fs_write(fd: Fd, buf: *const u8, len: usize) -> i64;
    fn toyos_fs_seek(fd: Fd, whence: u32, delta: i64) -> i64;
    fn toyos_fs_truncate(fd: Fd, size: u64) -> i64;
    fn toyos_fs_fsync(fd: Fd) -> i64;
    fn toyos_fs_stream(fd: Fd) -> i64;
    fn toyos_fs_mkdir(path: *const u8, len: usize) -> i64;
    fn toyos_fs_rmdir(path: *const u8, len: usize) -> i64;
    fn toyos_fs_unlink(path: *const u8, len: usize) -> i64;
    fn toyos_fs_rename(from: *const u8, from_len: usize, to: *const u8, to_len: usize) -> i64;
    fn toyos_fs_symlink(target: *const u8, target_len: usize, link: *const u8, link_len: usize) -> i64;
    fn toyos_fs_readlink(path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64;
    fn toyos_fs_readdir(path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64;
    fn toyos_fs_is_served(path: *const u8, len: usize) -> i64;
    fn toyos_fs_chdir(path: *const u8, len: usize) -> i64;
}

/// Open `path`: a descriptor, the lowest free.
pub fn open(path: &[u8], how: Open) -> Result<Fd, Error> {
    // SAFETY: `path` is live for the call, and `how` is a plain value.
    decode(unsafe { toyos_fs_open(path.as_ptr(), path.len(), how) }).map(|fd| fd as Fd)
}

/// What `path` names: the link itself where `follow` is false.
pub fn stat(path: &[u8], follow: bool) -> Result<Stat, Error> {
    let mut out = Stat::default();
    // SAFETY: both live for the call.
    decode(unsafe { toyos_fs_stat(path.as_ptr(), path.len(), follow, &mut out) }).map(|_| out)
}

pub fn fstat(fd: Fd) -> Result<Stat, Error> {
    let mut out = Stat::default();
    // SAFETY: `out` is live for the call.
    decode(unsafe { toyos_fs_fstat(fd, &mut out) }).map(|_| out)
}

pub fn read(fd: Fd, buf: &mut [u8]) -> Result<usize, Error> {
    // SAFETY: `buf` is live and writable for the call.
    decode(unsafe { toyos_fs_read(fd, buf.as_mut_ptr(), buf.len()) }).map(|n| n as usize)
}

pub fn write(fd: Fd, buf: &[u8]) -> Result<usize, Error> {
    // SAFETY: `buf` is live for the call.
    decode(unsafe { toyos_fs_write(fd, buf.as_ptr(), buf.len()) }).map(|n| n as usize)
}

/// Move the descriptor's offset: where it landed.
pub fn seek(fd: Fd, pos: SeekFrom) -> Result<u64, Error> {
    let (whence, delta) = match pos {
        SeekFrom::Start(n) => (SEEK_START, i64::try_from(n).map_err(|_| Error::Kernel(SyscallError::InvalidArgument))?),
        SeekFrom::Current(d) => (SEEK_CURRENT, d),
        SeekFrom::End(d) => (SEEK_END, d),
    };
    // SAFETY: plain values.
    decode(unsafe { toyos_fs_seek(fd, whence, delta) })
}

pub fn truncate(fd: Fd, size: u64) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fs_truncate(fd, size) }).map(drop)
}

pub fn fsync(fd: Fd) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fs_fsync(fd) }).map(drop)
}

/// A kernel handle a child may write the file through as its stdout: a copy
/// of a kernel file's own, or a pipe the file's server writes to it at its
/// offset. The caller holds it until the child has its own.
pub fn stream(fd: Fd) -> Result<OwnedHandle, Error> {
    // SAFETY: plain values; a handle answered is this process's alone.
    decode(unsafe { toyos_fs_stream(fd) }).map(|h| OwnedHandle(toyos_abi::RawHandle(h as u32)))
}

macro_rules! path_call {
    ($($(#[$doc:meta])* $name:ident => $sys:ident;)+) => {$(
        $(#[$doc])*
        pub fn $name(path: &[u8]) -> Result<(), Error> {
            // SAFETY: `path` is live for the call.
            decode(unsafe { $sys(path.as_ptr(), path.len()) }).map(drop)
        }
    )+};
}

path_call! {
    mkdir => toyos_fs_mkdir;
    rmdir => toyos_fs_rmdir;
    unlink => toyos_fs_unlink;
    /// Make `path` the working directory, which a file server judges when it
    /// serves it and the kernel when it does.
    chdir => toyos_fs_chdir;
}

/// `from` renamed `to`, both on one server.
pub fn rename(from: &[u8], to: &[u8]) -> Result<(), Error> {
    // SAFETY: both live for the call.
    decode(unsafe { toyos_fs_rename(from.as_ptr(), from.len(), to.as_ptr(), to.len()) }).map(drop)
}

/// Make `link` a symlink holding `target`, stored as written.
pub fn symlink(target: &[u8], link: &[u8]) -> Result<(), Error> {
    // SAFETY: both live for the call.
    decode(unsafe { toyos_fs_symlink(target.as_ptr(), target.len(), link.as_ptr(), link.len()) }).map(drop)
}

/// What the link at `path` holds.
pub fn read_link(path: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = alloc::vec![0u8; toyos_abi::syscall::MAX_PATH];
    // SAFETY: both live for the call; `out` is writable.
    let n = decode(unsafe { toyos_fs_readlink(path.as_ptr(), path.len(), out.as_mut_ptr(), out.len()) })? as usize;
    out.truncate(n);
    Ok(out)
}

/// The listing of the directory at `path`, in [`crate::fs::for_each_entry`]'s
/// encoding whoever serves it.
pub fn read_dir(path: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = alloc::vec![0u8; 64 * 1024];
    loop {
        // SAFETY: both live for the call; `out` is writable.
        let n = decode(unsafe { toyos_fs_readdir(path.as_ptr(), path.len(), out.as_mut_ptr(), out.len()) })? as usize;
        if n <= out.len() {
            out.truncate(n);
            return Ok(out);
        }
        out.resize(n, 0);
    }
}

/// The whole of the file at `path`, read into a memory object of this
/// process's own, wherever it is served: what a spawn names as its program
/// (`SpawnArgs::image`) and [`dl_open`] loads, since the kernel opens neither.
pub fn read_image(path: &[u8]) -> Result<Image, ImageRefused<Error>> {
    let fd = open(path, Open { read: true, cloexec: true, ..Open::default() }).map_err(ImageRefused::Read)?;
    let image = match fstat(fd) {
        Ok(stat) => Image::read(stat.size, |buf| read(fd, buf)),
        Err(e) => Err(ImageRefused::Read(e)),
    };
    // A refused close leaves nothing this read depends on.
    let _ = fd::close(fd);
    image
}

/// Why [`dl_open`] loaded nothing.
#[derive(Debug)]
pub enum DlOpenRefused {
    /// The library was not read into an image.
    Image(ImageRefused<Error>),
    /// The kernel refused to load the image it was handed.
    Load(SyscallError),
}

/// Load the shared library at `path` into this process, under its absolute
/// path as the name it goes by, and run its initialisers: the module's handle.
/// A second load of one name answers the module the first loaded.
pub fn dl_open(path: &[u8]) -> Result<u64, DlOpenRefused> {
    let text = core::str::from_utf8(path).map_err(|_| DlOpenRefused::Image(ImageRefused::Read(Error::NotUtf8)))?;
    let mut cwd = alloc::vec![0u8; toyos_abi::syscall::MAX_PATH];
    let n = toyos_abi::syscall::getcwd(&mut cwd);
    let name = absolute(&String::from_utf8_lossy(&cwd[..n.min(cwd.len())]), text);
    let image = read_image(name.as_bytes()).map_err(DlOpenRefused::Image)?;
    let (handle, len) = image.spawn_words();
    toyos_abi::syscall::dl_open(name.as_bytes(), toyos_abi::RawHandle(handle as u32), len).map_err(DlOpenRefused::Load)
}

/// Whether `path` is on a file server rather than the kernel.
pub fn is_served(path: &[u8]) -> bool {
    // SAFETY: `path` is live for the call.
    decode(unsafe { toyos_fs_is_served(path.as_ptr(), path.len()) }) == Ok(1)
}

/// `path`, absolute against `cwd` and without `.` or `..`.
pub fn absolute(cwd: &str, path: &str) -> String {
    let joined = match path.starts_with('/') {
        true => String::from(path),
        false => alloc::format!("{cwd}/{path}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    alloc::format!("/{}", parts.join("/"))
}

/// The state, in the copy that owns it.
#[cfg(all(feature = "fd-table", not(test)))]
mod owner;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_made_absolute_and_lexically_clean() {
        assert_eq!(absolute("/home/a", "b/./c"), "/home/a/b/c");
        assert_eq!(absolute("/home/a", "../b"), "/home/b");
        assert_eq!(absolute("/home/a", "/tmp//x/"), "/tmp/x");
        assert_eq!(absolute("/", "../../.."), "/");
    }

    #[test]
    fn every_error_crosses_the_c_interface_and_comes_back() {
        let mut every = alloc::vec![
            Error::Fd(fd::Error::BadFd),
            Error::Fd(fd::Error::Full),
            Error::NotUtf8,
            Error::Loop,
            Error::CrossesServers,
            Error::NotAFile,
        ];
        every.extend((0..=255).filter_map(|code| SyscallError::from_u64(u64::MAX - code)).map(Error::Kernel));
        every.extend((0..=255u8).filter_map(fs::Error::from_byte).map(Error::Served));
        assert!(every.len() > 40, "only {} errors", every.len());
        for e in every {
            let word = encode(e);
            assert!(word < 0, "{e:?} encodes as an answer");
            assert_eq!(decode(word), Err(e), "{e:?}");
        }
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(i64::MAX), Ok(i64::MAX as u64));
    }
}
