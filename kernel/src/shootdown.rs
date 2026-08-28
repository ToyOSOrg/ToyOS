//! The acknowledgement half of a TLB shootdown, with no hardware in it.
//! Compiled a second time into `kernel-loom/` against loom's atomics, so this file must hold no `crate::` references.
//! The read must happen before the flush, or a target could publish a generation its flush has not yet completed.

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

/// Which shootdown a flush answers for: monotonic and machine-wide, so one target's flush answers every initiator waiting on it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Generation(u64);

pub struct Shootdown {
    requested: AtomicU64,
    flushed: [AtomicU64; MAX_CPUS],
}

impl Shootdown {
    /// Must stay `const`: the kernel's single instance is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            flushed: [const { AtomicU64::new(0) }; MAX_CPUS],
        }
    }

    // Not `const`: loom's atomics have no const constructor.
    // No `Default` impl: it cannot be `const`, which the kernel's `static` requires.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            flushed: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Names the generation every other CPU now owes a flush for.
    pub fn issue(&self) -> Generation {
        // AcqRel, not Release: the acquire half stops a later load being hoisted above this fetch_add.
        Generation(self.requested.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// A target's whole side of the protocol.
    pub fn serve(&self, cpu: usize, flush: impl FnOnce()) {
        let owed = self.requested.load(OWED);
        flush();
        self.flushed[cpu].store(owed, Ordering::Release);
    }

    /// Does `cpu` owe anyone a flush?
    pub fn owes(&self, cpu: usize) -> bool {
        // A hint: `serve` carries the real ordering, so a stale `false` costs nothing.
        self.requested.load(Ordering::Relaxed) > self.flushed[cpu].load(Ordering::Relaxed)
    }

    /// Has `cpu` flushed since `generation` was issued?
    pub fn served(&self, cpu: usize, generation: Generation) -> bool {
        // Acquire: nothing reads through this edge yet, but `Relaxed` here would be silently unsafe once something does.
        self.flushed[cpu].load(Ordering::Acquire) >= generation.0
    }

    /// [`serve`](Self::serve) for a CPU not taking the interrupt.
    pub fn serve_if_owed(&self, cpu: usize, flush: impl FnOnce()) {
        if self.owes(cpu) {
            self.serve(cpu, flush);
        }
    }

    /// One turn of an initiator's wait for `cpu`: answer first, then ask.
    pub fn wait_turn(
        &self,
        me: usize,
        cpu: usize,
        generation: Generation,
        flush: impl FnOnce(),
    ) -> bool {
        // IF is masked here, so this CPU must serve itself or two initiators waiting on each other deadlock.
        self.serve_if_owed(me, flush);
        // Order is the fix: serving first is what publishes the generation a concurrent sibling is waiting on.
        self.served(cpu, generation)
    }
}
