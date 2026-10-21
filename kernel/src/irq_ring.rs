//! Per-CPU IRQ event records: ISRs stamp `(cpu, source)` with
//! `nanos_since_boot` and set `need_resched`; the next scheduler entry
//! on that CPU drains it into wakes. One coalescing slot per
//! `(cpu, source)`, 0 meaning empty and never a real timestamp.
//! Same-CPU only (indexed by `percpu::cpu_id()`), so Relaxed suffices.

use core::sync::atomic::{AtomicU64, Ordering};


/// Interrupt sources that drive scheduling; exhaustive, so a new variant requires updating every `match`.
/// The discriminant is `trace::Kind::IrqDrain`'s top byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IrqSource {
    Xhci = 2,
}

impl IrqSource {
    pub const COUNT: usize = 1;

    /// This source's record in a CPU's slots.
    const fn slot(self) -> usize {
        match self {
            Self::Xhci => 0,
        }
    }
}

/// A CPU's slot per source, written by that CPU's handlers and taken by that CPU.
pub struct Local([AtomicU64; IrqSource::COUNT]);

impl Local {
    pub fn new() -> Self {
        Self([const { AtomicU64::new(0) }; IrqSource::COUNT])
    }
}

fn slot(source: IrqSource) -> &'static AtomicU64 {
    &crate::cpu_local::here().irq_ring.0[source.slot()]
}

/// Records an IRQ on the current CPU (ISR-only); coalesces into the earlier timestamp.
pub fn isr_publish(source: IrqSource, timestamp_nanos: u64) {
    // MSI-X vectors are configured after clock calibration, so a real IRQ never stamps 0.
    assert!(timestamp_nanos != 0, "irq_ring: zero IRQ timestamp");
    let slot = slot(source);
    // ISRs run with IF=0, so this load-then-store can't interleave with a same-CPU `take`.
    if slot.load(Ordering::Relaxed) == 0 {
        slot.store(timestamp_nanos, Ordering::Relaxed);
    }
}

/// Consumes the current CPU's pending record for `source`, returning its IRQ-time timestamp.
pub fn take(source: IrqSource) -> Option<u64> {
    let slot = slot(source);
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
    slot(source).load(Ordering::Relaxed) != 0
}
