//! The two publish protocols of a program's log ring, and nothing about where
//! their words live.
//!
//! **The shared ring** takes any thread of the program. A writer reserves the
//! next position with a compare-and-swap on `head`, and only while the reader
//! has consumed the position one lap behind it — which `tail`, the reader's own
//! word, says; finding no room it adds one to `refused` and is done. With a
//! position it writes the slot's body and stores `position + 1` into the slot's
//! `published` word, `Release`. A writer never waits on another writer or on
//! the reader: its swap fails only because another writer's succeeded, so
//! every retry is somebody else's progress. Positions are dense — every one
//! reserved is published — so the reader walks them in order and stops at the
//! first not yet published.
//!
//! **A lane** is a ring with one writer, which a thread that may not retry at
//! all claims for itself — soundd's mix thread. Its write is a load, a
//! compare, a body copy and a store, and a full lane is a count in the lane's
//! own `refused` word, which only its writer stores. Wait-free.
//!
//! The reader of either stores its progress into `tail`, `Release`, after it
//! has copied the body out, which is what orders that copy before the next
//! lap's writer reuses the slot; a writer loads it `Acquire`.
//!
//! **Every word is the writer's to scribble.** The ring lives in memory the
//! writing process can write, so a reader trusts nothing it reads beyond the
//! equality and bound tests below: its position is its own and never read back
//! from `tail`, and a writer that lies can garble or stall only its own
//! records.
//!
//! Generic over where the words live, so `kernel-loom` runs this code against
//! loom's atomics and a program runs it against its mapped region.

use core::sync::atomic::Ordering;

/// One shared 64-bit word.
pub trait Word {
    fn load(&self, order: Ordering) -> u64;
    fn store(&self, value: u64, order: Ordering);
    fn fetch_add(&self, value: u64, order: Ordering) -> u64;
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64>;
}

/// Slots and their bodies, which both protocols have.
pub trait Slots {
    type Word: Word;
    type Body: Copy;

    /// Slots, at least one.
    fn slots(&self) -> u64;
    /// The next position a writer takes.
    fn head(&self) -> &Self::Word;
    /// The first position the reader has not consumed.
    fn tail(&self) -> &Self::Word;
    /// Writes that found no room, cumulative.
    fn refused(&self) -> &Self::Word;
    /// Store a body. Unsynchronised: the protocol makes its writer the slot's
    /// only accessor between taking the position and publishing it.
    fn write(&self, slot: usize, body: &Self::Body);
    /// Copy a body out. Unsynchronised, on the same argument.
    fn read(&self, slot: usize) -> Self::Body;
}

/// The shared ring's per-slot word.
pub trait Shared: Slots {
    /// `position + 1` once that position's body is whole.
    fn published(&self, slot: usize) -> &Self::Word;
}

/// What a write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Pushed {
    Written,
    /// No room; counted in `refused`.
    Refused,
}

/// Room for `position` while the reader has consumed up to `consumed`.
/// Wrapping, so a `tail` a writer scribbled past `head` reads as full.
fn room(position: u64, consumed: u64, slots: u64) -> bool {
    position.wrapping_sub(consumed) < slots
}

/// Write one record into the shared ring. Lock-free: see the module doc.
pub fn push<S: Shared>(ring: &S, body: &S::Body) -> Pushed {
    push_leaving(ring, body, 0)
}

/// [`push`], for a writer that leaves the last `keep` slots to others: it
/// finds no room once fewer than those are free.
pub fn push_leaving<S: Shared>(ring: &S, body: &S::Body, keep: u64) -> Pushed {
    let slots = ring.slots();
    let limit = slots.saturating_sub(keep).max(1);
    let position = loop {
        // `tail` first, `Acquire`: it pairs with the reader's `Release` of its
        // progress, so the reader's copy of the slot a lap back precedes this
        // writer's body — and so the `head` read after it is at least that
        // progress, which the reader reached through a position some writer
        // took. Read the other way round, a stale `head` under a fresh `tail`
        // would look like no room.
        let consumed = ring.tail().load(Ordering::Acquire);
        let position = ring.head().load(Ordering::Relaxed);
        if !room(position, consumed, limit) {
            ring.refused().fetch_add(1, Ordering::Relaxed);
            return Pushed::Refused;
        }
        if ring
            .head()
            .compare_exchange_weak(position, position + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break position;
        }
    };
    let slot = (position % slots) as usize;
    ring.write(slot, body);
    ring.published(slot).store(position + 1, publish_order());
    Pushed::Written
}

