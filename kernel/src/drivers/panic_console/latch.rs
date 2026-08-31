//! The capture latch: one writer CPU owns the panic snapshot until recovery and may re-enter.
//! No `crate::` references: `kernel-loom` compiles this file under `feature = "loom"` to drive the real latch in its tests.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, Ordering};

/// No CPU's token; a claimant's is never this (`cpu_id + 2`, or 1 pre-percpu).
const UNCLAIMED: u32 = 0;

/// `PAINTING`'s shape with an owner: the first claim holds until
/// [`release`](CaptureLatch::release), the owner re-enters, any other is refused.
pub struct CaptureLatch {
    owner: AtomicU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    Fresh,
    Reentrant,
    Refused,
}

impl CaptureLatch {
    // Must stay `const`: the latch is a kernel `static`, and loom's atomics have no const constructor.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { owner: AtomicU32::new(UNCLAIMED) }
    }

    // No `Default`: it can't be `const`, and the const arm above is what the `static` needs.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { owner: AtomicU32::new(UNCLAIMED) }
    }

    /// Whether `token` newly acquired, re-entered, or lost the latch.
    pub fn claim(&self, token: u32) -> Claim {
        match self.owner.compare_exchange(UNCLAIMED, token, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Claim::Fresh,
            Err(owner) if owner == token => Claim::Reentrant,
            Err(_) => Claim::Refused,
        }
    }

    pub fn owned_by(&self, token: u32) -> bool {
        self.owner.load(Ordering::Acquire) == token
    }

    /// Give the snapshot back only when `token` owns it.
    pub fn release(&self, token: u32) -> bool {
        self.owner
            .compare_exchange(token, UNCLAIMED, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }
}
