//! The diary: a ring per CPU of `toyos_abi::trace` records, written always from
//! the timer, the scheduler, `irq_ring` and every
//! `kernel::sched::hw::TraceEvent` the core emits, and read by
//! `SYS_TRACE_READ` on `Rights::TRACE`.
//!
//! Each ring is the log's slot protocol ([`crate::log::shard::Ring`]) and is
//! read through the log's walk, so a record is published whole or not at all
//! and a reader's cursor is its own. A record is reserved, stamped with the
//! CPU's raw counter and published under one `IrqGuard`, so a CPU's ring is in
//! stamp order and an interrupt's record lands in a slot of its own.
//!
//! [`record`] maps a `TraceEvent` onto its [`Kind`] and is `Machine::trace`.

use core::sync::atomic::{AtomicBool, Ordering};

use kernel::sched::hw::{TraceEvent, TraceKind};
pub use toyos_abi::trace::Kind;
use toyos_abi::trace::{TraceCursor, TraceRecord, NO_THREAD};
use toyos_abi::syscall::SyscallError;

use crate::arch::{cpu, percpu, IrqGuard};
use crate::log::read::{Rings, Stream};
use crate::log::shard::Ring;
use crate::user_ptr::UserBytesMut;

/// Records per CPU: 256 KiB of 32-byte slots, 2 MiB at eight CPUs.
const SLOTS: usize = 8192;

/// A slot's words past its sequence number: the stamp; the kind, CPU and
/// data; the pid and tid.
const WORDS: usize = 3;

type TraceRing = Ring<WORDS, SLOTS>;

const _: () = assert!(core::mem::size_of::<TraceRing>() == 64 + SLOTS * toyos_abi::trace::RECORD_BYTES);

static RINGS: [TraceRing; crate::sched::MAX_CPUS] = [const { TraceRing::new() }; crate::sched::MAX_CPUS];

/// Off until the clock is up, so no record is stamped before the counter it is read against is.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

/// Records an event on the current CPU against the thread running there; wait-free and safe from any context.
#[inline]
pub fn trace(kind: Kind, data: u32) {
    push(kind, None, data);
}

/// Reserve, stamp and publish under one guard, on the ring of the CPU the guard holds us to.
#[inline]
fn push(kind: Kind, task: Option<(u32, u32)>, data: u32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let guard = IrqGuard::close();
    let at = percpu::cpu_id();
    let Some(ring) = RINGS.get(at as usize) else { return };
    let (pid, tid) = task.unwrap_or_else(|| {
        (
            percpu::current_pid().map_or(NO_THREAD, |p| p.raw()),
            percpu::current_tid().map_or(NO_THREAD, |t| t.raw()),
        )
    });
    // SAFETY: the guard holds this CPU, which owns `ring`, through the publish.
    unsafe {
        let seq = ring.reserve(&guard);
        let stamp = cpu::counter();
        ring.publish(seq, &guard, |body| {
            body[0].store(stamp, Ordering::Relaxed);
            body[1].store(kind as u64 | (at as u64) << 16 | (data as u64) << 32, Ordering::Relaxed);
            body[2].store(pid as u64 | (tid as u64) << 32, Ordering::Relaxed);
        });
    }
}

impl Stream for TraceRing {
    type Record = TraceRecord;
    fn record(&self, seq: u64) -> Option<TraceRecord> {
        self.load(seq, |body| {
            let shape = body[1].load(Ordering::Relaxed);
            let thread = body[2].load(Ordering::Relaxed);
            TraceRecord {
                seq,
                stamp: body[0].load(Ordering::Relaxed),
                kind: shape as u16,
                cpu: (shape >> 16) as u16,
                data: (shape >> 32) as u32,
                pid: thread as u32,
                tid: (thread >> 32) as u32,
            }
        })
    }
}

/// Encodes a scheduler-core event into the ring; no wildcard arm, so a new `TraceKind` variant fails to compile rather than being silently dropped.
pub fn record(ev: TraceEvent) {
    let (kind, task, data) = match ev.kind {
        TraceKind::Schedule { name, .. } => (Kind::Pick, Some(name), 0),
        TraceKind::Wake { name, .. } => (Kind::Wake, Some(name), 0),
        TraceKind::ParkCommit { name, .. } => (Kind::ParkCommit, Some(name), 0),
        TraceKind::Migrate { name, to, .. } => (Kind::Migrate, Some(name), to.0),
        TraceKind::Adopt { name, .. } => (Kind::Adopt, Some(name), 0),
        TraceKind::Retire { name, .. } => (Kind::Retire, Some(name), 0),
        TraceKind::IdleEnter => (Kind::IdleEnter, None, 0),
        TraceKind::TimerFire => (Kind::TimerFire, None, 0),
    };
    // A task's name is its `TaskId::pack`, pid in the high half.
    push(kind, task.map(|name| ((name >> 32) as u32, name as u32)), data);
}

/// Record consumption of an `irq_ring` record (see [`Kind::IrqDrain`]).
pub fn trace_irq_drain(source: crate::irq_ring::IrqSource, latency_us: u64) {
    let data = ((source as u32) << 24) | (latency_us.min(0x00FF_FFFF) as u32);
    trace(Kind::IrqDrain, data);
}

/// Every CPU's ring a reader can name: one per CPU online now.
fn rings() -> Rings<WORDS, SLOTS> {
    let online = crate::smp::cpu_count() as usize;
    core::array::from_fn(|cpu| RINGS.get(cpu).filter(|_| cpu < online))
}

/// Copies records `cursor` has not seen into `out`, oldest first; never blocks.
pub fn read(cursor: &mut TraceCursor, out: &mut UserBytesMut, capacity: usize) -> Result<usize, SyscallError> {
    crate::log::user::read_rings(&rings(), &mut cursor.0, out, capacity, TraceRecord::as_bytes)
}

/// [`Kind::Mark`] records `count` of them on this CPU, `data` counting up, and
/// the counter ticks spent writing them with interrupts closed.
#[cfg(feature = "test-actuators")]
pub fn flood(count: u64) -> u64 {
    /// Records written under one closing of interrupts.
    const BATCH: u64 = 4096;
    let mut ticks = 0;
    let mut written = 0;
    while written < count {
        let batch = BATCH.min(count - written);
        let _closed = IrqGuard::close();
        let from = cpu::counter();
        for i in written..written + batch {
            trace(Kind::Mark, i as u32);
        }
        ticks += cpu::counter() - from;
        written += batch;
    }
    ticks
}
