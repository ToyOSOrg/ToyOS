//! Paged objects' calls (`object::paged`). Each resolves its handle and
//! clones the object out of the table first: a read parks, and nothing here
//! holds the table across a copy. Every byte range and size a pager names is
//! bounded here, before the object sees it; a page range is the object's to
//! clip.

use alloc::sync::Arc;

use kernel::paged::within_reach;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{
    PagedArgs, PagedOp, SyscallError, PAGED_DIRTY, PAGED_HOLDER_WRITE, PAGED_RANGE_PAGES, PAGED_RECORD, PAGED_WRITTEN,
};

use crate::object::paged::{PagedHolderObject, PagerObject, Unmade};
use crate::object::{ops, HandleError, HandleTable, KObjectRef};
use crate::process;
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;

const _: () = assert!(kernel::paged::HOLDER_PAGES == toyos_abi::syscall::PAGED_HOLDER_PAGES);
const _: () = assert!(kernel::paged::HOLDER_END == toyos_abi::syscall::PAGED_HOLDER_END);

pub(super) fn sys_pager_create(capacity: u64, dirty: u64, report_ns: u64) -> u64 {
    let handles = process::handles();
    if let Err(e) = handles.claim_pager() {
        return e.to_u64();
    }
    let made = PagerObject::create(capacity, dirty, report_ns).map_err(|why| match why {
        Unmade::Invalid => SyscallError::InvalidArgument,
        Unmade::Exhausted => SyscallError::ResourceExhausted,
    });
    let installed = made.and_then(|pager| ops::install(&mut handles.lock(), KObjectRef::Pager(pager)));
    match installed {
        Ok(h) => u64::from(h.0),
        Err(e) => {
            handles.unclaim_pager();
            e.to_u64()
        }
    }
}

fn pager(h: RawHandle, rights: Rights) -> Result<Arc<PagerObject>, u64> {
    process::with_handles(|handles| handles.get::<PagerObject>(h, rights)).map_err(|e| e.refuse())
}

pub(super) fn sys_paged_create(h: RawHandle, size: u64, anonymous: bool) -> u64 {
    match pager(h, Rights::WRITE) {
        Ok(pager) => pager.add(size, anonymous),
        Err(refused) => refused,
    }
}

pub(super) fn sys_paged_holder(h: RawHandle, object: u64, flags: u64) -> u64 {
    if flags & !PAGED_HOLDER_WRITE != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let pager = match pager(h, Rights::WRITE) {
        Ok(pager) => pager,
        Err(refused) => return refused,
    };
    let holder = match pager.holder(object, flags & PAGED_HOLDER_WRITE != 0) {
        Ok(holder) => holder,
        Err(e) => return e.to_u64(),
    };
    // Counted under the same hold as its install: no close can come between.
    let installed = process::with_handles(|handles| {
        let h = ops::install(handles, KObjectRef::Paged(Arc::clone(&holder)))?;
        holder.held();
        Ok::<_, SyscallError>(h)
    });
    match installed {
        Ok(h) => u64::from(h.0),
        Err(e) => e.to_u64(),
    }
}

/// A holder's read waits, so a pager's process is refused one: it would wait
/// on itself or on another pager. The handle is often a peer's, sent as a
/// holder, so another kind is refused `InvalidArgument`, never a fault.
pub(super) fn sys_paged_read(h: RawHandle, out: &mut UserBytesMut, offset: u64) -> u64 {
    match holder(h, Rights::READ) {
        Ok(holder) => holder.read(out, offset),
        Err(refused) => refused,
    }
}

/// A holder's write of `len` bytes at `offset`, whole or refused: the kernel
/// object checks the span ([`kernel::paged::holder_span`]).
pub(super) fn sys_paged_write(ctx: &SyscallContext<'_>, h: RawHandle, buf: UserAddr, len: u64, offset: u64) -> u64 {
    holder_write(ctx, h, buf, len, Some(offset))
}

/// A holder's write of `len` bytes at the object's size, which it reads under
/// its own lock, whole or refused.
pub(super) fn sys_paged_append(ctx: &SyscallContext<'_>, h: RawHandle, buf: UserAddr, len: u64) -> u64 {
    holder_write(ctx, h, buf, len, None)
}

fn holder_write(ctx: &SyscallContext<'_>, h: RawHandle, buf: UserAddr, len: u64, at: Option<u64>) -> u64 {
    let Some(src) = ctx.user_bytes(buf, len) else { return SyscallError::BadAddress.to_u64() };
    match holder(h, Rights::WRITE) {
        Ok(holder) => holder.write(&src, at),
        Err(refused) => refused,
    }
}

