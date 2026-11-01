//! What the allocator and accounting btrees hold, derived from a volume's
//! node pointers and leaf keys: bucket usage, backpointers, freespace,
//! need_discard, bucket_gens, the fragmentation LRU and accounting. mkfs
//! writes exactly this; fsck compares a volume against it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::bkey::{Bpos, Key, KEY_U64S};
use super::fsck::Finding;
use super::superblock::Superblock;
use super::value::{self, accounting_pos, class, data_type, AllocV4, Extent, Ptr, BUCKET_GENS_BYTES};
use super::{kind, Btree, Error, Result};

/// The device as the derivation sees it, sizes in sectors.
pub(crate) struct Space {
    pub bucket: u64,
    pub nbuckets: u64,
    pub first_bucket: u64,
    pub node: u64,
    /// Low bits of a backpointer's offset that tell apart pieces of one
    /// extent.
    pub bp_shift: u32,
    /// Sector ranges the superblock layout reserves.
    pub sb: Vec<(u64, u64)>,
    /// Journal buckets: each range's first bucket and count.
    pub journal: Vec<(u64, u64)>,
}

/// The widest backpointer shift a 44-bit sector leaves room for.
const MAX_BP_SHIFT: u32 = 20;
/// The fragmentation LRU of device 0.
const FRAGMENTATION_LRU: u64 = 8192;

impl Space {
    pub(crate) fn of(sb: &Superblock) -> Result<Space> {
        let bp_shift = match sb.flag(6, 40, 8) as u32 {
            0 => 10,
            s if s <= MAX_BP_SHIFT => s,
            _ => return Err(Error::Unsupported("backpointer shift past 20")),
        };
        let reserve = 1u64 << sb.header.layout.max_bits;
        let mut regions = alloc::vec![(0, super::superblock::PRIMARY)];
        regions.extend(sb.header.layout.offsets.iter().map(|&o| (o, o.saturating_add(reserve))));
        Ok(Space {
            bucket: u64::from(sb.member.bucket_sectors),
            nbuckets: sb.member.nbuckets,
            first_bucket: u64::from(sb.member.first_bucket),
            node: u64::from(sb.node_sectors()),
            bp_shift,
            sb: regions,
            journal: sb.journal.clone(),
        })
    }

    /// The bucket holding `[sector, sector + len)`, if one of the device's
    /// buckets holds all of it.
    fn bucket_of(&self, sector: u64, len: u64) -> Option<u64> {
        let b = sector / self.bucket;
        (b >= self.first_bucket && b < self.nbuckets && sector % self.bucket + len <= self.bucket).then_some(b)
    }
}

/// What one bucket holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    pub data_type: u8,
    pub dirty: u64,
    /// The generation its pointers carry; none for superblock and journal
    /// buckets, which nothing points into.
    pub gen: Option<u8>,
}

/// Usage, backpointers and accounting, summed key by key.
#[derive(Clone)]
pub(crate) struct Tally<'a> {
    space: &'a Space,
    buckets: BTreeMap<u64, Usage>,
    backpointers: BTreeMap<Bpos, [u8; 32]>,
    accounting: BTreeMap<Bpos, Vec<u64>>,
    /// Pointers and buckets that no consistent volume holds.
    pub findings: Vec<Finding>,
}

impl<'a> Tally<'a> {
    /// The superblock's and the journal's buckets marked.
    pub(crate) fn new(space: &'a Space) -> Self {
        let mut t = Tally { space, buckets: BTreeMap::new(), backpointers: BTreeMap::new(), accounting: BTreeMap::new(), findings: Vec::new() };
        for &(start, end) in &space.sb {
            let last = end.div_ceil(space.bucket).min(space.nbuckets);
            for b in start / space.bucket..last {
                let sectors = end.min((b + 1) * space.bucket) - start.max(b * space.bucket);
                t.mark(b, data_type::SB, sectors, None);
            }
        }
        for &(first, count) in &space.journal {
            for b in first..first + count {
                t.mark(b, data_type::JOURNAL, space.bucket, None);
            }
        }
        t
    }

    fn mark(&mut self, bucket: u64, data_type: u8, sectors: u64, gen: Option<u8>) {
        let usage = self.buckets.entry(bucket).or_insert(Usage { data_type, dirty: 0, gen });
        if usage.data_type != data_type {
            self.findings.push(Finding::Bucket { bucket, rule: "two data types in one bucket" });
        }
        if gen.is_some() && usage.gen != gen {
            self.findings.push(Finding::Bucket { bucket, rule: "pointers of two generations into one bucket" });
        }
        let full = usage.dirty <= self.space.bucket;
        usage.dirty += sectors;
        if full && usage.dirty > self.space.bucket {
            self.findings.push(Finding::Bucket { bucket, rule: "more sectors than the bucket holds" });
        }
    }

