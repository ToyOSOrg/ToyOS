//! Files on ToyOS: a path is served by the kernel (`/tmp`) or by a file
//! server behind one of the process's directory capabilities, and
//! `toyos::files` routes it, follows its links and keeps what it opened. A
//! `File` is one descriptor of the process's table either way.

use toyos::files::{self, At, Open};
use toyos_abi::syscall::{self, SeekFrom as AbiSeek};

use crate::ffi::OsString;
use crate::fmt;
use crate::fs::TryLockError;
use crate::hash::Hash;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut, SeekFrom};
use crate::path::{Path, PathBuf};
pub use crate::sys::fs::common::Dir;
use crate::sys::time::SystemTime;
use crate::sys::{fd_error, files_error, served_error, to_io_error};

/// A descriptor naming a file, which the `File` owns.
pub struct File(i32);

#[derive(Clone)]
pub struct FileAttr {
    size: u64,
    file_type: FileType,
    nlink: u64,
    atime: u64,
    mtime: u64,
    /// `toyos_abi::syscall::Stat`'s identity.
    volume: u64,
    node: u64,
}

/// A directory's listing, asked of its open descriptor a batch at a time.
pub struct ReadDir {
    dir: File,
    dir_path: PathBuf,
    batch: Vec<DirEntry>,
    index: usize,
    /// Where the next batch starts, `None` once the last was read.
    cursor: Option<u64>,
}

pub struct DirEntry {
    dir_path: PathBuf,
    name: OsString,
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
pub struct FileTimes {
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
}

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
    fn of(stat: files::Stat) -> FileAttr {
        FileAttr {
            size: stat.size,
            file_type: kernel_file_type(stat.file_type),
            nlink: stat.nlink,
            atime: stat.atime,
            mtime: stat.mtime,
            volume: stat.volume,
            node: stat.node,
        }
    }

    pub fn nlink(&self) -> u64 {
        self.nlink
    }

    pub fn volume(&self) -> u64 {
        self.volume
    }

    pub fn node(&self) -> u64 {
        self.node
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
        match self.atime {
            0 => Err(io::const_error!(io::ErrorKind::Unsupported, "the volume keeps no access time")),
            nanos => Ok(SystemTime::from_nanos(nanos)),
        }
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
    pub fn set_accessed(&mut self, t: SystemTime) {
        self.accessed = Some(t);
    }
    pub fn set_modified(&mut self, t: SystemTime) {
        self.modified = Some(t);
    }

    /// The change `toyos::files` makes: a time past what a file keeps is
    /// refused.
    fn change(&self) -> io::Result<files::Change> {
        let time = |t: Option<SystemTime>| match t {
            None => Ok(None),
            Some(t) => t.nanos().map(|n| Some(files::Time::At(n))).ok_or(io::const_error!(
                io::ErrorKind::InvalidInput,
                "a time past what a file keeps",
            )),
        };
        Ok(files::Change { size: None, atime: time(self.accessed)?, mtime: time(self.modified)? })
    }
}

impl fmt::Debug for ReadDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadDir").finish_non_exhaustive()
    }
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<io::Result<DirEntry>> {
        while self.index == self.batch.len() {
            let cursor = self.cursor?;
            let (listing, next) = match files::read_dir(self.dir.0, cursor) {
                Ok(read) => read,
                Err(e) => {
                    self.cursor = None;
                    return Some(Err(files_error(e)));
                }
            };
            let (mut batch, dir_path) = (Vec::new(), &self.dir_path);
            let decoded = toyos::fs::for_each_entry(&listing, |entry| {
                let (name, file_type) = (OsString::from(entry.name), FileType::of_kind(entry.kind));
                batch.push(DirEntry { dir_path: dir_path.clone(), name, file_type })
            });
            if let Err(e) = decoded {
                self.cursor = None;
                return Some(Err(served_error(e)));
            }
            (self.batch, self.index, self.cursor) = (batch, 0, next);
        }
        let e = &self.batch[self.index];
        self.index += 1;
        Some(Ok(DirEntry { dir_path: e.dir_path.clone(), name: e.name.clone(), file_type: e.file_type }))
    }
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.dir_path.join(&self.name)
    }

    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }

    /// Asked of the entry, as Unix's is.
    pub fn metadata(&self) -> io::Result<FileAttr> {
        lstat(&self.path())
    }

    /// What the listing said, or where it said only that the entry is no
    /// directory, as the kernel's does, what the entry is asked.
    pub fn file_type(&self) -> io::Result<FileType> {
        match self.file_type {
            FileType { is_file: false, is_dir: false, is_symlink: false } => lstat(&self.path()).map(|a| a.file_type),
            known => Ok(known),
        }
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

    /// What `toyos::files` opens: std's descriptors are not inherited, as
    /// std's are close-on-exec on unix.
    fn how(&self) -> Open {
        Open {
            read: self.read,
            write: self.write,
            append: self.append,
            create: self.create,
            create_new: self.create_new,
            truncate: self.truncate,
            directory: false,
            unnamed: false,
            cloexec: true,
        }
    }
}

