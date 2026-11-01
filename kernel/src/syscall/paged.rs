//! Paged objects' calls (`object::paged`). Each resolves its handle and
//! clones the object out of the table first: a read parks, and nothing here
//! holds the table across a copy. Every byte range and size a pager names is
//! bounded here, before the object sees it; a page range is the object's to
//! clip.

use alloc::sync::Arc;

use kernel::paged::within_reach;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{PagedArgs, PagedOp, SyscallError, PAGED_DIRTY, PAGED_RANGE_PAGES, PAGED_WRITTEN};

use crate::object::paged::{PagerObject, Unmade};
use crate::object::{ops, KObjectRef};
use crate::process::{self, PagerRole};
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;

pub(super) fn sys_pager_create(capacity: u64, dirty: u64, objects: u64, report_ns: u64) -> u64 {
    let claimed = process::with_process_data(|data| match data.pager {
        PagerRole::May => {
            data.pager = PagerRole::Made;
            Ok(())
        }
        PagerRole::Made => Err(SyscallError::AlreadyExists),
        PagerRole::None => Err(SyscallError::PermissionDenied),
    });
    if let Err(e) = claimed {
        return e.to_u64();
    }
    let made = PagerObject::create(capacity, dirty, objects, report_ns).map_err(|why| match why {
        Unmade::Invalid => SyscallError::InvalidArgument,
        Unmade::Exhausted => SyscallError::ResourceExhausted,
    });
    let installed = made.and_then(|pager| {
        process::with_process_data(|data| ops::install(&mut data.handles, KObjectRef::Pager(pager)))
    });
    match installed {
        Ok(h) => u64::from(h.0),
        Err(e) => {
            process::with_process_data(|data| data.pager = PagerRole::May);
            e.to_u64()
        }
    }
}

fn pager(h: RawHandle, rights: Rights) -> Result<Arc<PagerObject>, u64> {
    process::with_process_data(|data| data.handles.get::<PagerObject>(h, rights)).map_err(|e| e.refuse())
}

pub(super) fn sys_paged_create(h: RawHandle, size: u64) -> u64 {
    match pager(h, Rights::WRITE) {
        Ok(pager) => pager.add(size),
        Err(refused) => refused,
    }
}

pub(super) fn sys_paged_holder(h: RawHandle, object: u64) -> u64 {
    let pager = match pager(h, Rights::WRITE) {
        Ok(pager) => pager,
        Err(refused) => return refused,
    };
    let holder = match pager.holder(object) {
        Ok(holder) => holder,
        Err(e) => return e.to_u64(),
    };
    // Counted under the same hold as its install: no close can come between.
    let installed = process::with_process_data(|data| {
        let h = ops::install(&mut data.handles, KObjectRef::Paged(Arc::clone(&holder)))?;
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
    let resolved = process::with_process_data(|data| {
        let KObjectRef::Paged(holder) = data.handles.get_ref(h, Rights::NONE)? else {
            return Ok(Err(SyscallError::InvalidArgument));
        };
        let holder = Arc::clone(holder);
        if !data.handles.rights_of(h)?.contains(Rights::READ) {
            return Ok(Err(SyscallError::PermissionDenied));
        }
        Ok(if data.pager.is_pager() { Err(SyscallError::PermissionDenied) } else { Ok(holder) })
    });
    match resolved {
        Ok(Ok(holder)) => holder.read(out, offset),
        Ok(Err(e)) => e.to_u64(),
        Err(e) => crate::object::HandleError::refuse(e),
    }
}

pub(super) fn sys_paged_op(ctx: &SyscallContext<'_>, h: RawHandle, op: u64, args: UserAddr) -> u64 {
    let Ok(args) = ctx.copy_in::<PagedArgs>(args) else { return SyscallError::BadAddress.to_u64() };
    let pager = match pager(h, Rights::WRITE) {
        Ok(pager) => pager,
        Err(refused) => return refused,
    };
    let range_ok = |count: u64| (1..=PAGED_RANGE_PAGES).contains(&count) && args.offset.checked_add(count).is_some();
    let id = args.object;
    match op {
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
        x if x == PagedOp::EndWrite as u64 => pager.end_write(id),
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
