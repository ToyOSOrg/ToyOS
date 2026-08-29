//! Hand-off bank for threads that died in panic recovery: a second death on one
//! CPU before its next idle trip banks beside the first instead of erasing it.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

/// The vacant value; no live task packs to it — a pid would have to reach `u32::MAX`.
pub const EMPTY: u64 = u64::MAX;

/// Deaths one CPU can bank between idle trips; fixed, because the panic path
/// may hold any lock and may not allocate.
pub const SLOTS: usize = 8;

/// A fixed bank of packed task ids, written by the panic path and drained by
/// the idle loop.
pub struct PoisonSet {
    slots: [AtomicU64; SLOTS],
}

impl PoisonSet {
    /// Must stay `const`: seeded as a `static`, one per CPU.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { slots: [const { AtomicU64::new(EMPTY) }; SLOTS] }
    }

    // No `Default`: `Default::default` can't be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { slots: core::array::from_fn(|_| AtomicU64::new(EMPTY)) }
    }

    /// Bank one packed id; `false` means every slot was full and the id is
    /// dropped — loudly, by the caller. Each claim is one CAS, so a death
    /// taken mid-scan costs a slot, never a loss.
    #[cfg(not(feature = "poison-overwrite"))]
    pub fn bank(&self, packed: u64) -> bool {
        for slot in &self.slots {
            if slot
                .compare_exchange(EMPTY, packed, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
        false
    }

    // Kernel builds never enable `poison-overwrite`: it restores the erasing
    // one-slot swap, and `kernel-loom/tests/poison_set.rs` must red under it.
    #[cfg(feature = "poison-overwrite")]
    pub fn bank(&self, packed: u64) -> bool {
        self.slots[0].swap(packed, Ordering::Release);
        true
    }

    /// Hand every banked id to `f`, each at most once across every drain.
    pub fn drain(&self, mut f: impl FnMut(u64)) {
        for slot in &self.slots {
            let raw = slot.swap(EMPTY, Ordering::Acquire);
            if raw != EMPTY {
                f(raw);
            }
        }
    }
}
