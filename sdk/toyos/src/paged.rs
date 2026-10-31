//! Paged objects (`toyos_abi::syscall::SYS_PAGER_CREATE` and its calls): a
//! pager's memory, one object per file, which its holders read through the
//! kernel and the pager fills on request.
//!
//! A [`Pager`] is a process's one, made only by a process started as a pager;
//! it reads ready while a [`Notice`] is owed. A [`PagedOwner`] supplies,
//! writes, withdraws and writes back its object; neither duplicates nor
//! transfers. A [`Paged`] holder reads it, waiting for absent pages, and may
//! be duplicated and sent; a pager's own process is refused a holder's read.

use core::time::Duration;

use toyos_abi::syscall::{self, PagedArgs, PagedOp, SyscallError, NOTICE_NO_HOLDERS, NOTICE_REQUESTED, PAGED_DIRTY, PAGED_WRITTEN};

pub use toyos_abi::syscall::{PagedNotice, PAGED_RANGE_PAGES, PAGER_MAX_PAGES};

use crate::shm::Bytes;
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

pub const PAGE: u64 = 4096;

/// Why an owner's call did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The first page is absent: supply it.
    Absent,
    /// Every page the pager may hold is held: withdraw clean ones first.
    NoRoom,
    /// The pager's dirty ceiling is reached: write back first.
    DirtyFull,
    /// The pager ended, or the object's owner went.
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
    Requested { cookie: u64, first: u64, count: u64 },
    /// The last holder handle went.
    NoHolders { cookie: u64 },
}

/// This process's one pager.
pub struct Pager(OwnedHandle);

impl Pager {
    /// `capacity` pages for every object, `dirty` of them dirty at once; a
    /// reader waiting past `report` is reported overdue, and waits on.
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
            NOTICE_REQUESTED => Notice::Requested { cookie: n.cookie, first: n.first, count: n.count },
            NOTICE_NO_HOLDERS => Notice::NoHolders { cookie: n.cookie },
            kind => panic!("paged: the kernel answered a notice of kind {kind}"),
        }))
    }
}

impl AsHandle for Pager {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

/// The pager's handle on one of its objects.
pub struct PagedOwner(OwnedHandle);

impl PagedOwner {
    /// An object of `size` bytes, every page absent, which notices name by `cookie`.
    pub fn create(pager: &Pager, size: u64, cookie: u64) -> Result<Self, SyscallError> {
        let raw = syscall::paged_create(pager.0.raw(), size, cookie)?;
        // SAFETY: the kernel just installed it in this process's table.
        Ok(Self(unsafe { OwnedHandle::from_raw(raw) }))
    }

    /// A holder handle on the object, to send.
    pub fn holder(&self) -> Result<OwnedHandle, SyscallError> {
        let raw = syscall::paged_holder(self.0.raw())?;
        // SAFETY: the kernel just installed it in this process's table.
        Ok(unsafe { OwnedHandle::from_raw(raw) })
    }

    /// Up to `out.len()` bytes at `offset`, stopping at the size or the first
    /// absent page after the first; never waits. `out` may be a window
    /// another process shares.
    pub fn read(&self, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        // SAFETY: `out` is writable for its length while it is borrowed.
        unsafe { syscall::paged_read_into(self.0.raw(), out.as_mut_ptr(), out.len(), offset) }
            .map_err(|e| Refused::of(e, Some(SyscallError::WouldBlock)))
    }

    fn op(&self, op: PagedOp, args: PagedArgs) -> Result<u64, Refused> {
        syscall::paged_op(self.0.raw(), op, &args).map_err(|e| Refused::of(e, None))
    }

    /// `bytes` into fresh pages from page-aligned `offset`, at most
    /// [`PAGED_RANGE_PAGES`] of them: clean, as the file holds them, or dirty,
    /// which grows the size to their end. A page already present is kept.
    /// The pages installed.
    pub fn supply(&self, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        let args = PagedArgs { offset, len: bytes.len() as u64, buf: bytes.as_ptr() as u64, flags: if dirty { PAGED_DIRTY } else { 0 } };
        self.op(PagedOp::Supply, args)
    }

    /// `bytes` into the present pages from `offset`, dirty, the size growing
    /// as they land: how many were written before the first absent page.
    pub fn write(&self, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        let args = PagedArgs { offset, len: bytes.len() as u64, buf: bytes.as_ptr() as u64, flags: 0 };
        syscall::paged_op(self.0.raw(), PagedOp::Write, &args)
            .map(|n| n as usize)
            .map_err(|e| Refused::of(e, Some(SyscallError::NotFound)))
    }

    /// Drop the clean pages of `count` from page `first` that nothing holds:
    /// how many went.
    pub fn withdraw(&self, first: u64, count: u64) -> Result<u64, Refused> {
        self.op(PagedOp::Withdraw, PagedArgs { offset: first, len: count, ..PagedArgs::default() })
    }

    /// Every read of `count` pages from `first` answers `Io` until supplied again.
    pub fn fail(&self, first: u64, count: u64) -> Result<(), Refused> {
        self.op(PagedOp::Fail, PagedArgs { offset: first, len: count, ..PagedArgs::default() }).map(drop)
    }

    /// The size, as `ftruncate` sets it: a shrink drops every page past it,
    /// dirty or not.
    pub fn resize(&self, size: u64) -> Result<(), Refused> {
        self.op(PagedOp::Resize, PagedArgs { offset: size, ..PagedArgs::default() }).map(drop)
    }

    /// Begin writing back `count` pages from `first`: a bit per page begun.
    pub fn writeback_begin(&self, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        let args = PagedArgs { offset: first, len: count, buf: bits.as_mut_ptr() as u64, flags: 0 };
        self.op(PagedOp::WritebackBegin, args)?;
        Ok(bits)
    }

    /// End the write-back of exactly a range begun, `written` to the device
    /// or not: a bit per page still dirty.
    pub fn writeback_end(&self, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        let flags = if written { PAGED_WRITTEN } else { 0 };
        let args = PagedArgs { offset: first, len: count, buf: bits.as_mut_ptr() as u64, flags };
        self.op(PagedOp::WritebackEnd, args)?;
        Ok(bits)
    }
}

/// A holder's handle on a paged object.
pub struct Paged(OwnedHandle);

impl Paged {
    /// The holder handle a file server sent with a file it opened.
    pub fn received(handle: OwnedHandle) -> Self {
        Self(handle)
    }

    /// Up to `out.len()` bytes at `offset`, 0 at or past the size, waiting for
    /// every absent page until it is supplied, failed (`Io`, or a short count
    /// after some bytes), cut off by a shrink, or its pager ends (`Gone`).
    pub fn read(&self, out: &mut [u8], offset: u64) -> Result<usize, SyscallError> {
        syscall::paged_read(self.0.raw(), out, offset)
    }
}
