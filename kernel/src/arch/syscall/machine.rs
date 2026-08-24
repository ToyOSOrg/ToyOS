//! What a process may learn about the machine as a whole, and the one thing it
//! may do to it.
//!
//! **Three of the four ride a right, and the fourth is where the line is
//! drawn.** Every record every CPU wrote ([`sys_log_read`]), the roster of every
//! process by name (the second half of [`sys_sysinfo`]) and cutting the power
//! ([`sys_shutdown`]) each demand a `SysCap` carrying one bit, so what can reach
//! them is exactly what `/bin/init` endowed from `system.toml`. `SYS_SYSINFO`'s
//! *header* — memory, CPU count, uptime — stays ambient, and the caller's buffer
//! is what says which of the two questions it is asking: a buffer with no room
//! for an entry cannot be told anything about another process.
//!
//! [`sys_sched_info`] is the caller's own place in the run queue and demands
//! nothing.

use alloc::vec::Vec;

use crate::drivers::acpi;
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;
use crate::{log, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::demand_syscap;

/// Copy kernel log records into a caller's buffer, presenting a `SysCap` that
/// carries [`Rights::LOG`].
///
/// **Every record every CPU wrote, which is every process's business and no
/// process's right by default** — so it rides a right rather than being
/// ambient, exactly as minting a device claim and entering the RT band do.
///
/// The cursor is the caller's own memory in both directions: read once here,
/// walked by `log::user::read`, written back. **A cursor that cannot be written
/// back costs that caller the records this call took**, and nothing else — it
/// is the caller's own address, mapped a moment ago for the read, so a failure
/// is a process that unmapped its cursor under its own syscall.
pub(super) fn sys_log_read(
    ctx: &SyscallContext,
    syscap: RawHandle,
    cursor_ptr: UserAddr,
    out: &mut UserBytesMut,
    capacity: usize,
) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::LOG) {
        return e.refuse();
    }
    let mut cursor = match ctx.copy_in::<toyos_abi::log::LogCursor>(cursor_ptr) {
        Ok(cursor) => cursor,
        Err(e) => return e.to_u64(),
    };
    let count = match log::user::read(&mut cursor, out, capacity) {
        Ok(count) => count,
        Err(e) => return e.to_u64(),
    };
    match ctx.copy_out(cursor_ptr, &cursor) {
        Ok(()) => count as u64,
        Err(e) => e.to_u64(),
    }
}

/// Power the machine off, presenting a `SysCap` that carries
/// [`Rights::POWER`].
///
/// **The largest authority this kernel has, and the last one that was free.**
/// It took no argument at all: any process that could make a syscall could end
/// every other one, and a daemon endowed exactly one connector held this too.
/// It goes through `demand_syscap`, the prologue the five beside it share, so
/// what can cut the power is exactly what `/bin/init` endowed from
/// `system.toml`, as minting a device claim, entering the RT band, opening a
/// process by pid and reading the log already were.
///
/// The refusal is `HandleError`'s ordinary one and not a special case: a
/// capability that resolves without the bit is `PermissionDenied` and the
/// caller carries on, and a handle the caller does not hold ends it.
///
/// Everything below the check is unchanged and does not come back.
pub(super) fn sys_shutdown(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::POWER) {
        return e.refuse();
    }
    log!("Syncing filesystems...");
    // Drain the write-back queue first: a file closed but not yet drained has
    // its dirty pages only in the cache, so `sync_all` — which commits the
    // devices' own write caches — would miss it. `drain_all` puts every pending
    // file's bytes and metadata on its volume under the VFS lock before this,
    // and pops each entry under that lock so `sync_all` cannot slip into a gap
    // ahead of a flush (`crate::writeback`).
    crate::writeback::drain_all();
    crate::vfs::lock().sync_all();
    // The machine's whole interrupt census, before the last process on it stops
    // being able to say anything. Every other site prints a running total; this
    // is the one that is final.
    crate::irq_census::log_census();
    log!("Shutting down.");
    // **§6.3, in order, and the order is the whole of it.** At the moment they
    // are written these last two lines exist nowhere but the shards, and
    // `acpi::shutdown` does not come back — so a shutdown that loses its own
    // last lines is the one nobody can diagnose, and on a machine with no
    // serial port they exist nowhere else at all.
    //
    // 1. Wait, bounded, for `/bin/logd` to make them durable. **This is
    //    ordinary thread context**, so it yields rather than spins: at
    //    `--smp 1` logd and this caller are the same CPU and a spin here would
    //    guarantee the bound expired every time.
    crate::log::wait_for_durable();
    // 2. The console, after logd has answered, so the last record — including
    //    logd's own — is on the wire before the power goes. Inline, because
    //    `klogd` has no guarantee of another turn.
    //
    // `nvme_large_device` is the gate: it drives `run shutdown` and requires
    // both lines above on the host's serial capture, so a shutdown that went
    // back to leaving them in the ring reds a test rather than going unnoticed
    // on the one machine that has no other channel out.
    crate::log::console::drain_inline();
    acpi::shutdown();
}

