//! How many ranges of one space hold its node of a mapped object
//! (`object::paged::MapNode`). A range clears and shoots down its leaves
//! before it lets go; the last to let go, which may be a parked fault's hold
//! with no space lock, sees every other's clearing before the node leaves its
//! object's list, so a walk that no longer visits the space frees no frame a
//! leaf there may still reach. `kernel/loom/tests/paged_ranges.rs` checks it.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicU32, Ordering};

/// The order a letting go publishes its clearing with, and the last reads
/// every one with: `Arc`'s. Its negative control relaxes both.
#[cfg(not(feature = "ranges-less-relaxed"))]
const LESS: Ordering = Ordering::Release;
#[cfg(not(feature = "ranges-less-relaxed"))]
const LAST: Ordering = Ordering::Acquire;

#[cfg(feature = "ranges-less-relaxed")]
const LESS: Ordering = Ordering::Relaxed;
#[cfg(feature = "ranges-less-relaxed")]
const LAST: Ordering = Ordering::Relaxed;

pub struct Ranges(AtomicU32);

impl Ranges {
    /// A node's first range.
    pub fn one() -> Self {
        Self(AtomicU32::new(1))
    }

    /// Another range of a node a range holds.
    pub fn more(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// A range lets go: whether it was the last.
    #[must_use]
    pub fn less(&self) -> bool {
        if self.0.fetch_sub(1, LESS) != 1 {
            return false;
        }
        if LAST != Ordering::Relaxed {
            fence(LAST);
        }
        true
    }
}
