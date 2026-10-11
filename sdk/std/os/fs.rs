use crate::fs::Metadata;
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
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl MetadataExt for Metadata {
    fn dev(&self) -> u64 {
        self.as_inner().volume()
    }
    fn ino(&self) -> u64 {
        self.as_inner().node()
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
/// owns and gives up here.
///
/// For the one process no parent endows a namespace: the supervisor builds the machine's
/// directory capabilities itself. Refused, and the handle closed, when this
/// process already resolves through one.
///
/// # Safety
///
/// `namespace` must be a namespace handle the caller owns and uses nowhere
/// else from here on: it is closed with the process's resolution, as an
/// `OwnedFd` handed to `from_raw_fd` is.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub unsafe fn adopt_namespace(namespace: u32) -> io::Result<()> {
    // SAFETY: the caller's contract above.
    let ns = unsafe { toyos::namespace::Namespace::from_raw(toyos_abi::RawHandle(namespace)) };
    toyos::endow::adopt_namespace(ns)
        .map_err(|_| io::const_error!(io::ErrorKind::AlreadyExists, "this process already has a namespace"))
}
