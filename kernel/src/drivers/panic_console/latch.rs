//! The capture latch: the panic snapshot's one writer, ever — re-entrant for its owner.
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

    /// Whether `token` may write the snapshot; the acquire on failure pairs
    /// with [`release`](CaptureLatch::release)'s store for the next claimant.
    pub fn claim(&self, token: u32) -> bool {
        match self.owner.compare_exchange(UNCLAIMED, token, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => true,
            Err(owner) => owner == token,
        }
    }

    /// Give the snapshot back: the panic was survived. The owner's call alone.
    pub fn release(&self) {
        self.owner.store(UNCLAIMED, Ordering::Release);
    }
}