/// Write one record into a lane. Wait-free; `lane` has exactly one writer.
pub fn push_lane<L: Slots>(lane: &L, body: &L::Body) -> Pushed {
    let slots = lane.slots();
    // Only this writer stores `head` and `refused`, so it reads its own.
    let position = lane.head().load(Ordering::Relaxed);
    if !room(position, lane.tail().load(Ordering::Acquire), slots) {
        let refused = lane.refused().load(Ordering::Relaxed);
        lane.refused().store(refused + 1, Ordering::Relaxed);
        return Pushed::Refused;
    }
    lane.write((position % slots) as usize, body);
    lane.head().store(position + 1, publish_order());
    Pushed::Written
}

/// The publication's ordering. `Release` is what makes a reader that sees the
/// word see the body; `kernel-loom`'s negative control weakens it.
#[inline(always)]
fn publish_order() -> Ordering {
    #[cfg(not(feature = "log-ring-publish-relaxed"))]
    return Ordering::Release;
    #[cfg(feature = "log-ring-publish-relaxed")]
    return Ordering::Relaxed;
}

/// The one reader's place in a ring or a lane. Its own: never read back from
/// the memory it reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reader {
    next: u64,
    /// The writers' `refused` count as this reader last reported it.
    refused: u64,
}

impl Reader {
    pub const fn new() -> Self {
        Self { next: 0, refused: 0 }
    }

    /// The position this reader looks at next.
    pub fn position(&self) -> u64 {
        self.next
    }

    /// The next record of the shared ring, if one is published.
    pub fn next<S: Shared>(&mut self, ring: &S) -> Option<S::Body> {
        let slot = (self.next % ring.slots()) as usize;
        if ring.published(slot).load(Ordering::Acquire) != self.next + 1 {
            return None;
        }
        Some(self.consume(ring, slot))
    }

    /// The next record of a lane, if its writer has published one.
    pub fn next_lane<L: Slots>(&mut self, lane: &L) -> Option<L::Body> {
        if lane.head().load(Ordering::Acquire) <= self.next {
            return None;
        }
        let slot = (self.next % lane.slots()) as usize;
        Some(self.consume(lane, slot))
    }

    fn consume<L: Slots>(&mut self, slots: &L, slot: usize) -> L::Body {
        let body = slots.read(slot);
        self.next += 1;
        slots.tail().store(self.next, Ordering::Release);
        body
    }

    /// Writes refused since the last call. A writer that scribbles the word
    /// backwards reads as none.
    pub fn refused<L: Slots>(&mut self, slots: &L) -> u64 {
        let now = slots.refused().load(Ordering::Relaxed);
        let since = now.saturating_sub(self.refused);
        self.refused = self.refused.max(now);
        since
    }

    /// Every position up to the shared ring's `head` once none of its writers
    /// is left: published records to `found`, and a count of the positions
    /// reserved and never published. At most one lap, whatever `head` says.
    pub fn sweep<S: Shared>(&mut self, ring: &S, mut found: impl FnMut(S::Body)) -> u64 {
        let head = ring.head().load(Ordering::Acquire);
        let end = head.min(self.next.saturating_add(ring.slots()));
        let mut abandoned = 0;
        while self.next < end {
            match self.next(ring) {
                Some(body) => found(body),
                None => {
                    abandoned += 1;
                    self.next += 1;
                }
            }
        }
        abandoned
    }
}
