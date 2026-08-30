//! The user/kernel argument boundary: every raw syscall register word becomes
//! a typed argument here, and no handler below this file sees a raw one.
//! A count userland chose, multiplied by a stride, is refused on overflow —
//! never trusted as a window — because the refusal word is each site's own.
//!
//! An arm's `return` leaves the dispatch closure, not the function, so the
//! epilogue below the match — the zero-handle drain and the time accounting —
//! runs for a decode refusal exactly as for a handler. `syscall_total` and
//! `syscall_total_ns` then count and time the same set of calls.

use crate::object::{ops, KObjectRef};
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{device, process};
// The macro, not the module: only `SYS_DEBUG`'s arms below spell `log!` unqualified.
#[cfg(feature = "test-actuators")]
use crate::log;

use toyos_abi::handle::{RawHandle, Rights};
#[cfg(feature = "test-actuators")]
use toyos_abi::syscall::debug_action as DA;
use toyos_abi::syscall::*;
use toyos_untrusted::Untrusted;

use super::HANDLE_LEN;
#[cfg(feature = "test-actuators")]
use super::debug::{canary, debug_heap_alloc, FATAL_HALT_NONCE, LOCK_ACROSS_SWITCH, LOCK_ACROSS_SWITCH_ARMED};
use super::device::{holds_claim, sys_device_claim, sys_device_reg, sys_gpu_reset_scanout};
use super::fs::{
    sys_chdir, sys_delete, sys_getcwd, sys_mkdir, sys_open, sys_readdir, sys_readlink, sys_rename,
    sys_rmdir, sys_symlink,
};
use super::handles::{sys_close, sys_dup2, sys_handle_dup, with_object, with_object_ref};
use super::io::{
    sys_random, sys_read, sys_read_nonblock, sys_write, sys_write_nonblock,
};
use super::ipc::{
    sys_accept, sys_connection_join, sys_handle_recv, sys_handle_send, sys_inbox_setup,
    sys_inbox_submit, sys_namespace_build, sys_namespace_open, sys_pipe, sys_pipe_map,
    sys_port_create, sys_shm_create, sys_shm_map,
};
use super::machine::{sys_log_read, sys_sched_info, sys_shutdown, sys_sysinfo};
#[cfg(feature = "test-actuators")]
use super::machine::SYSINFO_BOUND_LOWERED;
use super::proc::{
    sys_endowments, sys_exit, sys_nanosleep, sys_process_open, sys_process_stats,
    sys_process_wait, sys_rt_enter, sys_spawn, sys_thread_exit, sys_thread_join, sys_thread_spawn,
};
use super::vm::{sys_dlopen, sys_dlsym, sys_mmap, sys_munmap, sys_query_modules, sys_tls_alloc_block};

/// A number a deleted syscall used is retired, never reused.
macro_rules! retired_syscalls {
    ($($num:literal => $name:literal),+ $(,)?) => {
        const RETIRED_SYSCALLS: &[(u64, &str)] = &[$(($num, $name)),+];

        const _: () = {
            let mut i = 1;
            while i < RETIRED_SYSCALLS.len() {
                assert!(
                    RETIRED_SYSCALLS[i - 1].0 < RETIRED_SYSCALLS[i].0,
                    "the retired-syscall table is not strictly ascending, so a \
                     number is retired twice or the list is unreadable",
                );
                i += 1;
            }
        };

        fn retired_syscall(num: u64) -> Option<&'static str> {
            RETIRED_SYSCALLS.iter().find(|(n, _)| *n == num).map(|(_, name)| *name)
        }
    };
}

retired_syscalls! {
    26 => "SYS_WAITPID",
    29 => "SYS_SEND_MSG",
    30 => "SYS_RECV_MSG",
    31 => "SYS_OPEN_DEVICE",
    32 => "SYS_REGISTER_NAME",
    33 => "SYS_FIND_PID",
    36 => "SYS_ALLOC_SHARED",
    37 => "SYS_GRANT_SHARED",
    38 => "SYS_MAP_SHARED",
    39 => "SYS_RELEASE_SHARED",
    65 => "SYS_KILL",
    68 => "SYS_PIPE_OPEN",
    70 => "SYS_PIPE_ID",
    85 => "SYS_LISTEN",
    87 => "SYS_CONNECT",
    96 => "SYS_SET_RT_PRIORITY",
}

