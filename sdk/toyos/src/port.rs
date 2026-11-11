//! A port, and the two ends of it.
//!
//! Two types, so a client holding a [`Connector`] cannot accept the service's
//! connections: that is a state the compiler refuses rather than a
//! `PermissionDenied` at run time.
//!
//! **A port exists before either process runs.** Whoever creates it hands the
//! acceptor to the server and the connector to its clients, so a connection
//! works from a client's first instruction whether or not the server has
//! reached `accept` or has even been spawned. There is nothing to retry and no
//! timeout anywhere.

use toyos_abi::syscall::{self, SyscallError, MAX_BADGE};

use crate::ipc::Connection;
use crate::{AsHandle, BorrowedHandle, OwnedHandle, RawHandle};

/// The server's end: connections are accepted from it, and nothing is written
/// to it.
pub struct Acceptor(pub(crate) OwnedHandle);

/// A client's end: connections are opened through it, and its queue cannot be
/// read.
pub struct Connector(pub(crate) OwnedHandle);

/// Open a connection through `connector`, a connector this process holds
/// carrying `WRITE`: one it was endowed ([`crate::endow::connector`]) or one
/// of its own.
///
/// [`SyscallError::Gone`] is "the server exited", and `PermissionDenied` a
/// connector given only to pass on. There is no "not yet". `InvalidArgument`
/// is a handle that is no connector: its type is its giver's word.
pub fn connect(connector: BorrowedHandle<'_>) -> Result<Connection, SyscallError> {
    syscall::connect(connector.raw()).map(|h| Connection(OwnedHandle(h)))
}

/// Make a port. Grants nothing on its own — a port with no clients is not
/// authority.
pub fn create() -> Result<(Acceptor, Connector), SyscallError> {
    let port = syscall::port_create()?;
    Ok((Acceptor(OwnedHandle(port.acceptor)), Connector(OwnedHandle(port.connector))))
}

impl Acceptor {
    /// Take the oldest queued connection, blocking until there is one.
    ///
    /// **It answers with the connection and nothing else.** Who connected is
    /// not the kernel's to assert: a server that wants to name its client
    /// reads it out of the protocol's first frame, where it is already the
    /// client's own claim about itself and already distrusted.
    pub fn accept(&self) -> Result<Connection, SyscallError> {
        syscall::accept(self.0.raw()).map(|h| Connection(OwnedHandle(h)))
    }

    /// A connection already queued, or `None` where none is: for a loop that
    /// must never wait on a client.
    pub fn try_accept(&self) -> Result<Option<Connection>, SyscallError> {
        match syscall::accept_nonblock(self.0.raw()) {
            Ok(h) => Ok(Some(Connection(OwnedHandle(h)))),
            Err(SyscallError::WouldBlock) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A connector to this port whose every connection carries `badge`, which
    /// [`Self::badge`] reads back: what this holder granted whoever holds the
    /// connector, never who that is.
    pub fn mint(&self, badge: &[u8]) -> Result<Connector, SyscallError> {
        syscall::port_mint(self.0.raw(), badge).map(|h| Connector(OwnedHandle(h)))
    }

    /// The badge `conn`, accepted from this port, was made with. `NotFound` is
    /// a connection through an unbadged connector; one accepted from another
    /// port is `PermissionDenied`.
    pub fn badge<'a>(&self, conn: &Connection, out: &'a mut [u8; MAX_BADGE]) -> Result<&'a [u8], SyscallError> {
        let len = syscall::port_badge(self.0.raw(), conn.as_handle().raw(), out)?;
        Ok(&out[..len])
    }

    /// Give up ownership, for a handle about to be endowed or transferred.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    /// # Safety
    /// `raw` must be a live acceptor handle this process owns and nothing else
    /// answers for.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }
}

impl Connector {
    /// Open a connection to the port ([`connect`]).
    pub fn connect(&self) -> Result<Connection, SyscallError> {
        connect(self.as_handle())
    }

    /// A second connector to the same port, for a holder handing one on while
    /// keeping its own.
    pub fn duplicate(&self) -> Result<Self, SyscallError> {
        self.0.try_clone().map(Self)
    }

    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    /// # Safety
    /// `raw` must be a live connector handle this process owns and nothing else
    /// answers for.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }
}

impl AsHandle for Acceptor {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl AsHandle for Connector {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}
