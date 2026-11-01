//! Shared memory with RAII.
//!
//! A region is a **handle**, not a token. Holding one is the whole of being
//! allowed to map it, so there is no grant and no list of pids to keep: giving
//! a peer access is [`SharedMemory::share`] plus
//! a frame that carries it ([`Connection::send_with`](crate::ipc::Connection::send_with)), and a
//! peer that never receives it can never name it.
//!
//! A mapping holds its region until it is taken away, and dropping this takes
//! it away before closing the handle: that is all the cleanup there is. Each
//! map is a mapping of its own, so dropping one value never unmaps another's.
//!
//! **No reference is formed over a writable region's bytes.** Any other
//! mapping of it — a peer's, a device's, this process's own through a second
//! handle — can write them meanwhile, which no `&[u8]` or `&mut [u8]` allows.
//! They are reached through [`Bytes`], or by `unsafe` code over
//! [`SharedMemory::as_ptr`] keeping the same rule.
//!
//! **A region one party writes and another only reads** is handed to the
//! reader without `WRITE` ([`ReadOnly`]): the kernel maps it readable only,
//! and its holder can never map it writable or lend it to a device.
//!
//! A handle is judged by the kernel's answer before it is mapped
//! ([`SharedMemory::lent`], [`SharedMemory::adopt`]), since mapping one that
//! names no region ends this process.
//!
//! **A region nobody can write is an [`Immutable`]**, taken only once the
//! kernel answers it is one: every mapping of it is read-only, so a plain
//! slice over it is sound.

use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;

use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, FileType, Stat, SyscallError, VmProt, PAGE_SIZE, SHM_DEVICE};

use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct SharedMemory {
    handle: OwnedHandle,
    ptr: *mut u8,
    len: usize,
    /// What the mapping covers: the whole region, whatever `len` views.
    mapped: usize,
}

// SAFETY: the mapping is this process's whichever thread holds it, and its
// bytes are reached only through `Bytes`, whose copies are opaque accesses
// that form no data race in Rust's model, whoever else writes the bytes.
unsafe impl Send for SharedMemory {}
// SAFETY: as for `Send`.
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    /// A fresh region of zeroes, rounded up to whole pages and mapped.
    ///
    /// Fallible: a size the kernel cannot express is `InvalidArgument` and
    /// memory it does not have is `ResourceExhausted`. A daemon reaches both
    /// through a client's request, so neither may be an assertion here.
    pub fn create(len: usize) -> Result<Self, SyscallError> {
        Self::adopt(OwnedHandle(syscall::shm_create(len)?), len)
    }

    /// [`create`](Self::create), counted to the job `job` is a handle to,
    /// carrying `WRITE`, rather than this process's own: memory laid out
    /// for a child started in it.
    pub fn create_for(job: crate::BorrowedHandle<'_>, len: usize) -> Result<Self, SyscallError> {
        Self::adopt(OwnedHandle(syscall::shm_create_for(job.raw(), len)?), len)
    }

    /// [`create`](Self::create) for a region a device will be lent, by this
    /// process's driver or by a driver it sends the region to.
    pub fn create_for_device(len: usize) -> Result<Self, SyscallError> {
        Self::adopt(OwnedHandle(syscall::shm_create_flags(len, SHM_DEVICE)?), len)
    }

    /// Map a region the kernel itself handed this process — a device's
    /// window, a DMA grant, a scanout — and view its first `len` bytes.
    /// `InvalidArgument`, and never mapped, unless the kernel answers it is a
    /// writable region of at least `len` bytes. The handle is consumed either
    /// way.
    ///
    /// A region a peer sent is [`Self::lent`]'s.
    pub fn adopt(handle: OwnedHandle, len: usize) -> Result<Self, SyscallError> {
        let stat = syscall::fstat(handle.raw())?;
        if !adoptable(&stat, len) {
            return Err(SyscallError::InvalidArgument);
        }
        // A region the kernel maps is within the address space, so its length is a `usize`.
        Ok(Self { ptr: map(&handle, VmProt::ReadWrite)?, handle, len, mapped: stat.size as usize })
    }

    /// Map a region a peer lent, whose protocol says it is `bytes` long, and
    /// view those bytes; refused, and never mapped, unless the kernel answers
    /// it is memory it allocated, `bytes` rounded up to whole pages, as
    /// [`Self::create`] makes it. The handle is consumed either way; one
    /// lent without `WRITE` is the kernel's `PermissionDenied`.
    pub fn lent(handle: OwnedHandle, bytes: usize) -> Result<Self, Lent> {
        let stat = syscall::fstat(handle.raw()).map_err(Lent::Kernel)?;
        judge(&stat, bytes)?;
        let ptr = map(&handle, VmProt::ReadWrite).map_err(Lent::Kernel)?;
        Ok(Self { ptr, handle, len: bytes, mapped: stat.size as usize })
    }

    /// A second handle to the same region, for sending to a peer.
    ///
    /// The send *moves* what it is given, so a sender that wants to keep the
    /// region duplicates first — which is the same rule spawn endowment has.
    pub fn share(&self) -> Result<OwnedHandle, SyscallError> {
        self.handle.try_clone()
    }

    /// [`Self::share`], without `WRITE`: a peer's [`ReadOnly::lent`] maps it,
    /// and nothing it does with the handle writes the region.
    pub fn share_read_only(&self) -> Result<OwnedHandle, SyscallError> {
        self.handle.as_handle().try_clone_narrowed(READ_ONLY)
    }

    /// The viewed bytes, as memory. A device's registers are reached through
    /// a [`Window`](crate::volatile::Window) instead.
    pub fn bytes(&self) -> Bytes<'_> {
        // SAFETY: the viewed bytes of this writable mapping, which lives as
        // long as `self`; `SharedMemory` reaches them no other way.
        unsafe { Bytes::from_raw(self.ptr, self.len) }
    }

    /// The first byte, for `unsafe` code that reaches the region as atomics
    /// or through a [`Window`](crate::volatile::Window), never as a reference
    /// to its bytes.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Give up ownership: the handle and its mapping stay, the mapping for
    /// the caller to take away with `vm_release` before it closes the handle.
    /// The mapping is [`len`](Self::len) in whole pages for a region this made
    /// or was lent, and the kernel's whole region for one adopted.
    pub fn into_raw(self) -> (RawHandle, *mut u8) {
        let this = core::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is never used or dropped again, so the handle moves
        // out of it once.
        let handle = unsafe { core::ptr::read(&this.handle) };
        (handle.into_raw(), this.ptr)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: the mapping `map` made, which nothing reaches once this drops;
        // the handle closes after it.
        let _ = unsafe { syscall::vm_release(self.ptr, self.mapped) };
    }
}

