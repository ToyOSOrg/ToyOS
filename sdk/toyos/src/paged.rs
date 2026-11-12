//! Paged objects (`toyos_abi::syscall::SYS_PAGER_CREATE` and its calls): a
//! pager's memory, one object per file, which its holders read through the
//! kernel and the pager fills on request.
//!
//! A [`Pager`] is a process's one, made only by a process started as a pager;
//! it names its objects by id, makes holders of them and reads ready while a
//! [`Notice`] is owed. A [`Paged`] holder reads one object, waiting for absent
//! pages, writes it where minted to, against the credit its pager granted,
//! and may be duplicated and sent; a pager's own process is refused a
//! holder's calls.

use core::time::Duration;

use toyos_abi::syscall::{
    self, PagedArgs, PagedOp, PagedRange, SyscallError, NOTICE_DIRTIED, NOTICE_NO_HOLDERS, NOTICE_REQUESTED, PAGED_DIRTY,
    PAGED_WRITTEN,
};

pub use toyos_abi::syscall::{
    PagedNotice, PagedStat, PagedStatus, PAGED_HOLDER_END, PAGED_HOLDER_PAGES, PAGED_RANGE_PAGES, PAGED_RECORD,
};

use crate::shm::Bytes;
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

/// The bytes of one page of an object.
pub const PAGE: u64 = syscall::PAGE_SIZE as u64;

/// Why a pager's call did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The first page is absent: supply it.
    Absent,
    /// Every page the pager may hold is held, or its job cannot be charged
    /// another object: let one go first.
    NoRoom,
    /// The pager's dirty ceiling is reached: write back first.
    DirtyFull,
    /// The object was released, or the pager ended.
    Gone,
    /// Anything else, which names a bug in the caller.
    Kernel(SyscallError),
}

impl Refused {
    /// `e`, where `absent` is the call's word for an absent first page.
    fn of(e: SyscallError, absent: Option<SyscallError>) -> Self {
        match e {
            e if Some(e) == absent => Self::Absent,
            SyscallError::ResourceExhausted => Self::NoRoom,
            SyscallError::WouldBlock => Self::DirtyFull,
            SyscallError::Gone => Self::Gone,
            e => Self::Kernel(e),
        }
    }
}

/// What a pager's object asks of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A reader waits for `count` pages from `first`.
    Requested { object: u64, first: u64, count: u64 },
    /// The last holder handle went.
    NoHolders { object: u64 },
    /// A holder dirtied pages, which [`Pager::take_dirtied`] names.
    Dirtied { object: u64 },
}

/// This process's one pager.
pub struct Pager(OwnedHandle);

impl Pager {
    /// `capacity` pages for every object, `dirty` of them dirty at once, its
    /// objects each charged as it is made; a reader waiting past `report` is
    /// reported overdue, and waits on.
    pub fn create(capacity: u64, dirty: u64, report: Duration) -> Result<Self, SyscallError> {
        let report = u64::try_from(report.as_nanos()).unwrap_or(u64::MAX);
        let raw = syscall::pager_create(capacity, dirty, report)?;
        // SAFETY: the kernel just installed it in this process's table.
        Ok(Self(unsafe { OwnedHandle::from_raw(raw) }))
    }

