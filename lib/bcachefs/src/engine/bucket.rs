//! A bucket's state as a transaction changes it, and the keys each change
//! writes: its alloc key, its freespace, need_discard and fragmentation-LRU
//! entries, its generation and the device's per-state accounting (A6,
//! A7.12). v1 changes buckets only by btree nodes: an empty bucket takes
//! nodes, and a btree bucket that loses its last node waits in need_discard
//! until its emptying is durable, when it takes nodes again (A6.19).

use super::bkey::Bpos;
use super::value::{class, data_type, AllocV4};
use super::{Error, Result};

/// The fragmentation LRU of device 0.
pub(crate) const FRAGMENTATION_LRU: u64 = 8192;

/// One bucket's alloc fields as v1 writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bucket {
    pub gen: u8,
    pub oldest_gen: u8,
    pub data_type: u8,
    pub dirty: u32,
    /// Set when it was made non-empty, cleared at the generation's raise.
    pub needs_gen_increment: bool,
    pub need_discard: bool,
    pub io_time: u64,
    pub journal_seq_nonempty: u64,
    pub journal_seq_empty: u64,
}

impl Bucket {
    /// A bucket with no alloc key: free at generation 0 (A6.10).
    pub(crate) const HOLE: Bucket = Bucket {
        gen: 0,
        oldest_gen: 0,
        data_type: data_type::FREE,
        dirty: 0,
        needs_gen_increment: false,
        need_discard: false,
        io_time: 0,
        journal_seq_nonempty: 0,
        journal_seq_empty: 0,
    };

    pub(crate) fn of(a: &AllocV4, flags: u32, io_time: u64, journal_seq_nonempty: u64) -> Bucket {
        Bucket {
            gen: a.gen,
            oldest_gen: a.oldest_gen,
            data_type: a.data_type,
            dirty: a.dirty,
            needs_gen_increment: flags & 2 != 0,
            need_discard: flags & 1 != 0,
            io_time,
            journal_seq_nonempty,
            journal_seq_empty: a.journal_seq_empty,
        }
    }

    /// The alloc_v4 value: 8 u64s, trailing records at 8, none present.
    /// Bytes of the alloc_v4 value it encodes.
    pub(crate) const BYTES: usize = 64;

    pub(crate) fn encode(&self) -> [u8; Self::BYTES] {
        let mut v = [0u8; Self::BYTES];
        v[..8].copy_from_slice(&self.journal_seq_nonempty.to_le_bytes());
        let flags = u32::from(self.need_discard) | u32::from(self.needs_gen_increment) << 1 | 8 << 2;
        v[8..12].copy_from_slice(&flags.to_le_bytes());
        v[12] = self.gen;
        v[13] = self.oldest_gen;
        v[14] = self.data_type;
        v[16..20].copy_from_slice(&self.dirty.to_le_bytes());
        v[24..32].copy_from_slice(&self.io_time.to_le_bytes());
        v[32..40].copy_from_slice(&self.io_time.to_le_bytes());
        v[48..56].copy_from_slice(&self.journal_seq_empty.to_le_bytes());
        v
    }

    /// Sectors of a non-empty bucket no live data fills, as accounting and
    /// the fragmentation LRU count them.
    pub(crate) fn fragmented(&self, bucket_sectors: u32) -> u32 {
        match self.data_type {
            t if data_type::empty(t) => 0,
            _ if self.dirty == 0 => bucket_sectors,
            _ => bucket_sectors.saturating_sub(self.dirty),
        }
    }

    /// The fragmentation LRU entry it needs, if any (A6.24).
    pub(crate) fn lru(&self, bucket: u64, bucket_sectors: u32) -> Option<Bpos> {
        let movable = matches!(self.data_type, data_type::BTREE | data_type::USER);
        let dirty = u64::from(self.dirty);
        (movable && 0 < dirty && dirty < u64::from(bucket_sectors)).then(|| {
            let time = dirty * (1 << 31) / u64::from(bucket_sectors);
            Bpos::new(FRAGMENTATION_LRU << 48 | time, bucket, 0)
        })
    }

    /// The generation an empty bucket's new nodes carry: a raise its
    /// emptying owed made first.
    pub(crate) fn reused_gen(&self) -> u8 {
        let mut b = *self;
        b.owed_raise();
        b.gen
    }

    /// The raise of the generation an emptying owes (A6.18).
    fn owed_raise(&mut self) {
        if self.needs_gen_increment && data_type::empty(self.data_type) {
            if self.gen == self.oldest_gen {
                self.oldest_gen = self.oldest_gen.wrapping_add(1);
            }
            self.gen = self.gen.wrapping_add(1);
            self.needs_gen_increment = false;
        }
    }

    /// `node` sectors of btree node added (A6.17): to an empty bucket, free
    /// or one whose emptying is durable, as need_discard then free would
    /// leave it (A6.19). `Corrupt` when the alloc key holds the bucket for
    /// anything else.
    pub(crate) fn fill(&mut self, node: u32, io_time: u64, bucket_sectors: u32) -> Result<()> {
        if matches!(self.data_type, data_type::FREE | data_type::NEED_DISCARD) {
            self.owed_raise();
            self.data_type = data_type::BTREE;
            self.needs_gen_increment = true;
            self.need_discard = true;
            self.io_time = io_time.max(1);
            self.journal_seq_nonempty = 0;
            self.journal_seq_empty = 0;
        }
        if self.data_type != data_type::BTREE || self.dirty + node > bucket_sectors {
            return Err(Error::Corrupt("a btree node placed in a bucket its alloc key holds otherwise"));
        }
        self.dirty += node;
        Ok(())
    }

    /// `node` sectors of btree node released. A bucket left empty waits in
    /// need_discard, its generation raised (A6.18); the caller stamps its
    /// empty sequence. Whether it was emptied; `Corrupt` when the alloc key
    /// does not hold the node.
    pub(crate) fn drain(&mut self, node: u32) -> Result<bool> {
        if self.data_type != data_type::BTREE || self.dirty < node {
            return Err(Error::Corrupt("a btree node released from a bucket its alloc key does not hold it in"));
        }
        self.dirty -= node;
        if self.dirty > 0 {
            return Ok(false);
        }
        self.data_type = data_type::NEED_DISCARD;
        self.owed_raise();
        Ok(true)
    }

    /// The device accounting record of its state, and its counters (A7.12).
    pub(crate) fn accounting(&self, bucket_sectors: u32) -> ([u8; 3], [u64; 3]) {
        ([class::DEV_DATA_TYPE, 0, self.data_type], [1, u64::from(self.dirty), u64::from(self.fragmented(bucket_sectors))])
    }
}
