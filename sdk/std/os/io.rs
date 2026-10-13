// Re-export standard fd traits
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub use fd::*;

use crate::os::fd;
use crate::sys::{AsInner, FromInner, IntoInner};

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl AsRawFd for crate::fs::File {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.as_inner().as_raw_fd()
    }
}

/// A stream's handle is read and written through its region, never as a
/// file: taken as one, it panics with `toyos::net`'s refusal.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FromRawFd for crate::fs::File {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> crate::fs::File {
        let handle = toyos_abi::RawHandle(fd as u32);
        if toyos::net::stream::is_stream(handle) {
            panic!("{}", toyos::net::stream::Misuse::AsFile(handle));
        }
        crate::fs::File::from_inner(crate::sys::fs::File::from_fd(handle))
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

/// A stream's handle taken back: one this process holds the stream's end for
/// shares it, and any other panics with `toyos::net`'s refusal, a stream
/// belonging to the process that connected or accepted it.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FromRawFd for crate::net::TcpStream {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> crate::net::TcpStream {
        if let Err(misuse) = toyos::net::stream::adopt(toyos_abi::RawHandle(fd as u32)) {
            panic!("{misuse}");
        }
        // SAFETY: the caller's, as `FromRawFd` states it.
        let owned = unsafe { fd::OwnedFd::from_raw_fd(fd) };
        crate::net::TcpStream::from_inner(crate::sys::net::TcpStream::from_fd(owned))
    }
}

/// The handle given out keeps reaching the stream's end, for whoever takes it
/// back.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl IntoRawFd for crate::net::TcpStream {
    #[inline]
    fn into_raw_fd(self) -> RawFd {
        self.into_inner().into_fd().into_raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl From<fd::OwnedFd> for crate::net::TcpStream {
    #[inline]
    fn from(owned: fd::OwnedFd) -> Self {
        if let Err(misuse) = toyos::net::stream::adopt(toyos_abi::RawHandle(owned.as_raw_fd() as u32)) {
            panic!("{misuse}");
        }
        crate::net::TcpStream::from_inner(crate::sys::net::TcpStream::from_fd(owned))
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl From<crate::net::TcpStream> for fd::OwnedFd {
    #[inline]
    fn from(stream: crate::net::TcpStream) -> Self {
        stream.into_inner().into_fd()
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
