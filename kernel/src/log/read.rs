//! Two record readers, each correct for one caller: [`snapshot_committed`]
//! (newest first, lock-free, never blocks) and [`drain_ordered`] (oldest
//! first in sequence order per shard, stopping at the first uncommitted
//! record).

use toyos_abi::log::{LogRecord, MAX_LOG_SHARDS};

use super::shard::{Shard, FIRST_SEQ};

/// Accepts one record; `false` means it was not taken and ends the walk.
pub trait RecordSink {
    fn put(&mut self, record: &LogRecord) -> bool;
}

#[derive(Clone, Copy)]
struct Descent {
    shard: Option<&'static Shard>,
    /// Next sequence number to try, descending.
    next: u64,
    /// Oldest sequence number this shard can still answer for.
    floor: u64,
    /// This shard's candidate, or `None` when it has nothing in the window.
    cand: Option<(u64, u64)>,
}

const IDLE: Descent = Descent { shard: None, next: 0, floor: u64::MAX, cand: None };

// Pinned against IST1's 16 KiB double-fault stack budget.
const _: () = assert!(core::mem::size_of::<Descent>() == 48);
const _: () = assert!(core::mem::size_of::<[Descent; MAX_LOG_SHARDS]>() == 384);

impl Descent {
    /// `from` relies on `emit` stamping `at_ns` inside the same interrupt-off
    /// bracket as the reservation, so sequence order is timestamp order.
    fn advance(&mut self, from: u64, to: u64) {
        self.cand = None;
        let Some(shard) = self.shard else { return };
        while self.next >= self.floor {
            let seq = self.next;
            self.next = seq - 1;
            let Some(at_ns) = shard.at_ns(seq) else { continue };
            if at_ns > to {
                continue;
            }
            if at_ns < from {
                return;
            }
            self.cand = Some((seq, at_ns));
            return;
        }
    }
}

/// A reader's position across shards, plus records it never saw.
pub struct Cursor {
    /// Next sequence number wanted from each shard.
    next: [u64; MAX_LOG_SHARDS],
    /// Overwritten-before-read count, derived as `oldest_readable - next`.
    lost: u64,
}

impl Cursor {
    /// A reader that has seen nothing; starts at [`FIRST_SEQ`], not zero, to avoid a phantom first-call loss.
    pub const fn new() -> Self {
        Self { next: [FIRST_SEQ; MAX_LOG_SHARDS], lost: 0 }
    }

    /// A caller's raw `LogCursor`, unvalidated: every field is clamped where it is used instead.
    pub fn from_reader(cursor: &toyos_abi::log::LogCursor) -> Self {
        Self { next: cursor.next, lost: cursor.lost }
    }

    /// Writes the walk state back into the caller's `LogCursor`.
    pub fn write_into(&self, cursor: &mut toyos_abi::log::LogCursor) {
        cursor.next = self.next;
        cursor.lost = self.lost;
    }