pub(super) fn sys_paged_stat(ctx: &SyscallContext<'_>, h: RawHandle, out: UserAddr) -> u64 {
    let stat = match holder(h, Rights::READ) {
        Ok(holder) => holder.stat(),
        Err(refused) => return refused,
    };
    match stat.and_then(|stat| ctx.copy_out(out, &stat)) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

pub(super) fn holder(h: RawHandle, right: Rights) -> Result<Arc<PagedHolderObject>, u64> {
    let handles = process::handles();
    let resolved = holder_with(&handles.lock(), h, right, handles.is_pager());
    match resolved {
        Ok(Ok(holder)) => Ok(holder),
        Ok(Err(e)) => Err(e.to_u64()),
        Err(e) => Err(HandleError::refuse(e)),
    }
}

/// The holder `h` names in `table`, if it carries `right` and its process is
/// no `pager`.
fn holder_with(
    table: &HandleTable,
    h: RawHandle,
    right: Rights,
    pager: bool,
) -> Result<Result<Arc<PagedHolderObject>, SyscallError>, HandleError> {
    let KObjectRef::Paged(holder) = table.get_ref(h, Rights::NONE)? else {
        return Ok(Err(SyscallError::InvalidArgument));
    };
    if !table.rights_of(h)?.contains(right) || pager {
        return Ok(Err(SyscallError::PermissionDenied));
    }
    Ok(Ok(Arc::clone(holder)))
}

pub(super) fn sys_paged_op(ctx: &SyscallContext<'_>, h: RawHandle, op: u64, args: UserAddr) -> u64 {
    let args = match ctx.copy_in::<PagedArgs>(args) { Ok(args) => args, Err(e) => return e.to_u64() };
    let pager = match pager(h, Rights::WRITE) {
        Ok(pager) => pager,
        Err(refused) => return refused,
    };
    let range_ok = |count: u64| (1..=PAGED_RANGE_PAGES).contains(&count) && args.offset.checked_add(count).is_some();
    let id = args.object;
    match op {
        x if x == PagedOp::Room as u64 => pager.room(),
        x if x == PagedOp::Read as u64 => {
            if args.flags != 0 || !within_reach(args.offset, args.len) {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(mut out) = ctx.user_bytes_mut(UserAddr::new(args.buf), args.len) else { return SyscallError::BadAddress.to_u64() };
            pager.owner_read(id, &mut out, args.offset)
        }
        x if x == PagedOp::Supply as u64 || x == PagedOp::Write as u64 => {
            if args.flags & !PAGED_DIRTY != 0 || (x == PagedOp::Write as u64 && args.flags != 0) {
                return SyscallError::InvalidArgument.to_u64();
            }
            // One supply or write is at most a write-back range's bytes, and
            // ends at or below the last page.
            if args.len > PAGED_RANGE_PAGES * kernel::paged::PAGE || !within_reach(args.offset, args.len) {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(src) = ctx.user_bytes(UserAddr::new(args.buf), args.len) else { return SyscallError::BadAddress.to_u64() };
            match x == PagedOp::Supply as u64 {
                true => pager.supply(id, args.offset, &src, args.flags & PAGED_DIRTY != 0),
                false => pager.write(id, args.offset, &src),
            }
        }
        x if x == PagedOp::Withdraw as u64 => pager.withdraw(id, args.offset, args.len),
        x if x == PagedOp::Fail as u64 => pager.fail(id, args.offset, args.len),
        x if x == PagedOp::Resize as u64 => pager.resize(id, args.offset),
        x if x == PagedOp::Release as u64 => pager.release(id),
        x if x == PagedOp::Hold as u64 => pager.hold(id),
        x if x == PagedOp::Unhold as u64 => pager.unhold(id),
        x if x == PagedOp::Grant as u64 => answered(pager.grant(id, args.len)),
        x if x == PagedOp::TakeBack as u64 => answered(pager.take_back(id)),
        x if x == PagedOp::SetRecord as u64 => {
            let record = match args.len {
                0 => None,
                len if len == PAGED_RECORD as u64 => match ctx.copy_in::<[u8; PAGED_RECORD]>(UserAddr::new(args.buf)) {
                    Ok(record) => Some(record),
                    Err(e) => return e.to_u64(),
                },
                _ => return SyscallError::InvalidArgument.to_u64(),
            };
            let mtime = (args.offset != u64::MAX).then_some(args.offset);
            answered(pager.set_record(id, record, mtime).map(|()| 0))
        }
        x if x == PagedOp::Status as u64 => {
            answered(pager.status(id).and_then(|status| ctx.copy_out(UserAddr::new(args.buf), &status)).map(|()| 0))
        }
        x if x == PagedOp::TakeDirtied as u64 => {
            answered(pager.take_dirtied(id).and_then(|range| ctx.copy_out(UserAddr::new(args.buf), &range)).map(|()| 0))
        }
        x if x == PagedOp::WritebackBegin as u64 || x == PagedOp::WritebackEnd as u64 => {
            let end = (x == PagedOp::WritebackEnd as u64).then_some(args.flags & PAGED_WRITTEN != 0);
            if args.flags & !PAGED_WRITTEN != 0 || (end.is_none() && args.flags != 0) || !range_ok(args.len) {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(mut out) = ctx.user_bytes_mut(UserAddr::new(args.buf), 64) else { return SyscallError::BadAddress.to_u64() };
            pager.writeback(id, args.offset, args.len, end, &mut out)
        }
        _ => SyscallError::InvalidArgument.to_u64(),
    }
}

pub(super) fn sys_paged_notices(h: RawHandle, out: &mut UserBytesMut) -> u64 {
    match pager(h, Rights::READ) {
        Ok(pager) => pager.notices(out),
        Err(refused) => refused,
    }
}

fn answered(answer: Result<u64, SyscallError>) -> u64 {
    answer.unwrap_or_else(SyscallError::to_u64)
}
