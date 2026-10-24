//! Shared memory with RAII.
//!
//! A region is a **handle**, not a token. Holding one is the whole of being
//! allowed to map it, so there is no grant and no list of pids to keep: giving
//! a peer access is [`SharedMemory::share`] plus
//! a frame that carries it ([`Connection::send_with`](crate::ipc::Connection::send_with)), and a
//! peer that never receives it can never name it.
//!
//! A mapping holds its region until it is taken away, and dropping this takes
//! it away before closing the handle: that is all the cleanup there is.
//!
//! **A region a peer lends is taken only through [`SharedMemory::lent`]**,
//! which believes the kernel's answer about it and not the peer's: a handle
//! the kernel's [`fstat`](syscall::fstat) says is not memory it allocated to a
//! region — another kind of object, or a device's registers — or is not the
//! length the protocol says, is refused before it is mapped, since mapping a
//! handle that names no region ends this process. [`SharedMemory::adopt`] is
//! for a region the kernel itself handed this process.
//!
//! **A region nobody can write is an [`Immutable`]**, taken only once the
//! kernel answers it is one: every mapping of it is read-only, so it has no
//! mutable view to offer.

use core::fmt;
use core::sync::atomic::AtomicU8;

use toyos_abi::syscall::{self, FileType, Stat, SyscallError, PAGE_SIZE, SHM_DEVICE};

use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct SharedMemory {
    handle: OwnedHandle,
    ptr: *mut u8,
    size: usize,
    /// What the mapping covers: the whole region, whatever `size` views.
    mapped: usize,
}

unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    /// A fresh region of zeroes, rounded up to whole pages and mapped.
    ///
    /// Fallible: a size the kernel cannot express is `InvalidArgument` and
    /// memory it does not have is `ResourceExhausted`. A daemon reaches both
    /// through a client's request, so neither may be an assertion here.
    pub fn create(size: usize) -> Result<Self, SyscallError> {
        Self::adopt(syscall::shm_create(size)?, size)
    }

    /// [`create`](Self::create) for a region a device will be lent, by this
    /// process's driver or by a driver it sends the region to.
    pub fn create_for_device(size: usize) -> Result<Self, SyscallError> {
        Self::adopt(syscall::shm_create_flags(size, SHM_DEVICE)?, size)
    }

    /// Map a region the kernel itself handed this process — a device's
    /// window, a DMA grant — and view its first `size` bytes.
    ///
    /// Never for a region a peer sent, which is [`Self::lent`]'s.
    pub fn adopt(handle: RawHandle, size: usize) -> Result<Self, SyscallError> {
        let handle = OwnedHandle(handle);
        let mapped = syscall::fstat(handle.raw())?.size as usize;
        Ok(Self { ptr: map(&handle)?, handle, size, mapped })
    }

    /// Map a region a peer lent, whose protocol says it is `bytes` long, and
    /// view those bytes; refused, and never mapped, unless the kernel answers
    /// it is memory it allocated, `bytes` rounded up to whole pages, as
    /// [`Self::create`] makes it. The handle is consumed either way.
    pub fn lent(handle: RawHandle, bytes: usize) -> Result<Self, Lent> {
        let handle = OwnedHandle(handle);
        let stat = syscall::fstat(handle.raw()).map_err(Lent::Kernel)?;
        judge(&stat, bytes)?;
        Ok(Self { ptr: map(&handle).map_err(Lent::Kernel)?, handle, size: bytes, mapped: stat.size as usize })
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

    /// Give up ownership: the handle and its mapping stay, the mapping for
    /// the caller to take away with `vm_release` of [`len`](Self::len) bytes
    /// before it closes the handle.
    pub fn into_raw(self) -> (RawHandle, *mut u8) {
        let this = core::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is never used or dropped again, so the handle moves
        // out of it once.
        let handle = unsafe { core::ptr::read(&this.handle) };
        (handle.into_raw(), this.ptr)
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

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: the mapping `map` made, which nothing reaches once this drops;
        // the handle closes after it.
        let _ = unsafe { syscall::vm_release(self.ptr, self.mapped) };
    }
}

