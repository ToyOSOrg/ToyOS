//! Files on ToyOS.
//!
//! A path names a file on one of two kinds of server. The kernel serves
//! `/system` (the signed image) and `/tmp`; every other directory a process
//! may reach is a capability in its namespace, `fs:<directory>`, served by a
//! file server process (`toyos::fs`). A path is made absolute against the
//! working directory, normalized, and sent to the capability with the longest
//! matching prefix, relative to it; a path under no capability the process
//! holds is the kernel's.
//!
//! A file server that restarts is survived: its capability is connected
//! again.

use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, OpenFlags, SyscallError};

use crate::collections::BTreeMap;
use crate::ffi::OsString;
use crate::fmt;
use crate::fs::TryLockError;
use crate::hash::Hash;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut, SeekFrom};
use crate::path::{Path, PathBuf};
use crate::sync::{Arc, Condvar, Mutex, MutexGuard};
pub use crate::sys::fs::common::Dir;
use crate::sys::time::SystemTime;
use crate::sys::to_io_error;

/// The deepest directory a capability names, in components.
const MAX_CAPABILITY_DEPTH: usize = 4;

/// The most absolute symlinks one path resolution follows.
const MAX_LINKS: usize = 40;

pub struct File(Inner);

enum Inner {
    Kernel(RawHandle),
    Served(Arc<Served>),
}

/// A file open on a file server.
struct Served {
    dir: Arc<Capability>,
    rel: String,
    append: bool,
    state: Mutex<Position>,
}

struct Position {
    fid: u64,
    generation: u64,
    offset: u64,
}

/// One directory capability this process holds, connected.
struct Capability {
    prefix: String,
    dir: Mutex<toyos::fs::Dir>,
    /// Hands `dir` over in ticket order: std's mutex lets the thread that let
    /// go barge ahead of the one it woke.
    turns: Mutex<Turns>,
    served: Condvar,
}

struct Turns {
    next: u64,
    serving: u64,
}

/// A directory's connection, held in turn.
struct Held<'a> {
    // Dropped before `_turn`, so the next ticket finds `dir` free.
    dir: MutexGuard<'a, toyos::fs::Dir>,
    _turn: Turn<'a>,
}

struct Turn<'a>(&'a Capability);

impl Capability {
    fn new(prefix: &str, dir: toyos::fs::Dir) -> Self {
        Self {
            prefix: String::from(prefix),
            dir: Mutex::new(dir),
            turns: Mutex::new(Turns { next: 0, serving: 0 }),
            served: Condvar::new(),
        }
    }

    fn lock(&self) -> Held<'_> {
        let mut turns = self.turns.lock().unwrap();
        let ticket = turns.next;
        turns.next += 1;
        drop(self.served.wait_while(turns, |t| t.serving != ticket).unwrap());
        let turn = Turn(self);
        Held { dir: self.dir.lock().unwrap(), _turn: turn }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.turns.lock().unwrap().serving += 1;
        self.0.served.notify_all();
    }
}

impl crate::ops::Deref for Held<'_> {
    type Target = toyos::fs::Dir;
    fn deref(&self) -> &toyos::fs::Dir {
        &self.dir
    }
}

impl crate::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut toyos::fs::Dir {
        &mut self.dir
    }
}

#[derive(Clone)]
pub struct FileAttr {
    size: u64,
    file_type: FileType,
    mtime: u64,
}

pub struct ReadDir {
    entries: Vec<DirEntry>,
    index: usize,
}

pub struct DirEntry {
    dir_path: PathBuf,
    name: OsString,
    size: u64,
    file_type: FileType,
}

#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct FileTimes {}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilePermissions {
    readonly: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct FileType {
    is_file: bool,
    is_dir: bool,
    is_symlink: bool,
}

#[derive(Debug)]
pub struct DirBuilder {}

