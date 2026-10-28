//! A link's region, mapped into this process.
//!
//! **Nothing on it is reached through a Rust reference but the header's
//! words**, and those are `&AtomicU32`: an atomic's `UnsafeCell` is what
//! withdraws `noalias` and `readonly` from memory a peer writes. Frames are
//! reached through [`Bytes`], one copy each.

#![allow(unsafe_code)]

use core::sync::atomic::AtomicU32;

use toyos::shm::{Bytes, Lent, SharedMemory};
use toyos_abi::syscall::SyscallError;

use crate::layout::{REGION_BYTES, WORDS};

pub struct Region {
    memory: SharedMemory,
}

impl Region {
    /// A fresh region, for a driver opening a link.
    pub fn create() -> Result<Self, SyscallError> {
        Ok(Self { memory: SharedMemory::create(REGION_BYTES)? })
    }

    /// A region a driver sent, refused unless it is memory the kernel
    /// allocated and exactly a region long.
    pub fn lent(handle: toyos::OwnedHandle) -> Result<Self, Lent> {
        Ok(Self { memory: SharedMemory::lent(handle, REGION_BYTES)? })
    }

    /// The header's words, as the ends take them.
    pub fn words(&self) -> &[AtomicU32; WORDS] {
        let base = self.memory.as_ptr();
        assert!((base as usize).is_multiple_of(align_of::<AtomicU32>()));
        // SAFETY: the mapping is `REGION_BYTES` long and lives as long as
        // `self.memory`, which the returned borrow cannot outlive; `WORDS`
        // words fit its header (`layout`'s assertion); the base is aligned;
        // and an atomic is the one type that may alias memory another process
        // writes.
        unsafe { &*(base as *const [AtomicU32; WORDS]) }
    }

    /// The whole region's bytes, which every [`toyos::transport::Span`] an
    /// end answers is inside.
    pub fn bytes(&self) -> Bytes<'_> {
        self.memory.bytes()
    }

    /// A second handle to the region, for the hello.
    pub fn share(&self) -> Result<toyos::OwnedHandle, SyscallError> {
        self.memory.share()
    }
}