    /// The notices owed, as many as `out` holds; the rest wait for the next call.
    pub fn notices<'a>(&self, out: &'a mut [PagedNotice]) -> Result<impl Iterator<Item = Notice> + 'a, SyscallError> {
        let n = syscall::paged_notices(self.0.raw(), out)?;
        Ok(out[..n].iter().map(|n| match n.kind {
            NOTICE_REQUESTED => Notice::Requested { object: n.object, first: n.first, count: n.count },
            NOTICE_NO_HOLDERS => Notice::NoHolders { object: n.object },
            NOTICE_DIRTIED => Notice::Dirtied { object: n.object },
            kind => panic!("paged: the kernel answered a notice of kind {kind}"),
        }))
    }

    /// An object of `size` bytes, every page absent: its id.
    pub fn object(&self, size: u64) -> Result<u64, Refused> {
        syscall::paged_create(self.0.raw(), size).map_err(|e| Refused::of(e, None))
    }

    /// Anonymous bytes, retained until shrunk or released. No backing can
    /// refill a present page, so withdrawal and writeback cannot discard it.
    pub fn anonymous(&self, size: u64) -> Result<u64, Refused> {
        syscall::paged_anonymous(self.0.raw(), size).map_err(|e| Refused::of(e, None))
    }

    /// Unused slots in this pager's page quota, without reserving them or
    /// promising that physical memory will be available for them.
    pub fn room(&self) -> Result<u64, Refused> {
        self.op(PagedOp::Room, PagedArgs::default(), None)
    }

    /// A holder handle on object `id`, which writes it where `writes`, to send.
    pub fn holder(&self, id: u64, writes: bool) -> Result<OwnedHandle, SyscallError> {
        let raw = syscall::paged_holder(self.0.raw(), id, writes)?;
        // SAFETY: the kernel just installed it in this process's table.
        Ok(unsafe { OwnedHandle::from_raw(raw) })
    }

    fn op(&self, op: PagedOp, args: PagedArgs, absent: Option<SyscallError>) -> Result<u64, Refused> {
        syscall::paged_op(self.0.raw(), op, &args).map_err(|e| Refused::of(e, absent))
    }

    /// Up to `out.len()` bytes of object `id` at `offset`, stopping at the
    /// size or the first absent page after the first; never waits. `out` may
    /// be a window another process shares.
    pub fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        let args = PagedArgs { object: id, offset, len: out.len() as u64, buf: out.as_mut_ptr() as u64, ..PagedArgs::default() };
        self.op(PagedOp::Read, args, Some(SyscallError::WouldBlock)).map(|n| n as usize)
    }

    /// `bytes` into fresh pages of object `id` from page-aligned `offset`, at
    /// most [`PAGED_RANGE_PAGES`] of them: clean, as the file holds them, or
    /// dirty, which grows the size to their end. Returns the pages installed.
    /// Clean supply skips present pages; dirty supply stops at the first
    /// present or refused page, so its count is the copied prefix.
    pub fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        let flags = if dirty { PAGED_DIRTY } else { 0 };
        let args = PagedArgs { object: id, offset, len: bytes.len() as u64, buf: bytes.as_ptr() as u64, flags };
        self.op(PagedOp::Supply, args, None)
    }

    /// `bytes` into the present pages of object `id` from `offset`, dirty,
    /// under [`Self::hold`], the size growing as they land: how many were
    /// written before the first absent page.
    pub fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        let args = PagedArgs { object: id, offset, len: bytes.len() as u64, buf: bytes.as_ptr() as u64, flags: 0 };
        self.op(PagedOp::Write, args, Some(SyscallError::NotFound)).map(|n| n as usize)
    }

    /// Hold object `id`'s writes until [`Self::unhold`]: no other write
    /// lands and no holder reads half of one meanwhile, so this pager's
    /// writes, truncations and reads in between are each one whole, however
    /// many calls they take. Every [`Self::write`], dirty [`Self::supply`]
    /// and [`Self::resize`] is made under it. The size, of this moment.
    pub fn hold(&self, id: u64) -> Result<u64, Refused> {
        self.op(PagedOp::Hold, PagedArgs { object: id, ..PagedArgs::default() }, None)
    }

    /// Give back what [`Self::hold`] held.
    pub fn unhold(&self, id: u64) -> Result<(), Refused> {
        self.op(PagedOp::Unhold, PagedArgs { object: id, ..PagedArgs::default() }, None).map(drop)
    }

    /// Drop the clean pages of `count` from page `first` that nothing holds:
    /// how many went.
    pub fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused> {
        self.op(PagedOp::Withdraw, PagedArgs { object: id, offset: first, len: count, ..PagedArgs::default() }, None)
    }

    /// Every read of `count` pages from `first` answers `Io` until supplied again.
    pub fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused> {
        self.op(PagedOp::Fail, PagedArgs { object: id, offset: first, len: count, ..PagedArgs::default() }, None).map(drop)
    }

    /// Set the size, under [`Self::hold`]: a shrink drops every page past it,
    /// dirty or not.
    pub fn resize(&self, id: u64, size: u64) -> Result<(), Refused> {
        self.op(PagedOp::Resize, PagedArgs { object: id, offset: size, ..PagedArgs::default() }, None).map(drop)
    }

    /// Begin writing back `count` pages from `first`: a bit per page begun.
    pub fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        let args = PagedArgs { object: id, offset: first, len: count, buf: bits.as_mut_ptr() as u64, ..PagedArgs::default() };
        self.op(PagedOp::WritebackBegin, args, None)?;
        Ok(bits)
    }

    /// End a range begun or its leading part, `written` to the device or not:
    /// a bit per page still dirty. Any remaining suffix stays under write-back.
    pub fn writeback_end(&self, id: u64, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        let flags = if written { PAGED_WRITTEN } else { 0 };
        let args = PagedArgs { object: id, offset: first, len: count, buf: bits.as_mut_ptr() as u64, flags };
        self.op(PagedOp::WritebackEnd, args, None)?;
        Ok(bits)
    }

    /// Let object `id` go: its holders answer `Gone`.
    pub fn release(&self, id: u64) -> Result<(), Refused> {
        self.op(PagedOp::Release, PagedArgs { object: id, ..PagedArgs::default() }, None).map(drop)
    }

    /// Let holders of object `id` turn `pages` more pages dirty, whose
    /// write-back this pager holds room for: the credit now.
    pub fn grant(&self, id: u64, pages: u64) -> Result<u64, Refused> {
        self.op(PagedOp::Grant, PagedArgs { object: id, len: pages, ..PagedArgs::default() }, None)
    }

    /// Take back the credit of object `id` its holders have not spent: how much.
    pub fn take_back(&self, id: u64) -> Result<u64, Refused> {
        self.op(PagedOp::TakeBack, PagedArgs { object: id, ..PagedArgs::default() }, None)
    }

    /// Object `id`'s record, where given, and modification time, where given,
    /// which its holders read from now on.
    pub fn set_record(&self, id: u64, record: Option<&[u8; PAGED_RECORD]>, mtime: Option<u64>) -> Result<(), Refused> {
        let (buf, len) = record.map_or((0, 0), |r| (r.as_ptr() as u64, PAGED_RECORD as u64));
        let args = PagedArgs { object: id, offset: mtime.unwrap_or(u64::MAX), len, buf, flags: 0 };
        self.op(PagedOp::SetRecord, args, None).map(drop)
    }

    /// Object `id`'s size and modification time, which its holders' writes
    /// move, and its credit left.
    pub fn status(&self, id: u64) -> Result<PagedStatus, Refused> {
        let mut status = PagedStatus::default();
        let args = PagedArgs { object: id, buf: &mut status as *mut PagedStatus as u64, ..PagedArgs::default() };
        self.op(PagedOp::Status, args, None)?;
        Ok(status)
    }

    /// The pages of object `id` its holders dirtied since the last call, as
    /// `(first, count)`: the next a holder dirties owes a notice again.
    pub fn take_dirtied(&self, id: u64) -> Result<Option<(u64, u64)>, Refused> {
        let mut range = PagedRange::default();
        let args = PagedArgs { object: id, buf: &mut range as *mut PagedRange as u64, ..PagedArgs::default() };
        self.op(PagedOp::TakeDirtied, args, None)?;
        Ok((range.count > 0).then_some((range.first, range.count)))
    }
}