impl FileType {
    fn of_kind(kind: u64) -> FileType {
        FileType {
            is_file: kind == toyos::fs::KIND_FILE,
            is_dir: kind == toyos::fs::KIND_DIR,
            is_symlink: kind == toyos::fs::KIND_SYMLINK,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    pub fn is_file(&self) -> bool {
        self.is_file
    }

    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}

impl FileAttr {
    fn of(stat: toyos::fs::Stat) -> FileAttr {
        FileAttr { size: stat.size, file_type: FileType::of_kind(stat.kind), mtime: stat.mtime }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn perm(&self) -> FilePermissions {
        FilePermissions { readonly: false }
    }

    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        match self.mtime {
            0 => Err(io::const_error!(io::ErrorKind::Unsupported, "the file is undated")),
            nanos => Ok(SystemTime::from_nanos(nanos)),
        }
    }

    pub fn accessed(&self) -> io::Result<SystemTime> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "ToyOS does not track access time"))
    }

    pub fn created(&self) -> io::Result<SystemTime> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "ToyOS does not track creation time"))
    }
}

impl FilePermissions {
    pub fn readonly(&self) -> bool {
        self.readonly
    }

    pub fn set_readonly(&mut self, readonly: bool) {
        self.readonly = readonly;
    }
}

impl FileTimes {
    pub fn set_accessed(&mut self, _t: SystemTime) {}
    pub fn set_modified(&mut self, _t: SystemTime) {}
}

impl fmt::Debug for ReadDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadDir").finish_non_exhaustive()
    }
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<io::Result<DirEntry>> {
        let e = self.entries.get(self.index)?;
        self.index += 1;
        Some(Ok(DirEntry {
            dir_path: e.dir_path.clone(),
            name: e.name.clone(),
            size: e.size,
            file_type: e.file_type,
        }))
    }
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.dir_path.join(&self.name)
    }

    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }

    pub fn metadata(&self) -> io::Result<FileAttr> {
        Ok(FileAttr { size: self.size, file_type: self.file_type, mtime: 0 })
    }

    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(self.file_type)
    }
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
        }
    }

    pub fn read(&mut self, read: bool) {
        self.read = read;
    }
    pub fn write(&mut self, write: bool) {
        self.write = write;
    }
    pub fn append(&mut self, append: bool) {
        self.append = append;
    }
    pub fn truncate(&mut self, truncate: bool) {
        self.truncate = truncate;
    }
    pub fn create(&mut self, create: bool) {
        self.create = create;
    }
    pub fn create_new(&mut self, create_new: bool) {
        self.create_new = create_new;
    }

    fn to_flags(&self) -> OpenFlags {
        let mut flags = OpenFlags(0);
        if self.read {
            flags |= OpenFlags::READ;
        }
        if self.write || self.append {
            flags |= OpenFlags::WRITE;
        }
        if self.append {
            flags |= OpenFlags::APPEND;
        }
        if self.create || self.create_new {
            flags |= OpenFlags::CREATE;
        }
        if self.truncate {
            flags |= OpenFlags::TRUNCATE;
        }
        flags
    }

    fn served_flags(&self) -> u64 {
        use toyos::fs::*;
        let mut flags = 0;
        if self.read {
            flags |= O_READ;
        }
        if self.write {
            flags |= O_WRITE;
        }
        if self.append {
            flags |= O_APPEND;
        }
        if self.create {
            flags |= O_CREATE;
        }
        if self.create_new {
            flags |= O_CREATE_NEW;
        }
        if self.truncate {
            flags |= O_TRUNCATE;
        }
        flags
    }
}

/// Where a path is served.
enum Route {
    Kernel(String),
    Served(Arc<Capability>, String),
}

/// Every capability this process has looked for, found or not.
static CAPABILITIES: Mutex<BTreeMap<String, Option<Arc<Capability>>>> = Mutex::new(BTreeMap::new());

fn served_error(e: SyscallError) -> io::Error {
    match e {
        SyscallError::Gone => io::const_error!(
            io::ErrorKind::StaleNetworkFileHandle,
            "the file server is gone, or restarted since this file was opened",
        ),
        e => to_io_error(e),
    }
}

