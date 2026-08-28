//! Handle-table access, resolved under the table's own guard. A refusal is
//! handed out of the guard so the caller can end the process after the lock
//! its own refusal needs is released.

use crate::object::{ops, KObjectRef};
use crate::process;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

/// Runs `f` on the object `h` names, holding the table's guard.
pub(super) fn with_object_ref<R>(
    h: RawHandle,
    need: Rights,
    f: impl FnOnce(&KObjectRef) -> R,
) -> Result<R, crate::object::HandleError> {
    process::with_process_data(|data| data.handles.get_ref(h, need).map(f))
}

/// Requires that `syscap` resolves to a `SysCap` holding exactly `need`.
pub(super) fn demand_syscap(syscap: RawHandle, need: Rights) -> Result<(), crate::object::HandleError> {
    process::with_process_data(|data| {
        data.handles
            .get::<crate::object::syscap::SysCap>(syscap, need)
            .map(|_| ())
    })
}

/// The same, for the calls whose answer is already a raw syscall word.
pub(super) fn with_object(h: RawHandle, need: Rights, f: impl FnOnce(&KObjectRef) -> u64) -> u64 {
    match with_object_ref(h, need, f) {
        Ok(v) => v,
        Err(e) => e.refuse(),
    }
}

pub(super) fn handle_result(r: Result<RawHandle, SyscallError>) -> u64 {
    match r {
        Ok(h) => h.0 as u64,
        Err(e) => e.to_u64(),
    }
}

/// Wakes nobody: `pipe::close_write`'s own zero-refcount path does that.
pub(super) fn sys_close(h: RawHandle) -> u64 {
    let result = process::with_process_data(|data| {
        ops::close(&mut data.handles, h, &mut data.pipe_maps)
    });
    match result {
        Ok(()) => 0,
        Err(e) => e.refuse(),
    }
}

/// A second handle to the same object, carrying no more than the first.
///
/// Device-claim exclusivity comes from `ops::initial_rights` withholding `Rights::DUP`, not a check in `sys_handle_dup`.
///
/// `want` is the wire form of `Option<Rights>` (`RIGHTS_UNCHANGED` = the source's own set), decoded here and nowhere else.
pub(super) fn sys_handle_dup(h: RawHandle, want: u64) -> u64 {
    let duplicated = process::with_process_data(|data| {
        let held = data.handles.rights_of(h)?;
        let rights = if want == RIGHTS_UNCHANGED {
            held
        } else {
            let bits = u32::try_from(want).map_err(|_| SyscallError::InvalidArgument)?;
            Rights::from_bits(bits).ok_or(SyscallError::InvalidArgument)?
        };
        Ok::<_, crate::object::Refusal>(data.handles.duplicate(h, rights)?)
    });
    match duplicated {
        Ok(new_h) => new_h.0 as u64,
        Err(e) => e.refuse(),
    }
}

/// A second handle to `old`, installed at `slot`.
///
/// `slot` is a slot, not a handle: a handle carries a generation this call has no business being told.
///
/// Displacing a handle wakes nobody: dropping the entry is what gives back the reference a wake is owed for.
pub(super) fn sys_dup2(old: RawHandle, slot: u64) -> u64 {
    let Ok(slot) = u16::try_from(slot) else {
        return SyscallError::ResourceExhausted.to_u64();
    };
    let result = process::with_process_data(|data| {
        let rights = data.handles.rights_of(old)?;
        let entry = data.handles.duplicate_entry(old, rights)?;
        data.handles
            .install_at(slot, entry)
            .map_err(|_| SyscallError::ResourceExhausted.into())
            .map(|(new_h, displaced)| (new_h, Displaced(displaced)))
    });
    match result {
        // Dropped here, outside the process lock: `HandleEntry`'s Drop can take
        // `vfs::lock()`, which every sibling thread's page-fault handler also nests under.
        Ok((new_h, displaced)) => {
            drop(displaced);
            new_h.0 as u64
        }
        Err(e) => crate::object::Refusal::refuse(e),
    }
}

/// A displaced handle's drop obligation, carried out of a closure by [`sys_dup2`].
// Never read: dropping it is the effect.
#[expect(dead_code)]
struct Displaced(Option<crate::object::HandleEntry>);
