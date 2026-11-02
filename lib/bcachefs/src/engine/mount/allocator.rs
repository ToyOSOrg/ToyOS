//! Where new btree nodes go, and how much room is left for them.
//!
//! **A bucket is written again only once its emptying is durable** (A6.19):
//! until the entry that emptied it is on the medium, a crash brings back the
//! pointers into it. blockring has no discard, so a bucket whose emptying is
//! durable is taken straight from need_discard, with no free state between.
//!
//! **Nor while a traversal begun before it emptied still reads.** A walk
//! follows interior keys it read before an await, so it may still lead into
//! a bucket the pointers have left. Each emptying advances an epoch; a bucket
//! is written again only once every traversal begun at or before its epoch
//! has ended.

use alloc::collections::{BTreeMap, BTreeSet};

use super::reclaim::{COMMON_REWRITE, MAX_REWRITE, RECLAIM_ROOM};
use super::FOOTPRINT;
use crate::engine::bkey::Bpos;
use crate::engine::read::Source;
use crate::engine::value::{accounting_pos, class, data_type};
use crate::engine::{journal, Btree, Error, Result};

/// Bytes beyond the records prepaid from the device geometry.
pub(super) fn charge(btree: Btree, value: &[u8]) -> u64 {
    let prepaid = match btree {
        Btree::Alloc => 104,
        Btree::Freespace | Btree::Lru | Btree::NeedDiscard => 40,
        Btree::BucketGens => 296,
        Btree::Backpointers if value.get(2) == Some(&data_type::BTREE) => 72,
        _ => 0,
    };
    super::key_bytes(value).saturating_sub(prepaid) as u64
}

pub(super) struct Metadata {
    pub bytes: u64,
    pub buckets: u64,
    pub nodes: u64,
    pub baseline: BTreeSet<u64>,
    pub sparse: BTreeSet<u64>,
    pub sparse_data: BTreeSet<u64>,
}

pub(super) async fn metadata_budget<S: Source>(source: &S) -> Result<Metadata> {
    let mut bytes = 0u64;
    let roots: BTreeSet<u64> = Btree::ALL.iter().filter_map(|&b| source.root_of(b).map(|r| r.ptr.ptr.sector)).collect();
    let mut baseline = BTreeSet::new();
    let mut nodes = 0;
    let mut sparse = BTreeSet::new();
    let mut sparse_data = BTreeSet::new();
    let shift = match source.superblock().flag(6, 40, 8) { 0 => 10, value => value as u32 };
    for btree in Btree::ALL {
        source.walk(btree, Bpos::MIN, |key, value| {
            bytes += charge(btree, value);
            if btree == Btree::Backpointers && value.get(2) == Some(&data_type::BTREE) {
                nodes += 1;
                let sector = key.pos.offset >> shift;
                if !roots.contains(&sector) { baseline.insert(sector); }
            }
            if btree == Btree::Alloc {
                let bucket = super::AllocV4::decode(value)?;
                if bucket.data_type == data_type::BTREE && u64::from(bucket.dirty) <= u64::from(source.superblock().member.bucket_sectors) / 2 {
                    sparse.insert(key.pos.offset);
                }
                if bucket.data_type == data_type::USER && u64::from(bucket.dirty) <= u64::from(source.superblock().member.bucket_sectors) / 2 {
                    sparse_data.insert(key.pos.offset);
                }
            }
            Ok(true)
        }).await?;
    }
    let pos = accounting_pos(&[class::DEV_DATA_TYPE, 0, data_type::BTREE]);
    let buckets = match source.get(Btree::Accounting, pos).await? {
        Some((_, value)) => journal::counters(&value)?.first().copied().unwrap_or(0),
        None => 0,
    };
    Ok(Metadata { bytes, buckets, nodes, baseline, sparse, sparse_data })
}

/// What a transaction's nodes may take of the room left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Draw {
    /// Leaves the freeing reserve.
    Ordinary,
    /// Removes names or nodes: unlink, rmdir, an orphan's release. It takes
    /// the freeing reserve too, so a full volume can still be emptied (§2.3).
    Freeing,
    Reserved(u64),
}

/// Node slots one freeing transaction can need: each node it dirties
/// rewritten in common.
const FREEING_RESERVE: u64 = (COMMON_REWRITE * FOOTPRINT) as u64;

/// Node slots past [`COMMON_REWRITE`] the oversized nodes of two transactions
/// can need: a node read packed from upstream splits in up to
/// [`MAX_REWRITE`] (`reclaim.rs`), and it can be changed only by the one
/// transaction in the slot and reclaim's, as no other enters while a changed
/// node is oversized.
const OVERSIZED: u64 = ((MAX_REWRITE - COMMON_REWRITE) * (FOOTPRINT + RECLAIM_ROOM)) as u64;

