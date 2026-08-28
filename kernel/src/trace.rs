//! Kernel event tracing — per-CPU ring buffer of scheduler/timer/IRQ events,
//! dumped from LLDB on a wedged kernel via the `TRACE_RINGS` symbol.
//! Writer is single-CPU but tolerates interrupt recursion via `fetch_add` on
//! the head index; payload writes are non-atomic, so a reader may see a torn
//! record for the most recent entry.
//!
//! [`Record`] is the `repr(C)` wire form of `toyos_sched::hw::TraceEvent`;
//! [`record`] maps one onto the other and is `Machine::trace`.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

use toyos_sched::hw::{TraceEvent, TraceKind};

use crate::arch::percpu;

pub const RING_CAPACITY: usize = 4096;

/// Event kind discriminant, stable — do not reorder; read by LLDB as a raw `u16`.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `pid`/`tid` name the incoming task; `data` = 0.
    SchedPick   = 1,
    SchedIdle   = 2,  // data = next_deadline_ms_low
    Preempt     = 3,  // data = 0
    Block       = 4,  // data = 0
    Wake        = 5,  // data = 0
    TimerArm    = 6,  // data = nanos (low 32)
    TimerStop   = 7,  // data = 0
    TimerFire   = 8,  // data = 0
    Mark        = 9,  // data = user-defined
    IdleEnter   = 10, // data = 0 — about to halt; the armed deadline is in TimerArm
    IdleExit    = 11, // data = 0 — ring observed cpu woke from halt
    /// Burst summary of Ring 0 timer fires since the last kernel→user transition; `data` = fire count.
    /// Batched so a demand-paging burst does not drown the ring buffer.
    TimerFireBurst = 12,
    /// `data` = IrqSource discriminant in the top byte, latency in µs in the low 24 bits.
    /// Distinct from [`Kind::Irq`]: this is consumption, not entry.
    IrqDrain = 13,
    /// The two-phase wait commit parked the task.
    ParkCommit = 14,
    /// `data` = destination cpu id.
    Migrate = 15,
    Adopt = 16,
    Retire = 17,
    /// The core observed an interrupt. `data` = 0.
    Irq = 18,
}

// Must list every `Kind` variant; `every_kind_is_pinned` below enforces completeness.
// Renumbering would silently relabel events in traces already captured off metal, which cannot be re-read.
const _: () = {
    assert!(Kind::SchedPick as u16 == 1);
    assert!(Kind::SchedIdle as u16 == 2);
    assert!(Kind::Preempt as u16 == 3);
    assert!(Kind::Block as u16 == 4);
    assert!(Kind::Wake as u16 == 5);
    assert!(Kind::TimerArm as u16 == 6);
    assert!(Kind::TimerStop as u16 == 7);
    assert!(Kind::TimerFire as u16 == 8);
    assert!(Kind::Mark as u16 == 9);
    assert!(Kind::IdleEnter as u16 == 10);
    assert!(Kind::IdleExit as u16 == 11);
    assert!(Kind::TimerFireBurst as u16 == 12);
    assert!(Kind::IrqDrain as u16 == 13);
    assert!(Kind::ParkCommit as u16 == 14);
    assert!(Kind::Migrate as u16 == 15);
    assert!(Kind::Adopt as u16 == 16);
    assert!(Kind::Retire as u16 == 17);
    assert!(Kind::Irq as u16 == 18);
};

// No wildcard arm: adding a `Kind` variant must break this compile, not silently match.
#[allow(dead_code)]
const fn every_kind_is_pinned(kind: Kind) {
    match kind {
        Kind::SchedPick
        | Kind::SchedIdle
        | Kind::Preempt
        | Kind::Block
        | Kind::Wake
        | Kind::TimerArm
        | Kind::TimerStop
        | Kind::TimerFire
        | Kind::Mark
        | Kind::IdleEnter
        | Kind::IdleExit
        | Kind::TimerFireBurst
        | Kind::IrqDrain
        | Kind::ParkCommit
        | Kind::Migrate
        | Kind::Adopt
        | Kind::Retire
        | Kind::Irq => (),
    }
}

/// Wire record: `repr(C)`, 24 bytes; field order chosen for LLDB hexdump reading.
#[repr(C)]
pub struct Record {
    pub timestamp_ns: u64,
    pub kind: u16,
    pub cpu: u8,
    pub _pad: u8,
    pub pid: u32,
    pub tid: u32,
    pub data: u32,
}

const _: () = assert!(core::mem::size_of::<Record>() == 24);