pub(super) fn syscall_dispatch(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    // Placed first so `nmi_gate` counts the call whatever it turns out to be.
    #[cfg(feature = "boot-actuators")]
    crate::nmi_gate::note_syscall();
    let t0 = crate::clock::nanos_since_boot();

    process::with_current_data(|data| {
        data.syscall_total += 1;
        // Clamped, not guarded: an unissued number still lands in the last bin, not nowhere.
        data.syscall_counts[(num as usize).min(toyos_abi::syscall::SYSCALL_PROFILE_OTHER)] += 1;
    });

    // SAFETY: current process's page tables remain active for the duration of this call.
    let ctx = unsafe { SyscallContext::new() };

    let bad_addr = SyscallError::BadAddress.to_u64();

    // A closure so an arm's `return` lands at the epilogue, not past it.
    let result = (move || -> u64 { match num {
        SYS_WRITE => {
            let Some(buf) = ctx.user_bytes(UserAddr::new(a2), a3) else { return bad_addr };
            sys_write(RawHandle(a1 as u32), &buf)
        }
        SYS_READ => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a2), a3) else { return bad_addr };
            sys_read(RawHandle(a1 as u32), &mut buf)
        }
        SYS_THREAD_EXIT => sys_thread_exit(a1 as i32),
        SYS_RANDOM => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
            sys_random(&mut buf)
        }
        SYS_CLOCK => crate::clock::nanos_since_boot(),
        SYS_OPEN => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_open(&path, OpenFlags(a3))
        }
        SYS_CLOSE => sys_close(RawHandle(a1 as u32)),
        SYS_SEEK => {
            let pos = match a3 {
                0 => SeekFrom::Start(a2),
                1 => SeekFrom::Current(a2 as i64),
                2 => SeekFrom::End(a2 as i64),
                _ => return SyscallError::InvalidArgument.to_u64(),
            };
            with_object(RawHandle(a1 as u32), Rights::READ, |o| ops::seek(o, pos))
        }
        // No right: fstat moves no content, so gating on READ would fail isatty() on write-only handles.
        SYS_FSTAT => {
            let stat = match with_object_ref(RawHandle(a1 as u32), Rights::NONE, ops::fstat) {
                Ok(stat) => stat,
                Err(e) => return e.refuse(),
            };
            match ctx.copy_out(UserAddr::new(a2), &stat) {
                Ok(()) => 0,
                Err(e) => e.to_u64(),
            }
        }
        // Cloned and run outside the process-data lock: fsync parks between flush
        // attempts, and a park under that lock is what the baseline tripwire refuses.
        // The clone outlives a concurrent close, the only other way this object is revoked.
        SYS_FSYNC => {
            match with_object_ref(RawHandle(a1 as u32), Rights::WRITE, KObjectRef::clone) {
                Ok(object) => ops::fsync(&object),
                Err(e) => e.refuse(),
            }
        }
        SYS_READDIR => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a3), a4) else { return bad_addr };
            sys_readdir(&path, &mut buf)
        }
        SYS_DELETE => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_delete(&path)
        }
        SYS_SHUTDOWN => sys_shutdown(RawHandle(a1 as u32)),
        SYS_CHDIR => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_chdir(&path)
        }
        SYS_GETCWD => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
            sys_getcwd(&mut buf)
        }
        SYS_PIPE => sys_pipe(),
        SYS_SPAWN => {
            let Ok(args) = ctx.copy_in::<SpawnArgs>(UserAddr::new(a1)) else { return bad_addr };
            let text = match ctx.user_str(UserAddr::new(args.argv_ptr), args.argv_len) { Ok(s) => s, Err(e) => return e.to_u64() };
            if args.endow_count as usize > toyos_abi::syscall::MAX_ENDOWMENTS
                || args.labels_len > toyos_abi::syscall::MAX_LABELS_LEN as u64
            {
                return SyscallError::InvalidArgument.to_u64();
            }
            // slot_map and endow stay windows, not copies: both counts are userland's
            // and each entry is read once. labels is copied because the child keeps it.
            let Some(slot_map) = (args.slot_map_count as usize)
                .checked_mul(crate::loader::SLOT_PAIR_LEN)
                .and_then(|len| ctx.user_bytes(UserAddr::new(args.slot_map_ptr), len as u64))
            else {
                return bad_addr;
            };
            let Some(endow) = (args.endow_count as usize)
                .checked_mul(core::mem::size_of::<toyos_abi::syscall::EndowEntry>())
                .and_then(|len| ctx.user_bytes(UserAddr::new(args.endow_ptr), len as u64))
            else {
                return bad_addr;
            };
            let labels = match ctx.user_vec(UserAddr::new(args.labels_ptr), args.labels_len) {
                Ok(bytes) => bytes,
                Err(e) => return e.to_u64(),
            };
            let pending = match process::build_child_handles(&slot_map, &endow, &labels) {
                Ok(built) => built,
                Err(e) => return e.refuse(),
            };
            // Copied and bounded like argv: env is kept for the child's whole life,
            // and `user_vec` is the one accessor that puts a userland size on the allocator.
            let env = if args.env_len > 0 {
                if args.env_len > crate::user_ptr::MAX_USER_STR {
                    return SyscallError::InvalidArgument.to_u64();
                }
                match ctx.user_vec(UserAddr::new(args.env_ptr), args.env_len) {
                    Ok(bytes) => bytes,
                    Err(e) => return e.to_u64(),
                }
            } else {
                alloc::vec::Vec::new()
            };
            sys_spawn(&text, pending, env)
        }
        SYS_PROCESS_WAIT => sys_process_wait(RawHandle(a1 as u32), a2),
        SYS_PROCESS_KILL => {
            match process::with_process_data(|data| {
                data.handles
                    .get::<crate::object::process::ProcessObject>(RawHandle(a1 as u32), Rights::MANAGE)
            }) {
                Ok(object) => {
                    // Killing yourself is exiting, not killing: kill_process retires every
                    // thread of its target, and retire_task asserts a CPU never retires itself.
                    // Reachable: TRANSFER lets a parent hand a child a handle to itself.
                    if object.pid() == process::current_process() {
                        // exit() never returns, so the held clone is dropped while it still can be.
                        drop(object);
                        process::exit(process::KILLED_EXIT_CODE);
                    }
                    process::kill_process(&object)
                }
                Err(e) => e.refuse(),
            }
        }
        SYS_PROCESS_OPEN => {
            sys_process_open(RawHandle(a1 as u32), process::Pid::from_raw(a2 as u32))
        }

        // No right: the one caller marks both ends of a pair, so requiring one would refuse the other.
        SYS_MARK_TTY => with_object(RawHandle(a1 as u32), Rights::NONE, ops::mark_tty),
        // Gated for display integrity, not memory access: contents are already behind shared_memory grants.
        // Ungated, any process could scan over the compositor's frames or move its cursor.
        SYS_GPU_PRESENT | SYS_GPU_SET_CURSOR | SYS_GPU_MOVE_CURSOR => {
            if let Err(e) = holds_claim(RawHandle(a1 as u32), device::DeviceType::Framebuffer) {
                return e.refuse();
            }
            let (hi2, lo2) = unpair(a2);
            let (hi3, lo3) = unpair(a3);
            match num {
                SYS_GPU_PRESENT => crate::gpu::present_rect(hi2, lo2, hi3, lo3),
                SYS_GPU_SET_CURSOR => crate::gpu::set_cursor(a2 as u32, a3 as u32),
                _ => crate::gpu::move_cursor(a2 as u32, a3 as u32),
            }
            0
        }
        SYS_THREAD_SPAWN => sys_thread_spawn(a1, a2, a3, a4),
        SYS_THREAD_JOIN => sys_thread_join(a1),
        // Both answer from the boot-time anchor, never the CMOS: NotSupported beats a
        // 1970-epoch number a caller cannot tell from real time.
        // Local time here, UTC in SYS_CLOCK_EPOCH: seconds-since-epoch are UTC by definition.
        SYS_CLOCK_REALTIME => crate::clock::local_secs().map_or(
            SyscallError::NotSupported.to_u64(),
            |secs| {
                let now = toyos_wallclock::Civil::from_unix_secs(secs);
                (now.hour << 16) | (now.min << 8) | now.sec
            },
        ),
        SYS_CLOCK_EPOCH => {
            crate::clock::utc_secs().map_or(SyscallError::NotSupported.to_u64(), |secs| secs)
        }
        // The header is ambient like SYS_CPU_COUNT; the roster after it costs
        // Rights::ROSTER because it is every process in the machine by name.
        SYS_SYSINFO => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a2), a3) else { return bad_addr };
            sys_sysinfo(RawHandle(a1 as u32), &mut buf)
        }
        SYS_NANOSLEEP => sys_nanosleep(a1),
        SYS_HANDLE_DUP => sys_handle_dup(RawHandle(a1 as u32), a2),
        SYS_HANDLE_DUP_AT => sys_dup2(RawHandle(a1 as u32), a2),
        SYS_GETPID => process::current_process().raw() as u64,
        SYS_RENAME => {
            let old = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            let new = match ctx.user_str(UserAddr::new(a3), a4) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_rename(&old, &new)
        }
        SYS_MKDIR => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_mkdir(&path)
        }
        SYS_RMDIR => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_rmdir(&path)
        }
        SYS_DLOPEN => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            // Refused here rather than at the write, so a bad address never leaves a
            // library the caller was never told about.
            let init_out = match a3 {
                0 => None,
                raw => match UserAddr::checked(raw) {
                    Some(addr) => Some(addr),
                    None => return bad_addr,
                },
            };
            // ctx carries the copy-out: sys_dlopen writes init_out only once the load succeeds.
            sys_dlopen(&ctx, &path, init_out)
        }
        SYS_DLSYM => {
            let name = match ctx.user_str(UserAddr::new(a2), a3) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_dlsym(a1, &name)
        }
        // A no-op by contract: a dlopen'd module is shared and lives for the
        // process — a reload returns the same handle (`sys_dlopen`), so there
        // is no per-open resource an unload would reclaim.
        SYS_DLCLOSE => 0,
        SYS_FTRUNCATE => {
            with_object(RawHandle(a1 as u32), Rights::WRITE, |o| ops::ftruncate(o, a2))
        }
        SYS_STACK_INFO => {
            let stack = process::with_current_data(|data| {
                (data.user_stack_base.raw() > 0)
                    .then_some((data.user_stack_base.raw(), data.user_stack_size))
            });
            let Some((base, size)) = stack else { return SyscallError::NotFound.to_u64() };
            match ctx
                .copy_out(UserAddr::new(a1), &base)
                .and_then(|()| ctx.copy_out(UserAddr::new(a2), &size))
            {
                Ok(()) => 0,
                Err(e) => e.to_u64(),
            }
        }
        SYS_CPU_COUNT => crate::arch::smp::cpu_count() as u64,
        SYS_FUTEX_WAIT => match UserAddr::checked(a1) {
            Some(addr) => process::futex_wait(addr, a2 as u32, a3),
            None => bad_addr,
        },
        SYS_FUTEX_WAKE => match UserAddr::checked(a1) {
            Some(addr) => process::futex_wake(addr, a2),
            None => bad_addr,
        },
        SYS_MMAP => sys_mmap(a1, a2, MmapProt(a3), MmapFlags(a4)),
        SYS_MUNMAP => sys_munmap(a1, a2),
        SYS_READ_NONBLOCK => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a2), a3) else { return bad_addr };
            sys_read_nonblock(RawHandle(a1 as u32), &mut buf)
        }
        SYS_WRITE_NONBLOCK => {
            let Some(buf) = ctx.user_bytes(UserAddr::new(a2), a3) else { return bad_addr };
            sys_write_nonblock(RawHandle(a1 as u32), &buf)
        }
        SYS_EXIT => sys_exit(a1 as i32),
        SYS_GET_ENV => {
            let env = process::with_process_data(|d| d.env.clone());
            if a2 == 0 {
                env.len() as u64
            } else {
                let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
                let copy_len = env.len().min(buf.len());
                buf.write_at(0, &env[..copy_len]);
                copy_len as u64
            }
        }
        SYS_CONNECTION_JOIN => {
            sys_connection_join(RawHandle(a1 as u32), RawHandle(a2 as u32))
        }
        SYS_PIPE_MAP => sys_pipe_map(RawHandle(a1 as u32)),
        // All three drive the NIC rings: without the claim, any process could drain
        // the used ring and exhaust all 256 RX slots by never refilling.
        SYS_NIC_RX_POLL => {
            if let Err(e) = holds_claim(RawHandle(a1 as u32), device::DeviceType::Nic) {
                return e.refuse();
            }
            match crate::net::poll_rx() {
                Some((buf_idx, frame_len)) => ((buf_idx as u64) << 16) | (frame_len as u64),
                None => 0,
            }
        }
        SYS_NIC_RX_DONE => {
            if let Err(e) = holds_claim(RawHandle(a1 as u32), device::DeviceType::Nic) {
                return e.refuse();
            }
            crate::net::refill_rx_buf(a2 as usize).map_or_else(|e| e.to_u64(), |()| 0)
        }
        SYS_NIC_TX => {
            if let Err(e) = holds_claim(RawHandle(a1 as u32), device::DeviceType::Nic) {
                return e.refuse();
            }
            match crate::net::submit_tx(a2 as usize) {
                Ok(()) => 0,
                Err(e) => e.to_u64(),
            }
        }
        SYS_SYMLINK => {
            let target = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            let link = match ctx.user_str(UserAddr::new(a3), a4) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_symlink(&target, &link)
        }
        SYS_READLINK => {
            let path = match ctx.user_str(UserAddr::new(a1), a2) { Ok(s) => s, Err(e) => return e.to_u64() };
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a3), a4) else { return bad_addr };
            sys_readlink(&path, &mut buf)
        }
        SYS_GPU_SET_RESOLUTION => {
            // Checked before the driver, so an unclaimed caller never turns two
            // arbitrary u32s into a physical allocation.
            let claim_h = RawHandle(a1 as u32);
            if let Err(e) = holds_claim(claim_h, device::DeviceType::Framebuffer) {
                return e.refuse();
            }
            // Checked before the allocation, same reason as the claim: a bad address
            // must not leave a resolution the caller was never told about.
            let Some(info_out) = UserAddr::checked(a3) else { return bad_addr };
            let (width, height) = unpair(a2);
            match crate::gpu::set_resolution(width, height) {
                // ctx lets sys_gpu_reset_scanout mint the buffers before it knows the
                // write-back length.
                Ok(gpu_info) => sys_gpu_reset_scanout(&ctx, claim_h, gpu_info, info_out),
                Err(e) => e.to_u64(),
            }
        }
        SYS_ENDOWMENTS => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
            sys_endowments(&mut buf)
        }
        SYS_DEVICE_CLAIM => sys_device_claim(RawHandle(a1 as u32), a2),
        SYS_RT_ENTER => sys_rt_enter(RawHandle(a1 as u32)),
        SYS_LOG_READ => {
            // checked_mul before mapping: a product that doesn't fit is a bad argument,
            // not kernel arithmetic wrapping into a window it then trusts.
            let Some(bytes) = (a4 as usize).checked_mul(toyos_abi::log::RECORD_BYTES) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            let Some(mut out) = ctx.user_bytes_mut(UserAddr::new(a3), bytes as u64) else {
                return bad_addr;
            };
            // ctx carries the capability check: sys_log_read must confirm it before
            // it copies in the caller's cursor.
            sys_log_read(&ctx, RawHandle(a1 as u32), UserAddr::new(a2), &mut out, a4 as usize)
        }
        SYS_ACCEPT => sys_accept(RawHandle(a1 as u32)),
        SYS_HANDLE_SEND => {
            if a3 as usize > MAX_TRANSFER_HANDLES {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(handles) = (a3 as usize)
                .checked_mul(HANDLE_LEN)
                .and_then(|len| ctx.user_bytes(UserAddr::new(a2), len as u64))
            else {
                return bad_addr;
            };
            sys_handle_send(RawHandle(a1 as u32), &handles, a3 as usize)
        }
        SYS_HANDLE_RECV => {
            if a3 as usize > MAX_TRANSFER_HANDLES {
                return SyscallError::InvalidArgument.to_u64();
            }
            let Some(mut out) = (a3 as usize)
                .checked_mul(HANDLE_LEN)
                .and_then(|len| ctx.user_bytes_mut(UserAddr::new(a2), len as u64))
            else {
                return bad_addr;
            };
            sys_handle_recv(RawHandle(a1 as u32), &mut out, a3 as usize)
        }
        SYS_SHM_CREATE => sys_shm_create(a1),
        SYS_SHM_MAP => sys_shm_map(RawHandle(a1 as u32)),
        SYS_PORT_CREATE => sys_port_create(),
        SYS_NAMESPACE_BUILD => {
            let Ok(args) = ctx.copy_in::<NamespaceBuild>(UserAddr::new(a1)) else {
                return bad_addr;
            };
            // ctx lets sys_namespace_build bound both counts before it reads either vector.
            sys_namespace_build(&ctx, &args)
        }
        SYS_NAMESPACE_OPEN => {
            let name = match ctx.user_str(UserAddr::new(a2), a3) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_namespace_open(RawHandle(a1 as u32), &name)
        }
        SYS_TLS_ALLOC_BLOCK => sys_tls_alloc_block(a1),
        // ctx carries the copy-out: sys_inbox_setup writes its answer only once setup succeeds.
        SYS_INBOX_SETUP => sys_inbox_setup(&ctx, a1 as u32, a2),
        SYS_INBOX_SUBMIT => {
            sys_inbox_submit(RawHandle(a1 as u32), a2 as u32, a3 as u32, a4)
        }
        SYS_QUERY_MODULES => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
            sys_query_modules(&mut buf)
        }
        // Feature-gated, not capability-checked: a shipping kernel has no SYS_DEBUG
        // arm, so the number falls to the same default InvalidArgument as any unassigned one.
        #[cfg(feature = "test-actuators")]
        SYS_DEBUG => match a1 {
            DA::PANIC => panic!("SYS_DEBUG: kernel panic triggered by userspace"),
            // SAFETY: unsound by design — a staged null read in Ring 0, gated behind test-actuators.
            // Volatile: a plain read could be optimized to unreachable, leaving nothing to fault.
            DA::NULL_READ => { unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()); } 0 }
            DA::LOCK_ACROSS_SWITCH => {
                if !LOCK_ACROSS_SWITCH_ARMED.swap(false, core::sync::atomic::Ordering::Relaxed) {
                    return SyscallError::InvalidArgument.to_u64();
                }
                let _held = LOCK_ACROSS_SWITCH.lock();
                crate::scheduler::yield_now();
                0
            }
            // Unlike every other action, this costs the machine, not just the caller's
            // process: one call is already a permanent halt.
            DA::FATAL_HALT => { log!("{}", FATAL_HALT_NONCE); crate::arch::apic::halt_all_cpus(); }
            // A real #DF, not simulated: pushing to a non-canonical rsp raises #SS,
            // and delivering that needs another push to the same rsp — the #DF condition.
            // Non-canonical rather than unmapped: on a bigger machine an unmapped
            // address can fall inside the direct map and simply get written to.
            // Only #DF has an IST, so every fault on the way there lands on this same unusable stack.
            DA::DOUBLE_FAULT => {
                log!("SYS_DEBUG: provoking a double fault");
                // SAFETY: unsound by design like NULL_READ; the #DF this raises never returns here.
                unsafe {
                    core::arch::asm!(
                        "mov rsp, {bad}",
                        "push 0",
                        bad = in(reg) 0x0000_8000_0000_0000u64,
                        options(noreturn),
                    );
                }
            }
            // Both sides of MAX_HEAP_ALLOC plus the alignment corner: the page-aligned
            // case pads past one page and must error, not panic inside the allocator's lock.
            DA::HEAP_AT_CEILING => debug_heap_alloc(crate::mm::MAX_HEAP_ALLOC, 8),
            DA::HEAP_OVER_CEILING => debug_heap_alloc(crate::mm::PAGE_2M as usize, 8),
            DA::HEAP_AT_CEILING_PAGE_ALIGNED => debug_heap_alloc(crate::mm::MAX_HEAP_ALLOC, 4096),
            // Returns, unlike other actions here: the console must survive being drawn over.
            DA::SCREEN_GRAFFITI => {
                crate::drivers::panic_console::graffiti();
                0
            }
            // A read, not a write: tests the page is absent without the feature also
            // handing userland a kernel store; returning 0 means the guard failed.
            DA::IDLE_GUARD_READ => {
                let addr = crate::arch::percpu::idle_guard_byte();
                log!("SYS_DEBUG: reading the idle stack guard at {addr:#x}");
                // SAFETY: deliberate fault — idle_guard_byte() is the last byte of a page taken out of the direct map, so the read is the #PF under test.
                // Volatile: a plain read would be optimized away, discarding the fault it exists to cause.
                unsafe { core::ptr::read_volatile(addr as *const u8) };
                0
            }
            DA::CANARY_ADDR => canary::address(),
            DA::CANARY_CHANGED => canary::changed() as u64,
            // Arms the last CPU a shootdown waits for to answer a2ns late; the arming
            // outlives the call so the caller can then time a syscall's own shootdown path.
            DA::TLB_ACK_DELAY_ARM => crate::arch::tlb::debug_arm_ack_delay(a2),
            DA::TLB_ACK_DELAY_DISARM => crate::arch::tlb::debug_disarm_ack_delay(),
            // The two leaks this object model accepts — a stranded Arc, the cross-pair
            // connection cycle — are visible in no other way than this census.
            // Per kind, not total: a total would hide one kind's leak behind another's churn.
            // Names are checked against OBJECT_KINDS rather than order assumed: this is
            // the one place the two declarations meet.
            DA::CENSUS_KIND => {
                assert!(
                    crate::object::census::live()
                        .map(|(kind, _)| kind)
                        .eq(toyos_abi::syscall::OBJECT_KINDS.iter().copied()),
                    "kobject! and toyos_abi::syscall::OBJECT_KINDS declare different objects",
                );
                match crate::object::census::live().nth(a2 as usize) {
                    Some((_, live)) => live,
                    None => SyscallError::InvalidArgument.to_u64(),
                }
            }
            DA::IDLE_STACK_HIGH_WATER => crate::arch::percpu::idle_stack_high_water() as u64,
            DA::IDLE_STACK_SIZE => crate::arch::percpu::idle_stack_size() as u64,
            // Armed, not #[cfg]'d, so it doesn't ship in every kernel this suite boots:
            // the real bound is unreachable (no guest makes 65,536 threads).
            DA::LOWER_SYSINFO_BOUND => {
                SYSINFO_BOUND_LOWERED.store(true, core::sync::atomic::Ordering::Relaxed);
                0
            }
            // Puts one free slot one lifecycle from the end so retirement is reachable without
            // 1,048,575 close/reopen round trips; confers nothing a process could not do itself.
            DA::SLOT_TO_LAST_GENERATION => {
                let Ok(slot) = u16::try_from(a2) else {
                    return SyscallError::InvalidArgument.to_u64();
                };
                match process::with_process_data(|d| d.handles.stage_last_generation(slot)) {
                    Some(h) => u64::from(h.0),
                    None => SyscallError::InvalidArgument.to_u64(),
                }
            }
            _ => SyscallError::InvalidArgument.to_u64(),
        },
        SYS_SCHED_INFO => match ctx.copy_out(UserAddr::new(a1), &sys_sched_info()) {
            Ok(()) => 0,
            Err(e) => e.to_u64(),
        },
        SYS_PROCESS_STATS => {
            let stats_size = core::mem::size_of::<toyos_abi::syscall::ProcessStats>() as u64;
            if a3 < stats_size { return SyscallError::InvalidArgument.to_u64(); }
            let Some(addr) = UserAddr::checked(a2) else { return bad_addr };
            // ctx carries the copy-out: sys_process_stats writes its answer only once the read succeeds.
            sys_process_stats(&ctx, RawHandle(a1 as u32), addr)
        },
        SYS_SET_THREAD_NAME => {
            // Refused by name, not clamped: a length past the bound is the caller's
            // bad argument, not a shorter name to silently write.
            let Ok(len) = Untrusted::new(a2).at_most(process::THREAD_NAME_LEN as u64) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            let len = len as usize;
            let Some(bytes) = ctx.user_bytes(UserAddr::new(a1), len as u64) else {
                return bad_addr;
            };
            let mut name = [0u8; process::THREAD_NAME_LEN];
            bytes.read_at(0, &mut name[..len]);
            process::set_current_thread_name(&name[..len]);
            0
        },
        SYS_DEVICE_REG_READ => sys_device_reg(RawHandle(a1 as u32), a2, a3, None),
        SYS_DEVICE_REG_WRITE => sys_device_reg(RawHandle(a1 as u32), a2, a3, Some(a4)),
        // Retired, not reused: an old binary is told which call it was, not that the number is nonsense.
        _ => match retired_syscall(num) {
            Some(name) => {
                crate::log!("syscall {num} is retired (formerly {name})");
                SyscallError::NotSupported.to_u64()
            }
            None => SyscallError::InvalidArgument.to_u64(),
        },
    } })();

    // The first of the object layer's three drain sites. Here, not at the drop that
    // queued it: a hook must not run under whatever guard the syscall held when the
    // last handle went.
    crate::object::drain_zero_handles();

    // Wall-clock, not CPU time: elapsed includes any preemption.
    let elapsed = crate::clock::nanos_since_boot() - t0;
    process::with_current_data(|data| {
        data.syscall_total_ns += elapsed;
    });

    result
}

/// The two `u32`s a device call packs into one argument word, taken apart here.
fn unpair(word: u64) -> (u32, u32) {
    ((word >> 32) as u32, word as u32)
}