impl AsHandle for SharedMemory {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// A range of a mapping another party can write, borrowed from what keeps it
/// mapped.
///
/// Each read is one copy the compiler sees only as an access to the range
/// (`crate::arch::copy`), so it is never repeated or assumed to hold still.
/// It is no snapshot: a writer meanwhile changes which bytes arrive. A reader
/// acts on its own copy and never reads the same bytes twice to check them.
#[derive(Clone, Copy)]
pub struct ReadBytes<'a> {
    ptr: *const u8,
    len: usize,
    _mapping: PhantomData<&'a ()>,
}

// SAFETY: a range of a mapping the process holds, reached only by opaque
// copies that form no data race in Rust's model.
unsafe impl Send for ReadBytes<'_> {}
// SAFETY: as for `Send`.
unsafe impl Sync for ReadBytes<'_> {}

impl<'a> ReadBytes<'a> {
    pub fn len(self) -> usize {
        self.len
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    /// `len` of these bytes from `offset`.
    pub fn sub(self, offset: usize, len: usize) -> Self {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "shm: {len} bytes at {offset:#x} run past {:#x}",
            self.len
        );
        // SAFETY: the assertion kept `offset + len` inside this range.
        Self { ptr: unsafe { self.ptr.add(offset) }, len, _mapping: PhantomData }
    }

    /// Copy `out.len()` bytes from `offset` out.
    pub fn read(self, offset: usize, out: &mut [u8]) {
        let from = self.sub(offset, out.len());
        // SAFETY: `sub` bounded the source inside the range, and `out` is a
        // reference, so it is not the mapping.
        unsafe { crate::arch::copy(out.as_mut_ptr(), from.ptr, out.len()) }
    }

    pub fn to_vec(self) -> Vec<u8> {
        let mut out = alloc::vec![0; self.len];
        self.read(0, &mut out);
        out
    }

    /// Copy the whole range to `dst`.
    ///
    /// # Safety
    /// `dst` is writable for [`len`](Self::len) bytes and none of them is in
    /// this range.
    pub unsafe fn read_to(self, dst: *mut u8) {
        // SAFETY: the caller's, and the range is mapped for `'a`.
        unsafe { crate::arch::copy(dst, self.ptr, self.len) }
    }
}

/// [`ReadBytes`] of a writable mapping, which each write reaches by one copy
/// as each read is.
#[derive(Clone, Copy)]
pub struct Bytes<'a>(ReadBytes<'a>);