/// Btree node slots, and the buckets that will hold them.
pub(super) struct Allocator {
    /// Node slots of a bucket.
    per_bucket: u64,
    /// The bucket being filled, its next slot and its generation.
    open: Option<(u64, u64, u8)>,
    data: Option<(u64, u64, u8)>,
    pins: BTreeMap<u64, (usize, Option<u64>)>,
    holes: BTreeSet<(u64, u64)>,
    durable: u64,
    /// Buckets taken whose filling is not yet applied, which the indexes
    /// still list as empty.
    taken: BTreeSet<u64>,
    metadata_taken: BTreeSet<u64>,
    /// Empty buckets on the medium, not taken.
    free: u64,
    /// Buckets each sequence emptied, until it is durable.
    returning: BTreeMap<u64, u64>,
    /// Buckets taken that a previous life emptied, for the tests.
    #[cfg(test)]
    pub reused: usize,
}

impl Allocator {
    /// `free` empty buckets of `per_bucket` node slots each.
    pub(super) fn new(per_bucket: u64, free: u64) -> Self {
        Allocator {
            per_bucket,
            open: None,
            data: None,
            pins: BTreeMap::new(),
            holes: BTreeSet::new(),
            durable: 0,
            taken: BTreeSet::new(),
            metadata_taken: BTreeSet::new(),
            free,
            returning: BTreeMap::new(),
            #[cfg(test)]
            reused: 0,
        }
    }

    fn open_slots(&self) -> u64 {
        self.open.map_or(0, |(_, next, _)| self.per_bucket - next)
    }

    /// Node slots there are or will be once every emptying is durable.
    fn slots(&self) -> u64 {
        self.open_slots() + self.per_bucket * (self.free + self.returning.values().sum::<u64>() - self.pins.values().filter(|(_, empty)| empty.is_some()).count() as u64)
    }

    pub(super) fn capacity(&self, metadata_buckets: u64) -> u64 {
        let pinned = self.pins.values().filter(|(_, empty)| empty.is_some()).count() as u64;
        (self.free + self.returning.values().sum::<u64>() - pinned) * self.per_bucket + self.occupied_metadata(metadata_buckets)
    }

    pub(super) fn occupied_metadata(&self, metadata_buckets: u64) -> u64 {
        (metadata_buckets + self.metadata_taken.len() as u64) * self.per_bucket
    }

    pub(super) fn metadata_open(&self, bucket: u64) -> bool {
        self.open.is_some_and(|(b, _, _)| b == bucket)
    }

    pub(super) fn movable_data(&self, bucket: u64) -> bool {
        !self.pins.contains_key(&bucket) && self.data.is_none_or(|(b, _, _)| b != bucket)
    }

    pub(super) fn stable_slots(&self, nodes: u64) -> u64 {
        // A closed bucket that cannot be evacuated is more than half
        // full. One open bucket may be arbitrarily empty.
        nodes * if self.per_bucket == 1 { 1 } else { 2 } + self.per_bucket
    }

    /// Node slots to be had now, past `blocked` buckets that a traversal
    /// may still lead into.
    pub(super) fn ready(&self, blocked: usize) -> u64 {
        let pinned = self.pins.values().filter(|(_, empty)| empty.is_some_and(|seq| seq <= self.durable)).count() as u64;
        self.open_slots() + self.per_bucket * self.free.saturating_sub(blocked as u64 + pinned)
    }

    /// Whether an emptying waits to be durable.
    pub(super) fn returning(&self) -> bool {
        !self.returning.is_empty()
    }

    /// Whether `draw` may hold `nodes` dirty, unpointed or granted. Each
    /// such node's next write takes at most [`COMMON_REWRITE`] slots, as do
    /// each of the `reclaim_room` nodes reclaim's own updates may dirty past
    /// them, and [`OVERSIZED`] covers nodes read packed. A write that
    /// replaces a node gives its bucket back only once that is durable, so
    /// nothing here counts on it.
    pub(super) fn transient(nodes: usize, reclaim_room: usize, draw: Draw) -> u64 {
        let reserve = match draw {
            Draw::Ordinary | Draw::Reserved(_) => FREEING_RESERVE,
            Draw::Freeing => 0,
        };
        (COMMON_REWRITE * (nodes + reclaim_room)) as u64 + OVERSIZED + reserve
    }

    pub(super) fn fits(&self, nodes: usize, reclaim_room: usize, draw: Draw) -> bool {
        Self::transient(nodes, reclaim_room, draw) <= self.slots()
    }

