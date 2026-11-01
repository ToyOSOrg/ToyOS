//! A session's shared region, mapped into this process.
//!
//! **Nothing on it is reached through a Rust reference but the ring words**,
//! and those are `&AtomicU32`: an atomic's `UnsafeCell` is what withdraws
//! `noalias` and `readonly` from memory a peer writes. The arena is reached
//! through [`Window`], a whole block at a time.

use core::sync::atomic::AtomicU32;

use toyos::shm::{Lent, SharedMemory};
use toyos_abi::syscall::SyscallError;
use toyos_blockring::layout::{RING_WORDS, SESSION_BYTES};
use toyos_blockring::Run;

use toyos::volatile::Window;

pub struct Region {
    memory: SharedMemory,
}

impl Region {
    /// A fresh region, for a client opening a session: memory the disk's
    /// driver can lend its device.
    pub fn create() -> Result<Self, SyscallError> {
        Ok(Self { memory: SharedMemory::create_for_device(SESSION_BYTES)? })
    }

    /// A region a client sent, refused unless it is memory the kernel
    /// allocated and exactly a session long.
    pub fn lent(handle: toyos::OwnedHandle) -> Result<Self, Lent> {
        Ok(Self { memory: SharedMemory::lent(handle, SESSION_BYTES)? })
    }

    /// The ring words, as the rings take them.
    pub fn words(&self) -> &[AtomicU32; RING_WORDS] {
        let base = self.memory.as_ptr();
        assert!((base as usize).is_multiple_of(align_of::<AtomicU32>()));
        // SAFETY: the mapping is `SESSION_BYTES` long and lives as long as
        // `self.memory`, which the returned borrow cannot outlive; the first
        // `RING_WORDS` words of it are inside it (`layout`'s own assertion);
        // the base is 2 MiB aligned; and an atomic is the one type that may
        // alias memory another process writes.
        unsafe { &*(base as *const [AtomicU32; RING_WORDS]) }
    }

    /// The arena blocks of `run`.
    pub fn arena(&self, run: &Run) -> Window {
        // SAFETY: `SESSION_BYTES` mapped for as long as `self.memory`, and
        // every window over it is used while the region is held.
        let whole = unsafe { Window::new(self.memory.as_ptr(), SESSION_BYTES) };
        whole.sub(run.span().offset, run.span().len)
    }

    /// The whole region, for the driver's own work on a region it made.
    pub fn window(&self) -> Window {
        // SAFETY: as `arena`'s.
        unsafe { Window::new(self.memory.as_ptr(), SESSION_BYTES) }
    }

    /// A second handle to the region, for a send.
    pub fn share(&self) -> Result<toyos::OwnedHandle, SyscallError> {
        self.memory.share()
    }

    pub fn handle(&self) -> toyos::BorrowedHandle<'_> {
        toyos::AsHandle::as_handle(&self.memory)
    }
}
