//! Gate for reap work pending on the idle loop: a raise is never lost.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, Ordering};

// Kernel builds never enable `reap-raise-relaxed`.
#[cfg(not(feature = "reap-raise-relaxed"))]
const ENROL: Ordering = Ordering::Release;
#[cfg(feature = "reap-raise-relaxed")]
const ENROL: Ordering = Ordering::Relaxed;

/// Whether the idle loop has cleanup waiting for it.
pub struct ReapGate {
    pending: AtomicBool,
}

impl ReapGate {
    /// Must stay `const`: `ReapGate` is seeded as a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { pending: AtomicBool::new(false) }
    }

    // No `Default`: `Default::default` can't be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { pending: AtomicBool::new(false) }
    }

    /// Call after publishing the work: the store's release carries it to the claimer.
    pub fn raise(&self) {
        self.pending.store(true, ENROL);
    }

    /// Returns `true` at most once per raise.
    pub fn take(&self) -> bool {
        // Relaxed load first: avoids an RMW on every idle-loop trip when nothing is pending.
        if !self.pending.load(Ordering::Relaxed) {
            return false;
        }
        self.pending.swap(false, Ordering::Acquire)
    }
}
