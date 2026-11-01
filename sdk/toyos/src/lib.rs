//! ToyOS userland SDK.
//!
//! Typed handles, IPC framing, ports and namespaces, shared memory, and
//! ergonomic wrappers over the kernel ABI defined in `toyos-abi`, and the
//! [`transport`] client/server sessions run over.

// `not(test)` so this crate's own decisions can be unit-tested on the host.
// [`net::hangup`] is a total function from a kernel word to a [`net::NetError`]
// and needs no guest to exercise, but a `#[test]` needs the `test` crate and
// that needs `std`. Every ordinary build — the `rustc-dep-of-std` one included
// — is `not(test)` and is `no_std` exactly as before; `cargo test` in this
// directory is the only build that is not, and it links no syscall.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod arch;

pub mod audio;
pub mod census;
pub mod device;
pub mod endow;
pub mod fd;
pub mod files;
pub mod fs;
pub mod poller;
pub mod power;
pub mod wake;
pub mod ipc;
pub mod job;
pub mod namespace;
pub mod paged;
pub mod launch;
pub mod log;
pub mod net;
pub mod port;
pub mod process;
pub mod quit;
pub mod relocate;
pub mod surface;
pub mod shm;
pub mod spawn;
pub mod syscap;
pub mod sync;
pub mod system;
pub mod timing;
pub mod transport;
pub mod volatile;

pub use ipc::Connection;
pub use device::{
    AcpiDev, DmaRegion, FramebufferDev, PciDev,
};

pub use toyos_abi::RawHandle;
pub use toyos_abi::ioport;

/// A type that owns or borrows a kernel handle, lent for as long as `self`.
///
/// Used by [`poller`] and every other API that takes any handle: a number
/// reaches them only through [`BorrowedHandle::borrow_raw`]
/// (`toyos_abi::handle`).
pub trait AsHandle {
    fn as_handle(&self) -> BorrowedHandle<'_>;
}

impl<T: AsHandle + ?Sized> AsHandle for &T {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        (**self).as_handle()
    }
}

/// A handle this process holds, lent for `'a` by what owns it.
///
/// `Copy`, and closes nothing: the owner outlives every borrow. Its number is
/// [`raw`](Self::raw)'s to read, which acts on nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct BorrowedHandle<'a> {
    raw: RawHandle,
    _owner: core::marker::PhantomData<&'a OwnedHandle>,
}

impl BorrowedHandle<'_> {
    /// Borrow a handle by number.
    ///
    /// # Safety
    /// `raw` is live in this process's table for the borrow's whole life, and
    /// whatever owns it does not close or give it away meanwhile.
    pub const unsafe fn borrow_raw(raw: RawHandle) -> Self {
        Self { raw, _owner: core::marker::PhantomData }
    }

    pub const fn raw(self) -> RawHandle {
        self.raw
    }

    /// A second handle to the same object, carrying what this one carries.
    pub fn try_clone_to_owned(self) -> Result<OwnedHandle, toyos_abi::syscall::SyscallError> {
        // SAFETY: a borrow is live for the call, and its owner answers for the
        // object's other holders.
        unsafe { toyos_abi::syscall::dup(self.raw) }.map(OwnedHandle)
    }

    /// [`try_clone_to_owned`](Self::try_clone_to_owned), carrying only
    /// `rights`; `PermissionDenied` for any this one does not carry.
    pub fn try_clone_narrowed(self, rights: toyos_abi::handle::Rights) -> Result<OwnedHandle, toyos_abi::syscall::SyscallError> {
        // SAFETY: as in `try_clone_to_owned`.
        unsafe { toyos_abi::syscall::dup_narrowed(self.raw, rights) }.map(OwnedHandle)
    }
}

impl AsHandle for BorrowedHandle<'_> {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        *self
    }
}

/// The two ends of a fresh pipe.
///
/// `SYS_PIPE` is unprivileged and always has been: what a pipe is *worth* is
/// who holds its ends, and nothing but a transfer puts one in somebody else's
/// table.
pub fn pipe_pair() -> Result<(Pipe, Pipe), toyos_abi::syscall::SyscallError> {
    let ends = toyos_abi::syscall::pipe()?;
    Ok((Pipe(OwnedHandle(ends.read)), Pipe(OwnedHandle(ends.write))))
}

/// A fresh null handle carrying `rights` ([`toyos_abi::syscall::null`]).
pub fn null(rights: toyos_abi::handle::Rights) -> Result<OwnedHandle, toyos_abi::syscall::SyscallError> {
    toyos_abi::syscall::null(rights).map(OwnedHandle)
}

