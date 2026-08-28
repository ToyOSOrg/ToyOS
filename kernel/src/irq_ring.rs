//! Per-CPU IRQ event records: ISRs stamp `(cpu, source)` with
//! `nanos_since_boot` and set `need_resched`; the next scheduler entry
//! on that CPU drains it into wakes. One coalescing slot per
//! `(cpu, source)`, 0 meaning empty and never a real timestamp.
//! Same-CPU only (indexed by `percpu::cpu_id()`), so Relaxed suffices.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu;
use crate::scheduler::MAX_CPUS;

/// Interrupt sources that drive scheduling; exhaustive, so a new variant requires updating every `match`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IrqSource {
    Audio,
    Net,
    Xhci,
    I8042,
}

impl IrqSource {
    pub const COUNT: usize = 4;
}

/// 64-byte aligned so two CPUs' slots never share a cache line.
#[repr(align(64))]
struct CpuSlots([AtomicU64; IrqSource::COUNT]);

static SLOTS: [CpuSlots; MAX_CPUS] =
    [const { CpuSlots([const { AtomicU64::new(0) }; IrqSource::COUNT]) }; MAX_CPUS];

/// Records an IRQ on the current CPU (ISR-only); coalesces into the earlier timestamp.
pub fn isr_publish(source: IrqSource, timestamp_nanos: u64) {
    // MSI-X vectors are configured after clock calibration, so a real IRQ never stamps 0.
    assert!(timestamp_nanos != 0, "irq_ring: zero IRQ timestamp");
    let slot = &SLOTS[percpu::cpu_id() as usize].0[source as usize];
    // ISRs run with IF=0, so this load-then-store can't interleave with a same-CPU `take`.
    if slot.load(Ordering::Relaxed) == 0 {
        slot.store(timestamp_nanos, Ordering::Relaxed);
    }
}

/// Consumes the current CPU's pending record for `source`, returning its IRQ-time timestamp.
pub fn take(source: IrqSource) -> Option<u64> {
    let slot = &SLOTS[percpu::cpu_id() as usize].0[source as usize];
    // Atomic swap: an interrupting ISR sees either the old record or the cleared slot, never a torn value.
    match slot.swap(0, Ordering::Relaxed) {
        0 => None,
        ts => {
            let latency_us = crate::clock::nanos_since_boot().saturating_sub(ts) / 1_000;
            crate::trace::trace_irq_drain(source, latency_us);
            Some(ts)
        }
    }
}

/// True if `source` has an undrained record on this CPU; non-consuming, unlike [`take`].
pub fn pending(source: IrqSource) -> bool {
    SLOTS[percpu::cpu_id() as usize].0[source as usize].load(Ordering::Relaxed) != 0
}

/// True if any IRQ record is undrained on the current CPU; non-consuming.
pub fn any_pending_self() -> bool {
    SLOTS[percpu::cpu_id() as usize]
        .0
        .iter()
        .any(|slot| slot.load(Ordering::Relaxed) != 0)
}