/// `path`'s bytes, as `toyos::files` takes a path.
fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

/// Whether `path` is on a file server rather than the kernel.
pub fn is_served(path: &Path) -> bool {
    files::is_served(bytes(path))
}

impl File {
    /// The file descriptor `fd` names, owned from here on.
    pub fn from_fd(fd: i32) -> Self {
        File(fd)
    }

    pub fn raw_fd(&self) -> i32 {
        self.0
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.0
    }

    /// This file's descriptor, given up: closing it is the caller's.
    pub fn into_raw_fd(self) -> i32 {
        let fd = self.0;
        crate::mem::forget(self);
        fd
    }

    /// The kernel handle behind this file, held for `f`: none for a file a
    /// file server holds.
    pub fn with_handle<T>(&self, f: impl FnOnce(toyos_abi::RawHandle) -> T) -> io::Result<T> {
        crate::sys::on_fd(self.0, |h| Ok(f(h.raw())))
    }

    /// A kernel handle a child may use this file through: a copy of a kernel
    /// file's own, or a pipe its server writes to the file at this file's
    /// offset. The caller holds it until the child has its own.
    pub fn as_child_stdio(&self) -> io::Result<toyos::OwnedHandle> {
        files::stream(self.0).map_err(files_error)
    }

    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        files::open(At::Cwd, bytes(path), opts.how()).map(File).map_err(files_error)
    }

    pub fn file_attr(&self) -> io::Result<FileAttr> {
        files::fstat(self.0).map(FileAttr::of).map_err(files_error)
    }

    pub fn fsync(&self) -> io::Result<()> {
        files::fsync(self.0).map_err(files_error)
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
        files::truncate(self.0, size).map_err(files_error)
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        files::read(self.0, buf).map_err(files_error)
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        crate::io::default_read_vectored(|b| self.read(b), bufs)
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
        files::write(self.0, buf).map_err(files_error)
    }

    /// Read at `offset`, leaving the file's offset where it is.
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        files::pread(self.0, buf, offset).map_err(files_error)
    }

    /// Write at `offset`, leaving the file's offset where it is, whether or
    /// not it appends.
    pub fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        files::pwrite(self.0, buf, offset).map_err(files_error)
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        crate::io::default_write_vectored(|b| self.write(b), bufs)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn seek(&self, pos: SeekFrom) -> io::Result<u64> {
        let pos = match pos {
            SeekFrom::Start(n) => AbiSeek::Start(n),
            SeekFrom::Current(n) => AbiSeek::Current(n),
            SeekFrom::End(n) => AbiSeek::End(n),
        };
        files::seek(self.0, pos).map_err(files_error)
    }

    pub fn size(&self) -> Option<io::Result<u64>> {
        Some(self.file_attr().map(|a| a.size))
    }

    pub fn tell(&self) -> io::Result<u64> {
        self.seek(SeekFrom::Current(0))
    }

    /// A second descriptor on the same open file, sharing its offset.
    pub fn duplicate(&self) -> io::Result<File> {
        toyos::fd::dup(self.0, 0, false).map(File).map_err(fd_error)
    }

    pub fn set_permissions(&self, _perm: FilePermissions) -> io::Result<()> {
        Ok(())
    }

    pub fn set_times(&self, times: FileTimes) -> io::Result<()> {
        files::set_times(At::Fd(self.0), b"", false, &times.change()?).map_err(files_error)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: the file owns its descriptor, and nothing names it after this.
        let _ = unsafe { toyos::fd::close(self.0) };
    }
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {}
    }

    pub fn mkdir(&self, p: &Path) -> io::Result<()> {
        files::mkdir(At::Cwd, bytes(p)).map_err(files_error)
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "File({})", self.0)
    }
}

