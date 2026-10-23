//! Descriptors, as unix has them, and the kernel handles behind them.
//!
//! **A [`RawFd`](fd::RawFd) is a number in the process's one descriptor
//! table** (`toyos::fd`), which std and libc share: a `File` is one, as on
//! unix, and C's `read(fd)` reaches the same object. A [`RawHandle`] is the
//! kernel handle a descriptor names, which ToyOS-only code watches or hands to
//! the kernel directly ([`AsRawHandle`]); a portable program needs none.

// Re-export standard fd traits
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub use fd::*;

use crate::os::fd;
use crate::sys::{AsInner, FromInner, IntoInner};

/// A kernel handle, as `toyos_abi::RawHandle` numbers it.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub type RawHandle = u32;

/// The kernel handle behind a descriptor.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait AsRawHandle {
    /// The handle, good for as long as `self` holds its descriptor: closing
    /// the descriptor closes it. A descriptor a library keeps as no kernel
    /// handle answers `toyos_abi::HANDLE_INVALID`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn as_raw_handle(&self) -> RawHandle;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl<T: AsFd + ?Sized> AsRawHandle for T {
    fn as_raw_handle(&self) -> RawHandle {
        toyos::fd::with(self.as_fd().as_raw_fd(), |object| object.handle.0)
            .expect("std::os::toyos: a borrowed descriptor is not open")
    }
}

/// The descriptor `self` holds; with `borrowed`, as `AsFd` lends it too
/// (the network types' `AsFd` is `std::os::fd`'s own).
macro_rules! as_raw_fd {
    ($($ty:ty => |$this:ident| $fd:expr $(, $borrowed:ident)?);+ $(;)?) => {$(
        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl AsRawFd for $ty {
            #[inline]
            fn as_raw_fd(&self) -> RawFd {
                let $this = self;
                $fd
            }
        }
        $(as_raw_fd!(@$borrowed $ty);)?
    )+};
    (@borrowed $ty:ty) => {
        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl AsFd for $ty {
            #[inline]
            fn as_fd(&self) -> BorrowedFd<'_> {
                // SAFETY: the descriptor is `self`'s, open for as long as the borrow.
                unsafe { BorrowedFd::borrow_raw(self.as_raw_fd()) }
            }
        }
    };
}

as_raw_fd! {
    crate::fs::File => |f| f.as_inner().as_raw_fd(), borrowed;
    crate::process::ChildStdin => |p| p.as_inner().raw_fd(), borrowed;
    crate::process::ChildStdout => |p| p.as_inner().raw_fd(), borrowed;
    crate::process::ChildStderr => |p| p.as_inner().raw_fd(), borrowed;
    crate::net::TcpStream => |s| s.as_inner().as_raw_fd();
    crate::net::TcpListener => |l| l.as_inner().as_raw_fd();
    crate::net::UdpSocket => |u| u.as_inner().as_raw_fd(), borrowed;
}

/// A stream's descriptor is read and written through its region, never as a
/// file: taken as one, it panics with `toyos::net`'s refusal.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FromRawFd for crate::fs::File {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> crate::fs::File {
        if let Ok(handle) = toyos::fd::with(fd, |o| o.handle) {
            if toyos::net::stream::is_stream(handle) {
                panic!("{}", toyos::net::stream::Misuse::AsFile(handle));
            }
        }
        crate::fs::File::from_inner(crate::sys::fs::File::from_fd(fd))
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl IntoRawFd for crate::fs::File {
    #[inline]
    fn into_raw_fd(self) -> RawFd {
        self.into_inner().into_raw_fd()
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl From<crate::fs::File> for OwnedFd {
    #[inline]
    fn from(file: crate::fs::File) -> OwnedFd {
        // SAFETY: the file's descriptor, given up to its new owner.
        unsafe { OwnedFd::from_raw_fd(file.into_raw_fd()) }
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl From<OwnedFd> for crate::fs::File {
    #[inline]
    fn from(owned: OwnedFd) -> crate::fs::File {
        // SAFETY: the owned descriptor, given up to the file.
        unsafe { crate::fs::File::from_raw_fd(owned.into_raw_fd()) }
    }
}

/// A stream's descriptor taken back: one whose stream this process holds an
/// end of shares it, and any other panics with `toyos::net`'s refusal. A
/// process holds an end by its connect, its accept, or a spawn that gave it a
/// holder.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl FromRawFd for crate::net::TcpStream {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> crate::net::TcpStream {
        // SAFETY: the caller's, as `FromRawFd` states it.
        crate::net::TcpStream::from(unsafe { fd::OwnedFd::from_raw_fd(fd) })
    }
}

/// The descriptor given out keeps naming the stream's end, for whoever takes
/// it back.
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
        let handle = owned.as_raw_handle();
        if let Err(misuse) = toyos::net::stream::adopt(toyos_abi::RawHandle(handle)) {
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

/// A listener's and a datagram socket's descriptor, given out and taken back
/// as a stream's is: taken back, one whose socket this process holds an end of
/// of that kind shares it, and any other panics. A process holds an end by its
/// listen or bind, or a spawn that gave it a holder.
macro_rules! socket_fd {
    ($($ty:ident, $kind:ident, $what:literal);+ $(;)?) => {$(
        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl FromRawFd for crate::net::$ty {
            #[inline]
            unsafe fn from_raw_fd(fd: RawFd) -> crate::net::$ty {
                // SAFETY: the caller's, as `FromRawFd` states it.
                crate::net::$ty::from(unsafe { fd::OwnedFd::from_raw_fd(fd) })
            }
        }

        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl IntoRawFd for crate::net::$ty {
            #[inline]
            fn into_raw_fd(self) -> RawFd {
                self.into_inner().into_fd().into_raw_fd()
            }
        }

        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl From<fd::OwnedFd> for crate::net::$ty {
            #[inline]
            fn from(owned: fd::OwnedFd) -> Self {
                let handle = owned.as_raw_handle();
                if toyos::net::socket::kind(toyos_abi::RawHandle(handle)) != Some(toyos::net::socket::Kind::$kind) {
                    panic!(concat!("toyos::net: handle {} is no ", $what, " of this process's: one reaches a process by its own bind or a spawn that gave it a holder of one"), handle);
                }
                crate::net::$ty::from_inner(crate::sys::net::$ty::from_fd(owned))
            }
        }

        #[stable(feature = "toyos_ext", since = "1.0.0")]
        impl From<crate::net::$ty> for fd::OwnedFd {
            #[inline]
            fn from(socket: crate::net::$ty) -> Self {
                socket.into_inner().into_fd()
            }
        }
    )+};
}

socket_fd! {
    TcpListener, Listener, "listener";
    UdpSocket, Datagram, "datagram socket";
}

/// Switch stdin between canonical and raw mode.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub fn set_stdin_raw(raw: bool) {
    crate::sys::stdio::set_stdin_raw(raw);
}
