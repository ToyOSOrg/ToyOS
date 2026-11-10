//! Files and directories through the capabilities a process holds, named
//! `fs:<directory>` in its namespace. Absolute paths and paths from the
//! working directory use the longest matching prefix, at most
//! [`MAX_CAPABILITY_DEPTH`] components deep. A relative path from a
//! descriptor stays on that directory's server. Absolute symlinks return
//! here for routing through the same capabilities.
//!
//! One SDK copy owns the descriptor table, connections and working directory;
//! every copy, including std and libc, reaches it through `toyos_fs_*`.
//! Kernel objects still support generic reads, writes and metadata queries.

#[cfg(any(feature = "fd-table", test))]
mod namespace;

use alloc::string::String;
use alloc::vec::Vec;

use toyos_abi::syscall::{FileType, SyscallError};

use crate::fd::{self, Fd};
use crate::fs;
use crate::process::{Image, ImageRefused};
use crate::shm::{Immutable, NotImmutable};
use crate::OwnedHandle;

/// The deepest directory a capability names, in components.
pub const MAX_CAPABILITY_DEPTH: usize = 4;

/// The most absolute links one path's resolution follows.
pub const MAX_LINKS: usize = 40;

/// Where a file seek starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

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
    /// Refused unless the path names a directory.
    pub directory: bool,
    /// A file with no name in the directory the path names, which only
    /// [`link`] ever names: Linux's `O_TMPFILE`.
    pub unnamed: bool,
    /// The descriptor is not inherited (`toyos::fd`'s flag).
    pub cloexec: bool,
}

/// Where a path is resolved from: the working directory, or a directory a
/// descriptor names. An absolute path is resolved from neither, and the empty
/// path from a descriptor is what it names itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Cwd,
    Fd(Fd),
}

impl At {
    /// The C interface's word: a descriptor, or [`AT_CWD`].
    const fn word(self) -> Fd {
        match self {
            At::Cwd => AT_CWD,
            At::Fd(fd) => fd,
        }
    }

    #[cfg_attr(not(all(feature = "fd-table", not(test))), allow(dead_code))]
    const fn of(word: Fd) -> Self {
        match word {
            AT_CWD => At::Cwd,
            fd => At::Fd(fd),
        }
    }
}

/// [`At::Cwd`] across the C interface; no descriptor is negative.
const AT_CWD: Fd = -1;

/// What a file is, whoever serves it. A time is nanoseconds since the Unix
/// epoch, 0 where its volume keeps none; `volume` and `node` mean what
/// `toyos_abi::syscall::Stat`'s do.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub file_type: FileType,
    pub size: u64,
    /// The names it has.
    pub nlink: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub volume: u64,
    pub node: u64,
}

impl Stat {
    /// What the kernel answers about an object of its own: one name,
    /// and of the times only when it was last written.
    pub fn of_kernel(stat: toyos_abi::syscall::Stat) -> Self {
        Self {
            file_type: stat.file_type,
            size: stat.size,
            nlink: 1,
            atime: 0,
            mtime: stat.mtime,
            ctime: 0,
            volume: stat.volume,
            node: stat.node,
        }
    }

    fn empty() -> Self {
        Self::of_kernel(toyos_abi::syscall::Stat::default())
    }
}

/// Why a call was not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A kernel object or transport refused the operation.
    Kernel(SyscallError),
    /// A file server's refusal, or its client end's ([`crate::fs::Error`]).
    Served(fs::Error),
    /// The descriptor table's.
    Fd(fd::Error),
    /// A path that is not UTF-8, which neither server takes.
    NotUtf8,
    /// More absolute links on the way than are followed.
    Loop,
    /// A rename or a link between different filesystem authorities.
    CrossesServers,
    /// A descriptor that names no file, but an object of a library's own.
    NotAFile,
    /// A read or a write of a stream's descriptor, refused as the stream
    /// refuses it (`crate::net::stream`).
    Stream(crate::net::stream::Refused),
    /// A path resolved from a descriptor that names no directory.
    NotADirectory,
}

// The C interface's words: a negative answer is an `Error`.
const BAD_FD: i64 = -1;
const FULL: i64 = -2;
const NOT_UTF8: i64 = -3;
const LOOP: i64 = -4;
const CROSSES: i64 = -5;
const NOT_A_FILE: i64 = -6;
const NOT_A_DIRECTORY: i64 = -7;
const KERNEL: i64 = 0x100;
const SERVED: i64 = 0x200;
const STREAM: i64 = 0x300;