impl<'a> Bytes<'a> {
    /// # Safety
    /// `ptr` names `len` writable bytes that stay mapped for all of `'a`,
    /// which this process reaches only through `Bytes`, atomics or volatile
    /// accesses meanwhile.
    pub unsafe fn from_raw(ptr: *mut u8, len: usize) -> Self {
        Self(ReadBytes { ptr, len, _mapping: PhantomData })
    }

    /// `len` of these bytes from `offset`.
    pub fn sub(self, offset: usize, len: usize) -> Self {
        Self(self.0.sub(offset, len))
    }

    /// Copy `data` in at `offset`.
    pub fn write(self, offset: usize, data: &[u8]) {
        let to = self.0.sub(offset, data.len());
        // SAFETY: `sub` bounded the destination inside the range, which is
        // writable, and `data` is a reference, so it is not the mapping.
        unsafe { crate::arch::copy(to.ptr.cast_mut(), data.as_ptr(), data.len()) }
    }

    /// Where the range begins, for a kernel call that writes it.
    pub(crate) fn as_mut_ptr(self) -> *mut u8 {
        self.0.ptr.cast_mut()
    }

    /// Write zeroes over the whole range.
    pub fn zero(self) {
        // SAFETY: the whole range, which is mapped writable for `'a`.
        unsafe { crate::arch::zero(self.0.ptr.cast_mut(), self.0.len) }
    }

    /// Copy the whole range in from `src`.
    ///
    /// # Safety
    /// `src` is readable for [`len`](ReadBytes::len) bytes, none of them is in
    /// this range, and no reference reaches them meanwhile.
    pub unsafe fn write_from(self, src: *const u8) {
        // SAFETY: the caller's, and the range is mapped writable for `'a`.
        unsafe { crate::arch::copy(self.0.ptr.cast_mut(), src, self.0.len) }
    }
}

impl<'a> core::ops::Deref for Bytes<'a> {
    type Target = ReadBytes<'a>;

    fn deref(&self) -> &ReadBytes<'a> {
        &self.0
    }
}

/// What a region's writable handle carries, but `WRITE`.
const READ_ONLY: Rights = Rights::MAP.union(Rights::DUP).union(Rights::TRANSFER);

fn map(handle: &OwnedHandle, prot: VmProt) -> Result<*mut u8, SyscallError> {
    // SAFETY: the mapping lives as long as what holds it, which takes it away.
    let ptr = unsafe { syscall::shm_map(handle.raw(), prot) }?;
    assert!(!ptr.is_null(), "shm_map answered null");
    Ok(ptr)
}

/// Whether the kernel's answer is a writable region with `len` bytes to view.
fn adoptable(stat: &Stat, len: usize) -> bool {
    matches!(stat.file_type, FileType::SharedMemory | FileType::DeviceMemory) && len as u64 <= stat.size
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

/// A read-only mapping of a region another party may write, whose bytes are
/// reached through [`ReadBytes`].
pub struct ReadOnly {
    handle: OwnedHandle,
    ptr: *const u8,
    len: usize,
    mapped: usize,
}

// SAFETY: as for `SharedMemory`, through `ReadBytes`.
unsafe impl Send for ReadOnly {}
// SAFETY: as for `Send`.
unsafe impl Sync for ReadOnly {}

impl ReadOnly {
    /// A fresh region of `len` bytes of zeroes, mapped here read-only, and
    /// the one handle that maps it writable, for the party that writes it.
    pub fn create(len: usize) -> Result<(Self, OwnedHandle), SyscallError> {
        let writer = OwnedHandle(syscall::shm_create(len)?);
        let handle = writer.as_handle().try_clone_narrowed(READ_ONLY)?;
        let ptr = map(&handle, VmProt::Read)?;
        // `shm_create` refused any `len` whose pages overflow.
        Ok((Self { handle, ptr, len, mapped: len.next_multiple_of(PAGE_SIZE) }, writer))
    }

    /// [`SharedMemory::lent`], mapped read-only, whatever the handle allows.
    pub fn lent(handle: OwnedHandle, bytes: usize) -> Result<Self, Lent> {
        let stat = syscall::fstat(handle.raw()).map_err(Lent::Kernel)?;
        judge(&stat, bytes)?;
        let ptr = map(&handle, VmProt::Read).map_err(Lent::Kernel)?;
        Ok(Self { handle, ptr, len: bytes, mapped: stat.size as usize })
    }

    pub fn bytes(&self) -> ReadBytes<'_> {
        ReadBytes { ptr: self.ptr, len: self.len, _mapping: PhantomData }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        // SAFETY: the mapping `map` made, which nothing reaches once this drops;
        // the handle closes after it.
        let _ = unsafe { syscall::vm_release(self.ptr.cast_mut(), self.mapped) };
    }
}

