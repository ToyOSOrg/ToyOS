//! Files on ToyOS: a path is served by the kernel (`/tmp`) or by a file
//! server behind one of the process's directory capabilities, and
//! `toyos::files` routes it, follows its links and keeps what it opened. A
//! `File` is one descriptor of the process's table either way.

use toyos::files::{self, Open};
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
    mtime: u64,
    /// `toyos_abi::syscall::Stat`'s identity.
    volume: u64,
    node: u64,
}

pub struct ReadDir {
    entries: Vec<DirEntry>,
    index: usize,
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
    fn of_kernel(stat: syscall::Stat) -> FileAttr {
        FileAttr {
            size: stat.size,
            file_type: kernel_file_type(stat.file_type),
            mtime: stat.mtime,
            volume: stat.volume,
            node: stat.node,
        }
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
        crate::sys::on_fd(self.0, |h| Ok(f(h)))
    }

    /// A kernel handle a child may use this file through: a copy of a kernel
    /// file's own, or a pipe its server writes to the file at this file's
    /// offset. The caller holds it until the child has its own.
    pub fn as_child_stdio(&self) -> io::Result<toyos::OwnedHandle> {
        files::stream(self.0).map_err(files_error)
    }

    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        files::open(bytes(path), opts.how()).map(File).map_err(files_error)
    }

    pub fn file_attr(&self) -> io::Result<FileAttr> {
        files::fstat(self.0).map(FileAttr::of_kernel).map_err(files_error)
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

    pub fn set_times(&self, _times: FileTimes) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = toyos::fd::close(self.0);
    }
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {}
    }

    pub fn mkdir(&self, p: &Path) -> io::Result<()> {
        files::mkdir(bytes(p)).map_err(files_error)
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
    let listing = files::read_dir(bytes(p)).map_err(files_error)?;
    let mut entries = Vec::new();
    toyos::fs::for_each_entry(&listing, |entry| {
        let (name, file_type) = (OsString::from(entry.name), FileType::of_kind(entry.kind));
        entries.push(DirEntry { dir_path: p.to_path_buf(), name, file_type })
    })
    .map_err(served_error)?;
    Ok(ReadDir { entries, index: 0 })
}

pub fn unlink(p: &Path) -> io::Result<()> {
    files::unlink(bytes(p)).map_err(files_error)
}

pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    files::rename(bytes(old), bytes(new)).map_err(files_error)
}

pub fn set_perm(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn set_perm_nofollow(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    Ok(())
}

pub fn rmdir(p: &Path) -> io::Result<()> {
    files::rmdir(bytes(p)).map_err(files_error)
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
    let target = files::read_link(bytes(p)).map_err(files_error)?;
    // SAFETY: a link's target is the bytes a path was written as.
    Ok(PathBuf::from(unsafe { OsString::from_encoded_bytes_unchecked(target) }))
}

pub fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    files::symlink(bytes(original), bytes(link)).map_err(files_error)
}

pub fn link(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links on ToyOS"))
}

pub fn stat(path: &Path) -> io::Result<FileAttr> {
    files::stat(bytes(path), true).map(FileAttr::of_kernel).map_err(files_error)
}

pub fn lstat(path: &Path) -> io::Result<FileAttr> {
    files::stat(bytes(path), false).map(FileAttr::of_kernel).map_err(files_error)
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
    files::read_image(bytes(path)).map_err(|refused| match refused {
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