    fn count(&mut self, record: &[u8], counters: &[u64]) {
        let sums = self.accounting.entry(accounting_pos(record)).or_insert_with(|| alloc::vec![0; counters.len()]);
        for (sum, c) in sums.iter_mut().zip(counters) {
            *sum = sum.wrapping_add(*c);
        }
    }

    fn backpointer(&mut self, btree: Btree, pos: Bpos, at: Bpos, value: [u8; 32]) {
        if self.backpointers.insert(at, value).is_some() {
            self.findings.push(Finding::Pointer { btree, pos, rule: "two pointers to one backpointer's position" });
        }
    }

    /// The node `ptr` points at, of `btree` at `level`, whose max key is
    /// `max`. A root's pointer counts as any other.
    pub(crate) fn node(&mut self, btree: Btree, level: u8, ptr: &Ptr, max: Bpos) {
        let node = self.space.node;
        let Some(bucket) = self.space.bucket_of(ptr.sector, node) else {
            self.findings.push(Finding::Pointer { btree, pos: max, rule: "node outside one bucket of the device" });
            return;
        };
        self.mark(bucket, data_type::BTREE, node, Some(ptr.gen));
        let at = Bpos::new(u64::from(ptr.dev), ptr.sector << self.space.bp_shift, 0);
        self.backpointer(btree, max, at, value::backpointer(btree as u8, level + 1, data_type::BTREE, ptr.gen, node as u32, max));
        self.count(&[class::REPLICAS, data_type::BTREE, 1, 1, 0], &[node]);
        let mut record = alloc::vec![class::BTREE];
        record.extend_from_slice(&(btree as u32).to_le_bytes());
        self.count(&record, &[node, 1, u64::from(level > 0)]);
    }

    /// A key of a leaf of `btree`.
    pub(crate) fn leaf(&mut self, btree: Btree, key: &Key, value: &[u8]) -> Result<()> {
        let mut external = 0;
        match (btree, key.kind) {
            (Btree::Inodes, kind::INODE | kind::INODE_V2 | kind::INODE_V3) => self.count(&[class::NR_INODES], &[1]),
            (Btree::Extents, kind::EXTENT) => external = self.extent(key, value)?,
            (Btree::Extents, kind::RESERVATION) => return Err(Error::Unsupported("reservation extent")),
            (Btree::Extents, kind::REFLINK_P) => return Err(Error::Unsupported("reflinked extent")),
            _ => {}
        }
        let counted = matches!(btree, Btree::Extents | Btree::Inodes | Btree::Dirents | Btree::Xattrs | Btree::Damage);
        if counted && key.pos.snapshot != 0 && !matches!(key.kind, kind::DELETED | kind::WHITEOUT | kind::EXTENT_WHITEOUT) {
            let mut record = alloc::vec![class::SNAPSHOT];
            record.extend_from_slice(&key.pos.snapshot.to_le_bytes());
            record.extend_from_slice(&(btree as u32).to_le_bytes());
            let bytes = 8 * (KEY_U64S + value.len() / 8) as u64;
            self.count(&record, &[1, bytes, external]);
        }
        Ok(())
    }

    /// An extent's one pointer: its bucket, backpointer and accounting.
    /// Its sectors on the device.
    fn extent(&mut self, key: &Key, value: &[u8]) -> Result<u64> {
        let extent = Extent::decode(value)?;
        let ptr = extent.ptr;
        if ptr.cached || ptr.unwritten {
            return Err(Error::Unsupported("cached or unwritten extent"));
        }
        let (offset, encoded) = match extent.crc {
            Some(crc) if crc.compression != 0 || crc.compressed != crc.uncompressed => return Err(Error::Unsupported("compressed extent")),
            Some(crc) => (u64::from(crc.offset), u64::from(crc.compressed)),
            None => (0, u64::from(key.size)),
        };
        let sectors = u64::from(key.size);
        let Some(bucket) = self.space.bucket_of(ptr.sector, encoded) else {
            self.findings.push(Finding::Pointer { btree: Btree::Extents, pos: key.pos, rule: "extent outside one bucket of the device" });
            return Ok(sectors);
        };
        self.mark(bucket, data_type::USER, sectors, Some(ptr.gen));
        let at = Bpos::new(u64::from(ptr.dev), (ptr.sector << self.space.bp_shift) + offset, 0);
        self.backpointer(Btree::Extents, key.pos, at, value::backpointer(0, 0, data_type::USER, ptr.gen, key.size, key.pos));
        self.count(&[class::REPLICAS, data_type::USER, 1, 1, 0], &[sectors]);
        let mut record = alloc::vec![class::INUM];
        record.extend_from_slice(&key.pos.inode.to_le_bytes());
        self.count(&record, &[1, sectors, sectors]);
        Ok(sectors)
    }

