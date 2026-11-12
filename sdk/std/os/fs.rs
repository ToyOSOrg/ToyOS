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

/// Resolve this process's files through `services`, each a connector it
/// gives up here under the name it is a service by.
///
/// For the one process no parent endows a service: the supervisor makes the
/// machine's directory capabilities itself. No child inherits them. Refused,
/// and the connectors closed, when this process has adopted some already, or
/// once std has resolved a file or looked up a service of its own.
///
/// # Safety
/// Each handle names a connector: it is taken as one on the caller's word.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub unsafe fn adopt_services(services: Vec<(String, crate::os::toyos::io::OwnedHandle)>) -> io::Result<()> {
    // SAFETY: the caller's.
    let services = services.into_iter().map(|(name, connector)| (name, unsafe { toyos::endow::FromHandle::from_handle(connector.into_inner()) })).collect();
    toyos::endow::adopt(services)
        .map(drop)
        .map_err(|_| io::const_error!(io::ErrorKind::AlreadyExists, "this process already resolves its files"))
}