/// The type of something a `stat` or `fstat` answered.
fn kernel_file_type(ty: syscall::FileType) -> FileType {
    FileType {
        is_file: ty == syscall::FileType::File,
        is_dir: ty == syscall::FileType::Directory,
        is_symlink: ty == syscall::FileType::Symlink,
    }
}

pub fn readdir(p: &Path) -> io::Result<ReadDir> {
    let how = Open { read: true, directory: true, cloexec: true, ..Open::default() };
    let dir = files::open(At::Cwd, bytes(p), how).map(File).map_err(files_error)?;
    Ok(ReadDir { dir, dir_path: p.to_path_buf(), batch: Vec::new(), index: 0, cursor: Some(0) })
}

pub fn unlink(p: &Path) -> io::Result<()> {
    files::unlink(At::Cwd, bytes(p), false).map_err(files_error)
}

pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    files::rename((At::Cwd, bytes(old)), (At::Cwd, bytes(new))).map_err(files_error)
}

pub fn set_perm(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn set_perm_nofollow(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn rmdir(p: &Path) -> io::Result<()> {
    files::unlink(At::Cwd, bytes(p), true).map_err(files_error)
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
    let target = files::read_link(At::Cwd, bytes(p)).map_err(files_error)?;
    // SAFETY: a link's target is the bytes a path was written as.
    Ok(PathBuf::from(unsafe { OsString::from_encoded_bytes_unchecked(target) }))
}

pub fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    files::symlink(bytes(original), (At::Cwd, bytes(link))).map_err(files_error)
}

/// `dst` a second name of what `src` names, a symlink itself and not what it
/// leads to.
pub fn link(src: &Path, dst: &Path) -> io::Result<()> {
    files::link((At::Cwd, bytes(src)), false, (At::Cwd, bytes(dst))).map_err(files_error)
}

pub fn stat(path: &Path) -> io::Result<FileAttr> {
    files::stat(At::Cwd, bytes(path), true).map(FileAttr::of).map_err(files_error)
}

pub fn lstat(path: &Path) -> io::Result<FileAttr> {
    files::stat(At::Cwd, bytes(path), false).map(FileAttr::of).map_err(files_error)
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

pub fn set_times(p: &Path, times: FileTimes) -> io::Result<()> {
    files::set_times(At::Cwd, bytes(p), true, &times.change()?).map_err(files_error)
}

pub fn set_times_nofollow(p: &Path, times: FileTimes) -> io::Result<()> {
    files::set_times(At::Cwd, bytes(p), false, &times.change()?).map_err(files_error)
}

/// The whole of the file at `path` as a program image: what a spawn of a
/// program the kernel cannot open itself hands the kernel.
pub fn read_image(path: &Path, job: Option<toyos::BorrowedHandle<'_>>) -> io::Result<toyos::process::Image> {
    files::read_image_for(bytes(path), job).map_err(|refused| match refused {
        toyos::process::ImageRefused::Empty => {
            io::const_error!(io::ErrorKind::InvalidData, "the program is an empty file")
        }
        toyos::process::ImageRefused::Memory(e) => to_io_error(e),
        toyos::process::ImageRefused::Shrank => {
            io::const_error!(io::ErrorKind::UnexpectedEof, "the program shrank while it was read")
        }
        toyos::process::ImageRefused::Read(e) => files_error(e),
    })
}

/// Make the working directory `p`, which a file server judges when it serves
/// it and the kernel judges when it does.
pub fn chdir(p: &Path) -> io::Result<()> {
    files::chdir(bytes(p)).map_err(files_error)
}
