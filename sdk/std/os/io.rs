// Re-export standard fd traits
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub use fd::*;

use crate::os::fd;
use crate::sys::{AsInner, FromInner};

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::fs::File {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().as_raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FromRawFd for crate::fs::File {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> crate::fs::File {
        crate::fs::File::from_inner(crate::sys::fs::File::from_fd(toyos_abi::RawHandle(fd as u32)))
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl IntoRawFd for crate::fs::File {
    #[inline]
    fn into_raw_fd(self) -> RawFd {
        self.as_inner().raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::net::TcpStream {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().as_raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::net::TcpListener {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().as_raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::process::ChildStdin {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::process::ChildStdout {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::process::ChildStderr {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().raw_fd()
    }
}

/// Switch stdin between canonical and raw mode.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub fn set_stdin_raw(raw: bool) {
    crate::sys::stdio::set_stdin_raw(raw);
}
