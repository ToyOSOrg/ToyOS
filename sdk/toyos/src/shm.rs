//! Shared memory with RAII.
//!
//! A region is a **handle**, not a token. Holding one is the whole of being
//! allowed to map it, so there is no grant and no list of pids to keep: giving
//! a peer access is [`SharedMemory::share`] plus
//! [`Connection::send_handles`](crate::ipc::Connection::send_handles), and a
//! peer that never receives it can never name it.
//!
//! The mapping goes away with the last handle, so dropping this is all the
//! cleanup there is.
//!
//! **A region a peer lends is taken only through [`SharedMemory::lent`]**,
//! which believes the kernel's answer about it and not the peer's: a handle
//! the kernel's [`fstat`](syscall::fstat) says is not memory it allocated to a
//! region — another kind of object, or a device's registers — or is not the
//! length the protocol says, is refused before it is mapped, since mapping a
//! handle that names no region ends this process. [`SharedMemory::adopt`] is
//! for a region the kernel itself handed this process.

use core::fmt;
use core::sync::atomic::AtomicU8;

use toyos_abi::syscall::{self, FileType, Stat, SyscallError, SHM_PAGE};

use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct SharedMemory {
    handle: OwnedHandle,
    ptr: *mut u8,
    size: usize,
}

unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    /// A fresh region, rounded up to whole 2 MiB pages and mapped.
    ///
    /// Fallible: a size the kernel cannot express is `InvalidArgument` and
    /// memory it does not have is `ResourceExhausted`. A daemon reaches both
    /// through a client's request, so neither may be an assertion here.
    pub fn create(size: usize) -> Result<Self, SyscallError> {
        Self::adopt(syscall::shm_create(size)?, size)
    }

    /// Map a region the kernel itself handed this process — a device's
    /// window, a DMA grant — and view its first `size` bytes.
    ///
    /// Never for a region a peer sent, which is [`Self::lent`]'s.
    pub fn adopt(handle: RawHandle, size: usize) -> Result<Self, SyscallError> {
        let handle = OwnedHandle(handle);
        Ok(Self { ptr: map(&handle)?, handle, size })
    }

    /// Map a region a peer lent, whose protocol says it is `bytes` long, and
    /// view those bytes; refused, and never mapped, unless the kernel answers
    /// it is memory it allocated, `bytes` rounded up to whole [`SHM_PAGE`]s,
    /// as [`Self::create`] makes it. The handle is consumed either way.
    pub fn lent(handle: RawHandle, bytes: usize) -> Result<Self, Lent> {
        let handle = OwnedHandle(handle);
        judge(&syscall::fstat(handle.raw()).map_err(Lent::Kernel)?, bytes)?;
        Ok(Self { ptr: map(&handle).map_err(Lent::Kernel)?, handle, size: bytes })
    }

    /// A second handle to the same region, for sending to a peer.
    ///
    /// The send *moves* what it is given, so a sender that wants to keep the
    /// region duplicates first — which is the same rule spawn endowment has.
    pub fn share(&self) -> Result<OwnedHandle, SyscallError> {
        syscall::dup(self.handle.raw()).map(OwnedHandle)
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Give up ownership: the handle and its mapping stay, and the mapping goes
    /// when the caller closes the handle.
    pub fn into_raw(self) -> (RawHandle, *mut u8) {
        (self.handle.into_raw(), self.ptr)
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.size) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.size) }
    }

    /// The region as the one type that may alias memory a peer writes.
    pub fn as_atomic(&self) -> &[AtomicU8] {
        // SAFETY: the mapping is `size` bytes and lives as long as `self`, and
        // `AtomicU8` has `u8`'s size and alignment.
        unsafe { core::slice::from_raw_parts(self.ptr as *const AtomicU8, self.size) }
    }
}

fn map(handle: &OwnedHandle) -> Result<*mut u8, SyscallError> {
    // SAFETY: the mapping lives as long as the handle, which the caller keeps beside it.
    let ptr = unsafe { syscall::shm_map(handle.raw()) }?;
    assert!(!ptr.is_null(), "shm_map answered null");
    Ok(ptr)
}

/// Why a lent region was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lent {
    /// The kernel would not describe or map it.
    Kernel(SyscallError),
    /// It is not memory the kernel allocated to a region: another kind of
    /// object, or a device's registers, which a reader would read and write
    /// as a device.
    NotRam,
    /// It is not the length the protocol says: `is` bytes, where `bytes`
    /// rounded up to whole pages was owed.
    Length { is: usize, bytes: usize },
}

impl fmt::Display for Lent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(e) => write!(f, "the kernel would not describe or map it ({e:?})"),
            Self::NotRam => write!(f, "it is not memory the kernel allocated"),
            Self::Length { is, bytes } => write!(f, "it is {is} bytes, not {bytes} in whole pages"),
        }
    }
}

/// The kernel's answer about a lent region, held to the protocol's length.
fn judge(stat: &Stat, bytes: usize) -> Result<(), Lent> {
    if stat.file_type != FileType::SharedMemory {
        return Err(Lent::NotRam);
    }
    if Some(stat.size as usize) != bytes.checked_next_multiple_of(SHM_PAGE) {
        return Err(Lent::Length { is: stat.size as usize, bytes });
    }
    Ok(())
}

impl AsHandle for SharedMemory {
    fn as_handle(&self) -> RawHandle {
        self.handle.raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(len: usize, file_type: FileType) -> Stat {
        Stat { file_type, size: len as u64, ..Stat::default() }
    }

    #[test]
    fn a_region_the_kernel_made_for_the_length_is_taken() {
        assert_eq!(judge(&answer(SHM_PAGE, FileType::SharedMemory), SHM_PAGE), Ok(()));
        assert_eq!(judge(&answer(2 * SHM_PAGE, FileType::SharedMemory), SHM_PAGE + 1), Ok(()));
    }

    #[test]
    fn a_region_that_is_not_ram_is_refused_whatever_its_length() {
        assert_eq!(judge(&answer(SHM_PAGE, FileType::DeviceMemory), SHM_PAGE), Err(Lent::NotRam));
    }

    #[test]
    fn a_region_longer_than_the_protocol_says_is_refused() {
        assert_eq!(
            judge(&answer(2 * SHM_PAGE, FileType::SharedMemory), SHM_PAGE),
            Err(Lent::Length { is: 2 * SHM_PAGE, bytes: SHM_PAGE })
        );
    }

    #[test]
    fn a_region_shorter_than_the_protocol_says_is_refused() {
        assert_eq!(
            judge(&answer(SHM_PAGE, FileType::SharedMemory), SHM_PAGE + 1),
            Err(Lent::Length { is: SHM_PAGE, bytes: SHM_PAGE + 1 })
        );
    }

    #[test]
    fn a_length_whose_rounding_overflows_is_refused() {
        assert_eq!(
            judge(&answer(SHM_PAGE, FileType::SharedMemory), usize::MAX),
            Err(Lent::Length { is: SHM_PAGE, bytes: usize::MAX })
        );
    }
}
