//! **The user/kernel argument boundary: every raw syscall register word becomes
//! a typed argument here, and no handler below this file sees one.**
//!
//! [`syscall_dispatch`] is the whole of it — one match over the number, one arm
//! per call, each arm turning `a1..a4` into what its handler takes: a bounded
//! `UserBytes` or `UserBytesMut` window, a copied string, a `copy_in` value, a
//! checked [`UserAddr`], a [`RawHandle`]. **This is the file to audit when a bug
//! class is about what userland named.** The cwd accumulation and the derived
//! allocations both lived in an arm here, and neither was findable while these
//! arms shared a file with every handler they call.
//!
//! A count userland chose, multiplied by a stride, is the derived-allocation
//! shape and it is written the same way at every site:
//! `checked_mul(..).and_then(|len| ctx.user_bytes(..))`. The product is what
//! must not wrap — an overflow is a refusal, never a window the kernel then
//! trusts — and the refusal word is each site's own, which is why the pattern is
//! spelled out per arm rather than hidden behind a helper that would have to
//! pick one.
//!
//! **Six handlers carry a [`SyscallContext`] past this file and decode inside
//! themselves**, each because it has an ordering constraint an arm cannot
//! express: `sys_log_read` demands its capability before it copies the caller's
//! cursor in, `sys_gpu_reset_scanout` mints the buffers before it knows the
//! length to write back, `sys_namespace_build` bounds two counts before it reads
//! either vector, and `sys_dlopen`, `sys_inbox_setup` and `sys_process_stats`
//! each write their answer out only after the work that produced it succeeded.
//! That is the residue and that is the whole of it: everything else below this
//! file is handed memory that has already been bounded.
//!
//! **An arm's `return` leaves the syscall without the epilogue below the
//! match** — the object layer's zero-handle drain and the call's own time
//! accounting. Every decode refusal takes that exit, which is why the match is
//! one function and why an arm calls a handler rather than being one: a handler
//! returning a refusal would run an epilogue that a refusing arm does not.

use crate::object::{ops, KObjectRef};
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{device, process};
// The macro, not the module: `crate::log` exports `log!` at the crate root, and
// only `SYS_DEBUG`'s arms below spell it unqualified.
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

