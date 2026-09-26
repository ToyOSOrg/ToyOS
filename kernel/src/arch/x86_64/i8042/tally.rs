//! Interrupt counts packed into one `u64`, updated with a single atomic op, so a read always sees carried and empty as of the same instant.
//! No `crate::` references: `kernel-loom` compiles this file under `feature = "loom"` to drive the real word in its tests.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

/// What one interrupt turned out to be, decided by the ISR after its burst.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Carried {
    /// The burst read at least one byte, all reached the ring before this was recorded.
    Bytes,
    /// OBF was already clear: the driver's polling init had taken the byte before the interrupt arrived.
    Nothing,
}

/// A carried/empty pair read at one instant; built only by [`Tally::read`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Counts {
    /// Interrupts that delivered at least one byte into the ring.
    pub carried: u32,
    /// Interrupts the ISR found nothing behind.
    pub empty: u32,
}

impl Counts {
    /// Total interrupts observed, saturating so it can't disagree with `carried + empty`.
    pub fn irqs(self) -> u32 {
        self.carried.saturating_add(self.empty)
    }
}

// The two halves are disjoint: an interrupt increments exactly one of them.
const CARRIED_ONE: u64 = 1;
const EMPTY_ONE: u64 = 1 << 32;

pub struct Tally {
    packed: AtomicU64,
}

impl Tally {
    // Must stay `const`: `Tally` is a kernel `static`, and loom's atomics have no const constructor.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { packed: AtomicU64::new(0) }
    }

    // No `Default`: it can't be `const`, and the const arm above is what the `static` needs.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { packed: AtomicU64::new(0) }
    }

    /// Account for one interrupt. Call once, from the ISR, after the burst — never on the way in.
    pub fn record(&self, carried: Carried) {
        // Sole writer: the ISR is pinned to one CPU, so this load-then-add can't race itself.
        let counts = Self::split(self.packed.load(Ordering::Relaxed));
        // Saturate rather than wrap: wrapping the low half would carry into the high half.
        if counts.carried == u32::MAX || counts.empty == u32::MAX {
            return;
        }
        let one = match carried {
            Carried::Bytes => CARRIED_ONE,
            Carried::Nothing => EMPTY_ONE,
        };
        self.packed.fetch_add(one, Ordering::Release);
    }

    /// The pair as it stood at one instant; the acquire pairs with `record`'s release, so a reader that sees a count sees the bytes behind it.
    pub fn read(&self) -> Counts {
        Self::split(self.packed.load(Ordering::Acquire))
    }

    fn split(packed: u64) -> Counts {
        Counts { carried: packed as u32, empty: (packed >> 32) as u32 }
    }
}