    pub(super) fn keeps(&self, metadata_buckets: u64, stable: u64, nodes: usize, reclaim_room: usize, draw: Draw, data: bool) -> bool {
        // Imported packing debt must not prevent its own removal. Freeing
        // still draws on the physical reserve checked by fits/data_fits.
        matches!(draw, Draw::Freeing) || self.stable_slots(stable) + Self::transient(nodes, reclaim_room, draw) + if data { self.per_bucket } else { 0 } <= self.capacity(metadata_buckets)
    }

    /// The open bucket's next slot: its sector and generation.
    pub(super) fn next_slot(&mut self, bucket_sectors: u64, node: u64) -> Option<(u64, u8)> {
        let (b, next, gen) = self.open.filter(|&(_, next, _)| next < self.per_bucket)?;
        self.open = (next + 1 < self.per_bucket).then_some((b, next + 1, gen));
        Some((b * bucket_sectors + next * node, gen))
    }

    pub(super) fn is_taken(&self, b: u64) -> bool {
        self.taken.contains(&b) || self.pins.contains_key(&b)
    }

    /// `b`, empty at generation `gen`, opened for nodes.
    pub(super) fn take(&mut self, b: u64, gen: u8) {
        self.taken.insert(b);
        self.metadata_taken.insert(b);
        self.free = self.free.saturating_sub(1);
        self.open = Some((b, 0, gen));
    }

    pub(super) fn take_data(&mut self, b: u64, gen: u8) {
        self.taken.insert(b);
        self.free = self.free.saturating_sub(1);
        self.data = Some((b, 0, gen));
    }

    pub(super) fn next_data(&mut self, bucket_sectors: u64, sectors: u64, fixed: bool) -> Option<(u64, u8)> {
        if fixed {
            if let Some((_, sector)) = self.holes.pop_first() { return Some((sector, 0)); }
        }
        let (b, next, gen) = self.data?;
        if sectors > bucket_sectors - next {
            self.data = None;
            return None;
        }
        self.data = (next + sectors < bucket_sectors).then_some((b, next + sectors, gen));
        Some((b * bucket_sectors + next, gen))
    }

    pub(super) fn data_fits(&self, nodes: usize, reclaim_room: usize, draw: Draw) -> bool {
        let need = Self::transient(nodes, reclaim_room, draw);
        need + self.per_bucket <= self.slots()
    }

    pub(super) fn free_bytes(&self, bucket_bytes: u64) -> u64 {
        let pinned = self.pins.values().filter(|(_, empty)| empty.is_some_and(|seq| seq <= self.durable)).count() as u64;
        self.free.saturating_sub(pinned) * bucket_bytes
    }

    pub(super) fn pin(&mut self, b: u64) {
        self.pins.entry(b).or_insert((0, None)).0 += 1;
    }

    pub(super) fn unpin(&mut self, b: u64) {
        let entry = self.pins.get_mut(&b).expect("a reserved bucket is pinned");
        entry.0 -= 1;
        if entry.0 == 0 { self.pins.remove(&b); }
    }

    pub(super) fn cancel_data(&mut self, sector: u64, bucket: u64) {
        let b = sector / bucket;
        let empty = self.pins[&b].1.is_some();
        self.unpin(b);
        if empty { return; }
        if self.taken.contains(&b) && !self.pins.contains_key(&b) {
            self.taken.remove(&b);
            self.free += 1;
            if self.data.is_some_and(|(open, _, _)| open == b) { self.data = None; }
            self.holes.retain(|(bucket, _)| *bucket != b);
        } else {
            self.holes.insert((b, sector));
        }
    }

    /// `b`'s filling applied: the indexes no longer list it.
    pub(super) fn filled(&mut self, b: u64) {
        self.taken.remove(&b);
        self.metadata_taken.remove(&b);
        if let Some((_, empty)) = self.pins.get_mut(&b) {
            if let Some(seq) = empty.take() {
                if seq <= self.durable {
                    self.free -= 1;
                } else {
                    let count = self.returning.get_mut(&seq).expect("a pinned emptying is returning");
                    *count -= 1;
                    if *count == 0 { self.returning.remove(&seq); }
                }
            }
        }
    }

    /// `b` emptied by sequence `seq`.
    pub(super) fn emptied(&mut self, b: u64, seq: u64) {
        // A round's own nodes go in the open bucket first, so the round
        // that releases its last node fills it again.
        assert!(self.open.is_none_or(|(open, _, _)| open != b), "the open bucket {b} emptied");
        if self.data.is_some_and(|(open, _, _)| open == b) {
            self.data = None;
        }
        if let Some((_, empty)) = self.pins.get_mut(&b) { *empty = Some(seq); }
        self.holes.retain(|(bucket, _)| *bucket != b);
        *self.returning.entry(seq).or_default() += 1;
    }

