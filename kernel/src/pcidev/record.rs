//! One claimed function's interrupt record: what its ISR writes, and what its
//! holder reads back.
//!
//! No `crate::` references: `kernel-loom` compiles this file directly under
//! `feature = "loom"`, because x86's TSO hides a missing acquire edge from
//! every guest test this suite runs.
//!
//! **Two parties race**: the ISR, on whichever CPU the unit routed the message
//! to, and the holder reading its record through a syscall on any CPU. The
//! scheduler pass that turns a message into a wake runs on the ISR's own CPU
//! after it, so those two do not interleave.
//!
//! **The invariant is that a count carries a timestamp.** A record answering
//! "two interrupts, at nanosecond zero" is a driver told its device spoke
//! before the machine started, and the only thing between the two fields is one
//! release edge — the count's — carrying the timestamp store before it.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

// Kernel builds never enable `device-irq-relaxed`.
#[cfg(not(feature = "device-irq-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(not(feature = "device-irq-relaxed"))]
const OBSERVE: Ordering = Ordering::Acquire;
#[cfg(feature = "device-irq-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;
#[cfg(feature = "device-irq-relaxed")]
const OBSERVE: Ordering = Ordering::Relaxed;

/// What the ISR writes and the claim reads back.
///
/// Atomics only: the handler takes no lock and allocates nothing.
pub struct Interrupt {
    /// Messages since the holder's last read.
    count: AtomicU32,
    /// When the most recent of them was taken.
    ///
    /// **The most recent and not the first**: a field the ISR *overwrites* is
    /// non-zero for every count a reader can observe, while one reserved for
    /// the first of a set is cleared by the reader that took the set and leaves
    /// the next count with nothing.
    at_nanos: AtomicU64,
    /// Set by the ISR, cleared by the scheduler pass that turns it into a wake.
    /// Same CPU as the ISR and after it, so nothing here races.
    pending: AtomicBool,
    /// The unit refused this function an access. Every call the claim answers
    /// refuses from here on: its bus mastering is gone, so a driver that kept
    /// going would be driving nothing.
    faulted: AtomicBool,
}

impl Interrupt {
    /// Must stay `const`: the slots are a `static` array.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            at_nanos: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
        }
    }

    // No `Default`: `Default::default` cannot be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            at_nanos: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
        }
    }

    /// Record one message. Called from the vector's ISR, so it takes no lock
    /// and allocates nothing.
    ///
    /// The timestamp is stored **before** the count, whose store is the
    /// release: a reader that sees the count has the timestamp that went with it.
    pub fn took(&self, at_nanos: u64) {
        self.at_nanos.store(at_nanos, Ordering::Relaxed);
        self.count.fetch_add(1, PUBLISH);
        self.pending.store(true, PUBLISH);
    }

    /// The messages since the last read and when the last of them landed, or
    /// `None` for none.
    pub fn take(&self) -> Option<(u32, u64)> {
        let count = self.count.swap(0, OBSERVE);
        if count == 0 {
            return None;
        }
        Some((count, self.at_nanos.load(Ordering::Relaxed)))
    }

    /// Whether a message is waiting, for a readiness check that consumes
    /// nothing.
    pub fn armed(&self) -> bool {
        self.count.load(OBSERVE) != 0
    }

    /// Whether a wake is owed, taken at most once per message. Answers `true`
    /// for the pass that owes it and `false` for every pass after.
    pub fn take_pending(&self) -> bool {
        self.pending.swap(false, OBSERVE)
    }

    /// The unit refused this function an access. Called from the fault handler,
    /// which takes no lock: one store, and every call the claim answers refuses
    /// from here on.
    pub fn fault(&self) {
        self.faulted.store(true, PUBLISH);
    }

    pub fn faulted(&self) -> bool {
        self.faulted.load(OBSERVE)
    }

    /// Back to the state a fresh slot is in, for a claim being minted or given
    /// up. The holder is not running at either point.
    pub fn clear(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.at_nanos.store(0, Ordering::Relaxed);
        self.pending.store(false, Ordering::Relaxed);
        self.faulted.store(false, Ordering::Relaxed);
    }
}
