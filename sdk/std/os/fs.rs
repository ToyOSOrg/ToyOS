use crate::io;
use crate::path::Path;

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