const EMPTY_EVENT: Record = Record {
    timestamp_ns: 0,
    kind: 0,
    cpu: 0,
    _pad: 0,
    pid: 0,
    tid: 0,
    data: 0,
};

#[repr(C, align(64))]
pub struct TraceRing {
    pub head: AtomicU64, // monotonic; slot index = head % RING_CAPACITY
    pub events: UnsafeCell<[Record; RING_CAPACITY]>,
}

// SAFETY: atomic slot allocation partitions writers; torn reads on the newest entry are tolerated.
unsafe impl Sync for TraceRing {}

impl TraceRing {
    const fn new() -> Self {
        Self {
            head: AtomicU64::new(0),
            events: UnsafeCell::new([EMPTY_EVENT; RING_CAPACITY]),
        }
    }
}

#[no_mangle]
pub static TRACE_RINGS: [TraceRing; crate::sched::MAX_CPUS] = [
    TraceRing::new(), TraceRing::new(), TraceRing::new(), TraceRing::new(),
    TraceRing::new(), TraceRing::new(), TraceRing::new(), TraceRing::new(),
];

/// Globally enable/disable tracing. Starts off; set to true once clock is up.
static ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

/// Records a trace event on the current CPU; wait-free and safe from any context, no-op until `enable()` is called.
#[inline]
pub fn trace(kind: Kind, data: u32) {
    let tid = percpu::current_tid().map_or(u32::MAX, |t| t.raw());
    let pid = percpu::current_pid().map_or(u32::MAX, |p| p.raw());
    push(percpu::cpu_id(), crate::clock::nanos_since_boot(), kind, pid, tid, data);
}

// `cpu` is explicit, not `cpu_id()`, because a wrong value is the only way to break the ring's per-CPU single-writer property.
#[inline]
fn push(cpu: u32, timestamp_ns: u64, kind: Kind, pid: u32, tid: u32, data: u32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let cpu = cpu as usize;
    if cpu >= crate::sched::MAX_CPUS { return; }
    let ring = &TRACE_RINGS[cpu];
    let slot = ring.head.fetch_add(1, Ordering::Relaxed) as usize % RING_CAPACITY;

    // SAFETY: fetch_add gives each IRQ-vs-kernel writer on this CPU a distinct slot.
    unsafe {
        let slot_ptr = (*ring.events.get()).as_mut_ptr().add(slot);
        core::ptr::write(slot_ptr, Record {
            timestamp_ns,
            kind: kind as u16,
            cpu: cpu as u8,
            _pad: 0,
            pid,
            tid,
            data,
        });
    }
}

/// Encodes a scheduler-core event into the ring; no wildcard arm, so a new `TraceKind` variant fails to compile rather than being silently dropped.
pub fn record(ev: TraceEvent) {
    let (kind, task, data) = match ev.kind {
        TraceKind::Schedule { task } => (Kind::SchedPick, Some(task), 0),
        TraceKind::Wake { task } => (Kind::Wake, Some(task), 0),
        TraceKind::Block { task } => (Kind::Block, Some(task), 0),
        TraceKind::ParkCommit { task } => (Kind::ParkCommit, Some(task), 0),
        TraceKind::Migrate { task, to } => (Kind::Migrate, Some(task), to.0),
        TraceKind::Adopt { task } => (Kind::Adopt, Some(task), 0),
        TraceKind::Retire { task } => (Kind::Retire, Some(task), 0),
        TraceKind::IdleEnter => (Kind::IdleEnter, None, 0),
        TraceKind::IdleExit => (Kind::IdleExit, None, 0),
        TraceKind::Irq => (Kind::Irq, None, 0),
        TraceKind::TimerFire => (Kind::TimerFire, None, 0),
    };
    // `TaskKey` packs `TaskId` with pid in the high half, matching `TaskId::pack`.
    let (pid, tid) = match task {
        Some(key) => ((key.0 >> 32) as u32, key.0 as u32),
        None => (
            percpu::current_pid().map_or(u32::MAX, |p| p.raw()),
            percpu::current_tid().map_or(u32::MAX, |t| t.raw()),
        ),
    };
    push(ev.cpu.0, ev.ts.0, kind, pid, tid, data);
}

/// Record consumption of an `irq_ring` record (see [`Kind::IrqDrain`]).
pub fn trace_irq_drain(source: crate::irq_ring::IrqSource, latency_us: u64) {
    let data = ((source as u32) << 24) | (latency_us.min(0x00FF_FFFF) as u32);
    trace(Kind::IrqDrain, data);
}