    /// Clamps this shard's position to what it can still answer for, counting the gap as loss.
    fn open(&mut self, i: usize, shard: Option<&'static Shard>) -> Option<u64> {
        let shard = shard?;
        let oldest = shard.oldest_readable();
        // Clamped to `FIRST_SEQ`: a zeroed cursor from the syscall boundary must not read as having missed everything.
        let want = self.next.get(i).copied().unwrap_or(FIRST_SEQ).max(FIRST_SEQ);
        self.lost += oldest.saturating_sub(want);
        let want = want.max(oldest);
        *self.next.get_mut(i)? = want;
        shard.at_ns(want)
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// A cursor's position, shared across contexts without a lock.
pub struct Published {
    // Atomics, not a `Cursor` behind a lock: the panic bypass reads this lock-free.
    next: [core::sync::atomic::AtomicU64; MAX_LOG_SHARDS],
    lost: core::sync::atomic::AtomicU64,
}

impl Published {
    pub const fn new() -> Self {
        use core::sync::atomic::AtomicU64;
        Self {
            next: [const { AtomicU64::new(FIRST_SEQ) }; MAX_LOG_SHARDS],
            lost: AtomicU64::new(0),
        }
    }

    /// The position, as a cursor to walk with.
    pub fn take(&self) -> Cursor {
        use core::sync::atomic::Ordering;
        Cursor {
            next: core::array::from_fn(|i| self.next[i].load(Ordering::Relaxed)),
            lost: self.lost.load(Ordering::Relaxed),
        }
    }

    /// Makes the walk position visible to lock-free readers; exclusion is the caller's job.
    pub fn put(&self, cursor: &Cursor) {
        use core::sync::atomic::Ordering;
        for (word, next) in self.next.iter().zip(cursor.next) {
            word.store(next, Ordering::Relaxed);
        }
        self.lost.store(cursor.lost, Ordering::Relaxed);
    }

    pub fn lost(&self) -> u64 {
        self.lost.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Resets the position to the oldest record every shard still holds.
    pub fn rewind(&self) {
        use core::sync::atomic::Ordering;
        for word in &self.next {
            word.store(FIRST_SEQ, Ordering::Relaxed);
        }
        // Also zeroes `lost`: after a rewind it counts only future drops, not what a prior consumer missed.
        self.lost.store(0, Ordering::Relaxed);
    }

    /// Is there a committed record this position has not taken? Lock-free.
    pub fn any_pending(&self) -> bool {
        any_committed(&self.take())
    }
}

/// Every record this cursor has not seen, oldest first merged by `at_ns`; a
/// stalled shard is skipped, not waited for.
pub fn drain_ordered(cursor: &mut Cursor, out: &mut impl RecordSink) -> usize {
    let shards = super::shards();
    let mut cand = [None; MAX_LOG_SHARDS];
    for (i, slot) in cand.iter_mut().enumerate() {
        *slot = cursor.open(i, shards[i]);
    }

    let mut emitted = 0;
    loop {
        let mut best: Option<(usize, u64)> = None;
        for (i, slot) in cand.iter().enumerate() {
            if let Some(at_ns) = *slot {
                if best.is_none_or(|(_, oldest)| at_ns < oldest) {
                    best = Some((i, at_ns));
                }
            }
        }
        let Some((i, _)) = best else { return emitted };
        let Some(shard) = shards[i] else { return emitted };

        // `match`, not `if let`: the empty arm explains itself below.
        #[allow(clippy::single_match)]
        match shard.read(cursor.next[i]) {
            Some(record) => {
                if !out.put(&record) {
                    return emitted;
                }
                emitted += 1;
                cursor.next[i] += 1;
            }
            // The slot was recycled between timestamp and body; `open` below
            // re-clamps `next[i]` and counts it as loss, so this arm need not advance.
            None => {}
        }
        cand[i] = cursor.open(i, shards[i]);
    }
}

/// The `at_ns` of the newest committed record, or zero if none — clamps
/// `LogCursor::durable` so a buggy `/bin/logd` can't wait forever.
pub fn newest_committed_at_ns() -> u64 {
    let mut newest = 0;
    for shard in super::shards().iter().flatten() {
        let mut descent = IDLE;
        descent.shard = Some(shard);
        // `head` counts reservations; the newest usable number is one below it.
        descent.next = shard.head().saturating_sub(1);
        descent.floor = shard.oldest_readable();
        descent.advance(0, u64::MAX);
        if let Some((_, at_ns)) = descent.cand {
            newest = newest.max(at_ns);
        }
    }
    newest
}

/// Is there a committed record this cursor has not taken, without taking it?
pub fn any_committed(cursor: &Cursor) -> bool {
    super::shards()
        .iter()
        .enumerate()
        .any(|(i, shard)| match shard {
            Some(shard) => {
                let want = cursor.next[i].max(FIRST_SEQ).max(shard.oldest_readable());
                shard.at_ns(want).is_some()
            }
            None => false,
        })
}

/// Every committed record stamped in `from..=to`, newest first merged by
/// `at_ns` until `out` is full; returns nothing, since no caller counts.
pub fn snapshot_committed(from: u64, to: u64, out: &mut impl RecordSink) {
    let mut descents = [IDLE; MAX_LOG_SHARDS];
    for (descent, shard) in descents.iter_mut().zip(super::shards()) {
        let Some(shard) = shard else { continue };
        // `head` counts reservations; the newest usable number is one below it.
        descent.shard = Some(shard);
        descent.next = shard.head().saturating_sub(1);
        descent.floor = shard.oldest_readable();
        descent.advance(from, to);
    }

    loop {
        let mut best: Option<(usize, u64)> = None;
        for (i, descent) in descents.iter().enumerate() {
            if let Some((_, at_ns)) = descent.cand {
                if best.is_none_or(|(_, newest)| at_ns > newest) {
                    best = Some((i, at_ns));
                }
            }
        }
        let Some((i, _)) = best else { return };
        let Some(descent) = descents.get_mut(i) else { return };
        let Some((seq, _)) = descent.cand else { return };

        // `None` here is a writer that recycled the slot; it's newer than
        // everything left, so skipping it keeps order.
        let copied = descent.shard.and_then(|shard| shard.read(seq));
        descent.advance(from, to);
        if let Some(record) = copied {
            if !out.put(&record) {
                return;
            }
        }
    }
}
