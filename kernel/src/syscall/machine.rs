//! What a process may learn about the machine, and the two things it may do to it.
//!
//! [`sys_log_read`], [`sys_trace_read`], the roster half of [`sys_sysinfo`], and both of
//! [`sys_shutdown`] and [`sys_reboot`] each require a `SysCap` bit from
//! `/system/bin/supervisor`'s `system.toml`; `SYS_SYSINFO`'s header is ambient, and
//! [`sys_sched_info`] demands nothing.

use alloc::vec::Vec;

use crate::log::console::StopWire;
use crate::power;
use crate::user_ptr::{SyscallContext, UserBytesMut};
use crate::UserAddr;
use crate::{log, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::demand_syscap;

/// Copies kernel log records into the caller's buffer; requires a `SysCap` carrying [`Rights::LOG`].
pub(super) fn sys_log_read(
    ctx: &SyscallContext,
    syscap: RawHandle,
    cursor_ptr: UserAddr,
    out: &mut UserBytesMut,
    capacity: usize,
) -> u64 {
    read_on_cursor(ctx, syscap, Rights::LOG, cursor_ptr, |cursor| log::user::read(cursor, out, capacity))
}

/// Copies the diary's records into the caller's buffer; requires a `SysCap` carrying [`Rights::TRACE`].
pub(super) fn sys_trace_read(
    ctx: &SyscallContext,
    syscap: RawHandle,
    cursor_ptr: UserAddr,
    out: &mut UserBytesMut,
    capacity: usize,
) -> u64 {
    read_on_cursor(ctx, syscap, Rights::TRACE, cursor_ptr, |cursor| crate::trace::read(cursor, out, capacity))
}

/// A read of records on a cursor the caller holds, under `need`.
///
/// A copy-out failure after a successful read costs the caller those records; the cursor round-trips through the caller's own memory.
fn read_on_cursor<C: toyos_abi::UserSafe>(
    ctx: &SyscallContext,
    syscap: RawHandle,
    need: Rights,
    cursor_ptr: UserAddr,
    read: impl FnOnce(&mut C) -> Result<usize, SyscallError>,
) -> u64 {
    if let Err(e) = demand_syscap(syscap, need) {
        return e.refuse();
    }
    let mut cursor = match ctx.copy_in::<C>(cursor_ptr) {
        Ok(cursor) => cursor,
        Err(e) => return e.to_u64(),
    };
    let count = match read(&mut cursor) {
        Ok(count) => count,
        Err(e) => return e.to_u64(),
    };
    match ctx.copy_out(cursor_ptr, &cursor) {
        Ok(()) => count as u64,
        Err(e) => e.to_u64(),
    }
}

fn quiesce(last: &str) -> Result<(crate::quiesce::Stopping, StopWire), SyscallError> {
    // Refused by name, and first: nothing below runs twice.
    if !crate::quiesce::claim_the_shutdown() {
        log!("power: this machine is already stopping, so this caller stops with the rest");
        return Err(SyscallError::AlreadyExists);
    }
    // Before the watchdog is disarmed and before a byte is synced: what the
    // control stages is a boot that ran its job list and then stopped, which is
    // the shape the T14 hangs in.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::wedge_before_reset() {
        crate::deadline::stage_a_wedge();
    }
    // The same machine ended by the same bound, with the bus busy rather than
    // idle: this one never stops writing, so the reset lands on a controller
    // that is moving bytes.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::usb_reset_under_load() {
        crate::usb_gate::sweep_under_load();
    }
    let parkable = crate::scheduler::Parkable::at_entry();
    // The console's writer holding the wire as the stop begins, which a
    // shipping `klogd` does at any moment.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::wire_held_across_the_stop() {
        crate::log::console::stage_a_held_wire(&parkable);
    }
    // First: what follows outlasts a feed cadence, and no pass runs to feed again.
    crate::arch::watchdog::disarm();
    // Every userland thread stops here, the log's writer with the rest:
    // `/system/bin/supervisor` had it flush before it asked for this stop.
    let (stopped, stopping) = crate::quiesce::stop();
    // From here on nothing carries a record to a file: the seal below takes
    // every one stamped after this one, the newest the stop found.
    let stop_began = crate::log::read::newest_committed();
    // Held to the machine's end: every record below reaches the wire before a
    // CPU is taken down, whoever was writing it when the stop began.
    let wire = crate::log::console::take_for_the_stop(&parkable);
    // The machine's census, which no process's start or end takes.
    crate::census::log();
    #[cfg(feature = "mask-windows")]
    crate::windows::report();
    // A shortfall is the budget spent, not the reset refused: it is said at
    // alert level, and the reset lands anyway.
    if stopped.stopped_the_machine() {
        log!("{stopped}");
    } else {
        crate::alert!("{stopped}");
    }
    // Above the boot's last word, because these are ordinary records and the
    // volume that carries them is still there: every USB disk's write cache is
    // emptied and waited for before anything is taken down.
    crate::drivers::xhci::flush_disks();
    log!("{last}");
    // Order is load-bearing: the console drain, the seal, then the caller's
    // non-returning call.
    wire.drain();
    // The next boot's loader reads this page to learn how the last one ended,
    // and a machine that was asked to stop is the one answer that is not a
    // death. Without it the loader would find the loader's own `ARMED` and
    // report a kernel that vanished. The reset's own account is appended under
    // it.
    crate::blackbox::record_done();
    // Under that seal, because it extends it: the stop's own records, the
    // census and the last word among them. Nothing wrote them to `/log`,
    // and on a machine with no serial port this is their only reader.
    crate::log::seal_tail(stop_began);
    // Whether or not the volume got them: a stick this boot's transport broke
    // on is a stick the next host may not be able to read the log off.
    crate::blackbox::append_recovery();
    // **The barrier, and last of all.** Below the sync, because before it the
    // volumes still have bytes to take and this takes the controller away from
    // them; and after everything else here,
    // because nothing may run between it and the register stop
    // `power::reboot`/`power::shutdown` do — which every reset this kernel
    // performs goes through. It is bounded, and the reset follows either way.
    crate::drivers::xhci::seal_shut();
    Ok((stopping, wire))
}

