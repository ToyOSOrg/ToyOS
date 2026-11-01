//! What std has no word for of a ToyOS stream: the doorbells a poller
//! watches, and a registration that arms its wants, so a readiness loop's
//! first event comes.
//!
//! A stream's readiness is its region's, and the kernel's readiness of its
//! handles only a wake: its receive bell is readable when netstack rang it for
//! bytes or their end, and its send bell when netstack rang it for room or a
//! failure. Each says "look", so a source is read and written until
//! `WouldBlock`, which arms the want the next ring answers.

use crate::io;
use crate::os::toyos::io::RawHandle;

/// ToyOS's extensions of [`crate::net::TcpStream`].
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait TcpStreamExt {
    /// The stream's send bell, a kernel handle watched for reading where the
    /// stream is watched for writing.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn bell(&self) -> io::Result<RawHandle>;

    /// The stream's receive bell, a kernel handle watched for reading where
    /// the stream is watched for reading.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn receive_bell(&self) -> io::Result<RawHandle>;

    /// Registers the stream for its readable and writable readiness: each
    /// named way's want armed and looked at, and netstack rung where one is
    /// ready, which it answers by ringing back.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn register(&self, readable: bool, writable: bool) -> io::Result<()>;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl TcpStreamExt for crate::net::TcpStream {
    fn bell(&self) -> io::Result<RawHandle> {
        crate::sys::AsInner::as_inner(self).bell()
    }

    fn receive_bell(&self) -> io::Result<RawHandle> {
        crate::sys::AsInner::as_inner(self).receive_bell()
    }

    fn register(&self, readable: bool, writable: bool) -> io::Result<()> {
        crate::sys::AsInner::as_inner(self).register(readable, writable)
    }
}