    /// The journal written through `seq`: what it emptied is free.
    pub(super) fn durable_through(&mut self, seq: u64) {
        self.durable = seq;
        let later = self.returning.split_off(&(seq + 1));
        self.free += self.returning.values().sum::<u64>();
        self.returning = later;
    }
}

/// Traversals in progress, against the buckets they may still lead into.
#[derive(Default)]
pub(super) struct Readers {
    /// Advanced at each bucket emptied: a traversal begun since cannot hold
    /// a pointer into it.
    epoch: u64,
    /// Traversals in progress, counted by the epoch each began in.
    active: BTreeMap<u64, usize>,
    /// Buckets this mount emptied that a traversal may still lead into, each
    /// with the epoch it emptied in.
    emptied: BTreeMap<u64, u64>,
}

impl Readers {
    pub(super) fn begin(&mut self) -> u64 {
        *self.active.entry(self.epoch).or_default() += 1;
        self.epoch
    }

    /// Whether the oldest traversal ended.
    pub(super) fn end(&mut self, ticket: u64) -> bool {
        let oldest = self.active.first_key_value().map(|(&e, _)| e) == Some(ticket);
        let n = self.active.get_mut(&ticket).expect("a traversal begun");
        *n -= 1;
        if *n == 0 {
            self.active.remove(&ticket);
        }
        oldest
    }

    pub(super) fn emptied(&mut self, b: u64) {
        self.emptied.insert(b, self.epoch);
        self.epoch += 1;
    }

    /// Forgets the buckets no traversal still in progress began before.
    fn prune(&mut self) {
        match self.active.first_key_value() {
            Some((&oldest, _)) => self.emptied.retain(|_, &mut e| e >= oldest),
            None => self.emptied.clear(),
        }
    }

    /// Whether a traversal may still lead into `b`.
    pub(super) fn blocks(&mut self, b: u64) -> bool {
        self.prune();
        self.emptied.contains_key(&b)
    }

    /// Buckets a traversal may still lead into.
    pub(super) fn blocked(&mut self) -> usize {
        self.prune();
        self.emptied.len()
    }
}

/// Empty buckets `v` counts: free and need_discard, by its device
/// accounting (A7.12).
pub(super) async fn empty_buckets<S: Source>(v: &S) -> Result<u64> {
    let mut empty = 0u64;
    for t in [data_type::FREE, data_type::NEED_DISCARD] {
        let pos: Bpos = accounting_pos(&[class::DEV_DATA_TYPE, 0, t]);
        if let Some((_, value)) = v.get(Btree::Accounting, pos).await? {
            let buckets = journal::counters(&value)?.first().copied().unwrap_or(0);
            empty = empty.checked_add(buckets).ok_or(Error::Corrupt("empty bucket counts past 2^64"))?;
        }
    }
    if empty > v.superblock().member.nbuckets {
        return Err(Error::Corrupt("more empty buckets counted than the device has"));
    }
    Ok(empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emptied_bucket_counts_as_room_but_is_free_only_once_durable() {
        let mut a = Allocator::new(2, 1);
        a.take(9, 0);
        assert_eq!(a.next_slot(64, 32), Some((9 * 64, 0)));
        assert_eq!((a.ready(0), a.slots()), (1, 1));
        a.emptied(3, 7);
        assert_eq!((a.ready(0), a.slots(), a.returning()), (1, 3, true));
        a.durable_through(6);
        assert_eq!(a.ready(0), 1, "sequence 7 is not yet durable");
        a.durable_through(7);
        assert_eq!((a.ready(0), a.ready(1), a.returning()), (3, 1, false));
        assert_eq!(a.next_slot(64, 32), Some((9 * 64 + 32, 0)));
        assert_eq!(a.next_slot(64, 32), None, "the open bucket is full and closed");
    }

    #[test]
    fn a_bucket_emptied_is_blocked_until_every_traversal_begun_before_ends() {
        let mut r = Readers::default();
        let before = r.begin();
        r.emptied(4);
        let after = r.begin();
        assert!(r.blocks(4));
        assert!(!r.end(after), "a later traversal is not the oldest");
        assert!(r.blocks(4), "the earlier one still reads");
        let later = r.begin();
        assert!(r.end(before));
        assert!(!r.blocks(4), "only one begun after the emptying reads");
        r.end(later);
        assert_eq!(r.blocked(), 0);
    }
}
