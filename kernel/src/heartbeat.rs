//! Emits `heartbeat: t=... alive=N/M mask=... ran=... gap=...` every quarter
//! second from the idle loop, with `i8042::report_line` printed beside it.
//!
//! `alive=`/`mask=`: this CPU reached a scheduler pass since the last line.
//! `ran=`: tasks dispatched machine-wide since the last line.
//! `gap=`: time since the previous line.
//!
//! Diagnostic only; the shipping kernel does not carry this module.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu;
use crate::sched::MAX_CPUS;

const PERIOD_NS: u64 = 250_000_000;

/// 0 until the first `poll`, which starts the clock without emitting a line.
static LAST_AT: AtomicU64 = AtomicU64::new(0);

/// Per-CPU last scheduler-pass timestamp; never reset.
static TICKED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Per-CPU dispatch count; monotonic, never reset.
static DISPATCHED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Dispatch total at the previous line, making `ran=` a delta.
static LAST_DISPATCHED: AtomicU64 = AtomicU64::new(0);

/// Records that this CPU reached a scheduler pass.
pub fn note_pass() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    if cpu < MAX_CPUS {
        // Not in the timer ISR: an interrupt taken by a CPU that never reaches a pass must not read as healthy.
        TICKED[cpu].store(crate::clock::nanos_since_boot().max(1), Ordering::Relaxed);
    }
}

/// Records that a task is being switched onto this CPU (not the idle context).
pub fn note_dispatch() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    if cpu < MAX_CPUS {
        // Load+store, not fetch_add: only this CPU ever writes this slot.
        let n = DISPATCHED[cpu].load(Ordering::Relaxed);
        DISPATCHED[cpu].store(n + 1, Ordering::Relaxed);
    }
}

/// Emits a heartbeat line if one is due; called from the idle loop.
pub fn poll() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let now = crate::clock::nanos_since_boot();
    let last = LAST_AT.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < PERIOD_NS {
        return;
    }
    // CAS, not store: dedupes the several CPUs that reach this in the same tick.
    if LAST_AT
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let cpus = (crate::arch::smp::cpu_count() as usize).min(MAX_CPUS);
    let dispatched: u64 = (0..cpus).map(|c| DISPATCHED[c].load(Ordering::Relaxed)).sum();
    // Saturating, not `-`: a diagnostic must not be the thing that panics.
    let ran = dispatched.saturating_sub(LAST_DISPATCHED.swap(dispatched, Ordering::Relaxed));
    // Still returns after the bookkeeping above: the first call only starts the clock.
    if last == 0 {
        return;
    }

    // Stamps are sampled once and reused below, so the summary and the per-CPU lines agree.
    let mut stamps = [0u64; MAX_CPUS];
    let mut mask = 0u64;
    let mut alive = 0u32;
    for cpu in 0..cpus {
        stamps[cpu] = TICKED[cpu].load(Ordering::Relaxed);
        // `last` is nonzero here, so this alone excludes a CPU that never reached a pass.
        if stamps[cpu] >= last {
            mask |= 1 << cpu;
            alive += 1;
        }
    }
    let (gs, gms) = split(now - last);
    let (ts, tms) = split(now);
    log!(
        "heartbeat: t={ts}.{tms:03}s alive={alive}/{cpus} mask={mask:#04x} ran={ran} \
         gap={gs}.{gms:03}s"
    );

    // Must not block: report_line uses try_lock and prints `rte=busy` rather than waiting.
    crate::drivers::i8042::report_line();

    for (cpu, &stamp) in stamps.iter().enumerate().take(cpus) {
        if mask & (1 << cpu) != 0 {
            continue;
        }
        match stamp {
            0 => log!("heartbeat: cpu{cpu} has never reached a scheduler pass"),
            stamp => {
                let (s, ms) = split(now.saturating_sub(stamp));
                log!("heartbeat: cpu{cpu} last reached one {s}.{ms:03}s ago");
            }
        }
    }
}

/// Splits into whole seconds and milliseconds, matching the log line's timestamp format.
fn split(nanos: u64) -> (u64, u64) {
    (nanos / 1_000_000_000, (nanos % 1_000_000_000) / 1_000_000)
}
