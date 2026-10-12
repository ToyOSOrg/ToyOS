//! A generation one CPU issues and every other acknowledges, with no hardware
//! in it: a TLB shootdown's, and a counters read's round.
//! Compiled a second time into `kernel/loom/` against loom's atomics, so this file must hold no `crate::` references.
//! The read must happen before the answer, or a target could publish a generation its answer does not cover;
//! and an initiator that sees a generation served sees everything that answer wrote.
//! A generation is narrow or wide, and an answer reaches as far as the widest generation it covers: one
//! answer covers every generation owed, so the reach is the target's to read and never the IPI's.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

/// Matches `sched::MAX_CPUS`.
pub const MAX_CPUS: usize = 8;

// Acquire: makes the flush that follows see the initiator's page-table write.
#[cfg(not(feature = "shootdown-serve-relaxed"))]
const OWED: Ordering = Ordering::Acquire;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-serve-relaxed")]
const OWED: Ordering = Ordering::Relaxed;

// Acquire: pairs with `serve`'s Release, so what its closure wrote is visible once a generation reads served.
#[cfg(not(feature = "shootdown-served-relaxed"))]
const SERVED: Ordering = Ordering::Acquire;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-served-relaxed")]
const SERVED: Ordering = Ordering::Relaxed;

/// Which shootdown a flush answers for: monotonic and machine-wide, so one target's flush answers every initiator waiting on it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Generation(u64);

/// How far an answer must reach: what a wide generation asks of it, a narrow one does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    Narrow,
    Wide,
}

pub struct Shootdown {
    requested: AtomicU64,
    /// Wide generations issued, each counted before it is issued.
    wide: AtomicU64,
    flushed: [AtomicU64; MAX_CPUS],
    /// Per CPU, `wide` as the last wide answer it finished read it.
    widened: [AtomicU64; MAX_CPUS],
}

impl Shootdown {
    /// Must stay `const`: the kernel's single instance is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            wide: AtomicU64::new(0),
            flushed: [const { AtomicU64::new(0) }; MAX_CPUS],
            widened: [const { AtomicU64::new(0) }; MAX_CPUS],
        }
    }

    // Not `const`: loom's atomics have no const constructor.
    // No `Default` impl: it cannot be `const`, which the kernel's `static` requires.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            wide: AtomicU64::new(0),
            flushed: core::array::from_fn(|_| AtomicU64::new(0)),
            widened: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Names the generation every other CPU now owes a flush for, of `reach`.
    pub fn issue(&self, reach: Reach) -> Generation {
        if reach == Reach::Wide {
            // Before the generation exists, and Release: a serve that owes the generation reads this
            // count, and an answer that reads the count follows the write it was raised for.
            self.wide.fetch_add(1, Ordering::Release);
        }
        // AcqRel, not Release: the acquire half stops a later load being hoisted above this fetch_add.
        Generation(self.requested.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// A target's whole side of the protocol: `flush` is told how far its answer must reach.
    pub fn serve(&self, cpu: usize, flush: impl FnOnce(Reach)) {
        let owed = self.requested.load(OWED);
        let wide = self.wide.load(Ordering::Acquire);
        let reach = if wide > self.widened[cpu].load(Ordering::Acquire) { Reach::Wide } else { Reach::Narrow };
        flush(reach);
        if reach == Reach::Wide {
            // After the flush: a serve nested inside this one, reading this record first, answers wide itself.
            self.widened[cpu].fetch_max(wide, Ordering::Release);
        }
        // A max, not a store: a serve nested inside this one may already have published a later generation.
        self.flushed[cpu].fetch_max(owed, Ordering::Release);
    }

    /// Does `cpu` owe anyone a flush?
    pub fn owes(&self, cpu: usize) -> bool {
        // A hint: `serve` carries the real ordering, so a stale `false` costs nothing.
        self.requested.load(Ordering::Relaxed) > self.flushed[cpu].load(Ordering::Relaxed)
    }

    /// Has `cpu` flushed since `generation` was issued?
    pub fn served(&self, cpu: usize, generation: Generation) -> bool {
        self.flushed[cpu].load(SERVED) >= generation.0
    }

    /// [`serve`](Self::serve) for a CPU not taking the interrupt; whether it served.
    pub fn serve_if_owed(&self, cpu: usize, flush: impl FnOnce(Reach)) -> bool {
        let owed = self.owes(cpu);
        if owed {
            self.serve(cpu, flush);
        }
        owed
    }

    /// One turn of an initiator's wait for `cpu`: answer first, then ask.
    pub fn wait_turn(
        &self,
        me: usize,
        cpu: usize,
        generation: Generation,
        flush: impl FnOnce(Reach),
    ) -> bool {
        // An initiator inside an `IrqGuard`, a handler or the tick's pass takes no IPI, so this CPU must serve itself or two initiators waiting on each other deadlock.
        self.serve_if_owed(me, flush);
        // Order is the fix: serving first is what publishes the generation a concurrent sibling is waiting on.
        self.served(cpu, generation)
    }
}