/// One owned handle, closed when it drops.
///
/// `!Copy` and `!Clone`, so a handle cannot be closed twice and cannot be
/// forgotten by accident — [`OwnedHandle::into_raw`] is the single spelling for
/// giving up ownership, and the single thing to grep for when asking who does.
///
/// Consumers hold the typed wrappers below; this is what a handle is while it
/// travels, the currency of [`Connection::send_with`].
pub struct OwnedHandle(pub(crate) RawHandle);

impl OwnedHandle {
    /// Take ownership of a handle this process holds.
    ///
    /// # Safety
    /// `raw` must be a live handle in this process's table that nothing else
    /// answers for: this closes it when it drops. A handle
    /// [`net::stream`](crate::net::stream) answers for, a stream's, is never
    /// one: it reaches its end only through that module, and it stays in its
    /// process, which another reaches only through an export.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(raw)
    }

    pub(crate) fn raw(&self) -> RawHandle { self.0 }

    /// [`BorrowedHandle::try_clone_to_owned`].
    pub fn try_clone(&self) -> Result<OwnedHandle, toyos_abi::syscall::SyscallError> {
        self.as_handle().try_clone_to_owned()
    }

    /// Give up ownership: the handle stays open and this stops answering for
    /// it.
    pub fn into_raw(self) -> RawHandle {
        let raw = self.0;
        #[allow(clippy::disallowed_methods, reason = "the handle is the caller's from here, so this drop must not close it")]
        core::mem::forget(self);
        raw
    }

    pub(crate) fn read(&self, buf: &mut [u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::read(self.0, buf)
    }

    pub(crate) fn write(&self, buf: &[u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::write(self.0, buf)
    }

    pub(crate) fn read_nonblock(&self, buf: &mut [u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::read_nonblock(self.0, buf)
    }

    pub(crate) fn write_nonblock(&self, buf: &[u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::write_nonblock(self.0, buf)
    }
}

impl core::fmt::Debug for OwnedHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("OwnedHandle").field(&self.0).finish()
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this owns the handle, and nothing uses it once it drops.
        unsafe { toyos_abi::syscall::close(self.0) };
    }
}

impl AsHandle for OwnedHandle {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: owned by `self`, which outlives the borrow.
        unsafe { BorrowedHandle::borrow_raw(self.0) }
    }
}

/// A claimed hardware device, out of this process's endowment table.
///
/// There is no `open`: the supervisor mints every claim from the machine's one
/// system capability and endows it, so which process drives a device is a fact
/// the image was built with. See [`endow::device`].
pub struct Device(pub(crate) OwnedHandle);

impl Device {
    /// Give up ownership, for a claim about to be endowed. A claim carries no
    /// `DUP` right, so this is the only way one changes hands.
    pub fn into_raw(self) -> RawHandle { self.0.into_raw() }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.read(buf)
    }
}

impl AsHandle for Device {
    fn as_handle(&self) -> BorrowedHandle<'_> { self.0.as_handle() }
}

/// One end of a kernel pipe.
///
/// Arrives either from [`pipe_pair`] or over a connection: a pipe end is a
/// handle now, and there is no id to hand a peer instead of the thing itself.
pub struct Pipe(pub(crate) OwnedHandle);