/// The most live threads `SYS_SYSINFO` will describe.
///
/// A *derived* collection, in the sense the loader's relocation index is: the
/// caller's buffer bounds what is written and bounds nothing about what is
/// built, because the sort needs every entry before the first one can be
/// chosen. One `(Tid, &ProcessEntry, &ThreadEntry)` is 24 bytes and this is
/// one allocation, so it has to stay under `mm::MAX_HEAP_ALLOC` (2,093,056) —
/// which it did not: nothing caps the thread count, and any process may call
/// this, so ~87,000 threads turned an ordinary syscall into the allocator's
/// fail-fast assert.
///
/// 65,536 leaves the allocation at 1,572,864 bytes, a factor of 1.3 under the
/// ceiling, and the reservation below is exact so there is no growth-by-
/// doubling overshoot to absorb. A machine with more live threads than this
/// gets `ResourceExhausted` from `ps`, which is a refusal rather than a
/// kernel panic — the bound is policy, the ceiling it is derived from is not.
const MAX_SYSINFO_THREADS: usize = 65_536;

/// The bound `SYS_DEBUG`'s `DA::LOWER_SYSINFO_BOUND` action puts in its place,
/// so the refusal has a gate.
///
/// Nothing in this harness can make 65,536 threads — each carries a 128 KiB
/// kernel stack, which is 8 GiB of a guest given 128 MiB — so only the number
/// can move, and moving it runs the whole refusal: the count, the comparison
/// and the error return are the shipped ones.
///
/// **Armed at runtime rather than compiled in, and that is the whole of why the
/// action exists.** As a `#[cfg]` it rode into every kernel the suite booted on
/// `test-actuators`' coat-tails, so `SYS_SYSINFO` answered against 16 in every
/// guest and the shipped 65,536 was executed by nothing.
#[cfg(feature = "test-actuators")]
const GATED_SYSINFO_THREADS: usize = 16;

#[cfg(feature = "test-actuators")]
pub(super) static SYSINFO_BOUND_LOWERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// What [`sys_sysinfo`] compares against on this boot.
fn sysinfo_thread_bound() -> usize {
    #[cfg(feature = "test-actuators")]
    if SYSINFO_BOUND_LOWERED.load(core::sync::atomic::Ordering::Relaxed) {
        return GATED_SYSINFO_THREADS;
    }
    MAX_SYSINFO_THREADS
}