#[cfg(any(feature = "fd-table", test))]
fn encode(e: Error) -> i64 {
    match e {
        Error::Fd(fd::Error::BadFd) => BAD_FD,
        Error::Fd(fd::Error::Full) => FULL,
        Error::NotUtf8 => NOT_UTF8,
        Error::Loop => LOOP,
        Error::CrossesServers => CROSSES,
        Error::NotAFile => NOT_A_FILE,
        Error::NotADirectory => NOT_A_DIRECTORY,
        Error::Kernel(e) => -(KERNEL + (u64::MAX - e.to_u64()) as i64),
        Error::Served(e) => -(SERVED + i64::from(e.byte())),
        Error::Stream(refused) => match crate::net::stream::Refused::byte(refused) {
            Some(byte) => -(STREAM + i64::from(byte)),
            // A handle the descriptor's own stream refused as no stream of
            // the process's: a descriptor that names no file.
            None => NOT_A_FILE,
        },
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
        NOT_A_DIRECTORY => Err(Error::NotADirectory),
        _ => {
            let code = -word;
            let kernel = (KERNEL..SERVED).contains(&code).then(|| SyscallError::from_u64(u64::MAX - (code - KERNEL) as u64)).flatten();
            let served = (SERVED..SERVED + 256).contains(&code).then(|| fs::Error::from_byte((code - SERVED) as u8)).flatten();
            let stream = (STREAM..STREAM + 256)
                .contains(&code)
                .then(|| crate::net::stream::Refused::from_byte((code - STREAM) as u8))
                .flatten();
            match (kernel, served, stream) {
                (Some(e), _, _) => Err(Error::Kernel(e)),
                (_, Some(e), _) => Err(Error::Served(e)),
                (_, _, Some(e)) => Err(Error::Stream(e)),
                _ => panic!("toyos::files: an answer {word} no error encodes"),
            }
        }
    }
}

const SEEK_START: u32 = 0;
const SEEK_CURRENT: u32 = 1;
const SEEK_END: u32 = 2;

/// What [`set_times`] sets: the wire's own words (`crate::fs::Change`).
pub use crate::fs::{Change, Time};

