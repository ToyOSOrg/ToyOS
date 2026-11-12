//! Process and thread syscalls: spawn, wait, exit, and self-ops on a thread.
//!
//! A handle is the authority over a process; a pid alone is not.
//!
//! The parking calls clone what they wait on out of the table before
//! blocking, so no guard is held across a park.

use alloc::vec::Vec;

use crate::watch;
use crate::time::{Deadline, Duration};
use crate::UserAddr;
use crate::process;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;
use kernel::sched::affinity::CpuMask;
use kernel::sched::task::WaitClass;

use super::cancelled;
use super::handles::demand_syscap;

/// End the calling thread. A detached one first gives back the range it
/// names, its own stack, which it no longer runs on here; a range refused is
/// answered and the thread runs on.
pub(super) fn sys_thread_exit(code: i32, flags: u64, at: u64, len: u64) -> u64 {
    let joined = match flags {
        0 if at == 0 && len == 0 => process::Joined::Joinable,
        THREAD_EXIT_DETACHED => {
            if len != 0 {
                if let Err(e) = super::vm::release(at, len) {
                    return e.to_u64();
                }
            }
            process::Joined::Detached
        }
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    process::thread_exit(code, joined);
}

/// Let another runnable thread of the caller's band have this CPU.
pub(super) fn sys_yield() -> u64 {
    crate::scheduler::yield_now();
    0
}

pub(super) fn sys_exit(code: i32) -> u64 {
    #[cfg(feature = "mask-windows")]
    crate::windows::hold_once();
    process::exit(toyos_abi::end::End::Exited(code));
}

/// `SpawnArgs::place`: the caller itself, or the process a handle carrying `WRITE` names — a handle a peer sent, so a wrong type is refused rather than ending the caller.
pub(super) fn spawn_place(place: u64) -> Result<process::Parent, u64> {
    if place == u64::from(toyos_abi::HANDLE_INVALID.0) {
        return Ok(process::Parent::Under(process::current_process()));
    }
    let Ok(raw) = u32::try_from(place) else {
        return Err(SyscallError::InvalidArgument.to_u64());
    };
    process::with_handles(|handles| {
        handles.get_sent::<crate::object::process::ProcessObject>(RawHandle(raw), Rights::WRITE)
    })
    .map(|object| process::Parent::Under(object.pid()))
    .map_err(|refused| refused.refuse())
}

/// The program `args` lays out: each range checked as an ordinary mapping,
/// and each object it names looked up with the rights its range needs before
/// an endowment could move the handle naming it.
pub(super) fn spawn_program(ctx: &crate::user_ptr::SyscallContext<'_>, args: &SpawnArgs) -> Result<crate::loader::Program, u64> {
    let invalid = || SyscallError::InvalidArgument.to_u64();
    let count = usize::try_from(args.maps_count).ok().filter(|n| (1..=MAX_SPAWN_MAPS).contains(n)).ok_or_else(invalid)?;
    if args.build_id_len > MAX_BUILD_ID as u64 {
        return Err(invalid());
    }
    let mut maps = Vec::new();
    maps.try_reserve_exact(count).map_err(|_| SyscallError::ResourceExhausted.to_u64())?;
    for i in 0..count {
        let at = args.maps_ptr.checked_add((i * core::mem::size_of::<SpawnMap>()) as u64).ok_or_else(invalid)?;
        let raw = ctx.copy_in::<SpawnMap>(UserAddr::new(at)).map_err(SyscallError::to_u64)?;
        let object = match raw.object {
            handle if handle == u64::from(toyos_abi::HANDLE_INVALID.0) => None,
            handle => {
                let raw_handle = u32::try_from(handle).map_err(|_| invalid())?;
                let rights = crate::loader::Map::rights(&raw).map_err(SyscallError::to_u64)?;
                let object = process::with_handles(|handles| {
                    handles.get::<crate::object::shm::SharedMemObject>(RawHandle(raw_handle), rights)
                });
                Some(object.map_err(|refused| refused.refuse())?)
            }
        };
        maps.push(crate::loader::Map::checked(&raw, object).map_err(SyscallError::to_u64)?);
    }
    let build_id = match args.build_id_len {
        0 => None,
        len => {
            let bytes = ctx.user_vec(UserAddr::new(args.build_id_ptr), len).map_err(SyscallError::to_u64)?;
            toyos_symbols::frame::BuildId::new(&bytes)
        }
    };
    Ok(crate::loader::Program { maps, entry: args.entry, image_base: args.image_base, build_id })
}

/// Start a program under `parent` and return the handle to it the spawn's commit put in the caller's table.
pub(super) fn sys_spawn(
    path: &str,
    name: [u8; process::THREAD_NAME_LEN],
    pending: crate::loader::PendingHandles,
    program: crate::loader::Program,
    parent: process::Parent,
) -> u64 {
    // Nothing to clean up: spawn's frame owns the child's resources on error.
    match process::spawn(path, name, pending, program, parent) {
        Ok((_, handle)) => u64::from(handle.0),
        Err(e) => e.refuse(),
    }
}

/// `SpawnArgs::pager`: none, or a `SysCap` carrying [`Rights::PAGER`].
pub(super) fn spawn_pager(pager: u64) -> Result<bool, u64> {
    if pager == u64::from(toyos_abi::HANDLE_INVALID.0) {
        return Ok(false);
    }
    let raw = u32::try_from(pager).map_err(|_| SyscallError::InvalidArgument.to_u64())?;
    demand_syscap(RawHandle(raw), Rights::PAGER).map(|_| true).map_err(|refused| refused.refuse())
}

/// Take how a process ended, blocking until it has; repeatable across waiters, and `WNOHANG` skips the block.
pub(super) fn sys_process_wait(h: RawHandle, flags: u64) -> u64 {
    // First, so the answer is the bit and not the handle's own refusal — which
    // for an unheld handle is the kill policy, so a call carrying both returns
    // instead of ending the caller.
    if flags & !PROCESS_WAIT_FLAGS_KNOWN != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let object = match process::with_handles(|handles| {
        handles.get::<crate::object::process::ProcessObject>(h, Rights::WAIT)
    }) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    if flags & WNOHANG == 0 {
        let parkable = crate::scheduler::Parkable::at_entry();
        if watch::wait_until(
            &parkable,
            object.watch(),
            0,
            WaitClass::Other,
            Deadline::never(),
            || object.finished(),
        )
        .is_err()
        {
            return cancelled();
        }
    }
    match object.end() {
        Some(end) => end.to_word(),
        // Reachable from userland (WNOHANG raced the exit), so this refuses rather than asserts.
        None => SyscallError::WouldBlock.to_u64(),
    }
}

/// Ask a process and everything below it to quit, gated by `MANAGE` as a kill is.
pub(super) fn sys_process_quit(h: RawHandle, reason: u64) -> u64 {
    // First, so a word that names no reason is the answer, not the handle's
    // own refusal.
    let Some(reason) = u8::try_from(reason).ok().and_then(toyos_abi::quit::Reason::from_byte) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    match process::with_handles(|handles| {
        handles.get::<crate::object::process::ProcessObject>(h, Rights::MANAGE)
    }) {
        Ok(object) => {
            process::quit_process(&object, reason);
            0
        }
        Err(e) => e.refuse(),
    }
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

/// Hold every idle CPU to waking within `bound_ns`, gated on a `SysCap`
/// carrying [`Rights::WAKE_LATENCY`], and install the hold's handle.
pub(super) fn sys_wake_latency_hold(syscap: RawHandle, bound_ns: u64) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::WAKE_LATENCY) {
        return e.refuse();
    }
    if bound_ns == 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let object = match crate::object::wake_latency::WakeLatencyObject::new(bound_ns) {
        Ok(object) => crate::object::KObjectRef::WakeLatency(object),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    match process::with_handles(|handles| crate::object::ops::install(handles, object)) {
        Ok(h) => u64::from(h.0),
        Err(e) => e.to_u64(),
    }
}

/// Make a new job, gated on a `SysCap` carrying [`Rights::JOBS`], and install a handle to it.
pub(super) fn sys_job_create(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::JOBS) {
        return e.refuse();
    }
    let object = match crate::object::job::JobObject::new(crate::object::job::new_job()) {
        Ok(object) => crate::object::KObjectRef::Job(object),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    match process::with_handles(|handles| crate::object::ops::install(handles, object)) {
        Ok(h) => u64::from(h.0),
        Err(e) => e.to_u64(),
    }
}

/// Install a handle to the job the caller runs in.
pub(super) fn sys_job_own() -> u64 {
    let job = process::with_process_data(|data| data.job.clone());
    let object = match crate::object::job::JobObject::new(job) {
        Ok(object) => crate::object::KObjectRef::Job(object),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    match process::with_handles(|handles| crate::object::ops::install(handles, object)) {
        Ok(h) => u64::from(h.0),
        Err(e) => e.to_u64(),
    }
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
    match out.write_at(0, &buf) {
        Ok(()) => needed as u64,
        Err(faulted) => SyscallError::from(faulted).to_u64(),
    }
}

/// Point the calling thread's thread pointer at `tp` (x86-64's FS base,
/// AArch64's `TPIDR_EL0`), which the thread keeps across every switch, and
/// answer its tid. The kernel never reads through it: what lies there is the
/// program's. Zero or a user-half address; anything else is `InvalidArgument`,
/// since a non-canonical FS base would fault in the kernel's ring.
pub(super) fn sys_set_thread_pointer(tp: u64) -> u64 {
    if tp != 0 && UserAddr::checked(tp).is_none() {
        return SyscallError::InvalidArgument.to_u64();
    }
    // SAFETY: zero or a user-half address, so canonical; the register is this
    // thread's, saved with it at every switch.
    unsafe { crate::arch::cpu::write_thread_pointer(tp) };
    u64::from(process::current_tid().raw())
}

/// Spawn a thread; refuses a `stack_base` above `stack_ptr` (no stack to clamp to)
/// and an `entry` outside the user half, which the return to it would fault on
/// in the kernel's ring. The stack pointer is the thread's own to fault on.
pub(super) fn sys_thread_spawn(entry: u64, stack_ptr: u64, arg: u64, stack_base: u64) -> u64 {
    if stack_base > stack_ptr {
        return SyscallError::InvalidArgument.to_u64();
    }
    let Some(entry) = toyos_userbound::Entry::new(entry) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    // A None here is a resource failure or teardown race, never a bad argument.
    process::spawn_thread(entry, stack_ptr, arg, stack_base)
        .map_or(SyscallError::ResourceExhausted.to_u64(), |t| t.raw() as u64)
}

/// The scheduler face of the calling process's live thread `tid`. Another process's thread is not one of them,
/// which is the whole of the authority: a mask is a placement choice a process makes for its own threads.
fn own_thread(tid: u64) -> Option<alloc::sync::Arc<crate::sched::payload::KShared>> {
    let tid = process::Tid::from_raw(u32::try_from(tid).ok()?);
    let shared = process::thread_sched(process::current_process(), tid)?.shared;
    (shared.state() != kernel::sched::task::TaskState::Dead).then_some(shared)
}

pub(super) fn sys_thread_set_affinity(tid: u64, words: &[u64]) -> u64 {
    let mask = match CpuMask::from_words(words, crate::smp::cpu_count() as usize) {
        Ok(mask) => mask,
        Err(_) => return SyscallError::InvalidArgument.to_u64(),
    };
    let Some(shared) = own_thread(tid) else {
        return SyscallError::NotFound.to_u64();
    };
    crate::sched::driver::set_affinity(&shared, mask);
    0
}

/// Its mask, less the CPUs that are not online.
pub(super) fn sys_thread_get_affinity(tid: u64) -> Result<[u64; CPU_MASK_WORDS], SyscallError> {
    let shared = own_thread(tid).ok_or(SyscallError::NotFound)?;
    Ok(*shared.mask().load().0.within(crate::smp::cpu_count() as usize).words())
}

/// Wait for a thread of this process to die.
pub(super) fn sys_thread_join(tid: u64) -> u64 {
    let tid = process::Tid::from_raw(tid as u32);
    let caller = process::current_process();
    // None means never existed or already collected; the predicate below answers both.
    let target = process::thread_sched(caller, tid);
    let parkable = crate::scheduler::Parkable::at_entry();
    let join = toyos_proclife::join::Join::default();
    let ask = || process::ask_join(&join, tid, caller);
    loop {
        // Both refusals are one answer here; `JoinRefused` keeps them apart because they aren't the same fact.
        if let Some(answer) = ask() {
            return answer.map_or(SyscallError::NotFound.to_u64(), |_| 0);
        }
        let Some(sched) = target.as_ref() else {
            // Nothing to arm on and no zombie: the join will never answer differently.
            return SyscallError::NotFound.to_u64();
        };
        // Arms on the target thread's own watch, not a wake-by-name to the main thread.
        if watch::wait_until(
            &parkable,
            sched.handle.watch(),
            tid.raw() as u64,
            WaitClass::Other,
            Deadline::never(),
            || ask().is_some(),
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
    let _ = watch::wait_until(
        &parkable,
        handle.watch(),
        0,
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
    let object = match process::with_handles(|handles| {
        handles.get::<crate::object::process::ProcessObject>(h, Rights::READ)
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

/// Hold memory back for the job a handle names, gated on a `SysCap` carrying
/// [`Rights::FLOOR`] and the job handle's `WRITE`.
pub(super) fn sys_job_floor(syscap: RawHandle, job: RawHandle, bytes: u64) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::FLOOR) {
        return e.refuse();
    }
    let object = match process::with_handles(|handles| handles.get::<crate::object::job::JobObject>(job, Rights::WRITE)) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    match object.set_floor(bytes) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

/// Copy out what the account of the job a handle names holds, gated on its `READ`.
pub(super) fn sys_account_info(ctx: &crate::user_ptr::SyscallContext, job: RawHandle, out: UserAddr) -> u64 {
    let object = match process::with_handles(|handles| handles.get::<crate::object::job::JobObject>(job, Rights::READ)) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    match ctx.copy_out(out, &object.account()) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}
