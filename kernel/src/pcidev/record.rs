//! One claimed function's interrupt record: what its ISR writes, and what its
//! holder reads back.
//!
//! No `crate::` references: `kernel-loom` compiles this file directly under
//! `feature = "loom"`, because what these words prevent is an interleaving and
//! not an ordering, and no guest test in this suite lands on it.
//!
//! **Two parties race**: the ISR, on whichever CPU the unit routed the message
//! to, and the holder reading its record through a syscall on any CPU. The
//! scheduler pass that turns a message into a wake runs on the ISR's own CPU
//! after it, so those two do not interleave.
//!
//! **The invariant is that every message is counted exactly once, and owes
//! exactly one wake.** Both are read-modify-writes and neither is a load
//! followed by a store: a reader that loaded a count and then cleared it drops
//! every message the ISR recorded in between, and a driver that misses one
//! waits for a device that has already spoken. No ordering carries anything
//! across these words — each is the whole of what it says — so the orderings
//! here are `Relaxed` and the model is about the interleaving.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Every word here is the whole of what it says and orders nothing else, so
/// the only property is the interleaving — which `device-irq-lossy` is the
/// control for and `kernel-loom` is the model of.
const ORDER: Ordering = Ordering::Relaxed;

/// The negative control: the two read-modify-writes become a load and a store,
/// which is the whole of what this record's design is. Never on in a kernel
/// build.
#[cfg(feature = "device-irq-lossy")]
macro_rules! take_word {
    ($word:expr, $empty:expr) => {{
        let held = $word.load(ORDER);
        $word.store($empty, ORDER);
        held
    }};
}
#[cfg(not(feature = "device-irq-lossy"))]
macro_rules! take_word {
    ($word:expr, $empty:expr) => {
        $word.swap($empty, ORDER)
    };
}

#[cfg(feature = "device-irq-lossy")]
macro_rules! bump {
    ($word:expr) => {{
        let held = $word.load(ORDER);
        $word.store(held.wrapping_add(1), ORDER);
    }};
}
#[cfg(not(feature = "device-irq-lossy"))]
macro_rules! bump {
    ($word:expr) => {
        $word.fetch_add(1, ORDER)
    };
}

/// What the ISR writes and the claim reads back.
///
/// Atomics only: the handler takes no lock and allocates nothing.
pub struct Interrupt {
    /// Messages since the holder's last read.
    count: AtomicU32,
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
            pending: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
        }
    }

    /// Record one message. Called from the vector's ISR, so it takes no lock
    /// and allocates nothing.
    ///
    /// `fetch_add` and not a store: the reader may take the count between any
    /// two of these, and what it took plus what is left has to be what arrived.
    pub fn took(&self) {
        bump!(self.count);
        self.pending.store(true, ORDER);
    }

    /// The messages since the last read, or `None` for none.
    ///
    /// `swap` and not a load followed by a store: a message the ISR records
    /// between the two would be cleared without ever having been counted.
    pub fn take(&self) -> Option<u32> {
        match take_word!(self.count, 0) {
            0 => None,
            count => Some(count),
        }
    }

    /// Whether a message is waiting, for a readiness check that consumes
    /// nothing.
    pub fn armed(&self) -> bool {
        self.count.load(ORDER) != 0
    }

    /// Whether a wake is owed, taken at most once per message. Answers `true`
    /// for the pass that owes it and `false` for every pass after.
    ///
    /// `swap` for the same reason as [`Self::take`]: two passes that both
    /// loaded `true` would both wake one message's watchers.
    pub fn take_pending(&self) -> bool {
        take_word!(self.pending, false)
    }

    /// The unit refused this function an access. Called from the fault handler,
    /// which takes no lock: one store, and every call the claim answers refuses
    /// from here on.
    pub fn fault(&self) {
        self.faulted.store(true, ORDER);
    }

    pub fn faulted(&self) -> bool {
        self.faulted.load(ORDER)
    }

    /// Back to the state a fresh slot is in, for a claim being minted or given
    /// up. The holder is not running at either point.
    pub fn clear(&self) {
        self.count.store(0, ORDER);
        self.pending.store(false, ORDER);
        self.faulted.store(false, ORDER);
    }
}