fn map(handle: &OwnedHandle) -> Result<*mut u8, SyscallError> {
    // SAFETY: the mapping lives as long as `SharedMemory`, which takes it away.
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
    if Some(stat.size as usize) != bytes.checked_next_multiple_of(PAGE_SIZE) {
        return Err(Lent::Length { is: stat.size as usize, bytes });
    }
    Ok(())
}

impl AsHandle for SharedMemory {
    fn as_handle(&self) -> RawHandle {
        self.handle.raw()
    }
}

/// A region nobody can write, mapped read-only: a payload the loader verified,
/// as the kernel hands it out (`toyos_abi::boot::BootImage`).
pub struct Immutable {
    handle: OwnedHandle,
    ptr: *const u8,
    len: usize,
}

// SAFETY: the mapping is read-only and nobody writes the region.
unsafe impl Send for Immutable {}
// SAFETY: as for `Send`: there is nothing to race with.
unsafe impl Sync for Immutable {}

impl Immutable {
    /// Map `handle`, refused and never mapped unless the kernel answers it is
    /// a region nobody can write. Its length is the kernel's answer. The handle
    /// is consumed either way.
    pub fn adopt(handle: OwnedHandle) -> Result<Self, NotImmutable> {
        let stat = syscall::fstat(handle.raw()).map_err(NotImmutable::Kernel)?;
        if stat.file_type != FileType::ImmutableMemory {
            return Err(NotImmutable::Writable);
        }
        // A region the kernel maps is within the address space, so its length is a `usize`.
        Ok(Self { ptr: map(&handle).map_err(NotImmutable::Kernel)?, handle, len: stat.size as usize })
    }

    /// `len` of its bytes from `offset`, a whole page in, as a region of
    /// their own over the same pages, unmapped here
    /// (`toyos_abi::syscall::shm_sub`).
    pub fn sub(&self, offset: u64, len: u64) -> Result<OwnedHandle, SyscallError> {
        syscall::shm_sub(self.handle.raw(), offset, len).map(OwnedHandle)
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes, lives as long as `self`, and is
        // written by nobody, so a shared reference over it is sound.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// Why a handle was not taken as an [`Immutable`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotImmutable {
    /// The kernel would not describe or map it.
    Kernel(SyscallError),
    /// It names something other than a region nobody can write.
    Writable,
}

impl fmt::Display for NotImmutable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(e) => write!(f, "the kernel would not describe or map it ({e:?})"),
            Self::Writable => write!(f, "it is not a region nobody can write"),
        }
    }
}

impl AsHandle for Immutable {
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
        assert_eq!(judge(&answer(PAGE_SIZE, FileType::SharedMemory), PAGE_SIZE), Ok(()));
        assert_eq!(judge(&answer(2 * PAGE_SIZE, FileType::SharedMemory), PAGE_SIZE + 1), Ok(()));
    }

    #[test]
    fn a_region_that_is_not_ram_is_refused_whatever_its_length() {
        assert_eq!(judge(&answer(PAGE_SIZE, FileType::DeviceMemory), PAGE_SIZE), Err(Lent::NotRam));
    }

    #[test]
    fn a_region_longer_than_the_protocol_says_is_refused() {
        assert_eq!(
            judge(&answer(2 * PAGE_SIZE, FileType::SharedMemory), PAGE_SIZE),
            Err(Lent::Length { is: 2 * PAGE_SIZE, bytes: PAGE_SIZE })
        );
    }

    #[test]
    fn a_region_shorter_than_the_protocol_says_is_refused() {
        assert_eq!(
            judge(&answer(PAGE_SIZE, FileType::SharedMemory), PAGE_SIZE + 1),
            Err(Lent::Length { is: PAGE_SIZE, bytes: PAGE_SIZE + 1 })
        );
    }

    #[test]
    fn a_length_whose_rounding_overflows_is_refused() {
        assert_eq!(
            judge(&answer(PAGE_SIZE, FileType::SharedMemory), usize::MAX),
            Err(Lent::Length { is: PAGE_SIZE, bytes: usize::MAX })
        );
    }
}
