//! A region's page whose frame is drawn at its first touch, alone or with the
//! rest of a fault's run.
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

    /// The frame installed, or `None` for a page never touched: for the
    /// region's end, when nothing else reaches it.
    pub fn drawn(&self) -> Option<u64> {
        let frame = self.0.load(Ordering::Relaxed);
        (frame != HOLE).then_some(frame)
    }
}

/// The frame of each of `slots` into `out`: the one it holds, each taken with
/// one load, or for every hole one of the frames a single call of `draw`
/// writes into `drawn`, drawn and installed while the guard `first_touch`
/// answers is held, the region's lock that every first touch of its pages
/// takes. So a page draws once however many touches and runs ask for it.
/// `out` and `drawn` are as long as `slots`.
pub fn frames<G>(slots: &[&Slot], out: &mut [u64], drawn: &mut [u64], first_touch: impl FnOnce() -> G, draw: impl FnOnce(&mut [u64])) {
    if look(slots, out, FOUND) == 0 {
        return;
    }
    let _held = first_touch();
    // Under the lock, which orders these loads after every install made
    // under it, and what was written into those frames.
    let holes = look(slots, out, Ordering::Relaxed);
    if holes == 0 {
        return;
    }
    let drawn = &mut drawn[..holes];
    draw(drawn);
    let holes = slots.iter().zip(out.iter_mut()).filter(|(_, frame)| **frame == HOLE);
    for ((slot, frame), &fresh) in holes.zip(drawn.iter()) {
        *frame = fresh;
        slot.0.store(fresh, INSTALL);
    }
}

/// Each of `slots`' frames into `out`, a hole as [`HOLE`]: how many holes.
fn look(slots: &[&Slot], out: &mut [u64], order: Ordering) -> usize {
    let mut holes = 0;
    for (slot, frame) in slots.iter().zip(out.iter_mut()) {
        *frame = slot.0.load(order);
        holes += usize::from(*frame == HOLE);
    }
    holes
}