impl AsHandle for Pager {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

/// Why a received handle is no holder this process can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotAHolder {
    /// A holder this process may not read: it is a pager, which the kernel
    /// refuses a holder's read, or the holder carries no `READ`.
    Refused,
    /// A holder whose object or pager has ended since it was sent.
    Ended,
    /// Some other object.
    Other,
}

/// A holder's handle on a paged object.
pub struct Paged(OwnedHandle);

impl Paged {
    /// A handle a file server sent as a holder, once the kernel has answered
    /// a read of nothing through it as a holder answers one.
    pub fn received(handle: OwnedHandle) -> Result<Self, NotAHolder> {
        match syscall::paged_read(handle.raw(), &mut [], 0) {
            Ok(0) => Ok(Self(handle)),
            Err(SyscallError::PermissionDenied) => Err(NotAHolder::Refused),
            Err(SyscallError::Gone) => Err(NotAHolder::Ended),
            _ => Err(NotAHolder::Other),
        }
    }

    /// Up to `out.len()` bytes at `offset`, 0 at or past the size, waiting for
    /// every absent page until it is supplied, failed (`Io`, or a short count
    /// after some bytes), cut off by a shrink, or its object or pager ends
    /// (`Gone`). It reads each write whole or not at all, waiting out one
    /// under way; one that has met three is `WouldBlock`: ask the pager,
    /// whose read holds the writes off.
    pub fn read(&self, out: &mut [u8], offset: u64) -> Result<usize, SyscallError> {
        syscall::paged_read(self.0.raw(), out, offset)
    }

    /// `bytes` at `offset`, one write whole: the bytes written. `WouldBlock`
    /// with nothing written where another write is under way, the write spans
    /// more than [`PAGED_HOLDER_PAGES`] pages or ends past
    /// [`PAGED_HOLDER_END`], or the pager has to act first —
    /// supply a page, grant credit, write back — and the holder asks it to
    /// write instead; `Busy` while a program runs from the object;
    /// `PermissionDenied` for a holder not minted to write.
    pub fn write(&self, bytes: &[u8], offset: u64) -> Result<usize, SyscallError> {
        syscall::paged_write(self.0.raw(), bytes, offset)
    }

    /// All of `bytes` at the size, which the kernel reads as it admits them,
    /// as [`Self::write`] writes: the size after them.
    pub fn append(&self, bytes: &[u8]) -> Result<u64, SyscallError> {
        syscall::paged_append(self.0.raw(), bytes)
    }

    /// The object's size, modification time and its pager's record, of one moment.
    pub fn stat(&self) -> Result<PagedStat, SyscallError> {
        syscall::paged_stat(self.0.raw())
    }
}
