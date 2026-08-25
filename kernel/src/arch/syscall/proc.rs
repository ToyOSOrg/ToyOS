//! Processes and threads: starting one, waiting on one, ending one, and the
//! three calls a thread makes about itself.
//!
//! **A handle is the authority over a process and a pid is not.** Every call
//! here that reaches another process takes a `Process` handle carrying the right
//! it needs, and the one call that turns a pid into such a handle
//! ([`sys_process_open`]) demands a `SysCap` first — so what can reach a process
//! it did not start is exactly what `/bin/init` endowed.
//!
//! The parking calls — [`sys_process_wait`], [`sys_thread_join`],
//! [`sys_nanosleep`] — clone what they wait on out of the table before they
//! block, for `super::io`'s reason: a guard held across a park is what the
//! baseline tripwire fires on, and a cancelled wait answers `super::cancelled`.

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

/// Start a program and answer a handle to it.
///
/// The child is spawned before the handle is installed, so a caller whose table
/// is full has already made a process — and one nobody can name is one nobody
/// can wait for or kill. It is killed rather than left running, which is what
/// makes the answer "no process was started" true.
pub(super) fn sys_spawn(
    text: &str,
    pending: crate::loader::PendingHandles,
    env: alloc::vec::Vec<u8>,
) -> u64 {
    let args: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
    let cwd = process::with_process_data(|data| data.cwd.clone());
    // Refused with this frame holding nothing: `spawn`'s own frame owned the
    // child's address space and its stacks, and the three handle kinds that end
    // the caller do so from `refuse`.
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
            process::kill_process(&object);
            e.to_u64()
        }
    }
}

/// Take a process's exit code, blocking until there is one.
///
/// **The code is on the object, so this is a read and not a claim.** Two
/// threads may wait on one process and both get the code; a wait long after the
/// process is gone gets it too. `WNOHANG` is the same question with the park
/// taken out.
pub(super) fn sys_process_wait(h: RawHandle, flags: u64) -> u64 {
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
        // Zero-extended: an exit code is an `i32`, and sign-extending -1 would
        // land on `SyscallError`'s encoding.
        Some(code) => code as u32 as u64,
        // One answer for both arms rather than an `expect` on the blocking one.
        // `publish_exit` fills the slot before it stores `finished`, and the
        // wait above returns only when `finished` holds, so this is
        // unreachable — but it is reachable *from userland*, which is the whole
        // reason it may not be an assertion: a wait that came back without its
        // condition is a refusal the caller already handles (it is what
        // `WNOHANG` answers), never a kernel that dies holding a userland
        // thread's syscall.
        None => SyscallError::WouldBlock.to_u64(),
    }
}

/// A `Process` handle for a pid, presenting a `SysCap` that carries
/// [`Rights::MANAGE`].
///
/// The one place a pid becomes authority over anything, and the kernel mints
/// exactly one cap that carries the right — so what can reach a process it did
/// not start is exactly what init endowed.
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

/// Enter the real-time band, presenting a `SysCap` that carries
/// [`Rights::RT`].
///
/// The RT band has no priority above it, so unbounded threads in it starve
/// soundd's mix thread at its own level. Gating it on holding an audio claim
/// would be no privilege at all — whoever won the first-come race for the sound
/// card would get the band with it — so it is endowed per manifest rather than
/// won.
pub(super) fn sys_rt_enter(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::RT) {
        return e.refuse();
    }
    crate::scheduler::set_current_rt(true);
    0
}

/// Answer this process's endowment table.
///
/// An empty buffer asks how many bytes the answer needs, so a caller sizes once
/// and reads once. A short one is refused rather than truncated: half an
/// endowment table is not a smaller endowment table, it is a caller that would
/// go on to look up a label that is not in what it got.
pub(super) fn sys_endowments(out: &mut crate::user_ptr::UserBytesMut) -> u64 {
    let data_arc = process::process_data();
    let data = data_arc.lock();
    let needed = data.endowments.encoded_len();
    if out.is_empty() {
        return needed as u64;
    }
    if out.len() < needed {
        return SyscallError::InvalidArgument.to_u64();
    }
    let mut buf = alloc::vec![0u8; needed];
    data.endowments.encode(&mut buf);
    drop(data);
    out.write_at(0, &buf);
    needed as u64
}

/// `spawn_thread` stores `stack_ptr - stack_base`, and both are raw syscall
/// arguments. A base above the pointer describes no stack at all, so there is
/// nothing to clamp it to and it is refused.
pub(super) fn sys_thread_spawn(entry: u64, stack_ptr: u64, arg: u64, stack_base: u64) -> u64 {
    if stack_base > stack_ptr {
        return SyscallError::InvalidArgument.to_u64();
    }
    // Every `None` from `spawn_thread` is a resource failure (TLS, kernel
    // stack, virtual address space) or a teardown race, never a bad argument.
    process::spawn_thread(entry, stack_ptr, arg, stack_base)
        .map_or(SyscallError::ResourceExhausted.to_u64(), |t| t.raw() as u64)
}

/// Wait for a thread of this process to die.
///
/// **It arms on the thread it names**: the target's own `TaskHandle` carries the
/// watch, `thread_exit` posts to it, and the `ThreadSched` held across the park
/// is what keeps that watch alive — never a wake by name to the process's main
/// thread, into a hashed bucket, re-checked by whoever happened to be woken.
pub(super) fn sys_thread_join(tid: u64) -> u64 {
    let tid = process::Tid::from_raw(tid as u32);
    let caller = process::current_process();
    // Resolved once. `None` is a thread that never existed or is already
    // collected, and the predicate below answers both.
    let target = process::thread_sched(caller, tid);
    let parkable = crate::scheduler::Parkable::at_entry();
    loop {
        match process::wait_thread_zombie(tid, caller) {
            Ok(Some(_)) => return 0,
            Ok(None) => {}
            Err(()) => return SyscallError::NotFound.to_u64(),
        }
        let Some(sched) = target.as_ref() else {
            // Nothing to arm on and the zombie is not there: the thread is
            // gone in a way `wait_thread_zombie` will keep answering the same
            // way, so waiting cannot change it.
            return SyscallError::NotFound.to_u64();
        };
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
    // The caller's own arithmetic, which is exactly what a `Deadline` is: the
    // ABI still carries a relative span, and this is the one place it becomes
    // an instant.
    let deadline = Deadline::at(crate::clock::now() + Duration::from_nanos(nanos));
    // **Armed on nothing but time.** A sleep has no subject — what ends it is
    // the deadline the caller chose — so it arms on its own thread, where
    // nothing posts, with no condition to re-check; a deadline already passed
    // fires at the next scheduler entry.
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

/// Accounting for the process a handle names, alive or exited.
///
/// **Repeatable, and not a claim on anything.** With a handle there is nothing
/// to stash: a live process is sampled from its own data and an exited one from
/// the object, and neither reading spends anything.
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
