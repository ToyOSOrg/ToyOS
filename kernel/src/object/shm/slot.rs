//! A region's page whose frame is drawn at its first touch: the one decision
//! CPUs touching it at once race for is whose frame it keeps. Exactly one draw
//! is installed, every toucher is handed that one and what its drawer wrote
//! into it, and every other draw goes back.
//!
//! Compiled a second time by `kernel-loom`, so it names only atomics.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

/// No frame yet: never a frame's address, which is page-aligned.
const HOLE: u64 = u64::MAX;

// `slot-install-relaxed` is the negative control: the install publishes the
// frame without its contents, and `kernel/loom/tests/shm_slot.rs` must red.
#[cfg(not(feature = "slot-install-relaxed"))]
const INSTALL: Ordering = Ordering::AcqRel;
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

    /// The page's frame: the one installed, or else `draw`'s, installed now.
    /// Where another toucher's draw was installed first, this one's goes to
    /// `give_back` and theirs is answered. Whoever is handed a frame sees what
    /// its drawer wrote into it before it was installed.
    pub fn frame(&self, draw: impl FnOnce() -> u64, give_back: impl FnOnce(u64)) -> u64 {
        let installed = self.0.load(FOUND);
        if installed != HOLE {
            return installed;
        }
        let drawn = draw();
        match self.0.compare_exchange(HOLE, drawn, INSTALL, FOUND) {
            Ok(_) => drawn,
            Err(installed) => {
                give_back(drawn);
                installed
            }
        }
    }

    /// The frame installed, or `None` for a page never touched: for the
    /// region's end, when nothing else reaches it.
    pub fn drawn(&self) -> Option<u64> {
        let frame = self.0.load(Ordering::Relaxed);
        (frame != HOLE).then_some(frame)
    }
}
