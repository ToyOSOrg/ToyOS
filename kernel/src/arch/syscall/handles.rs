//! The handle table as the ABI reaches it: the two shapes every handle-taking
//! call is built out of, and the calls that are about a handle rather than
//! about what it names.
//!
//! **A refusal is answered outside the table's own guard, always.** Three of
//! `HandleError`'s five kinds end the caller, and ending it takes the same
//! non-reentrant lock the closure that resolved the handle was running under —
//! so [`with_object_ref`] hands the error *out* and every caller refuses after
//! the lock is gone. That rule is why these are functions and not a macro:
//! there is one place where the guard ends and it is visible.

use crate::object::{ops, KObjectRef};
use crate::process;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

/// Run `f` on the object a handle names, under the table's own guard.
///
/// The one shape every handle-taking syscall uses. `f` runs while the process
/// data is held, exactly where the descriptor dispatch used to run, so nothing
/// clones an `Arc` out for a call that is over before the guard is.
pub(super) fn with_object_ref<R>(
    h: RawHandle,
    need: Rights,
    f: impl FnOnce(&KObjectRef) -> R,
) -> Result<R, crate::object::HandleError> {
    process::with_process_data(|data| data.handles.get_ref(h, need).map(f))
}

/// Demand that `syscap` resolves to a `SysCap` carrying `need`, and nothing more.
///
/// The prologue every authority-bearing syscall shares — `SYS_PROCESS_OPEN`,
/// `SYS_DEVICE_CLAIM`, `SYS_RT_ENTER`, `SYS_LOG_READ`, `SYS_SHUTDOWN` and the
/// roster half of `SYS_SYSINFO`: resolve the handle, require the one right, and
/// hand the error *out* of the table's guard so the caller refuses after the
/// lock is gone — `HandleError::refuse` may take the process down and needs that
/// lock itself. The resolved cap is discarded; the bit is the whole of the
/// question. A caller with an ordering constraint — `sys_sysinfo` demands before
/// it takes the process table lock — keeps that in the caller.
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

/// **Closing a handle wakes nobody, and that is the whole of it.**
///
/// A handle to a pipe end is not the end. `pipe::close_write` decrements the
/// reference count and wakes readers when it reaches *zero* — the one place
/// that knows whether the writer is gone — and the release that gets there runs
/// off this call's own zero-handle drain. A second wake here fired on *every*
/// close, so a pipe with a live writer and no bytes in it was announced
/// readable; a one-shot inbox watch consumed on that never fires again.
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
/// `PermissionDenied` is the answer for a device claim: it is the one object
/// that admits a single handle, and `ops::initial_rights` says so by
/// withholding `Rights::DUP` — exclusivity is a property of the type rather
/// than of a check here. Before that, `dup` handed back a claim's exclusivity
/// while leaving the caller a working handle.
///
/// `want` is the wire form of `Option<Rights>` — [`RIGHTS_UNCHANGED`] for the
/// source's own set — decoded here and nowhere else. A set with a bit no right
/// uses is a caller with a bug, and so is one the source does not hold: rights
/// only shrink, and the refusal names which.
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

/// A second handle to the same object, at a slot the caller picks.
///
/// The second argument is a **slot**, not a handle: a handle carries a
/// generation this call has no business being told, and the one it hands back
/// is the slot's own. Whatever was at that slot is closed first, and the slot's
/// generation moves — so an older handle to it is `Stale` rather than a name
/// for whatever landed there.
///
/// Displacing a handle wakes nobody, for [`sys_close`]'s reason: the reference
/// the entry held is what a wake is owed for, and dropping it is what gives it
/// back.
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
        // **Dropped here, and `install_at` is `#[must_use]` to say so.** A
        // `File` is an `immediate` row, so a `dup2` over the last handle to a
        // modified file runs `vfs::lock()` and a device round trip in this
        // statement. Inside the closure that would happen holding the process's
        // own lock — the one every sibling thread's page-fault handler takes —
        // four ticket spinlocks deep, on a path userland reaches with one
        // syscall.
        Ok((new_h, displaced)) => {
            drop(displaced);
            new_h.0 as u64
        }
        Err(e) => crate::object::Refusal::refuse(e),
    }
}

/// A handle a call displaced, on its way out of the guard that displaced it.
///
/// It exists to make the obligation survive a `?`: a bare `Option<HandleEntry>`
/// carried out of a closure is easy to drop at the wrong statement, and
/// `install_at`'s contract is about *where* the decrement happens rather than
/// whether it happens.
// **Never read, and being dropped is the whole of what it does** — the
// decrement is `HandleEntry`'s own `Drop`, so a reader would be a second way
// to spend the obligation. `expect` rather than `allow`: the day something
// does read it, this line reds and whoever wrote the reader has to say why the
// drop was not enough.
#[expect(dead_code)]
struct Displaced(Option<crate::object::HandleEntry>);
