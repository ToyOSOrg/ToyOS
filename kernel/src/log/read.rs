//! Two record readers, each correct for one caller: [`snapshot_committed`]
//! (the log's, newest first, lock-free, never blocks) and [`drain_ordered`]
//! (any [`Stream`]'s, oldest first in sequence order per ring, stopping at the
//! first uncommitted record).
//!
//! Both walk a ring per CPU the firmware names and keep one position per ring
//! in storage their caller owns, so neither allocates nor puts a per-CPU array
//! on its stack: a fatal path reads the log with positions sized at boot.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use toyos_abi::log::LogRecord;

use crate::cpu_slice::CpuSlice;

use super::shard::{Ring, Shard, FIRST_SEQ};
use super::stamp::LogStamp;

/// Accepts one record; `false` means it was not taken and ends the walk.
pub trait RecordSink<R = LogRecord> {
    fn put(&mut self, record: &R) -> bool;
}

/// A ring whose slots decode as one kind of record.
pub trait Stream {
    type Record;
    /// Record `seq`, or `None` if the ring cannot answer for it.
    fn record(&self, seq: u64) -> Option<Self::Record>;
}

impl Stream for Shard {
    type Record = LogRecord;
    fn record(&self, seq: u64) -> Option<LogRecord> {
        self.read(seq)
    }
}

/// A ring per CPU the firmware names; `None` is one no reader can reach yet,
/// a CPU that was never attempted or has not published its ring.
pub trait Rings<const W: usize, const N: usize> {
    fn count(&self) -> usize;
    fn ring(&self, cpu: usize) -> Option<&'static Ring<W, N>>;
}

impl<const W: usize, const N: usize> Rings<W, N> for [Option<&'static Ring<W, N>>] {
    fn count(&self) -> usize {
        self.len()
    }

    fn ring(&self, cpu: usize) -> Option<&'static Ring<W, N>> {
        self.get(cpu).copied().flatten()
    }
}

/// A walk's position in each ring, and what it counted lost.
pub trait Positions {
    fn count(&self) -> usize;
    fn next(&self, ring: usize) -> u64;
    fn set_next(&mut self, ring: usize, next: u64);
    fn lose(&mut self, records: u64);
}

/// A reader's position across rings, plus records it never saw: the walk a
/// syscall makes of its caller's cursor.
pub struct Cursor {
    /// Next sequence number wanted from each ring.
    next: Vec<u64>,
    /// Overwritten-before-read count, derived as `oldest_readable - next`.
    lost: u64,
}

impl Cursor {
    /// A caller's positions as a walk, or `None` for a position past the number its ring issues next; the walk
    /// counts this read's loss alone.
    pub fn from_reader<const W: usize, const N: usize>(next: &[u64], rings: &(impl Rings<W, N> + ?Sized)) -> Option<Self> {
        // An unpublished ring is held to the head it is published with, so no cursor is ahead of one that appears mid-walk.
        let issued = |cpu: usize| rings.ring(cpu).map_or(FIRST_SEQ, Ring::head);
        let ahead = next.iter().enumerate().any(|(cpu, &next)| next > issued(cpu));
        (!ahead).then(|| Self { next: next.to_vec(), lost: 0 })
    }

    pub fn next(&self) -> &[u64] {
        &self.next
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }
}

impl Positions for Cursor {
    fn count(&self) -> usize {
        self.next.len()
    }

    fn next(&self, ring: usize) -> u64 {
        self.next[ring]
    }

    fn set_next(&mut self, ring: usize, next: u64) {
        self.next[ring] = next;
    }

    fn lose(&mut self, records: u64) {
        // Cannot overflow: `lost` starts at zero and grows only as far as positions move, which never pass a head.
        self.lost += records;
    }
}

/// Positions shared across contexts without a lock: shard 0's from the
/// start, every other CPU's once [`Published::size`] has run. Exclusion
/// between walkers is the caller's.
pub struct Published {
    // Atomics, not a `Cursor` behind a lock: the panic bypass walks this lock-free.
    boot: AtomicU64,
    aps: CpuSlice<AtomicU64>,
    lost: AtomicU64,
}

impl Published {
    pub const fn new() -> Self {
        Self { boot: AtomicU64::new(FIRST_SEQ), aps: CpuSlice::new(), lost: AtomicU64::new(0) }
    }

    fn count(&self) -> usize {
        1 + self.aps.len()
    }

    /// A position for each of `cpus` CPUs' rings.
    pub fn size(&self, cpus: usize) {
        self.aps.size(cpus.saturating_sub(1), |_| AtomicU64::new(FIRST_SEQ));
    }

    fn word(&self, ring: usize) -> &AtomicU64 {
        match ring.checked_sub(1) {
            None => &self.boot,
            Some(ap) => &self.aps[ap],
        }
    }

    /// Records the walks over this position counted lost.
    pub fn lost(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// Take `other`'s place: what a walk from here reads, `other`'s walkers no
    /// longer move.
    pub fn copy_from(&self, other: &Published) {
        for ring in 0..self.count() {
            self.word(ring).store(other.word(ring).load(Ordering::Relaxed), Ordering::Relaxed);
        }
        self.lost.store(other.lost(), Ordering::Relaxed);
    }

    /// Is there a committed record this position has not taken? Lock-free.
    pub fn any_pending(&self) -> bool {
        any_committed(self)
    }
}

impl Positions for &Published {
    fn count(&self) -> usize {
        Published::count(self)
    }

    fn next(&self, ring: usize) -> u64 {
        self.word(ring).load(Ordering::Relaxed)
    }