/// The machine's header, then the process roster for as much of `out` as is
/// left, presenting a `SysCap` that carries [`Rights::ROSTER`] for the second.
///
/// **Two answers under one number, and only the second is authority.** The
/// header is total and used memory, the CPU count, the live-thread count, the
/// uptime and the two accumulators a CPU percentage comes out of — a machine
/// fact like `SYS_CPU_COUNT`, and `free`, the compositor's taskbar and netd's
/// memory budget all read it and nothing else. The entries after it are one per
/// live thread, each carrying a pid, a scheduler state, a resident size, an
/// accumulated CPU time and a 28-byte **name**: a census of everything the
/// machine is running, which was ambient until the owner ruled on 2026-08-20
/// that it rides a right. A process endowed one connector learned the name,
/// size and CPU share of every daemon and every program the user had open.
///
/// **The buffer says which of the two is being asked for**, because that is
/// what it already said: a buffer with no room for an entry cannot be told
/// anything about another process, so nothing is demanded of `syscap` and a
/// header-only caller passes `HANDLE_INVALID`. `max_entries` is the whole of
/// that decision and it is taken here, above the demand, so the two can never
/// disagree about which call this is.
///
/// The demand goes through `demand_syscap`, as at the five arms beside this, and
/// its refusal is `HandleError`'s ordinary one: a capability that resolves
/// without the bit is `PermissionDenied` and the caller carries on, and a handle
/// the caller does not hold ends it. It is demanded here, before the table lock,
/// because `refuse` takes the process down and needs that lock itself.
///
/// [`Rights::ROSTER`]: toyos_abi::handle::Rights::ROSTER
pub(super) fn sys_sysinfo(syscap: RawHandle, out: &mut UserBytesMut) -> u64 {
    const HEADER_SIZE: usize = toyos_abi::syscall::SYSINFO_HEADER_SIZE;
    const ENTRY_SIZE: usize = toyos_abi::syscall::SYSINFO_ENTRY_SIZE;
    if out.len() < HEADER_SIZE {
        return SyscallError::InvalidArgument.to_u64();
    }
    let max_entries = (out.len() - HEADER_SIZE) / ENTRY_SIZE;
    if max_entries > 0 {
        if let Err(e) = demand_syscap(syscap, Rights::ROSTER) {
            return e.refuse();
        }
    }

    let (total_mem, used_mem) = crate::mm::pmm::stats();
    let cpu_count = crate::arch::smp::cpu_count();
    let uptime = crate::clock::nanos_since_boot();
    let total_cpu_ns = crate::scheduler::total_cpu_ns();
    let total_available_ns = uptime * cpu_count as u64;

    let guard = process::PROCESS_TABLE.lock();
    let table = guard.as_ref().unwrap();

    let entry_count: u32 = table.iter().flat_map(|(_, proc)| proc.threads().iter().map(move |(tid, thread)| (tid, proc, thread))).count() as u32;
    if entry_count as usize > sysinfo_thread_bound() {
        return SyscallError::ResourceExhausted.to_u64();
    }

    let mut header = [0u8; HEADER_SIZE];
    header[0..8].copy_from_slice(&total_mem.to_le_bytes());
    header[8..16].copy_from_slice(&used_mem.to_le_bytes());
    header[16..20].copy_from_slice(&cpu_count.to_le_bytes());
    header[20..24].copy_from_slice(&entry_count.to_le_bytes());
    header[24..32].copy_from_slice(&uptime.to_le_bytes());
    header[32..40].copy_from_slice(&total_cpu_ns.to_le_bytes());
    header[40..48].copy_from_slice(&total_available_ns.to_le_bytes());
    out.write_at(0, &header);

    // **The ambient call ends here, having built no roster at all.** Every
    // header-only caller in the tree — `free`, the compositor's taskbar, netd's
    // memory budget — used to pay for a `Vec` of every thread in the machine
    // and a sort of it, to write nothing out of either. It is also what makes
    // the demand above a fact about the whole path rather than about the write
    // loop: with no room for an entry, nothing about another process is
    // collected, let alone copied.
    if max_entries == 0 {
        return HEADER_SIZE as u64;
    }

    // Collect and sort by (pid, tid) for stable output. Reserved exactly from
    // the count above, so the buffer is `entry_count * 24` and not whatever
    // the next doubling step would have been.
    let mut entries: Vec<(process::Tid, &process::ProcessEntry, &process::ThreadEntry)> =
        Vec::with_capacity(entry_count as usize);
    entries.extend(table.iter().flat_map(|(_, proc)| proc.threads().iter().map(move |(tid, thread)| (tid, proc, thread))));
    entries.sort_by_key(|(tid, proc, _)| (proc.pid(), *tid));

    let mut pos = HEADER_SIZE;
    for (i, &(tid, proc, thread)) in entries.iter().enumerate() {
        if i >= max_entries {
            break;
        }

        let state: u8 = if matches!(thread.state(), process::ThreadLocation::Zombie(_)) {
            3
        } else {
            thread.sched().map_or(3, crate::scheduler::task_sched_state)
        };
        let is_thread: u8 = if tid != proc.main_tid() { 1 } else { 0 };

        let memory = if let Some(data) = proc.process_data().try_lock() {
            let demand = data.demand_pages.iter().map(|p| p.size() as u64).sum::<u64>();
            let mmap = data.mmap_regions.iter().filter_map(|r| r._pages.as_ref()).map(|p| p.size() as u64).sum::<u64>();
            let tls = data.elf.dynamic_tls_blocks.values().map(|p| p.size() as u64).sum::<u64>();
            let libs: u64 = data.elf.loaded_libs.iter().map(|l| match &l.memory {
                crate::elf::LibMemory::Owned(alloc) => alloc.size() as u64,
                crate::elf::LibMemory::Shared { rw_alloc, .. } => rw_alloc.size() as u64,
            }).sum();
            demand + mmap + tls + libs
        } else {
            0
        };
        let cpu_ns = thread.sched().map_or(0, crate::scheduler::task_cpu_ns);
        let pid = proc.pid();

        let name = if thread.name()[0] != 0 { thread.name() } else { proc.name() };

        let mut entry = [0u8; ENTRY_SIZE];
        entry[0..4].copy_from_slice(&pid.raw().to_le_bytes());
        entry[4..8].copy_from_slice(&tid.raw().to_le_bytes());
        entry[8] = state;
        entry[9] = is_thread;
        entry[16..24].copy_from_slice(&memory.to_le_bytes());
        entry[24..32].copy_from_slice(&cpu_ns.to_le_bytes());
        entry[32..60].copy_from_slice(name);
        out.write_at(pos, &entry);

        pos += ENTRY_SIZE;
    }

    pos as u64
}

pub(super) fn sys_sched_info() -> toyos_abi::syscall::SchedInfo {
    let pid = process::current_process();
    toyos_abi::syscall::SchedInfo {
        vruntime: crate::scheduler::process_vruntime(pid),
        min_vruntime: crate::scheduler::global_min_vruntime(),
        lag: crate::scheduler::process_lag(pid),
    }
}