/// Powers the machine off; requires a `SysCap` carrying [`Rights::POWER`]. Returns only when refused.
// The power-off is demanded before anything is torn down, as the reset is below.
pub(super) fn sys_shutdown(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::POWER) {
        return e.refuse();
    }
    if let Some(why) = power::shutdown_refused() {
        log!("shutdown: {why} — refused");
        return SyscallError::NotSupported.to_u64();
    }
    match quiesce("Shutting down.") {
        Ok((stopping, wire)) => power::shutdown(stopping, wire),
        Err(e) => e.to_u64(),
    }
}

/// Returns the machine to firmware; requires a `SysCap` carrying [`Rights::POWER`]. Returns only when refused.
// The reset is demanded before anything is torn down: a machine without one is left running, not synced, stopped and still on.
pub(super) fn sys_reboot(syscap: RawHandle) -> u64 {
    if let Err(e) = demand_syscap(syscap, Rights::POWER) {
        return e.refuse();
    }
    if !power::can_reboot() {
        log!("reboot: this machine has no reset this kernel performs — refused");
        return SyscallError::NotSupported.to_u64();
    }
    match quiesce("Rebooting.") {
        Ok((_, wire)) => power::reboot(wire),
        Err(e) => e.to_u64(),
    }
}

/// The most live threads `SYS_SYSINFO` will describe; kept under `mm::MAX_HEAP_ALLOC` so an unbounded thread count cannot trip the allocator's fail-fast assert.
const MAX_SYSINFO_THREADS: usize = 65_536;

