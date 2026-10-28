use crate::fs::{File, Metadata};
use crate::io;
use crate::path::Path;
use crate::sys::AsInner;

/// ToyOS's answers about a file beyond the portable ones.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait MetadataExt {
    /// The volume the file is on (`toyos_abi::syscall::Stat::volume`), as
    /// Unix's `dev`: 0 for what has none.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn dev(&self) -> u64;
    /// The file on that volume (`toyos_abi::syscall::Stat::node`), as Unix's
    /// `ino`: with [`dev`](Self::dev), the same through every path and handle
    /// that reaches the file and no other existing file's.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn ino(&self) -> u64;
    /// The names the file has, as Unix's `nlink`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn nlink(&self) -> u64;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl MetadataExt for Metadata {
    fn dev(&self) -> u64 {
        self.as_inner().volume()
    }
    fn ino(&self) -> u64 {
        self.as_inner().node()
    }
    fn nlink(&self) -> u64 {
        self.as_inner().nlink()
    }
}

/// Reads and writes at an offset, which leave the file's own where it is, as
/// Unix's `FileExt` does.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait FileExt {
    /// Read at `offset`: how many bytes, 0 at or past the file's end.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
    /// Write at `offset`, whether or not the file was opened to append:
    /// how many bytes.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize>;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FileExt for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.as_inner().read_at(buf, offset)
    }
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        self.as_inner().write_at(buf, offset)
    }
}

/// Creates a new symbolic link on the filesystem.
///
/// The `link` path will be a symbolic link pointing to the `original` path.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    crate::sys::fs::symlink(original.as_ref(), link.as_ref())
}

/// Resolve this process's files through `namespace`, a namespace handle it
/// gives up here.
///
/// For the one process no parent endows a namespace: the supervisor builds the machine's
/// directory capabilities itself. Refused, and the handle closed, when this
/// process already resolves through one.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub fn adopt_namespace(namespace: crate::os::toyos::io::OwnedHandle) -> io::Result<()> {
    toyos::endow::adopt_namespace(toyos::endow::FromHandle::from_handle(namespace.into_inner()))
        .map_err(|_| io::const_error!(io::ErrorKind::AlreadyExists, "this process already has a namespace"))
}