impl Pipe {
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.read(buf)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.write(buf)
    }

    pub fn read_nonblock(&self, buf: &mut [u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.read_nonblock(buf)
    }

    pub fn write_nonblock(&self, buf: &[u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.write_nonblock(buf)
    }

    /// Consume the `Pipe`, giving up the handle without closing it.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    /// Take ownership of a pipe end that arrived over a connection.
    ///
    /// # Safety
    /// `raw` must be a live pipe-end handle this process owns and nothing else
    /// answers for.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }
}

impl AsHandle for Pipe {
    fn as_handle(&self) -> BorrowedHandle<'_> { self.0.as_handle() }
}

/// A pipe end a peer sent: its type is the sender's word, as a connection's
/// is ([`Connection::received`]).
impl From<OwnedHandle> for Pipe {
    fn from(handle: OwnedHandle) -> Self { Self(handle) }
}

/// Each typed handle, given up as the handle it owns: to send, endow or
/// provide it.
macro_rules! owned_from {
    ($($ty:ty => |$this:ident| $owned:expr);+ $(;)?) => {$(
        impl From<$ty> for OwnedHandle {
            fn from($this: $ty) -> Self { $owned }
        }
    )+};
}

owned_from! {
    Pipe => |p| p.0;
    Device => |d| d.0;
    Console => |c| c.0;
    ipc::Connection => |c| c.0;
    port::Acceptor => |a| a.0;
    port::Connector => |c| c.0;
    namespace::Namespace => |n| n.0;
    syscap::SysCap => |c| c.0;
    job::Job => |j| j.0;
    process::Process => |p| p.0;
    device::FramebufferDev => |d| d.0 .0;
    device::PciDev => |d| d.0 .0;
    device::AcpiDev => |d| d.0 .0;
}

#[cfg(all(feature = "std", target_os = "toyos", not(test)))]
extern crate std;

/// std's handle types (`std::os::toyos::io`), which a copy of this crate
/// linked beside std converts to and from: std links a copy of its own, so
/// only a number crosses, owned or lent as it was.
#[cfg(all(feature = "std", target_os = "toyos", not(test)))]
mod std_io {
    use std::os::toyos::io as sio;
    use sio::{FromRawHandle, IntoRawHandle};

    use super::{BorrowedHandle, OwnedHandle, RawHandle};

    impl From<OwnedHandle> for sio::OwnedHandle {
        fn from(handle: OwnedHandle) -> Self {
            // SAFETY: given up by its one owner, to this one.
            unsafe { sio::OwnedHandle::from_raw_handle(handle.into_raw().0) }
        }
    }

    impl From<sio::OwnedHandle> for OwnedHandle {
        fn from(handle: sio::OwnedHandle) -> Self {
            // SAFETY: as above.
            unsafe { OwnedHandle::from_raw(RawHandle(handle.into_raw_handle())) }
        }
    }

    impl<'a> From<BorrowedHandle<'a>> for sio::BorrowedHandle<'a> {
        fn from(handle: BorrowedHandle<'a>) -> Self {
            // SAFETY: lent for `'a` by its owner, as the source was.
            unsafe { sio::BorrowedHandle::borrow_raw(handle.raw().0) }
        }
    }

    impl<'a> From<sio::BorrowedHandle<'a>> for BorrowedHandle<'a> {
        fn from(handle: sio::BorrowedHandle<'a>) -> Self {
            // SAFETY: as above.
            unsafe { BorrowedHandle::borrow_raw(RawHandle(sio::AsRawHandle::as_raw_handle(&handle))) }
        }
    }

    /// std's handles and descriptors that name a kernel handle, lent for a
    /// poll or a copy as this crate's are: closed only with their owner.
    macro_rules! std_as_handle {
        ($($ty:ty),+ $(,)?) => {$(
            impl super::AsHandle for $ty {
                fn as_handle(&self) -> BorrowedHandle<'_> {
                    // SAFETY: `self` owns or borrows the handle for as long as
                    // it is borrowed.
                    unsafe { BorrowedHandle::borrow_raw(RawHandle(sio::AsRawHandle::as_raw_handle(self))) }
                }
            }
        )+};
    }

    std_as_handle!(
        sio::OwnedHandle,
        sio::BorrowedHandle<'_>,
        std::process::Child,
        std::process::ChildStdin,
        std::process::ChildStdout,
        std::process::ChildStderr,
        std::net::TcpStream,
        std::net::TcpListener,
        std::net::UdpSocket,
    );

    macro_rules! std_owned_from {
        ($($ty:ty),+ $(,)?) => {$(
            impl From<$ty> for sio::OwnedHandle {
                fn from(typed: $ty) -> Self { OwnedHandle::from(typed).into() }
            }
        )+};
    }

    std_owned_from!(
        super::Pipe,
        super::wake::Waker,
        super::wake::Bell,
        super::Device,
        super::Console,
        super::ipc::Connection,
        super::port::Acceptor,
        super::port::Connector,
        super::namespace::Namespace,
        super::syscap::SysCap,
        super::job::Job,
        super::process::Process,
        super::device::FramebufferDev,
        super::device::PciDev,
        super::device::AcpiDev,
    );
}

/// A console handle: the one the supervisor endows `/system/bin/logkeeper` with,
/// the only one in the machine that may write.
///
/// **A write takes whole lines and says how many bytes that was.** The
/// kernel queues each line for its console writer and takes only what its
/// queue has room for, so a short count is a writer ahead of the console —
/// poll for `WRITABLE` and send the rest.
pub struct Console(pub(crate) OwnedHandle);

impl Console {
    pub fn write_nonblock(&self, buf: &[u8]) -> Result<usize, toyos_abi::syscall::SyscallError> {
        self.0.write_nonblock(buf)
    }

    /// Say whether this holder carries the log off the machine by another
    /// channel (`toyos_abi::syscall::console_carried`); the console's end
    /// lets go of it.
    pub fn carry_elsewhere(&self, elsewhere: bool) -> Result<(), toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::console_carried(self.0.raw(), elsewhere)
    }
}

impl AsHandle for Console {
    fn as_handle(&self) -> BorrowedHandle<'_> { self.0.as_handle() }
}