    /// What every derived btree holds, given the stored alloc keys of the
    /// buckets that have them: they keep their generations, and an empty
    /// bucket its empty state. A fresh volume has none.
    pub(crate) fn finish(mut self, stored: &BTreeMap<u64, AllocV4>) -> Derived {
        let s = self.space;
        let mut states: BTreeMap<u8, [u64; 3]> = BTreeMap::new();
        let mut freespace: Vec<(Bpos, u32)> = Vec::new();
        let mut need_discard = Vec::new();
        let mut bucket_gens: BTreeMap<u64, [u8; BUCKET_GENS_BYTES]> = BTreeMap::new();
        let mut lru = Vec::new();
        let mut run: Option<(u64, u64, u64)> = None; // first, end, generation bits
        let flush = |run: Option<(u64, u64, u64)>, freespace: &mut Vec<(Bpos, u32)>| {
            if let Some((first, end, bits)) = run {
                freespace.push((Bpos::new(0, end | bits << 56, 0), (end - first) as u32));
            }
        };
        for b in s.first_bucket..s.nbuckets {
            let alloc = stored.get(&b).copied().unwrap_or_default();
            let usage = self.buckets.get(&b);
            let state = match usage {
                Some(u) => u.data_type,
                None if data_type::empty(alloc.data_type) => alloc.data_type,
                None => data_type::FREE,
            };
            let gen = usage.and_then(|u| u.gen).unwrap_or(alloc.gen);
            if gen != 0 {
                bucket_gens.entry(b >> 8).or_insert([0; BUCKET_GENS_BYTES])[(b & 255) as usize] = gen;
            }
            let dirty = usage.map_or(0, |u| u.dirty);
            let fragmented = match state {
                _ if data_type::empty(state) => 0,
                _ if dirty == 0 => s.bucket,
                _ => s.bucket.saturating_sub(dirty),
            };
            let counts = states.entry(state).or_default();
            counts[0] += 1;
            counts[1] += dirty;
            counts[2] += fragmented;
            if matches!(state, data_type::BTREE | data_type::USER) && 0 < dirty && dirty < s.bucket {
                let time = dirty * (1 << 31) / s.bucket;
                lru.push(Bpos::new(FRAGMENTATION_LRU << 48 | time, b, 0));
            }
            if state == data_type::NEED_DISCARD {
                need_discard.push(Bpos::new(alloc.journal_seq_empty, b, 0));
            }
            let bits = u64::from(gen.wrapping_sub(alloc.oldest_gen) >> 4);
            run = match run {
                Some((first, end, b_bits)) if state == data_type::FREE && end == b && b_bits == bits => Some((first, b + 1, bits)),
                _ => {
                    flush(run, &mut freespace);
                    (state == data_type::FREE).then_some((b, b + 1, bits))
                }
            };
        }
        flush(run, &mut freespace);
        for (state, counts) in states {
            self.count(&[class::DEV_DATA_TYPE, 0, state], &counts);
        }
        // A total of zero is compared as absent (A7.27, A7.28).
        self.accounting.retain(|_, c| c.iter().any(|&c| c != 0));
        Derived { buckets: self.buckets, freespace, need_discard, bucket_gens, lru, backpointers: self.backpointers, accounting: self.accounting }
    }
}

/// The derived btrees' contents.
pub(crate) struct Derived {
    /// Every bucket that holds sectors.
    pub buckets: BTreeMap<u64, Usage>,
    /// Each free range's key position and size, ranges as long as their
    /// buckets' generation bits agree.
    pub freespace: Vec<(Bpos, u32)>,
    pub need_discard: Vec<Bpos>,
    /// Each group of 256 buckets with a nonzero generation.
    pub bucket_gens: BTreeMap<u64, [u8; BUCKET_GENS_BYTES]>,
    pub lru: Vec<Bpos>,
    pub backpointers: BTreeMap<Bpos, [u8; 32]>,
    pub accounting: BTreeMap<Bpos, Vec<u64>>,
}