extern "C" {
    fn toyos_fs_open(at: Fd, path: *const u8, len: usize, how: Open) -> i64;
    fn toyos_fs_stat(at: Fd, path: *const u8, len: usize, follow: bool, out: *mut Stat) -> i64;
    fn toyos_fs_read(fd: Fd, buf: *mut u8, len: usize) -> i64;
    fn toyos_fs_write(fd: Fd, buf: *const u8, len: usize) -> i64;
    fn toyos_fs_pread(fd: Fd, buf: *mut u8, len: usize, offset: u64) -> i64;
    fn toyos_fs_pwrite(fd: Fd, buf: *const u8, len: usize, offset: u64) -> i64;
    fn toyos_fs_seek(fd: Fd, whence: u32, delta: i64) -> i64;
    fn toyos_fs_truncate(fd: Fd, size: u64) -> i64;
    fn toyos_fs_fsync(fd: Fd) -> i64;
    fn toyos_fs_stream(fd: Fd) -> i64;
    fn toyos_fs_export(word: u64) -> i64;
    fn toyos_fs_image(fd: Fd) -> i64;
    fn toyos_fs_mkdir(at: Fd, path: *const u8, len: usize) -> i64;
    fn toyos_fs_unlink(at: Fd, path: *const u8, len: usize, dir: bool) -> i64;
    fn toyos_fs_rename(from_at: Fd, from: *const u8, from_len: usize, to_at: Fd, to: *const u8, to_len: usize) -> i64;
    fn toyos_fs_link(from_at: Fd, from: *const u8, from_len: usize, follow: bool, to_at: Fd, to: *const u8, to_len: usize) -> i64;
    fn toyos_fs_symlink(target: *const u8, target_len: usize, at: Fd, link: *const u8, link_len: usize) -> i64;
    fn toyos_fs_readlink(at: Fd, path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64;
    fn toyos_fs_set_times(at: Fd, path: *const u8, len: usize, follow: bool, flags: u64, atime: u64, mtime: u64) -> i64;
    fn toyos_fs_readdir(fd: Fd, cursor: u64, out: *mut u8, cap: usize, next: *mut u64) -> i64;
    fn toyos_fs_getcwd(out: *mut u8, len: usize) -> usize;
    fn toyos_fs_chdir(path: *const u8, len: usize) -> i64;
}

/// Open `path` from `at`: a descriptor, the lowest free. A directory opens to
/// read, and names a directory later paths are resolved from.
pub fn open(at: At, path: &[u8], how: Open) -> Result<Fd, Error> {
    // SAFETY: `path` is live for the call, and `how` is a plain value.
    decode(unsafe { toyos_fs_open(at.word(), path.as_ptr(), path.len(), how) }).map(|fd| fd as Fd)
}

/// What `path` from `at` names: the link itself where `follow` is false.
pub fn stat(at: At, path: &[u8], follow: bool) -> Result<Stat, Error> {
    let mut out = Stat::empty();
    // SAFETY: both live for the call.
    decode(unsafe { toyos_fs_stat(at.word(), path.as_ptr(), path.len(), follow, &mut out) }).map(|_| out)
}

/// What the descriptor names.
pub fn fstat(fd: Fd) -> Result<Stat, Error> {
    stat(At::Fd(fd), b"", false)
}

pub fn read(fd: Fd, buf: &mut [u8]) -> Result<usize, Error> {
    // SAFETY: `buf` is live and writable for the call.
    decode(unsafe { toyos_fs_read(fd, buf.as_mut_ptr(), buf.len()) }).map(|n| n as usize)
}

pub fn write(fd: Fd, buf: &[u8]) -> Result<usize, Error> {
    // SAFETY: `buf` is live for the call.
    decode(unsafe { toyos_fs_write(fd, buf.as_ptr(), buf.len()) }).map(|n| n as usize)
}

/// Read at `offset`, leaving the descriptor's offset where it is.
pub fn pread(fd: Fd, buf: &mut [u8], offset: u64) -> Result<usize, Error> {
    // SAFETY: `buf` is live and writable for the call.
    decode(unsafe { toyos_fs_pread(fd, buf.as_mut_ptr(), buf.len(), offset) }).map(|n| n as usize)
}

/// Write at `offset`, leaving the descriptor's offset where it is, whether or
/// not it appends.
pub fn pwrite(fd: Fd, buf: &[u8], offset: u64) -> Result<usize, Error> {
    // SAFETY: `buf` is live for the call.
    decode(unsafe { toyos_fs_pwrite(fd, buf.as_ptr(), buf.len(), offset) }).map(|n| n as usize)
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

/// A child's output handle: a copy of a generic object handle, or a pipe
/// the filesystem server writes at the open description's offset. The caller
/// holds it until the child has its own.
pub fn stream(fd: Fd) -> Result<OwnedHandle, Error> {
    // SAFETY: plain values; a handle answered is this process's alone.
    decode(unsafe { toyos_fs_stream(fd) }).map(|h| OwnedHandle(toyos_abi::RawHandle(h as u32)))
}

/// Export a served description for another process, sharing its access mode
/// and offset. The returned connection becomes an `fd:<n>:file` endowment.
pub fn export(fd: Fd) -> Result<OwnedHandle, Error> {
    fd::with(fd, export_held).map_err(Error::Fd)?
}

/// The object remains held across the owner ABI call, including if its
/// descriptor is concurrently closed and its number reused.
pub(crate) fn export_held(object: fd::Lent<'_>) -> Result<OwnedHandle, Error> {
    if object.tag() != fd::SERVED_FILE { return Err(Error::NotAFile); }
    // SAFETY: the caller's Lent keeps this object alive for the call.
    decode(unsafe { toyos_fs_export(object.word()) }).map(|h| OwnedHandle(toyos_abi::RawHandle(h as u32)))
}

#[cfg(all(feature = "fd-table", not(test)))]
pub(crate) fn adopt_exported(handle: OwnedHandle) -> Result<fd::Object, Error> {
    owner::adopt_exported(handle)
}

pub fn mkdir(at: At, path: &[u8]) -> Result<(), Error> {
    // SAFETY: `path` is live for the call.
    decode(unsafe { toyos_fs_mkdir(at.word(), path.as_ptr(), path.len()) }).map(drop)
}

/// Remove the name `path`: a directory's, which must be empty, only where
/// `dir`, and then nothing else.
pub fn unlink(at: At, path: &[u8], dir: bool) -> Result<(), Error> {
    // SAFETY: `path` is live for the call.
    decode(unsafe { toyos_fs_unlink(at.word(), path.as_ptr(), path.len(), dir) }).map(drop)
}

/// Validate `path` as a directory and make it the process's working directory.
pub fn chdir(path: &[u8]) -> Result<(), Error> {
    // SAFETY: `path` is live for the call.
    decode(unsafe { toyos_fs_chdir(path.as_ptr(), path.len()) }).map(drop)
}

/// `from` renamed `to`, both on one server.
pub fn rename(from: (At, &[u8]), to: (At, &[u8])) -> Result<(), Error> {
    // SAFETY: both live for the call.
    let word = unsafe { toyos_fs_rename(from.0.word(), from.1.as_ptr(), from.1.len(), to.0.word(), to.1.as_ptr(), to.1.len()) };
    decode(word).map(drop)
}

/// Give what `from` names, a link in its last component followed where
/// `follow`, the name `to` too, on one server.
pub fn link(from: (At, &[u8]), follow: bool, to: (At, &[u8])) -> Result<(), Error> {
    // SAFETY: both live for the call.
    let word = unsafe {
        toyos_fs_link(from.0.word(), from.1.as_ptr(), from.1.len(), follow, to.0.word(), to.1.as_ptr(), to.1.len())
    };
    decode(word).map(drop)
}

/// Make `link` a symlink holding `target`, stored as written.
pub fn symlink(target: &[u8], link: (At, &[u8])) -> Result<(), Error> {
    // SAFETY: both live for the call.
    decode(unsafe { toyos_fs_symlink(target.as_ptr(), target.len(), link.0.word(), link.1.as_ptr(), link.1.len()) }).map(drop)
}

/// What the link at `path` holds.
pub fn read_link(at: At, path: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = alloc::vec![0u8; toyos_abi::syscall::MAX_PATH];
    // SAFETY: both live for the call; `out` is writable.
    let n = decode(unsafe { toyos_fs_readlink(at.word(), path.as_ptr(), path.len(), out.as_mut_ptr(), out.len()) })? as usize;
    out.truncate(n);
    Ok(out)
}

/// Make `change`'s times to what `path` from `at` names, following a link in
/// its last component where `follow`; the empty path from a descriptor is
/// what it names. Its size is [`truncate`]'s.
pub fn set_times(at: At, path: &[u8], follow: bool, change: &Change) -> Result<(), Error> {
    let mut request = fs::Request::new();
    change.encode(&mut request);
    let (flags, atime, mtime) = (request.flags & !fs::SET_SIZE, request.atime, request.mtime);
    // SAFETY: `path` is live for the call.
    decode(unsafe { toyos_fs_set_times(at.word(), path.as_ptr(), path.len(), follow, flags, atime, mtime) }).map(drop)
}

/// The listing a descriptor of a directory opened, from `cursor` (0 at its
/// start), in [`crate::fs::for_each_entry`]'s encoding whoever serves it:
/// the batch and the cursor to ask from next, `None` after the last.
pub fn read_dir(fd: Fd, cursor: u64) -> Result<(Vec<u8>, Option<u64>), Error> {
    let mut out = alloc::vec![0u8; 64 * 1024];
    loop {
        let mut next = fs::READDIR_END;
        // SAFETY: all live for the call; `out` and `next` are writable.
        let n = decode(unsafe { toyos_fs_readdir(fd, cursor, out.as_mut_ptr(), out.len(), &mut next) })? as usize;
        if n <= out.len() {
            out.truncate(n);
            return Ok((out, (next != fs::READDIR_END).then_some(next)));
        }
        out.resize(n, 0);
    }
}

/// The whole of the file at `path`, wherever it is served: what a spawn names
/// as its program (`SpawnMap::object`) and a library's loader maps, since the
/// kernel opens neither. The object its server holds it in where there is one
/// ([`crate::fs::IMAGE`]), and otherwise a copy read into memory of this
/// process's own.
pub fn read_image(path: &[u8]) -> Result<Image, ImageRefused<Error>> {
    read_image_for(path, None)
}

/// [`read_image`], a copy counted to the job `job` is a handle to: a
/// child's program, which it keeps (`Image::read_for`).
pub fn read_image_for(path: &[u8], job: Option<crate::BorrowedHandle<'_>>) -> Result<Image, ImageRefused<Error>> {
    let fd = open(At::Cwd, path, Open { read: true, cloexec: true, ..Open::default() }).map_err(ImageRefused::Read)?;
    // SAFETY: a plain value; a handle answered is this process's alone.
    let held = decode(unsafe { toyos_fs_image(fd) }).map(|h| OwnedHandle(toyos_abi::RawHandle(h as u32)));
    let image = match held {
        Ok(handle) => Immutable::adopt(handle).map(Image::immutable).map_err(|why| match why {
            NotImmutable::Kernel(e) => ImageRefused::Memory(e),
            NotImmutable::Writable => ImageRefused::Read(Error::Served(fs::Error::Malformed)),
        }),
        Err(Error::Served(fs::Error::Unsupported)) => match fstat(fd) {
            Ok(stat) => Image::read_for(job, stat.size, |buf| read(fd, buf)),
            Err(e) => Err(ImageRefused::Read(e)),
        },
        Err(e) => Err(ImageRefused::Read(e)),
    };
    // A refused close leaves nothing this read depends on.
    // SAFETY: the descriptor this function opened, named nowhere else.
    let _ = unsafe { fd::close(fd) };
    image
}

/// `path` as a path names something: UTF-8, and not empty, which names
/// nothing, as POSIX resolves no empty path. A call that takes the empty path
/// as the descriptor itself (`stat` and `set_times` of `At::Fd`, a `link`
/// from one) says so before it asks for a path.
#[cfg_attr(not(all(feature = "fd-table", not(test))), allow(dead_code))]
fn named(path: &[u8]) -> Result<&str, Error> {
    if path.len() > fs::MAX_PATH {
        return Err(Error::Served(fs::Error::NameTooLong));
    }
    match core::str::from_utf8(path) {
        Ok("") => Err(Error::Served(fs::Error::NotFound)),
        Ok(path) => Ok(path),
        Err(_) => Err(Error::NotUtf8),
    }
}

/// The process's working directory. Returns the required length, writing
/// the complete path only when it fits in `out`.
pub fn getcwd(out: &mut [u8]) -> usize {
    // SAFETY: `out` lends its writable bytes for this call.
    unsafe { toyos_fs_getcwd(out.as_mut_ptr(), out.len()) }
}

/// The environment variable a spawn states its child's starting directory in,
/// POSIX's: [`starting_in`] writes it, and the child's working directory
/// starts there ([`starting_directory`]).
pub const START: &str = "PWD";

/// `env`, `KEY=VALUE\0` repeated, stating `dir` as the child's starting
/// directory in place of any [`START`] it held. `None` for a `dir` that is not
/// absolute, is past [`fs::MAX_PATH`] or holds a NUL, which no child reads back.
pub fn starting_in(env: &[u8], dir: &str) -> Option<Vec<u8>> {
    if !dir.starts_with('/') || dir.len() > fs::MAX_PATH || dir.contains('\0') {
        return None;
    }
    let mut out = Vec::with_capacity(env.len() + START.len() + dir.len() + 2);
    for entry in env.split(|&b| b == 0).filter(|e| !e.is_empty() && stated(e).is_none()) {
        out.extend_from_slice(entry);
        out.push(0);
    }
    out.extend_from_slice(START.as_bytes());
    out.push(b'=');
    out.extend_from_slice(dir.as_bytes());
    out.push(0);
    Some(out)
}

/// The directory a process starts in, read from `env`, the entries of the
/// environment its spawn gave it: its [`START`], lexically clean, or `/`, its
/// view's root, where it states none. `None` for one [`starting_in`] would
/// not have written.
pub fn starting_directory<'a>(mut env: impl Iterator<Item = &'a [u8]>) -> Option<String> {
    let Some(dir) = env.find_map(stated) else { return Some(String::from("/")) };
    let dir = core::str::from_utf8(dir).ok().filter(|d| d.starts_with('/') && d.len() <= fs::MAX_PATH)?;
    Some(absolute("/", dir))
}

/// The value of an environment entry naming [`START`].
fn stated(entry: &[u8]) -> Option<&[u8]> {
    entry.strip_prefix(START.as_bytes())?.strip_prefix(b"=")
}

/// `path`, absolute against `cwd` and without `.` or `..`, which are read off
/// the text (`issues/a-working-directory-path-reads-its-dot-dot-off-the-text.md`).
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

    /// Where a child given `env` starts.
    fn starts(env: &[u8]) -> Option<String> {
        starting_directory(env.split(|&b| b == 0))
    }

    #[test]
    fn a_path_is_made_absolute_and_lexically_clean() {
        assert_eq!(absolute("/home/a", "b/./c"), "/home/a/b/c");
        assert_eq!(absolute("/home/a", "../b"), "/home/b");
        assert_eq!(absolute("/home/a", "/tmp//x/"), "/tmp/x");
        assert_eq!(absolute("/", "../../.."), "/");
    }

    /// What a parent states is what its child starts in, whatever the
    /// environment it was given already said.
    #[test]
    fn a_child_starts_where_its_spawn_says_and_nowhere_its_environment_said() {
        let given = b"HOME=/home/toy\0PWD=/elsewhere\0PWDX=/x\0TERM=toyos\0";
        let env = starting_in(given, "/home/toy/Games").expect("an absolute directory");
        assert_eq!(starts(&env).as_deref(), Some("/home/toy/Games"));
        assert_eq!(env, b"HOME=/home/toy\0PWDX=/x\0TERM=toyos\0PWD=/home/toy/Games\0");
        assert_eq!(starts(&starting_in(b"", "/").unwrap()).as_deref(), Some("/"));
        assert_eq!(starts(b"PWD=/a/./b/../c\0").as_deref(), Some("/a/c"));
    }

    #[test]
    fn a_spawn_stating_no_directory_starts_its_child_at_the_root() {
        assert_eq!(starts(b"").as_deref(), Some("/"));
        assert_eq!(starts(b"PWDX=/x\0HOME=/home\0").as_deref(), Some("/"));
    }

    #[test]
    fn a_directory_no_child_can_read_back_is_never_stated() {
        let longest = alloc::format!("/{}", "a".repeat(fs::MAX_PATH - 1));
        assert!(starting_in(b"", &longest).is_some());
        for dir in ["", "home/toy", "./x", "/a\0b", &alloc::format!("/{}", "a".repeat(fs::MAX_PATH))] {
            assert_eq!(starting_in(b"", dir), None, "{dir:?}");
        }
        for env in [&b"PWD=home\0"[..], b"PWD=\0", b"PWD=/\xff\0"] {
            assert_eq!(starts(env), None, "{env:?}");
        }
    }

    /// The path boundary refuses empty, non-UTF-8 and oversized names.
    #[test]
    fn the_empty_path_names_nothing() {
        assert_eq!(named(b""), Err(Error::Served(fs::Error::NotFound)));
        assert_eq!(named(b"."), Ok("."));
        assert_eq!(named(b"/"), Ok("/"));
        assert_eq!(named(b"\xff"), Err(Error::NotUtf8));
        assert!(named(&alloc::vec![b'a'; fs::MAX_PATH]).is_ok());
        assert_eq!(named(&alloc::vec![b'a'; fs::MAX_PATH + 1]), Err(Error::Served(fs::Error::NameTooLong)));
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
            Error::NotADirectory,
        ];
        every.extend((0..=255).filter_map(|code| SyscallError::from_u64(u64::MAX - code)).map(Error::Kernel));
        every.extend((0..=255u8).filter_map(fs::Error::from_byte).map(Error::Served));
        let streams: alloc::vec::Vec<Error> =
            (0..=255u8).filter_map(crate::net::stream::Refused::from_byte).map(Error::Stream).collect();
        assert_eq!(streams.len(), 8, "every stream refusal but a foreign handle's");
        every.extend(streams);
        assert!(every.len() > 40, "only {} errors", every.len());
        for e in every {
            let word = encode(e);
            assert!(word < 0, "{e:?} encodes as an answer");
            assert_eq!(decode(word), Err(e), "{e:?}");
        }
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(i64::MAX), Ok(i64::MAX as u64));
        let foreign = Error::Stream(crate::net::stream::Refused::NotThisProcess(toyos_abi::RawHandle(7)));
        assert_eq!(decode(encode(foreign)), Err(Error::NotAFile), "a handle no stream of the process's names no file");
    }
}
