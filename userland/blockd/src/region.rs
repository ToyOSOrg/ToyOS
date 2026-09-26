//! A session's shared region, mapped into this process.
//!
//! **Nothing on it is reached through a Rust reference but the ring words**,
//! and those are `&AtomicU32`: an atomic's `UnsafeCell` is what withdraws
//! `noalias` and `readonly` from memory a peer writes. The arena is reached
//! through [`Window`], a whole block at a time.

use core::sync::atomic::AtomicU32;

use toyos::shm::SharedMemory;
use toyos_abi::syscall::SyscallError;
use toyos_blockring::layout::{arena_byte, ARENA_BLOCKS, RING_WORDS, SESSION_BYTES};
use toyos_blockring::BLOCK_BYTES;

use crate::window::Window;

pub struct Region {
    memory: SharedMemory,
}

impl Region {
    /// A fresh region, for a client opening a session.
    pub fn create() -> Result<Self, SyscallError> {
        Ok(Self { memory: SharedMemory::create(SESSION_BYTES)? })
    }

    /// A region a client sent. Shared memory comes in whole 2 MiB pages and
    /// never less, so every region a peer can send is at least a session long.
    pub fn adopt(handle: toyos::RawHandle) -> Result<Self, SyscallError> {
        Ok(Self { memory: SharedMemory::adopt(handle, SESSION_BYTES)? })
    }

    /// The ring words, as the rings take them.
    pub fn words(&self) -> &[AtomicU32] {
        let base = self.memory.as_ptr();
        assert!(base as usize % align_of::<AtomicU32>() == 0);
        // SAFETY: the mapping is `SESSION_BYTES` long and lives as long as
        // `self.memory`, which the returned borrow cannot outlive; the first
        // `RING_WORDS` words of it are inside it (`layout`'s own assertion);
        // the base is 2 MiB aligned; and an atomic is the one type that may
        // alias memory another process writes.
        unsafe { core::slice::from_raw_parts(base as *const AtomicU32, RING_WORDS) }
    }

    /// Arena blocks `first..first + blocks`.
    pub fn arena(&self, first: u32, blocks: u32) -> Window {
        assert!(
            first.checked_add(blocks).is_some_and(|end| end <= ARENA_BLOCKS),
            "blockd: arena blocks {first}+{blocks} past the arena"
        );
        // SAFETY: `SESSION_BYTES` mapped for as long as `self.memory`, and
        // every window over it is used while the region is held.
        let whole = unsafe { Window::new(self.memory.as_ptr(), SESSION_BYTES) };
        whole.sub(arena_byte(first), blocks as usize * BLOCK_BYTES)
    }

    /// A second handle to the region, for a send.
    pub fn share(&self) -> Result<toyos::RawHandle, SyscallError> {
        self.memory.share()
    }

    pub fn handle(&self) -> toyos::RawHandle {
        use toyos::AsHandle;
        self.memory.as_handle()
    }
}