/// `path`, absolute against the working directory and without `.` or `..`.
fn absolute(path: &Path) -> io::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| io::const_error!(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    let joined = if text.starts_with('/') {
        String::from(text)
    } else {
        let cwd = crate::env::current_dir()?;
        let cwd = cwd.to_str().expect("getcwd answers UTF-8");
        format!("{cwd}/{text}")
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
    Ok(format!("/{}", parts.join("/")))
}

/// The capability named `fs:<prefix>`, remembering the answer either way.
///
/// The connect waits for the server's hello, so it is made outside the lock: a
/// server that has not answered holds up only the threads that asked it, and
/// of two threads that connect at once the first answer kept is the one used.
fn capability(prefix: &str) -> io::Result<Option<Arc<Capability>>> {
    if let Some(found) = CAPABILITIES.lock().unwrap().get(prefix) {
        return Ok(found.clone());
    }
    let Some(names) = toyos::endow::namespace() else {
        return Ok(None);
    };
    let name = format!("{}{prefix}", toyos::fs::CAPABILITY_PREFIX);
    let found = match toyos::fs::Dir::connect(names, &name) {
        Ok(dir) => Some(Arc::new(Capability::new(prefix, dir))),
        Err(SyscallError::NotFound) | Err(SyscallError::InvalidArgument) => None,
        Err(e) => return Err(served_error(e)),
    };
    Ok(CAPABILITIES.lock().unwrap().entry(String::from(prefix)).or_insert(found).clone())
}

/// Which server `abs` is on, and the path there.
fn route(abs: &str) -> io::Result<Route> {
    let parts: Vec<&str> = abs.split('/').filter(|p| !p.is_empty()).collect();
    for depth in (1..=parts.len().min(MAX_CAPABILITY_DEPTH)).rev() {
        let prefix = format!("/{}", parts[..depth].join("/"));
        if let Some(cap) = capability(&prefix)? {
            return Ok(Route::Served(cap, parts[depth..].join("/")));
        }
    }
    Ok(Route::Kernel(String::from(abs)))
}

/// Ask the server behind `path`, following an absolute link it meets to
/// wherever that lands in this process's own table.
fn on_path<T>(
    path: &Path,
    mut kernel: impl FnMut(&str) -> io::Result<T>,
    mut served: impl FnMut(&Arc<Capability>, &mut toyos::fs::Dir, &str) -> Result<T, toyos::fs::Refused>,
) -> io::Result<T> {
    let mut abs = absolute(path)?;
    for _ in 0..MAX_LINKS {
        let (cap, rel) = match route(&abs)? {
            Route::Kernel(abs) => return kernel(&abs),
            Route::Served(cap, rel) => (cap, rel),
        };
        let mut dir = cap.lock();
        match served(&cap, &mut dir, &rel) {
            Ok(answer) => return Ok(answer),
            Err(toyos::fs::Refused::Error(e)) => return Err(served_error(e)),
            Err(toyos::fs::Refused::Link(len)) => {
                let mut buf = [0u8; toyos::fs::MAX_PATH];
                let target = dir.link_target(len, &mut buf);
                let target = crate::str::from_utf8(target).map_err(|_| {
                    io::const_error!(io::ErrorKind::InvalidData, "a link target is not UTF-8")
                })?;
                abs = absolute(Path::new(target))?;
            }
        }
    }
    Err(io::const_error!(io::ErrorKind::FilesystemLoop, "too many symbolic links"))
}

/// Whether `path` is on a file server rather than the kernel.
pub fn is_served(path: &Path) -> bool {
    absolute(path).and_then(|abs| route(&abs)).is_ok_and(|r| matches!(r, Route::Served(..)))
}

impl Served {
    fn with<T>(
        &self,
        op: impl FnOnce(&mut toyos::fs::Dir, &mut Position) -> Result<T, SyscallError>,
    ) -> io::Result<T> {
        let mut dir = self.dir.lock();
        let mut pos = self.state.lock().unwrap();
        op(&mut dir, &mut pos).map_err(served_error)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let mut dir = self.dir.lock();
        let pos = self.state.lock().unwrap();
        dir.close(pos.fid, pos.generation);
    }
}

impl File {
    pub fn from_fd(fd: RawHandle) -> Self {
        File(Inner::Kernel(fd))
    }

    /// The kernel handle behind this file. A file a file server holds has
    /// none: `as_child_stdio` is how one reaches a child.
    pub fn raw_fd(&self) -> i32 {
        match &self.0 {
            Inner::Kernel(h) => h.0 as i32,
            Inner::Served(_) => panic!("a file on a file server has no kernel handle"),
        }
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.raw_fd()
    }

    /// A kernel handle a child may write this file through: the file's own, or
    /// a pipe its server appends to the file from this file's offset. The
    /// pipe lives as long as the answer and the child's copy of it.
    pub fn as_child_stdio(&self) -> io::Result<(RawHandle, Option<toyos::Pipe>)> {
        match &self.0 {
            Inner::Kernel(h) => Ok((*h, None)),
            Inner::Served(s) => {
                let pipe = s.with(|dir, pos| {
                    let at = if s.append { dir.fstat(pos.fid, pos.generation)?.size } else { pos.offset };
                    dir.stream(pos.fid, pos.generation, at)
                })?;
                Ok((toyos::AsHandle::as_handle(&pipe), Some(pipe)))
            }
        }
    }

    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        on_path(
            path,
            |abs| {
                let fd = syscall::open(abs.as_bytes(), opts.to_flags()).map_err(to_io_error)?;
                Ok(File(Inner::Kernel(fd)))
            },
            |cap, dir, rel| {
                let flags = opts.served_flags();
                let opened = dir.open(rel, flags)?;
                Ok(File(Inner::Served(Arc::new(Served {
                    dir: Arc::clone(cap),
                    rel: String::from(rel),
                    append: opts.append,
                    state: Mutex::new(Position {
                        fid: opened.fid,
                        generation: opened.generation,
                        offset: 0,
                    }),
                }))))
            },
        )
    }

    pub fn file_attr(&self) -> io::Result<FileAttr> {
        match &self.0 {
            Inner::Kernel(h) => {
                let stat = syscall::fstat(*h).map_err(to_io_error)?;
                Ok(FileAttr {
                    size: stat.size,
                    file_type: opened_file_type(stat.file_type),
                    mtime: stat.mtime,
                })
            }
            Inner::Served(s) => s.with(|dir, pos| dir.fstat(pos.fid, pos.generation)).map(FileAttr::of),
        }
    }

    pub fn fsync(&self) -> io::Result<()> {
        match &self.0 {
            Inner::Kernel(h) => syscall::fsync(*h).map_err(to_io_error),
            Inner::Served(s) => s.with(|dir, pos| dir.fsync(pos.fid, pos.generation)),
        }
    }

    pub fn datasync(&self) -> io::Result<()> {
        self.fsync()
    }

    pub fn lock(&self) -> io::Result<()> {
        Ok(())
    }
    pub fn lock_shared(&self) -> io::Result<()> {
        Ok(())
    }
    pub fn try_lock(&self) -> Result<(), TryLockError> {
        Ok(())
    }
    pub fn try_lock_shared(&self) -> Result<(), TryLockError> {
        Ok(())
    }
    pub fn unlock(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn truncate(&self, size: u64) -> io::Result<()> {
        match &self.0 {
            Inner::Kernel(h) => syscall::ftruncate(*h, size).map_err(to_io_error),
            Inner::Served(s) => s.with(|dir, pos| dir.truncate(pos.fid, pos.generation, size)),
        }
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.0 {
            Inner::Kernel(h) => syscall::read(*h, buf).map_err(to_io_error),
            Inner::Served(s) => s.with(|dir, pos| {
                let n = dir.read(pos.fid, pos.generation, pos.offset, buf)?;
                pos.offset += n as u64;
                Ok(n)
            }),
        }
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for buf in bufs {
            match self.read(buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) => {
                    if total == 0 {
                        return Err(e);
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(total)
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let n = self.read(cursor.ensure_init())?;
        unsafe { cursor.advance(n) };
        Ok(())
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        match &self.0 {
            Inner::Kernel(h) => syscall::write(*h, buf).map_err(to_io_error),
            Inner::Served(s) => s.with(|dir, pos| {
                let written = dir.write(pos.fid, pos.generation, pos.offset, buf)?;
                pos.offset = written.offset;
                Ok(written.len)
            }),
        }
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for buf in bufs {
            match self.write(buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) => {
                    if total == 0 {
                        return Err(e);
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(total)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn flush(&self) -> io::Result<()> {
        self.fsync()
    }

    pub fn seek(&self, pos: SeekFrom) -> io::Result<u64> {
        match &self.0 {
            Inner::Kernel(h) => {
                let abi_pos = match pos {
                    SeekFrom::Start(n) => syscall::SeekFrom::Start(n),
                    SeekFrom::Current(n) => syscall::SeekFrom::Current(n),
                    SeekFrom::End(n) => syscall::SeekFrom::End(n),
                };
                syscall::seek(*h, abi_pos).map_err(to_io_error)
            }
            Inner::Served(s) => s.with(|dir, p| {
                let (base, delta) = match pos {
                    SeekFrom::Start(n) => (n, 0),
                    SeekFrom::Current(d) => (p.offset, d),
                    SeekFrom::End(d) => (dir.fstat(p.fid, p.generation)?.size, d),
                };
                let at = base
                    .checked_add_signed(delta)
                    .filter(|&at| at <= toyos::fs::MAX_FILE_BYTES)
                    .ok_or(SyscallError::InvalidArgument)?;
                p.offset = at;
                Ok(at)
            }),
        }
    }

    pub fn size(&self) -> Option<io::Result<u64>> {
        Some(self.file_attr().map(|a| a.size))
    }

    pub fn tell(&self) -> io::Result<u64> {
        self.seek(SeekFrom::Current(0))
    }

    pub fn duplicate(&self) -> io::Result<File> {
        match &self.0 {
            Inner::Kernel(h) => Ok(File(Inner::Kernel(syscall::dup(*h).map_err(to_io_error)?))),
            Inner::Served(s) => Ok(File(Inner::Served(Arc::clone(s)))),
        }
    }

    pub fn set_permissions(&self, _perm: FilePermissions) -> io::Result<()> {
        Ok(())
    }

    pub fn set_times(&self, _times: FileTimes) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Inner::Kernel(h) = self.0 {
            syscall::close(h);
        }
    }
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {}
    }

    pub fn mkdir(&self, p: &Path) -> io::Result<()> {
        on_path(
            p,
            |abs| syscall::mkdir(abs.as_bytes()).map_err(to_io_error),
            |_, dir, rel| dir.mkdir(rel),
        )
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Inner::Kernel(h) => write!(f, "File({})", h.0),
            Inner::Served(s) => write!(f, "File({}/{})", s.dir.prefix, s.rel),
        }
    }
}

/// Whether the kernel's `path` names a directory, asked through `readdir`: a
/// directory too large to list is still one, which is why
/// `ResourceExhausted` is a yes.
fn kernel_is_dir(path: &[u8]) -> bool {
    let mut buf = [0u8; 1];
    match syscall::readdir(path, &mut buf) {
        Ok(_) | Err(SyscallError::ResourceExhausted) => true,
        Err(_) => false,
    }
}

/// The type of something the kernel's `open` accepted, which is never a
/// directory.
fn opened_file_type(ty: syscall::FileType) -> FileType {
    FileType { is_file: ty == syscall::FileType::File, is_dir: false, is_symlink: false }
}

/// A listing too large for a first buffer is asked again at the size it
/// named; a directory that outgrows its own listing this many times is
/// refused rather than waited on.
const LIST_ATTEMPTS: usize = 4;

pub fn readdir(p: &Path) -> io::Result<ReadDir> {
    let dir_path = p.to_path_buf();
    let entries = on_path(
        p,
        |abs| {
            let mut buf = vec![0u8; 65536];
            let mut n = 0;
            let mut fits = false;
            for _ in 0..LIST_ATTEMPTS {
                n = syscall::readdir(abs.as_bytes(), &mut buf).map_err(to_io_error)?;
                if n <= buf.len() {
                    fits = true;
                    break;
                }
                buf.clear();
                buf.resize(n, 0);
            }
            if !fits {
                return Err(io::const_error!(
                    io::ErrorKind::Interrupted,
                    "directory kept growing while it was being listed",
                ));
            }
            Ok(kernel_entries(&dir_path, &buf[..n]))
        },
        |_, dir, rel| {
            let mut buf = vec![0u8; 65536];
            for _ in 0..LIST_ATTEMPTS {
                let n = dir.read_dir(rel, &mut buf)?;
                if n <= buf.len() {
                    let mut entries = Vec::new();
                    toyos::fs::for_each_entry(&buf[..n], |kind, size, name| {
                        entries.push(DirEntry {
                            dir_path: dir_path.clone(),
                            name: OsString::from(name),
                            size,
                            file_type: FileType::of_kind(kind),
                        })
                    })?;
                    return Ok(entries);
                }
                buf.resize(n, 0);
            }
            Err(toyos::fs::Refused::Error(SyscallError::ResourceExhausted))
        },
    )?;
    Ok(ReadDir { entries, index: 0 })
}

/// The entries of a whole `readdir` listing, read by `syscall::dirent`.
fn kernel_entries(dir_path: &Path, data: &[u8]) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let mut at = 0;
    while let Some(entry) = syscall::dirent(data, &mut at) {
        entries.push(DirEntry {
            dir_path: dir_path.to_path_buf(),
            name: OsString::from(crate::str::from_utf8(entry.name).unwrap_or("")),
            size: entry.size,
            file_type: FileType { is_file: !entry.is_dir, is_dir: entry.is_dir, is_symlink: false },
        });
    }
    entries
}

pub fn unlink(p: &Path) -> io::Result<()> {
    on_path(
        p,
        |abs| syscall::delete(abs.as_bytes()).map_err(to_io_error),
        |_, dir, rel| dir.unlink(rel),
    )
}

pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    let (from, to) = (route(&absolute(old)?)?, route(&absolute(new)?)?);
    match (from, to) {
        (Route::Kernel(a), Route::Kernel(b)) => {
            syscall::rename(a.as_bytes(), b.as_bytes()).map_err(to_io_error)
        }
        (Route::Served(ca, a), Route::Served(cb, b)) if Arc::ptr_eq(&ca, &cb) => {
            ca.lock().rename(&a, &b).map_err(|e| match e {
                toyos::fs::Refused::Error(e) => served_error(e),
                toyos::fs::Refused::Link(_) => io::const_error!(
                    io::ErrorKind::CrossesDevices,
                    "the rename crosses a link out of its directory",
                ),
            })
        }
        _ => Err(io::const_error!(
            io::ErrorKind::CrossesDevices,
            "the two paths are on different servers",
        )),
    }
}

pub fn set_perm(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn set_perm_nofollow(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn rmdir(p: &Path) -> io::Result<()> {
    on_path(
        p,
        |abs| syscall::rmdir(abs.as_bytes()).map_err(to_io_error),
        |_, dir, rel| dir.rmdir(rel),
    )
}

pub fn remove_dir_all(path: &Path) -> io::Result<()> {
    for entry in readdir(path)? {
        let entry = entry?;
        let child_path = entry.path();
        if entry.file_type()?.is_dir() {
            remove_dir_all(&child_path)?;
        } else {
            unlink(&child_path)?;
        }
    }
    rmdir(path)
}

pub fn exists(path: &Path) -> io::Result<bool> {
    match stat(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn readlink(p: &Path) -> io::Result<PathBuf> {
    on_path(
        p,
        |abs| {
            let mut buf = [0u8; 4096];
            let n = syscall::readlink(abs.as_bytes(), &mut buf).map_err(to_io_error)?;
            // SAFETY: The kernel returns valid UTF-8 paths as raw bytes.
            Ok(PathBuf::from(unsafe { OsString::from_encoded_bytes_unchecked(buf[..n].to_vec()) }))
        },
        |_, dir, rel| {
            let mut buf = [0u8; toyos::fs::MAX_PATH];
            let n = dir.read_link(rel, &mut buf)?;
            let text = crate::str::from_utf8(&buf[..n]).map_err(|_| SyscallError::Io)?;
            Ok(PathBuf::from(text))
        },
    )
}

pub fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    let target = original
        .to_str()
        .ok_or_else(|| io::const_error!(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    on_path(
        link,
        |abs| syscall::symlink(target.as_bytes(), abs.as_bytes()).map_err(to_io_error),
        |_, dir, rel| dir.symlink(target, rel),
    )
}

pub fn link(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links on ToyOS"))
}

fn kernel_stat(abs: &str) -> io::Result<FileAttr> {
    if let Ok(fd) = syscall::open(abs.as_bytes(), OpenFlags::READ) {
        let result = syscall::fstat(fd);
        syscall::close(fd);
        let st = result.map_err(to_io_error)?;
        return Ok(FileAttr {
            size: st.size,
            file_type: opened_file_type(st.file_type),
            mtime: st.mtime,
        });
    }
    if kernel_is_dir(abs.as_bytes()) {
        return Ok(FileAttr {
            size: 0,
            file_type: FileType { is_file: false, is_dir: true, is_symlink: false },
            mtime: 0,
        });
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "file not found"))
}

pub fn stat(path: &Path) -> io::Result<FileAttr> {
    on_path(path, kernel_stat, |_, dir, rel| dir.stat(rel, true).map(FileAttr::of))
}

pub fn lstat(path: &Path) -> io::Result<FileAttr> {
    on_path(
        path,
        |abs| {
            let mut link_buf = [0u8; 4096];
            if let Ok(n) = syscall::readlink(abs.as_bytes(), &mut link_buf) {
                return Ok(FileAttr {
                    size: n as u64,
                    file_type: FileType { is_file: false, is_dir: false, is_symlink: true },
                    mtime: 0,
                });
            }
            kernel_stat(abs)
        },
        |_, dir, rel| dir.stat(rel, false).map(FileAttr::of),
    )
}

pub fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    crate::path::absolute(p)
}

pub fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    let mut read = OpenOptions::new();
    read.read(true);
    let reader = File::open(from, &read)?;
    let mut write = OpenOptions::new();
    write.write(true);
    write.truncate(true);
    write.create(true);
    let writer = File::open(to, &write)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut done = 0;
        while done < n {
            done += writer.write(&buf[done..n])?;
        }
        total += n as u64;
    }
    Ok(total)
}

pub fn set_times(_p: &Path, _times: FileTimes) -> io::Result<()> {
    Ok(())
}

pub fn set_times_nofollow(_p: &Path, _times: FileTimes) -> io::Result<()> {
    Ok(())
}

/// The whole of the file at `path` as a program image: what a spawn of a
/// program the kernel cannot open itself hands the kernel.
pub fn read_image(path: &Path) -> io::Result<toyos::process::Image> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    let file = File::open(path, &opts)?;
    let len = file.file_attr()?.size;
    toyos::process::Image::read(len, |buf| file.read(buf)).map_err(|refused| match refused {
        toyos::process::ImageRefused::Empty => {
            io::const_error!(io::ErrorKind::InvalidData, "the program is an empty file")
        }
        toyos::process::ImageRefused::Memory(e) => to_io_error(e),
        toyos::process::ImageRefused::Shrank => {
            io::const_error!(io::ErrorKind::UnexpectedEof, "the program shrank while it was read")
        }
        toyos::process::ImageRefused::Read(e) => e,
    })
}

/// Make the working directory `p`, which a file server judges when it serves
/// it and the kernel judges when it does.
pub fn chdir(p: &Path) -> io::Result<()> {
    let abs = absolute(p)?;
    if let Route::Served(..) = route(&abs)? {
        if !stat(Path::new(&abs))?.file_type.is_dir {
            return Err(io::const_error!(io::ErrorKind::NotADirectory, "not a directory"));
        }
    }
    syscall::chdir(abs.as_bytes()).map_err(to_io_error)
}
