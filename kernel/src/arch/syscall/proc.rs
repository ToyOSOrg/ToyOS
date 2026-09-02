//! Process and thread syscalls: spawn, wait, exit, and self-ops on a thread.
//!
//! A handle is the authority over a process; a pid alone is not. Only
//! `sys_process_open` mints a handle from a pid, gated on a `SysCap`.
//!
//! The parking calls clone what they wait on out of the table before
//! blocking, so no guard is held across a park.

use alloc::vec::Vec;

use crate::completion;
use crate::object::{ops, KObjectRef};
use crate::time::{Deadline, Duration};
use crate::UserAddr;
use crate::process;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;
use toyos_sched::task::WaitClass;

use super::cancelled;
use super::handles::{demand_syscap, handle_result};

pub(super) fn sys_thread_exit(code: i32) -> u64 {
    process::thread_exit(code);
}

pub(super) fn sys_exit(code: i32) -> u64 {
    process::exit(code);
}

/// Start a program and return a handle to it; kill the child if the handle can't be installed.
pub(super) fn sys_spawn(
    text: &str,
    pending: crate::loader::PendingHandles,
    env: alloc::vec::Vec<u8>,
) -> u64 {
    let args: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
    let cwd = process::with_process_data(|data| data.cwd.clone());
    // Nothing to clean up yet: spawn's frame owns the child's resources on error.
    let object = match process::spawn(&args, pending, cwd, env) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    let installed = process::with_process_data(|data| {
        ops::install(&mut data.handles, KObjectRef::Process(object.clone()))
    });
    match installed {
        Ok(h) => h.0 as u64,
        Err(e) => {
            // Unnamed, it can be neither waited on nor killed later, so it is killed here.
            process::kill_process(&object);
            e.to_u64()
        }
    }
}

/// Take a process's exit code, blocking until there is one; repeatable across waiters, and `WNOHANG` skips the block.
pub(super) fn sys_process_wait(h: RawHandle, flags: u64) -> u64 {
    // First, so the answer is the bit and not the handle's own refusal:
    // `WNOHANG` is the whole of this word and the other 63 bits mean nothing.
    if flags & !WNOHANG != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let object = match process::with_process_data(|data| {
        data.handles.get::<crate::object::process::ProcessObject>(h, Rights::WAIT)
    }) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    if flags & WNOHANG == 0 {
        let parkable = crate::scheduler::Parkable::at_entry();
        if completion::wait_until(
            &parkable,
            completion::Subject::of(object.watch()),
            completion::Token::new(0),
            WaitClass::Other,
            Deadline::never(),
            || object.finished(),
        )
        .is_err()
        {
            return cancelled();
        }
    }
    match object.exit_code() {
        // Zero-extended: sign-extending -1 would collide with SyscallError's encoding.
        Some(code) => code as u32 as u64,
        // Reachable from userland (WNOHANG raced the exit), so this refuses rather than asserts.
        None => SyscallError::WouldBlock.to_u64(),
    }
}

/// Mint a `Process` handle for a pid, gated on a `SysCap` carrying [`Rights::MANAGE`].
pub(super) fn sys_process_open(syscap: RawHandle, pid: process::Pid) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::MANAGE) {
        return e.refuse();
    }
    let Some(object) = process::process_object(pid) else {
        return SyscallError::NotFound.to_u64();
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::Process(object)))
    })
}

/// Enter the real-time band, gated on a `SysCap` carrying [`Rights::RT`].
pub(super) fn sys_rt_enter(syscap: RawHandle) -> u64 {
    // Gated by manifest, not audio ownership: winning the sound-card race would grant the band too.
    if let Err(e) = demand_syscap(syscap, Rights::RT) {
        return e.refuse();
    }
    crate::scheduler::set_current_rt(true);
    0
}

/// Answer this process's endowment table; an empty buffer asks only for its size.
pub(super) fn sys_endowments(out: &mut crate::user_ptr::UserBytesMut) -> u64 {
    let data_arc = process::process_data();
    let data = data_arc.lock();
    let needed = data.endowments.encoded_len();
    if out.is_empty() {
        return needed as u64;
    }
    // Refused rather than truncated: a partial table would look up labels it lacks.
    if out.len() < needed {
        return SyscallError::InvalidArgument.to_u64();
    }
    let mut buf = alloc::vec![0u8; needed];
    data.endowments.encode(&mut buf);
    drop(data);
    out.write_at(0, &buf);
    needed as u64
}

/// Spawn a thread; refuses a `stack_base` above `stack_ptr` (no stack to clamp to).
pub(super) fn sys_thread_spawn(entry: u64, stack_ptr: u64, arg: u64, stack_base: u64) -> u64 {
    if stack_base > stack_ptr {
        return SyscallError::InvalidArgument.to_u64();
    }
    // A None here is a resource failure or teardown race, never a bad argument.
    process::spawn_thread(entry, stack_ptr, arg, stack_base)
        .map_or(SyscallError::ResourceExhausted.to_u64(), |t| t.raw() as u64)
}

/// Wait for a thread of this process to die.
pub(super) fn sys_thread_join(tid: u64) -> u64 {
    let tid = process::Tid::from_raw(tid as u32);
    let caller = process::current_process();
    // None means never existed or already collected; the predicate below answers both.
    let target = process::thread_sched(caller, tid);
    let parkable = crate::scheduler::Parkable::at_entry();
    loop {
        match process::wait_thread_zombie(tid, caller) {
            Ok(Some(_)) => return 0,
            Ok(None) => {}
            Err(()) => return SyscallError::NotFound.to_u64(),
        }
        let Some(sched) = target.as_ref() else {
            // Nothing to arm on and no zombie: wait_thread_zombie will never answer differently.
            return SyscallError::NotFound.to_u64();
        };
        // Arms on the target thread's own watch, not a wake-by-name to the main thread.
        if completion::wait_until(
            &parkable,
            completion::Subject::of(sched.handle.watch()),
            completion::Token::new(tid.raw() as u64),
            WaitClass::Other,
            Deadline::never(),
            || matches!(process::wait_thread_zombie(tid, caller), Ok(Some(_)) | Err(())),
        )
        .is_err()
        {
            return cancelled();
        }
    }
}

pub(super) fn sys_nanosleep(nanos: u64) -> u64 {
    // The ABI's relative span becomes an absolute Deadline here, and only here.
    let deadline = Deadline::at(crate::clock::now() + Duration::from_nanos(nanos));
    // Armed on its own thread with no subject: nothing posts, only the deadline fires it.
    let parkable = crate::scheduler::Parkable::at_entry();
    let Some(handle) = crate::sched::driver::current_handle() else {
        return 0;
    };
    let _ = completion::wait_until(
        &parkable,
        completion::Subject::of(handle.watch()),
        completion::Token::new(0),
        WaitClass::Other,
        deadline,
        || false,
    );
    0
}

/// Read this process's accounting, alive or exited; repeatable, spends nothing.
pub(super) fn sys_process_stats(
    ctx: &crate::user_ptr::SyscallContext,
    h: RawHandle,
    out: UserAddr,
) -> u64 {
    let object = match process::with_process_data(|data| {
        data.handles.get::<crate::object::process::ProcessObject>(h, Rights::READ)
    }) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    let Some(stats) = process::stats_of(&object) else {
        return SyscallError::NotFound.to_u64();
    };
    match ctx.copy_out(out, &stats) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}