impl AsHandle for ReadOnly {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.handle.as_handle()
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
        Ok(Self { ptr: map(&handle, VmProt::Read).map_err(NotImmutable::Kernel)?, handle, len: stat.size as usize })
    }

    /// `len` of its bytes from `offset`, a whole page in, as a region of
    /// their own over the same pages, unmapped here
    /// (`toyos_abi::syscall::shm_sub`).
    pub fn sub(&self, offset: u64, len: u64) -> Result<OwnedHandle, SyscallError> {
        // SAFETY: the handle is this region's own, and memory nobody writes
        // has no writer to undo.
        unsafe { syscall::shm_sub(self.handle.raw(), offset, len) }.map(OwnedHandle)
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

impl Drop for Immutable {
    fn drop(&mut self) {
        // SAFETY: the mapping `map` made, which nothing reaches once this drops;
        // the handle closes after it.
        let _ = unsafe { syscall::vm_release(self.ptr.cast_mut(), self.len) };
    }
}

impl AsHandle for Immutable {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.handle.as_handle()
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

    #[test]
    fn an_adopted_view_is_at_most_the_region() {
        for kind in [FileType::SharedMemory, FileType::DeviceMemory] {
            assert!(adoptable(&answer(2 * PAGE_SIZE, kind), 2 * PAGE_SIZE));
            assert!(adoptable(&answer(2 * PAGE_SIZE, kind), PAGE_SIZE + 1));
            assert!(!adoptable(&answer(2 * PAGE_SIZE, kind), 2 * PAGE_SIZE + 1));
            assert!(!adoptable(&answer(PAGE_SIZE, kind), usize::MAX));
        }
    }

    #[test]
    fn only_a_writable_region_is_adopted() {
        for kind in [FileType::ImmutableMemory, FileType::Pipe, FileType::Unknown] {
            assert!(!adoptable(&answer(PAGE_SIZE, kind), 1), "{kind:?}");
        }
    }

    /// `Bytes` over `buf`, as a mapping would be.
    fn over(buf: &mut [u8]) -> Bytes<'_> {
        // SAFETY: `buf` is reached only through the answer while it lives.
        unsafe { Bytes::from_raw(buf.as_mut_ptr(), buf.len()) }
    }

    #[test]
    fn every_length_and_alignment_is_copied_whole_and_nothing_beside_it() {
        let source: Vec<u8> = (0..400).map(|i| (i * 7 + 3) as u8).collect();
        for at in 0..16 {
            for len in 0..300 {
                let mut region = vec![0xEEu8; 400];
                over(&mut region).write(at, &source[..len]);
                assert_eq!(&region[at..at + len], &source[..len], "write of {len} at {at}");
                assert!(region[..at].iter().chain(&region[at + len..]).all(|&b| b == 0xEE), "write of {len} at {at} spilled");

                let mut out = vec![0xEEu8; len + 16];
                over(&mut region).read(at, &mut out[at % 8..at % 8 + len]);
                assert_eq!(&out[at % 8..at % 8 + len], &source[..len], "read of {len} at {at}");
                assert!(out[..at % 8].iter().chain(&out[at % 8 + len..]).all(|&b| b == 0xEE), "read of {len} at {at} spilled");
            }
        }
    }

    #[test]
    fn a_sub_range_is_its_own_bytes() {
        let mut region: Vec<u8> = (0..64).collect();
        let part = over(&mut region).sub(8, 16);
        assert_eq!(part.len(), 16);
        assert_eq!(part.to_vec(), (8..24).collect::<Vec<u8>>());
        part.write(15, &[0xAA]);
        assert_eq!(region[23], 0xAA);
    }

    #[test]
    fn a_zeroed_range_is_zero_and_nothing_beside_it() {
        for len in [0, 1, 15, 16, 17, 31, 32, 33, 255, 256, 257, 1000] {
            let mut region = vec![0xEEu8; len + 2];
            over(&mut region).sub(1, len).zero();
            assert_eq!(region[0], 0xEE);
            assert!(region[1..=len].iter().all(|&b| b == 0), "{len}");
            assert_eq!(region[len + 1], 0xEE);
        }
    }

    #[test]
    #[should_panic(expected = "run past")]
    fn a_read_past_the_range_panics() {
        let mut region = [0u8; 16];
        over(&mut region).sub(4, 8).read(4, &mut [0; 5]);
    }

    #[test]
    #[should_panic(expected = "run past")]
    fn a_write_past_the_range_panics() {
        let mut region = [0u8; 16];
        over(&mut region).write(12, &[0; 5]);
    }

    #[test]
    #[should_panic(expected = "run past")]
    fn an_offset_that_overflows_panics() {
        let mut region = [0u8; 16];
        over(&mut region).sub(usize::MAX, 2);
    }
}