/// The numbers deleted syscalls used, and what each one was.
///
/// **A number a deleted syscall used is retired, never reused.** This was two
/// hand-written `29 | 30 =>` arms with the names in a trailing comment; a third
/// pair would have been a table, and thirteen more arrive with this branch.
///
/// The rows must be strictly ascending, which is checked rather than asked for:
/// it is the whole of what stops one number being retired twice.
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
    // Which CPU is spinning on `syscall` is what `syscall-window-nmi` aims its
    // storm at, and this is the only place that knows. One relaxed load and a
    // predictable branch, ahead of everything else so the call is counted
    // whatever it turns out to be; in a shipping kernel `nmi_gate` does not
    // exist and neither does this line.
    #[cfg(feature = "boot-actuators")]
    crate::nmi_gate::note_syscall();
    let t0 = crate::clock::nanos_since_boot();

    process::with_current_data(|data| {
        data.syscall_total += 1;
        // Clamped rather than guarded: a number this ABI does not issue is
        // still a call the total counted, so it lands in the last bin instead
        // of nowhere.
        data.syscall_counts[(num as usize).min(toyos_abi::syscall::SYSCALL_PROFILE_OTHER)] += 1;
    });

    // SAFETY: current process's page tables remain active for the duration of this call.
    let ctx = unsafe { SyscallContext::new() };

    let bad_addr = SyscallError::BadAddress.to_u64();

    let result = match num {
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
        // **No right.** `fstat` asks the handle what kind of thing it names
        // and how big it is; it moves no content in either direction, so
        // gating it on `READ` would make `isatty(1)` and libc's line-buffering
        // decision fail on every write-only handle they are asked about.
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
        // The object is cloned out and the call runs **outside** the
        // process-data lock, unlike `with_object`'s other users: `ops::fsync`
        // is a blocking call since the slow-vs-failed split — it yields and
        // parks between flush attempts — and a park under `with_process_data`
        // is the §6.4 tripwire by construction. Same shape as `sys_read`'s
        // resolve-then-block loop, taken once because a `File` is never
        // revoked mid-call by anything but `close`, which the clone outlives.
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
            // One pair read out of each window at a time rather than the
            // vectors copied wholesale: both counts are userland's, and a copy
            // would put them on the allocator for a loop that reads each entry
            // exactly once. The label blob is the exception — the child keeps
            // it, so it is copied in and bounded by `MAX_LABELS_LEN`.
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
            // The env blob is kept for the child's whole life, so it needs a
            // bound of its own — `user_vec` is the one accessor that puts a
            // userland-chosen size on the allocator. Same constant as argv:
            // both are userland text the kernel owns a copy of.
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
                    // **Killing yourself is exiting, and `kill_process` cannot
                    // do it.** It retires every thread of its target, and
                    // `retire_task` asserts that a CPU never retires the task
                    // it is running on — so a process holding a `MANAGE`
                    // handle to itself panicked the kernel. Nothing stops one
                    // holding one: `Process` carries `TRANSFER`, so a parent
                    // may send a child the child's own handle.
                    if object.pid() == process::current_process() {
                        // `exit` does not come back and nothing unwinds past
                        // it, so the clone this match is holding is dropped
                        // where it can still be dropped.
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

        // No right either, and for the same reason: its one caller marks
        // both ends of a pair, so a right either end lacks would refuse one
        // of the two.
        SYS_MARK_TTY => with_object(RawHandle(a1 as u32), Rights::NONE, ops::mark_tty),
        // Display integrity, not memory access: framebuffer *contents* are
        // behind shared_memory grants either way. Ungated, any process could
        // scan out over the compositor's frames and move the cursor.
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
        // Both answer out of the anchor `clock` took at boot, so neither
        // touches the CMOS: this used to be a port handshake per call that
        // could block on the update flag for as long as a second, which made
        // `SystemTime::now()` in a loop pathological. `NotSupported` is a
        // machine that never said what time it is — for the life of this boot
        // it does not support being asked, and the alternative is serving a
        // number from 1970 that a caller cannot tell from a real one.
        //
        // Local time in the first and UTC in the second, which is what each
        // has always claimed to be: the wall clock on a screen wants the
        // machine's own zone, and seconds since the epoch are UTC by
        // definition.
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
        // The capability is first, as it is at every other arm that takes one.
        // The buffer decides whether it is looked at: the header is a machine
        // fact like `SYS_CPU_COUNT` and stays ambient, and the roster after it
        // costs `Rights::ROSTER` because it is every process in the machine by
        // name.
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
            // Refused here rather than at the write, so a process that named an
            // address the kernel will not write to is not left holding a
            // library it was never told about.
            let init_out = match a3 {
                0 => None,
                raw => match UserAddr::checked(raw) {
                    Some(addr) => Some(addr),
                    None => return bad_addr,
                },
            };
            sys_dlopen(&ctx, &path, init_out)
        }
        SYS_DLSYM => {
            let name = match ctx.user_str(UserAddr::new(a2), a3) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_dlsym(a1, &name)
        }
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
        // All three drive the NIC's rings, so without the claim any process
        // could pop frames out of the used ring before netd sees them and, by
        // never refilling, exhaust all 256 RX slots.
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
            // Checked before the driver, so a caller with no claim never gets
            // its two arbitrary u32s turned into a contiguous physical
            // allocation.
            let claim_h = RawHandle(a1 as u32);
            if let Err(e) = holds_claim(claim_h, device::DeviceType::Framebuffer) {
                return e.refuse();
            }
            // Checked before the allocation for the same reason the claim is: a
            // caller that named an address the kernel will not write to must
            // not be left with a resolution it is never told about.
            let Some(info_out) = UserAddr::checked(a3) else { return bad_addr };
            let (width, height) = unpair(a2);
            match crate::gpu::set_resolution(width, height) {
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
            // The record count is userland's, so the byte length is computed
            // from it before anything is mapped: a product that does not fit is
            // a caller's argument being wrong, not the kernel's arithmetic
            // wrapping into a window it then trusts.
            let Some(bytes) = (a4 as usize).checked_mul(toyos_abi::log::RECORD_BYTES) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            let Some(mut out) = ctx.user_bytes_mut(UserAddr::new(a3), bytes as u64) else {
                return bad_addr;
            };
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
            sys_namespace_build(&ctx, &args)
        }
        SYS_NAMESPACE_OPEN => {
            let name = match ctx.user_str(UserAddr::new(a2), a3) { Ok(s) => s, Err(e) => return e.to_u64() };
            sys_namespace_open(RawHandle(a1 as u32), &name)
        }
        SYS_TLS_ALLOC_BLOCK => sys_tls_alloc_block(a1),
        SYS_INBOX_SETUP => sys_inbox_setup(&ctx, a1 as u32, a2),
        SYS_INBOX_SUBMIT => {
            sys_inbox_submit(RawHandle(a1 as u32), a2 as u32, a3 as u32, a4)
        }
        SYS_QUERY_MODULES => {
            let Some(mut buf) = ctx.user_bytes_mut(UserAddr::new(a1), a2) else { return bad_addr };
            sys_query_modules(&mut buf)
        }
        // **The whole of `SYS_DEBUG` is here or it is nowhere.** A shipping
        // kernel carries no debug syscall: the number falls to the dispatch's
        // default and answers `InvalidArgument`, which is what an unassigned
        // number answers, so there is nothing for a process to reach and
        // nothing for it to discover. Every action below is a *test's* only
        // route to a state the host cannot stage, and four of them cost the
        // caller its process or the machine its CPUs by design — which is why
        // the feature is the boundary rather than a capability check inside it.
        #[cfg(feature = "test-actuators")]
        SYS_DEBUG => match a1 {
            DA::PANIC => panic!("SYS_DEBUG: kernel panic triggered by userspace"),
            // SAFETY: **this one is unsound by design and that is the whole
            // action** — a null dereference in Ring 0, staged so a test can see
            // what the kernel does with a fault the kernel itself caused. It is
            // behind `test-actuators`, which no shipped kernel is built with.
            // Volatile so the read is actually emitted; a plain one the
            // optimiser may fold to `unreachable` and then nothing faults.
            DA::NULL_READ => { unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()); } 0 }
            DA::LOCK_ACROSS_SWITCH => {
                if !LOCK_ACROSS_SWITCH_ARMED.swap(false, core::sync::atomic::Ordering::Relaxed) {
                    return SyscallError::InvalidArgument.to_u64();
                }
                let _held = LOCK_ACROSS_SWITCH.lock();
                crate::scheduler::yield_now();
                0
            }
            // Not compiled into a kernel anyone ships. Every other action
            // costs the caller its own process; this one costs the machine,
            // and no latch fixes that — one call is already a permanent halt.
            DA::FATAL_HALT => { log!("{}", FATAL_HALT_NONCE); crate::arch::apic::halt_all_cpus(); }
            // A real double fault, produced the way the hardware produces one:
            // fault while RSP cannot be pushed to. The push below raises #SS on
            // a non-canonical stack address, and delivering *that* needs another
            // push to the same RSP, which is the #DF condition. Nothing
            // simulated — the point is to run the IST1 stack, and only the CPU
            // can put us there.
            //
            // Non-canonical rather than merely unmapped, because "unmapped" is
            // a claim about this machine's memory map that a bigger machine
            // falsifies quietly: an address inside the direct map would simply
            // be written to, and the test would pass having faulted nothing.
            // Only #DF has an IST, so every other vector on the way is
            // delivered onto this same unusable stack.
            DA::DOUBLE_FAULT => {
                log!("SYS_DEBUG: provoking a double fault");
                // SAFETY: unsound by design, like `NULL_READ` above and behind
                // the same feature — the comment on this arm is the argument for
                // why only the hardware can produce the state under test.
                // `options(noreturn)` is honest: the `push` raises `#SS` on a
                // non-canonical `rsp`, delivering that needs another push to the
                // same `rsp`, and the `#DF` that follows never comes back here.
                unsafe {
                    core::arch::asm!(
                        "mov rsp, {bad}",
                        "push 0",
                        bad = in(reg) 0x0000_8000_0000_0000u64,
                        options(noreturn),
                    );
                }
            }
            // Both sides of `mm::MAX_HEAP_ALLOC`, and the alignment corner
            // between them. 5 must succeed, 6 must panic, and 7 — the same
            // size page-aligned, which `memalign` pads past what one page can
            // back — must come back as an error rather than as a panic taken
            // inside the allocator's lock, which is what it used to be.
            DA::HEAP_AT_CEILING => debug_heap_alloc(crate::mm::MAX_HEAP_ALLOC, 8),
            DA::HEAP_OVER_CEILING => debug_heap_alloc(crate::mm::PAGE_2M as usize, 8),
            DA::HEAP_AT_CEILING_PAGE_ALIGNED => debug_heap_alloc(crate::mm::MAX_HEAP_ALLOC, 4096),
            // Returns, unlike every other action here: what is under test is
            // what the *console* does next, so the machine and the process
            // both have to survive being drawn over.
            DA::SCREEN_GRAFFITI => {
                crate::drivers::panic_console::graffiti();
                0
            }
            // A read, not a write: the property under test is that the page is
            // absent, and a read establishes it without the feature also
            // handing userland a kernel store. Returning 0 is the failure —
            // without a guard page that byte is dlmalloc's bookkeeping for the
            // chunk the idle stack lives in, and the read succeeds.
            DA::IDLE_GUARD_READ => {
                let addr = crate::arch::percpu::idle_guard_byte();
                log!("SYS_DEBUG: reading the idle stack guard at {addr:#x}");
                // SAFETY: the third of this feature's deliberate faults. The
                // address is `percpu::idle_guard_byte()`, the last byte of a page
                // `alloc_idle_stack` took out of the direct map, so the read is
                // the `#PF` the test is asserting on — and a kernel *without* the
                // guard returns from it, which is the failure. Volatile because
                // the value is discarded and a plain read would be deleted.
                unsafe { core::ptr::read_volatile(addr as *const u8) };
                0
            }
            DA::CANARY_ADDR => canary::address(),
            DA::CANARY_CHANGED => canary::changed() as u64,
            // Make the last CPU a shootdown waits for answer `a2` nanoseconds
            // late, take one, and answer with how long it took. The number is
            // the gate: an initiator that does not wait measures roughly the
            // cost of one ICR write however slow its siblings are. The arming
            // outlives the call, so the caller can then time an ordinary
            // syscall and learn whether *its* free path shoots down.
            DA::TLB_ACK_DELAY_ARM => crate::arch::tlb::debug_arm_ack_delay(a2),
            DA::TLB_ACK_DELAY_DISARM => crate::arch::tlb::debug_disarm_ack_delay(),
            // The live-object count for one kind. The two leaks this object
            // model accepts — an `Arc` stranded on a killed thread's stack, and
            // the cross-pair connection cycle — are visible in no other way, so
            // the census needs a reader or it is a counter nothing ever reads.
            //
            // Per kind and not a total: a total hides a leak of one kind behind
            // churn in another, and six of the thirteen kinds had no census
            // assertion anywhere in the estate.
            //
            // The names are checked rather than the order assumed: this is the
            // one place two declarations of the same list meet, and an index
            // that quietly names its neighbour is worse than no census.
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
            // Lower `SYS_SYSINFO`'s thread bound to `GATED_SYSINFO_THREADS`
            // for the rest of the boot. The bound itself is unreachable — no
            // guest can make 65,536 threads — and this is the only way to run
            // the refusal against the shipped count, comparison and error
            // return. Armed rather than compiled, because as a `#[cfg]` it
            // travelled into every kernel this suite booted.
            DA::LOWER_SYSINFO_BOUND => {
                SYSINFO_BOUND_LOWERED.store(true, core::sync::atomic::Ordering::Relaxed);
                0
            }
            // Put one of the caller's own free slots one lifecycle from the
            // end, and answer the handle its next install will carry. A slot's
            // counter is twenty bits, so the only other way to a table at the
            // end of one is 1,048,575 close/reopen round trips: the retirement
            // ruling of 2026-08-20 would be gated by a test nobody could afford
            // to run, which is how it went ungated. Nothing here is simulated —
            // what follows the call is the shipped install, the shipped close
            // and `HandleTable::retire`'s own decision.
            //
            // It acts on the caller's own table and confers nothing: a process
            // can already close its own handles, and all this says is which
            // generation the next one comes back at.
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
            sys_process_stats(&ctx, RawHandle(a1 as u32), addr)
        },
        SYS_SET_THREAD_NAME => {
            // `a2` used to be clamped to `THREAD_NAME_LEN` with `.min`, which
            // silently set the truncated prefix of a name too long to fit
            // rather than telling the caller its name did not fit —
            // `issues/isolation/untrusted-sites-not-yet-adopted.md`'s
            // pattern for the whole file. Refused by name instead: a length
            // past the bound is the caller's argument being wrong, not a
            // shorter name to go write.
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
        // A number a deleted syscall used is retired, never reused, so an old
        // binary is told which call it is asking for rather than that its
        // number is nonsense.
        _ => match retired_syscall(num) {
            Some(name) => {
                crate::log!("syscall {num} is retired (formerly {name})");
                SyscallError::NotSupported.to_u64()
            }
            None => SyscallError::InvalidArgument.to_u64(),
        },
    };

    // The first of the object layer's three drain sites. Here rather than at
    // the drop that queued it: a hook must not run under whatever guard the
    // syscall was holding when the last handle went (`object::drain_zero_handles`).
    crate::object::drain_zero_handles();

    // Track wall-clock syscall time (includes preemption)
    let elapsed = crate::clock::nanos_since_boot() - t0;
    process::with_current_data(|data| {
        data.syscall_total_ns += elapsed;
    });

    result
}

/// The two `u32`s a device call packs into one argument word, taken apart at
/// the boundary and carried no further.
fn unpair(word: u64) -> (u32, u32) {
    ((word >> 32) as u32, word as u32)
}
