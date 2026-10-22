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

pub mod audio;
pub mod census;
pub mod device;
pub mod devset;
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
pub mod launch;
pub mod log;
pub mod net;
pub mod port;
pub mod process;
pub mod relocate;
pub mod surface;
pub mod shm;
pub mod spawn;
pub mod syscap;
pub mod sync;
pub mod system;
pub mod transport;
pub mod volatile;

pub use ipc::Connection;
pub use device::{
    AcpiDev, DmaRegion, FramebufferDev, PciDev,
};

pub use toyos_abi::RawHandle;
pub use toyos_abi::ioport;

/// Trait for types that wrap a kernel handle.
///
/// Used by [`poller`] and other APIs that accept any handle type.
///
/// **One thing, one name.** Six of the typed wrappers below and in [`device`]
/// and [`ipc`] carried a second public accessor with an identical body until
/// 2026-08-20, when the wave that confined libc's descriptor vocabulary to
/// `sdk/libc` deleted them rather than renaming them: a second word for
/// what `as_handle` already says is the blur that ruling exists to remove.
pub trait AsHandle {
    fn as_handle(&self) -> RawHandle;
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
    /// one: it reaches its end only through that module, and a stream does not
    /// move between processes.
    pub unsafe fn from_raw(raw: RawHandle) -> Self {
        Self(raw)
    }

    pub(crate) fn raw(&self) -> RawHandle { self.0 }

    /// Give up ownership: the handle stays open and this stops answering for
    /// it.
    pub(crate) fn into_raw(self) -> RawHandle {
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

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        toyos_abi::syscall::close(self.0);
    }
}

impl AsHandle for OwnedHandle {
    fn as_handle(&self) -> RawHandle { self.0 }
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
    fn as_handle(&self) -> RawHandle { self.0.raw() }
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

    pub fn pipe_map(&self) -> Result<*mut u8, toyos_abi::syscall::SyscallError> {
        toyos_abi::syscall::pipe_map(self.0.raw())
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
    fn as_handle(&self) -> RawHandle { self.0.raw() }
}

impl From<Pipe> for OwnedHandle {
    fn from(pipe: Pipe) -> Self { pipe.0 }
}

/// A console handle: the one `/system/bin/supervisor` endows `/system/bin/logkeeper` with,
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
}

impl AsHandle for Console {
    fn as_handle(&self) -> RawHandle { self.0.raw() }
}