/// How far past the machine's live threads at arming `DA::LOWER_SYSINFO_BOUND` puts the bound.
#[cfg(feature = "test-actuators")]
const LOWERED_SYSINFO_HEADROOM: usize = 16;

/// `MAX_SYSINFO_THREADS` until `DA::LOWER_SYSINFO_BOUND` lowers it for the rest of the boot.
#[cfg(feature = "test-actuators")]
static SYSINFO_BOUND: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(MAX_SYSINFO_THREADS);

/// Lowers [`sys_sysinfo`]'s bound to the machine's own live threads plus a fixed headroom, counted as `sys_sysinfo` counts them.
#[cfg(feature = "test-actuators")]
pub(super) fn lower_sysinfo_bound() {
    let guard = process::PROCESS_TABLE.lock();
    let live = live_threads(guard.as_ref().unwrap());
    SYSINFO_BOUND.store(live + LOWERED_SYSINFO_HEADROOM, core::sync::atomic::Ordering::Relaxed);
}

/// What [`sys_sysinfo`] compares against on this boot.
fn sysinfo_thread_bound() -> usize {
    #[cfg(feature = "test-actuators")]
    return SYSINFO_BOUND.load(core::sync::atomic::Ordering::Relaxed);
    #[cfg(not(feature = "test-actuators"))]
    MAX_SYSINFO_THREADS
}

/// Every thread in the process table, zombies included: the roster's entries.
fn live_threads(table: &process::ProcessTable) -> usize {
    table.iter().map(|(_, proc)| proc.threads().iter().count()).sum()
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
    let cpu_count = crate::smp::cpu_count();
    let uptime = crate::clock::nanos_since_boot();
    let total_cpu_ns = crate::scheduler::total_cpu_ns();
    let total_available_ns = uptime * cpu_count as u64;

    let guard = process::PROCESS_TABLE.lock();
    let table = guard.as_ref().unwrap();

    let entry_count = live_threads(table) as u32;
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

/// Every online CPU's counters into `out`, or the CPU count when `out` is
/// empty: requires a `SysCap` carrying `Rights::COUNTERS`, and answers the
/// counters that time programs and the power envelope only where it carries
/// `Rights::TRACE` too.
pub(super) fn sys_counters(syscap: RawHandle, out: &mut UserBytesMut) -> u64 {
    let rights = match demand_syscap(syscap, Rights::COUNTERS) {
        Ok(rights) => rights,
        Err(e) => return e.refuse(),
    };
    match crate::counters::read(rights, out) {
        Ok(records) => records as u64,
        Err(e) => e.to_u64(),
    }
}

/// The most records one `SYS_DEVICE_INVENTORY` buffer may be declared to hold,
/// 64 KiB of them: a declared length past it is refused before it becomes a
/// window, and a machine with more records than this is refused by name rather
/// than answered in part.
pub(super) const MAX_INVENTORY_RECORDS: u64 = 1024;

/// Every inventory record, into `out`, or nothing: requires a `SysCap`
/// carrying `Rights::INVENTORY`. An empty `out` answers the count; one too
/// short for every record is refused whole with `ResourceExhausted`.
pub(super) fn sys_device_inventory(syscap: RawHandle, out: &mut UserBytesMut) -> u64 {
    use toyos_abi::inventory::RECORD_BYTES;
    // Demanded before anything is collected: `refuse` takes the process down
    // and walks the tables `collect` locks.
    if let Err(e) = demand_syscap(syscap, Rights::INVENTORY) {
        return e.refuse();
    }
    let records = crate::inventory::collect();
    if out.is_empty() {
        return records.len() as u64;
    }
    if out.len() < records.len() * RECORD_BYTES {
        return SyscallError::ResourceExhausted.to_u64();
    }
    for (i, record) in records.iter().enumerate() {
        out.write_at(i * RECORD_BYTES, &record.encode().0);
    }
    records.len() as u64
}
