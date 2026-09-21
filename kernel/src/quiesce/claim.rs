//! The one shutdown a boot gets: whichever caller takes the claim runs it.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, Ordering};

pub struct Claim {
    taken: AtomicBool,
}

impl Claim {
    /// Must stay `const`: the claim is seeded as a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { taken: AtomicBool::new(false) }
    }

    // No `Default`: `Default::default` can't be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { taken: AtomicBool::new(false) }
    }

    /// `true` for exactly one caller, however many ask at once: one exchange,
    /// because a read and a write that can come apart let two callers in.
    // Kernel builds never enable `shutdown-claim-split`.
    #[cfg(not(feature = "shutdown-claim-split"))]
    pub fn take(&self) -> bool {
        !self.taken.swap(true, Ordering::AcqRel)
    }

    #[cfg(feature = "shutdown-claim-split")]
    pub fn take(&self) -> bool {
        if self.taken.load(Ordering::Acquire) {
            return false;
        }
        self.taken.store(true, Ordering::Release);
        true
    }
}
