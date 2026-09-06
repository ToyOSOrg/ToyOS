//! What a process may learn about the machine, and the two things it may do to it.
//!
//! [`sys_log_read`], the roster half of [`sys_sysinfo`], and both of
//! [`sys_shutdown`] and [`sys_reboot`] each require a `SysCap` bit from
//! `/system/bin/init`'s `system.toml`; `SYS_SYSINFO`'s header is ambient, and
//! [`sys_sched_info`] demands nothing.

use alloc::vec::Vec;

use crate::drivers::acpi;
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;
use crate::{log, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::demand_syscap;

/// Copies kernel log records into the caller's buffer; requires a `SysCap` carrying [`Rights::LOG`].
///
/// A copy-out failure after a successful read costs the caller those records; the cursor round-trips through the caller's own memory.
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

fn quiesce(last: &str) {
    // First: what follows outlasts a feed cadence, and no pass runs to feed again.
    crate::drivers::watchdog::disarm();
    log!("Syncing filesystems...");
    // drain_all before sync_all: a closed-but-undrained file's dirty pages are only in the cache, which sync_all would miss.
    crate::writeback::drain_all();
    crate::vfs::lock().sync_all();
    // The final census: no process runs after this to report another.
    crate::irq_census::log_census();
    log!("{last}");
    // Order is load-bearing: wait_for_durable, then drain_inline, then the caller's non-returning call.
    crate::log::wait_for_durable();
    crate::log::console::drain_inline();
}

/// Powers the machine off; requires a `SysCap` carrying [`Rights::POWER`]. Does not return.
pub(super) fn sys_shutdown(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::POWER) {
        return e.refuse();
    }
    quiesce("Shutting down.");
    acpi::shutdown();
}

/// Returns the machine to firmware; requires a `SysCap` carrying [`Rights::POWER`]. Returns only when refused.
// The register is demanded before anything is torn down: a machine whose FADT names none is left running, not synced, stopped and still on.
pub(super) fn sys_reboot(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::POWER) {
        return e.refuse();
    }
    if !acpi::can_reboot() {
        log!("reboot: this machine's FADT names no reset register — refused");
        return SyscallError::NotSupported.to_u64();
    }
    quiesce("Rebooting.");
    acpi::reboot();
}

/// The most live threads `SYS_SYSINFO` will describe; kept under `mm::MAX_HEAP_ALLOC` so an unbounded thread count cannot trip the allocator's fail-fast assert.
const MAX_SYSINFO_THREADS: usize = 65_536;

/// Test-only override for `MAX_SYSINFO_THREADS`, armed at runtime by `DA::LOWER_SYSINFO_BOUND` so the shipped bound stays exercised.
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

/// The machine's header, then the live-thread roster for as much of `out` as fits; the roster requires a `SysCap` carrying `Rights::ROSTER`, demanded only when `out` has room for an entry.
pub(super) fn sys_sysinfo(syscap: RawHandle, out: &mut UserBytesMut) -> u64 {
    const HEADER_SIZE: usize = toyos_abi::syscall::SYSINFO_HEADER_SIZE;
    const ENTRY_SIZE: usize = toyos_abi::syscall::SYSINFO_ENTRY_SIZE;
    if out.len() < HEADER_SIZE {
        return SyscallError::InvalidArgument.to_u64();
    }
    let max_entries = (out.len() - HEADER_SIZE) / ENTRY_SIZE;
    if max_entries > 0 {
        // Demanded before the table lock below: `refuse` takes the process down and needs that lock itself.
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

    // Header-only callers pay for no allocation and no sort of the roster below.
    if max_entries == 0 {
        return HEADER_SIZE as u64;
    }

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