    fn set_next(&mut self, ring: usize, next: u64) {
        self.word(ring).store(next, Ordering::Relaxed);
    }

    fn lose(&mut self, records: u64) {
        self.lost.fetch_add(records, Ordering::Relaxed);
    }
}

/// Clamps `ring`'s position to what it can still answer for, counting the gap as loss, and answers the stamp
/// there.
fn open<const W: usize, const N: usize>(at: &mut impl Positions, i: usize, ring: Option<&'static Ring<W, N>>) -> Option<u64> {
    let ring = ring?;
    let oldest = ring.oldest_readable();
    // Clamped to `FIRST_SEQ`: a zeroed cursor from the syscall boundary must not read as having missed everything.
    let want = at.next(i).max(FIRST_SEQ);
    at.lose(oldest.saturating_sub(want));
    let want = want.max(oldest);
    at.set_next(i, want);
    ring.stamp(want)
}

/// Every record `at` has not seen in `rings`, oldest first merged by stamp;
/// a stalled ring is skipped, not waited for. `at` holds a position per ring.
pub fn drain_ordered<const W: usize, const N: usize>(
    rings: &(impl Rings<W, N> + ?Sized),
    at: &mut impl Positions,
    out: &mut impl RecordSink<<Ring<W, N> as Stream>::Record>,
) -> usize
where
    Ring<W, N>: Stream,
{
    let count = rings.count().min(at.count());
    let mut emitted = 0;
    loop {
        let mut best: Option<(usize, u64)> = None;
        for i in 0..count {
            if let Some(at_ns) = open(at, i, rings.ring(i)) {
                if best.is_none_or(|(_, oldest)| at_ns < oldest) {
                    best = Some((i, at_ns));
                }
            }
        }
        let Some((i, _)) = best else { return emitted };
        let Some(ring) = rings.ring(i) else { return emitted };
        // `match`, not `if let`: the empty arm explains itself below.
        #[allow(clippy::single_match)]
        match ring.record(at.next(i)) {
            Some(record) => {
                if !out.put(&record) {
                    return emitted;
                }
                emitted += 1;
                at.set_next(i, at.next(i) + 1);
            }
            // The slot was recycled between timestamp and body; the next `open`
            // re-clamps the position and counts it as loss.
            None => {}
        }
    }
}

/// The log's shards: cpu0's boot shard, then every AP's the registry holds.
pub struct Shards;

impl Rings<{ super::shard::BODY_WORDS }, { super::shard::SHARD_RECORDS }> for Shards {
    fn count(&self) -> usize {
        super::shard_count()
    }

    fn ring(&self, cpu: usize) -> Option<&'static Shard> {
        super::shard(cpu)
    }
}

/// The newest committed record in `shard` stamped in `from..=to`, at or below
/// position `next`, and the position the search reached.
fn newest_in(shard: &Shard, mut next: u64, from: u64, to: u64) -> (u64, Option<(u64, u64)>) {
    let floor = shard.oldest_readable();
    while next >= floor {
        let Some(at_ns) = shard.stamp(next) else {
            next -= 1;
            continue;
        };
        if at_ns > to {
            next -= 1;
            continue;
        }
        if at_ns < from {
            // Sequence order is stamp order: everything older is older still.
            return (0, None);
        }
        return (next, Some((next, at_ns)));
    }
    (0, None)
}

/// The stamp of the newest committed record, or zero if none: the upper bound
/// of a snapshot that must not chase records committed while it walks.
pub fn newest_committed() -> LogStamp {
    let newest = (0..super::shard_count())
        .filter_map(super::shard)
        .filter_map(|shard| newest_in(shard, shard.head().saturating_sub(1), 0, u64::MAX).1)
        .map(|(_, at_ns)| at_ns)
        .max()
        .unwrap_or(0);
    LogStamp::since_zero(newest)
}

/// Is there a committed record `at` has not taken, without taking it?
pub fn any_committed(at: &Published) -> bool {
    (0..super::shard_count().min(at.count())).any(|i| match super::shard(i) {
        Some(shard) => {
            let want = (&at).next(i).max(FIRST_SEQ).max(shard.oldest_readable());
            shard.stamp(want).is_some()
        }
        None => false,
    })
}

/// Every committed record stamped in `from..=to`, newest first merged by
/// `at_ns` until `out` is full, walking with `scratch`'s positions, which the
/// caller holds alone; returns nothing, since no caller counts.
pub fn snapshot_committed(from: LogStamp, to: LogStamp, out: &mut impl RecordSink, scratch: &Published) {
    let (from, to) = (from.nanos(), to.nanos());
    let mut at = scratch;
    let count = super::shard_count().min(at.count());
    for i in 0..count {
        // `head` counts reservations; the newest usable number is one below it.
        at.set_next(i, super::shard(i).map_or(0, |shard| shard.head().saturating_sub(1)));
    }
    loop {
        let mut best: Option<(usize, u64, u64)> = None;
        for i in 0..count {
            let Some(shard) = super::shard(i) else { continue };
            let (next, found) = newest_in(shard, at.next(i), from, to);
            at.set_next(i, next);
            if let Some((seq, at_ns)) = found {
                if best.is_none_or(|(_, _, newest)| at_ns > newest) {
                    best = Some((i, seq, at_ns));
                }
            }
        }
        let Some((i, seq, _)) = best else { return };
        at.set_next(i, seq - 1);
        // `None` here is a writer that recycled the slot; it's newer than
        // everything left, so skipping it keeps order.
        if let Some(record) = super::shard(i).and_then(|shard| shard.read(seq)) {
            if !out.put(&record) {
                return;
            }
        }
    }
}
