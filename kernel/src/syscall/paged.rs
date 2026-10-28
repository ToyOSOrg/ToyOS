//! Paged objects' calls (`object::paged`). Each resolves its handle and
//! clones the object out of the table first: a read parks, and nothing here
//! holds the table across a copy.

use alloc::sync::Arc;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{PagedArgs, PagedOp, SyscallError, PAGED_DIRTY, PAGED_RANGE_PAGES, PAGED_WRITTEN};

use crate::object::paged::{PagedHolderObject, PagedOwnerObject, PagerObject, Unmade};
use crate::object::{ops, KObjectRef};
use crate::process::{self, PagerRole};
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;

pub(super) fn sys_pager_create(capacity: u64, dirty: u64, report_ns: u64) -> u64 {
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
    let made = PagerObject::create(capacity, dirty, report_ns).map_err(|why| match why {
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

pub(super) fn sys_paged_create(pager: RawHandle, size: u64, cookie: u64) -> u64 {
    let pager = match process::with_process_data(|data| data.handles.get::<PagerObject>(pager, Rights::READ)) {
        Ok(pager) => pager,
        Err(e) => return e.refuse(),
    };
    match PagedOwnerObject::create(&pager, size, cookie) {
        Ok(owner) => match process::with_process_data(|data| ops::install(&mut data.handles, KObjectRef::PagedOwner(owner))) {
            Ok(h) => u64::from(h.0),
            Err(e) => e.to_u64(),
        },
        Err(e) => e.to_u64(),
    }
}

pub(super) fn sys_paged_holder(owner: RawHandle) -> u64 {
    let owner = match process::with_process_data(|data| data.handles.get::<PagedOwnerObject>(owner, Rights::READ)) {
        Ok(owner) => owner,
        Err(e) => return e.refuse(),
    };
    let holder = match owner.holder() {
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

enum Readable {
    Owner(Arc<PagedOwnerObject>),
    Holder(Arc<PagedHolderObject>),
}

/// A holder's read waits, so a pager's process is refused one: it would wait
/// on itself or on another pager.
pub(super) fn sys_paged_read(h: RawHandle, out: &mut UserBytesMut, offset: u64) -> u64 {
    let resolved = process::with_process_data(|data| {
        let object = data.handles.get_ref(h, Rights::READ)?;
        Ok(match object {
            KObjectRef::PagedOwner(o) => Ok(Readable::Owner(Arc::clone(o))),
            KObjectRef::Paged(p) if data.pager.is_pager() => Err(SyscallError::PermissionDenied),
            KObjectRef::Paged(p) => Ok(Readable::Holder(Arc::clone(p))),
            _ => Err(SyscallError::NotSupported),
        })
    });
    match resolved {
        Ok(Ok(Readable::Owner(o))) => o.read(out, offset),
        Ok(Ok(Readable::Holder(p))) => p.read(out, offset),
        Ok(Err(e)) => e.to_u64(),
        Err(e) => crate::object::HandleError::refuse(e),
    }
}

pub(super) fn sys_paged_op(ctx: &SyscallContext<'_>, owner: RawHandle, op: u64, args: UserAddr) -> u64 {
    let Ok(args) = ctx.copy_in::<PagedArgs>(args) else { return SyscallError::BadAddress.to_u64() };
    let owner = match process::with_process_data(|data| data.handles.get::<PagedOwnerObject>(owner, Rights::WRITE)) {
        Ok(owner) => owner,
        Err(e) => return e.refuse(),
    };
    let range_ok = |count: u64| (1..=PAGED_RANGE_PAGES).contains(&count);
    match op {
        x if x == PagedOp::Supply as u64 || x == PagedOp::Write as u64 => {
            if args.flags & !PAGED_DIRTY != 0 || (x == PagedOp::Write as u64 && args.flags != 0) {
                return SyscallError::InvalidArgument.to_u64();
            }
            // One supply or write is at most a write-back range's bytes.
            if args.len > PAGED_RANGE_PAGES * kernel::paged::PAGE {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(src) = ctx.user_bytes(UserAddr::new(args.buf), args.len) else { return SyscallError::BadAddress.to_u64() };
            match x == PagedOp::Supply as u64 {
                true => owner.supply(args.offset, &src, args.flags & PAGED_DIRTY != 0),
                false => owner.write(args.offset, &src),
            }
        }
        x if x == PagedOp::Withdraw as u64 => owner.withdraw(args.offset, args.len),
        x if x == PagedOp::Fail as u64 => owner.fail(args.offset, args.len),
        x if x == PagedOp::Resize as u64 => owner.resize(args.offset),
        x if x == PagedOp::WritebackBegin as u64 || x == PagedOp::WritebackEnd as u64 => {
            let end = (x == PagedOp::WritebackEnd as u64).then_some(args.flags & PAGED_WRITTEN != 0);
            if args.flags & !PAGED_WRITTEN != 0 || (end.is_none() && args.flags != 0) || !range_ok(args.len) {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(mut out) = ctx.user_bytes_mut(UserAddr::new(args.buf), 64) else { return SyscallError::BadAddress.to_u64() };
            owner.writeback(args.offset, args.len, end, &mut out)
        }
        _ => SyscallError::InvalidArgument.to_u64(),
    }
}

pub(super) fn sys_paged_notices(pager: RawHandle, out: &mut UserBytesMut) -> u64 {
    match process::with_process_data(|data| data.handles.get::<PagerObject>(pager, Rights::READ)) {
        Ok(pager) => pager.notices(out),
        Err(e) => e.refuse(),
    }
}
