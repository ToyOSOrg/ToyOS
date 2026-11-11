//! A region's page whose frame is drawn at its first touch. A touch that finds
//! the frame installed takes it with one load; the first touch draws it while
//! holding the region's first-touch lock, which every first touch of the
//! region's pages takes, so a page draws exactly once, against its own
//! promise, and every toucher is handed that frame with what its drawer wrote
//! into it.
//!
//! Compiled a second time by `kernel-loom`, so it names only atomics and takes
//! the lock as its caller's.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

/// No frame yet: never a frame's address, which is page-aligned.
const HOLE: u64 = u64::MAX;

// `slot-install-relaxed` is the negative control: the install publishes the
// frame without its contents, and `kernel/loom/tests/shm_slot.rs` must red.
#[cfg(not(feature = "slot-install-relaxed"))]
const INSTALL: Ordering = Ordering::Release;
#[cfg(not(feature = "slot-install-relaxed"))]
const FOUND: Ordering = Ordering::Acquire;
#[cfg(feature = "slot-install-relaxed")]
const INSTALL: Ordering = Ordering::Relaxed;
#[cfg(feature = "slot-install-relaxed")]
const FOUND: Ordering = Ordering::Relaxed;

/// A page's frame by its physical address, or none yet.
pub struct Slot(AtomicU64);

impl Slot {
    /// A page nothing has touched.
    pub fn hole() -> Self {
        Self(AtomicU64::new(HOLE))
    }

    /// The page's frame: the one installed, or else `draw`'s, drawn and
    /// installed while the guard `first_touch` answers is held, which is the
    /// region's lock that every first touch of its pages takes.
    pub fn frame<G>(&self, first_touch: impl FnOnce() -> G, draw: impl FnOnce() -> u64) -> u64 {
        let installed = self.0.load(FOUND);
        if installed != HOLE {
            return installed;
        }
        let _held = first_touch();
        // Under the lock, which orders this after any install its last holder
        // made, and what that holder wrote into the frame.
        let installed = self.0.load(Ordering::Relaxed);
        if installed != HOLE {
            return installed;
        }
        let drawn = draw();
        self.0.store(drawn, INSTALL);
        drawn
    }

    /// The frame installed, or `None` for a page never touched: for the
    /// region's end, when nothing else reaches it.
    pub fn drawn(&self) -> Option<u64> {
        let frame = self.0.load(Ordering::Relaxed);
        (frame != HOLE).then_some(frame)
    }
}
