//! A minimal complete volume: superblock copies, an empty
//! journal and `clean`, the root subvolume, snapshot, snapshot tree and
//! root directory, the inode cursor, and the allocator and accounting state
//! of exactly those nodes. Nothing it writes names a journal sequence, so
//! `clean`'s is 0.

use alloc::vec::Vec;

use toyos_blockdev::{BlockDev, BLOCK};
use toyos_crypto::Random;

use super::bkey::{Bpos, Key};
use super::crypt::MasterKey;
use super::node::{self, Build};
use super::superblock::{entry_type, field_type, member_flag, Crypt, Field, Header, Kdf, Layout, Member, Superblock, KEY_VERSION, LAYOUT, MAX_NODE_SECTORS, PRIMARY};
use super::value::{self, class, data_type, inode_field, Alloc, InodeValue, NodePtr, Ptr};
use super::{kind, put16, put64, Btree, Error, Result, SECTOR, VERSION};

/// What a volume is formatted with.
pub struct Options<'a> {
    /// At most 31 bytes.
    pub label: &'a [u8],
    /// Nanoseconds since the Unix epoch: the volume's time base and every
    /// time it records.
    pub time: u64,
    /// ChaCha20/Poly1305 with 128-bit MACs for data and metadata, its key
    /// wrapped under a passphrase; `None` for crc32c.
    pub encryption: Option<Encryption<'a>>,
}

/// How the master key is wrapped.
pub struct Encryption<'a> {
    /// The exact bytes upstream's tools take as the passphrase: no NUL, and
    /// no trailing newline, which they strip.
    pub passphrase: &'a [u8],
    pub kdf: Kdf,
}

/// The root directory's inode number, the root subvolume's id and its
/// snapshot.
pub(crate) const ROOT_INODE: u64 = 4096;
pub(crate) const ROOT_SUBVOLUME: u32 = 1;
pub(crate) const ROOT_SNAPSHOT: u32 = u32::MAX;
pub(crate) const SNAPSHOT_TREE: u32 = 1;
/// The first inode number the cursor hands out.
pub(crate) const FIRST_INODE: u64 = 1 << 31;

/// Each copy's reserve, 1 MiB as `format` lays it out.
const SB_MAX_BITS: u8 = 11;
const SB_SECTORS: u64 = 1 << SB_MAX_BITS;
/// Block size: the device's (`toyos_blockdev::BLOCK`), in sectors.
const BLOCK_SECTORS: u64 = (BLOCK / SECTOR) as u64;
/// Bucket sizes from 16 KiB to 2 MiB, at least 2 048 buckets where the
/// device allows.
const MIN_BUCKET: u64 = 32;
const MAX_BUCKET: u64 = 4096;
const WANT_BUCKETS: u64 = 2048;
const MIN_BUCKETS: u64 = 512;

/// Every btree a v1 volume writes, each given a root here: a node count
/// that only grows with its keys keeps the layout's fixed point monotone.
const BTREES: [Btree; 15] = [
    Btree::Extents,
    Btree::Inodes,
    Btree::Dirents,
    Btree::Alloc,
    Btree::Subvolumes,
    Btree::Snapshots,
    Btree::Lru,
    Btree::Freespace,
    Btree::NeedDiscard,
    Btree::Backpointers,
    Btree::BucketGens,
    Btree::SnapshotTrees,
    Btree::DeletedInodes,
    Btree::LoggedOps,
    Btree::Accounting,
];

/// Writes the volume to `dev`, every identifier and key drawn from `random`,
/// then flushes.
pub async fn mkfs(dev: &impl BlockDev, options: &Options<'_>, random: &mut impl Random) -> Result<()> {
    if options.label.len() > 31 {
        return Err(Error::Invalid("label longer than 31 bytes"));
    }
    if options.encryption.as_ref().is_some_and(|e| e.passphrase.contains(&0)) {
        return Err(Error::Invalid("passphrase holding NUL"));
    }
    let geometry = Geometry::new(dev.blocks() * BLOCK_SECTORS)?;
    let mut draw = Draw(random);
    let internal_uuid = draw.uuid()?;
    let user_uuid = draw.uuid()?;
    let member_uuid = draw.uuid()?;
    let hash_seed = draw.u64()?;
    let key = match &options.encryption {
        Some(_) => Some(MasterKey::new(draw.bytes()?)),
        None => None,
    };

    // The least node count that holds the keys of its own nodes, from below.
    let mut nodes = BTREES.len() as u64;
    let plan = loop {
        let plan = Plan::new(&geometry, nodes, hash_seed)?;
        if plan.nodes() == nodes {
            break plan;
        }
        assert!(plan.nodes() > nodes, "the layout's node count shrank as its nodes grew");
        nodes = plan.nodes();
    };
    let seqs = (0..nodes).map(|_| draw.nonzero()).collect::<Result<Vec<u64>>>()?;
    let tree = plan.lay_out(&geometry, &seqs)?;

    let magic = node::magic(&internal_uuid);
    for n in &tree.nodes {
        let build = Build { btree: n.btree, level: n.level, seq: n.seq, min: n.min, max: n.max, journal_seq: 0, keys: &n.keys };
        let bytes = node::encode(magic, BLOCK, key.as_ref(), &build)?;
        write(dev, n.sector, &bytes).await?;
    }

    let member = Member {
        uuid: member_uuid,
        nbuckets: geometry.nbuckets,
        first_bucket: 0,
        bucket_sectors: geometry.bucket as u16,
        bitmap_shift: tree.bitmap_shift,
        last_mount: options.time / 1_000_000_000,
        flags: member_flag::DATA_ALLOWED | member_flag::DURABILITY_1 | member_flag::FREESPACE_INITIALIZED | member_flag::ROTATIONAL_KNOWN,
        seq: 1,
        bitmap: tree.bitmap,
        journal_bucket: 0,
        journal_offset: 0,
        identity_zero: true,
    };
    let crypt = match (&options.encryption, &key) {
        (Some(e), Some(key)) => Some(Crypt { kdf: e.kdf, wrapped: key.wrapped(&internal_uuid, e.passphrase, e.kdf)? }),
        _ => None,
    };
    let mut label = [0u8; 32];
    label[..options.label.len()].copy_from_slice(options.label);
    let header = Header {
        version: VERSION,
        version_min: VERSION,
        internal_uuid,
        user_uuid,
        label,
        offset: 0,
        seq: 1,
        block_sectors: BLOCK_SECTORS as u16,
        dev_idx: 0,
        nr_devices: 1,
        time_base: options.time,
        time_precision: 1,
        flags: flags(geometry.node, crypt.is_some()),
        write_time: options.time / 1_000_000_000,
        features: [FEATURES, 0],
        compat: [COMPAT_INITIALIZED | COMPAT_CLEAN, 0],
        layout: geometry.layout.clone(),
    };
    let fields = fields(&geometry, &member, crypt.as_ref(), &tree.roots);
    for &offset in &geometry.layout.offsets {
        write(dev, offset, &Superblock::encode(&header, &fields, offset)).await?;
    }
    // Sectors 0–6 are zeros here; the layout copy is sector 7.
    let mut first = alloc::vec![0u8; BLOCK];
    let at = LAYOUT as usize * SECTOR;
    first[at..at + Layout::BYTES].copy_from_slice(&geometry.layout.encode());
    write(dev, 0, &first).await?;
    dev.flush().await?;
    Ok(())
}

/// The feature bits `format` sets, all of a clean volume's.
pub(crate) const FEATURES: u64 = 0x000f_ba80;
/// Compat bits 2–5, set at initialization; 0–1 at a clean shutdown.
pub(crate) const COMPAT_INITIALIZED: u64 = 0x3c;
pub(crate) const COMPAT_CLEAN: u64 = 0x3;

/// Flag words 0–6 as `format` sets them, clean and
/// initialized, at node size `node` sectors.
fn flags(node: u64, encrypted: bool) -> [u64; 7] {
    let word0 = 1 // initialized
        | 1 << 1 // clean
        | 1 << 2 // superblock checksum crc32c
        | 1 << 8 // error action fix_safe
        | node << 12 // btree node size, sectors
        | 8 << 28 // copygc reserve, percent
        | 1 << 40 // metadata checksum crc32c
        | 1 << 44 // data checksum crc32c
        | 1 << 48 // metadata replicas wanted
        | 1 << 52 // data replicas wanted
        | 1 << 56 // POSIX ACLs
        | 1 << 63; // promote whole extents
    let encryption = if encrypted { 1 << 9 | 1 << 10 } else { 0 }; // wide_macs, ChaCha20/Poly1305
    let word1 = 2 // string hash siphash
        | encryption
        | 9 << 14 // encoded extent max, 2^9 sectors
        | 1 << 20 // metadata replicas required
        | 1 << 24; // data replicas required
    let word3 = 1000 << 30; // journal flush delay, ms
    let word4 = 100 | 1 << 32; // journal reclaim delay, ms; transaction names
    let incompat = 1 << 10 | 36; // 1.36: no user replicas in the superblock
    let word5 = u64::from(VERSION) | 30 << 16 | incompat << 32 | u64::from(VERSION) << 48;
    let word6 = 30 << 4 | 3 << 14 | 16 << 40; // write error timeout, checksum retries, backpointer shift
    [word0, word1, 0, word3, word4, word5, word6]
}

/// `ext`'s extended flags as `format` writes them.
const EXT_FLAGS: u64 = 0x0900_2850_1000;
/// The downgrade field of 1.39 with incompat 1.36: the entries for 1.38 and
/// 1.39 that upstream regenerates on every superblock write.
pub(crate) const DOWNGRADE: [u8; 52] = [
    0x26, 0x04, 0x00, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0x00, 0x7c, 0x00, 0x72, 0x00, 0x7b, 0x00,
    0x27, 0x04, 0x20, 0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0x00, 0x83, 0x00, 0x69, 0x00, 0x10, 0x01,
];
/// Each extent entry type's size in u64s, pointer through reconcile_bp.
pub(crate) const EXTENT_TYPE_U64S: [u8; 9] = [1, 1, 2, 3, 1, 1, 1, 1, 1];

fn fields(geometry: &Geometry, member: &Member, crypt: Option<&Crypt>, roots: &[(Btree, u8, Vec<u8>)]) -> Vec<Field> {
    let mut ext = alloc::vec![0u8; 14 * 8];
    put64(&mut ext, 11 * 8, EXT_FLAGS);
    let mut members = alloc::vec![0u8; 8];
    put16(&mut members, 0, Member::BYTES as u16);
    members.extend_from_slice(&member.encode());
    let mut journal = alloc::vec![0u8; 16];
    put64(&mut journal, 0, geometry.journal.0);
    put64(&mut journal, 8, geometry.journal.1);

    // `clean`: sequence 0, the key version, both clocks at 0, every root.
    let mut clean = alloc::vec![0u8; 16];
    record(&mut clean, KEY_VERSION, 0, entry_type::USAGE, &0u64.to_le_bytes());
    for rw in 0..2u8 {
        let mut clock = [0u8; 16];
        clock[0] = rw;
        record(&mut clean, 0, 0, entry_type::CLOCK, &clock);
    }
    for (btree, level, key) in roots {
        record(&mut clean, *btree as u8, *level, entry_type::BTREE_ROOT, key);
    }

    let mut out = alloc::vec![
        Field { kind: field_type::EXT, body: ext },
        Field { kind: field_type::MEMBERS_V2, body: members },
    ];
    if let Some(crypt) = crypt {
        out.push(Field { kind: field_type::CRYPT, body: crypt.encode() });
    }
    out.extend([
        Field { kind: field_type::CLEAN, body: clean },
        Field { kind: field_type::EXTENT_TYPE_U64S, body: EXTENT_TYPE_U64S.to_vec() },
        Field { kind: field_type::DOWNGRADE, body: DOWNGRADE.to_vec() },
        Field { kind: field_type::JOURNAL_V2, body: journal },
        // The btree's replicas entry, on device 0.
        Field { kind: field_type::REPLICAS_V0, body: alloc::vec![data_type::BTREE, 1, 0] },
    ]);
    out
}

/// Appends a `jset_entry` record.
fn record(out: &mut Vec<u8>, btree: u8, level: u8, kind_: u8, payload: &[u8]) {
    let mut header = [0u8; 8];
    put16(&mut header, 0, (payload.len() / 8) as u16);
    header[2] = btree;
    header[3] = level;
    header[4] = kind_;
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
}

async fn write(dev: &impl BlockDev, sector: u64, bytes: &[u8]) -> Result<()> {
    let at = sector * SECTOR as u64;
    assert!(at.is_multiple_of(BLOCK as u64), "a write off a block boundary");
    let mut blocks = alloc::vec![[0u8; BLOCK]; bytes.len().div_ceil(BLOCK)];
    blocks.as_flattened_mut()[..bytes.len()].copy_from_slice(bytes);
    dev.write(at / BLOCK as u64, &blocks).await?;
    Ok(())
}

struct Draw<'r, R>(&'r mut R);

impl<R: Random> Draw<'_, R> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.0.fill(&mut b).map_err(|_| Error::Entropy)?;
        Ok(b)
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn nonzero(&mut self) -> Result<u64> {
        loop {
            let v = self.u64()?;
            if v != 0 {
                return Ok(v);
            }
        }
    }

    fn uuid(&mut self) -> Result<[u8; 16]> {
        loop {
            let v = self.bytes()?;
            if v != [0; 16] {
                return Ok(v);
            }
        }
    }
}

/// Where everything goes on a device of `sectors`.
struct Geometry {
    /// Sectors per bucket.
    bucket: u64,
    nbuckets: u64,
    /// Sectors per btree node.
    node: u64,
    layout: Layout,
    /// The journal's first bucket and bucket count.
    journal: (u64, u64),
    /// Superblock sectors in each bucket that holds any.
    sb: Vec<(u64, u64)>,
}

impl Geometry {
    fn new(sectors: u64) -> Result<Geometry> {
        let wanted = (sectors / WANT_BUCKETS).max(1);
        let bucket = (1u64 << wanted.ilog2()).clamp(MIN_BUCKET, MAX_BUCKET);
        let nbuckets = sectors / bucket;
        if nbuckets < MIN_BUCKETS || sectors < 4 * SB_SECTORS {
            return Err(Error::Invalid("device smaller than 512 buckets"));
        }
        let second = (PRIMARY + SB_SECTORS).next_multiple_of(BLOCK_SECTORS);
        let end = (sectors - SB_SECTORS) / bucket * bucket;
        if end < second + SB_SECTORS {
            return Err(Error::Invalid("device too small for three superblock copies"));
        }
        let layout = Layout { max_bits: SB_MAX_BITS, offsets: alloc::vec![PRIMARY, second, end] };
        let mut sb: Vec<(u64, u64)> = Vec::new();
        let regions = core::iter::once((0, PRIMARY)).chain(layout.offsets.iter().map(|&o| (o, o + SB_SECTORS)));
        for (start, stop) in regions {
            for b in start / bucket..stop.div_ceil(bucket).min(nbuckets) {
                let marked = stop.min((b + 1) * bucket) - start.max(b * bucket);
                match sb.iter_mut().find(|(bb, _)| *bb == b) {
                    Some((_, sectors)) => *sectors += marked,
                    None => sb.push((b, marked)),
                }
            }
        }
        sb.sort_unstable();
        let front = (second + SB_SECTORS).div_ceil(bucket);
        let journal = (front, (nbuckets / 128).max(8));
        Ok(Geometry { bucket, nbuckets, node: bucket.min(MAX_NODE_SECTORS), layout, journal, sb })
    }

    /// The first bucket btree nodes go in.
    fn btree_start(&self) -> u64 {
        self.journal.0 + self.journal.1
    }

    fn nodes_per_bucket(&self) -> u64 {
        self.bucket / self.node
    }

    /// The bucket and sector of node slot `i`.
    fn slot(&self, i: u64) -> (u64, u64) {
        let bucket = self.btree_start() + i / self.nodes_per_bucket();
        (bucket, bucket * self.bucket + i % self.nodes_per_bucket() * self.node)
    }

    /// Node counts of the buckets `n` nodes fill.
    fn btree_buckets(&self, n: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let per = self.nodes_per_bucket();
        (0..n.div_ceil(per)).map(move |i| (self.btree_start() + i, per.min(n - i * per)))
    }
}

/// Every btree's keys for `nodes` node slots, before the node values that
/// depend on where each node goes.
struct Plan {
    /// Per btree of [`BTREES`]: each key's position and encoded bytes.
    keys: Vec<Vec<(Bpos, Vec<u8>)>>,
    /// Each btree's levels: per level, each node's key range in the level
    /// below (or in `keys`, for leaves).
    shapes: Vec<Vec<Vec<core::ops::Range<usize>>>>,
    node_bytes: usize,
}

/// A first bset fills at most two thirds of its node, as upstream splits.
fn capacity(node_bytes: usize) -> usize {
    node_bytes * 2 / 3
}

/// An interior key: a `btree_ptr_v2` with one device pointer.
const POINTER_KEY: usize = 40 + NodePtr::BYTES;

impl Plan {
    fn new(g: &Geometry, nodes: u64, hash_seed: u64) -> Result<Plan> {
        let (last, _) = g.slot(nodes - 1);
        let end_copy = (g.layout.offsets[2] / g.bucket).min(g.nbuckets);
        if last >= end_copy {
            return Err(Error::Invalid("device too small for its metadata"));
        }
        let node_bytes = (g.node as usize) * SECTOR;
        let mut keys: Vec<Vec<(Bpos, Vec<u8>)>> = BTREES.iter().map(|_| Vec::new()).collect();
        let mut put = |btree: Btree, key: Key, value: &[u8]| {
            let i = BTREES.iter().position(|&b| b == btree).expect("a btree mkfs writes");
            keys[i].push((key.pos, key.encode(value)));
        };

        let root = root_inode(hash_seed);
        put(Btree::Inodes, Key::new(kind::INODE_V3, Bpos::new(0, ROOT_INODE, ROOT_SNAPSHOT)), &root);
        put(Btree::Subvolumes, Key::new(kind::SUBVOLUME, Bpos::new(0, u64::from(ROOT_SUBVOLUME), 0)), &value::subvolume(ROOT_SNAPSHOT, ROOT_INODE));
        put(Btree::Snapshots, Key::new(kind::SNAPSHOT, Bpos::new(0, u64::from(ROOT_SNAPSHOT), 0)), &value::snapshot(ROOT_SUBVOLUME, SNAPSHOT_TREE));
        put(Btree::SnapshotTrees, Key::new(kind::SNAPSHOT_TREE, Bpos::new(0, u64::from(SNAPSHOT_TREE), 0)), &value::snapshot_tree(ROOT_SUBVOLUME, ROOT_SNAPSHOT));
        put(Btree::LoggedOps, Key::new(kind::INODE_ALLOC_CURSOR, Bpos::new(1, 1, 0)), &value::inode_cursor(0, FIRST_INODE));

        // Alloc keys for used buckets only; the rest are free holes.
        let journal = g.journal.0..g.journal.0 + g.journal.1;
        let btree: Vec<(u64, u64)> = g.btree_buckets(nodes).collect();
        let mut used: Vec<(u64, Alloc)> = g.sb.iter().map(|&(b, s)| (b, Alloc { data_type: data_type::SB, dirty_sectors: s as u32 })).collect();
        used.extend(journal.map(|b| (b, Alloc { data_type: data_type::JOURNAL, dirty_sectors: g.bucket as u32 })));
        used.extend(btree.iter().map(|&(b, n)| (b, Alloc { data_type: data_type::BTREE, dirty_sectors: (n * g.node) as u32 })));
        used.sort_unstable_by_key(|(b, _)| *b);
        for (b, alloc) in &used {
            put(Btree::Alloc, Key::new(kind::ALLOC_V4, Bpos::new(0, *b, 0)), &alloc.encode());
        }
        let mut free = 0;
        let mut ranges = Vec::new();
        for &(b, _) in &used {
            if b > free {
                ranges.push((free, b));
            }
            free = b + 1;
        }
        if free < g.nbuckets {
            ranges.push((free, g.nbuckets));
        }
        for &(start, end) in &ranges {
            let key = Key { size: (end - start) as u32, ..Key::new(kind::SET, Bpos::new(0, end, 0)) };
            put(Btree::Freespace, key, &[]);
        }
        // A fragmentation entry for each partly filled btree bucket.
        for &(b, n) in &btree {
            let dirty = n * g.node;
            if dirty < g.bucket {
                let time = dirty * (1 << 31) / g.bucket;
                put(Btree::Lru, Key::new(kind::SET, Bpos::new(8192 << 48 | time, b, 0)), &[]);
            }
        }
        // Backpointers' positions; their values follow the layout.
        for i in 0..nodes {
            let (_, sector) = g.slot(i);
            put(Btree::Backpointers, Key::new(kind::BACKPOINTER, Bpos::new(0, sector << 16, 0)), &[0; 32]);
        }
        // Accounting's positions and sizes; its values follow the layout.
        for (record, counters) in accounting_classes() {
            put(Btree::Accounting, Key::new(kind::ACCOUNTING, value::accounting_pos(&record)), &alloc::vec![0u8; 8 * counters]);
        }
        for list in &mut keys {
            list.sort_unstable_by_key(|(pos, _)| *pos);
        }
        let shapes = keys.iter().map(|k| shape(k.iter().map(|(_, b)| b.len()), node_bytes)).collect::<Result<Vec<_>>>()?;
        Ok(Plan { keys, shapes, node_bytes })
    }

    fn nodes(&self) -> u64 {
        self.shapes.iter().flatten().map(|level| level.len() as u64).sum()
    }
}

/// The accounting classes a fresh volume carries, as class records
/// and counter counts, in a fixed order whose values [`Tree::accounting`]
/// fills.
fn accounting_classes() -> Vec<(Vec<u8>, usize)> {
    let mut out = alloc::vec![
        (alloc::vec![class::NR_INODES], 1),
        (alloc::vec![class::REPLICAS, data_type::BTREE, 1, 1, 0], 1),
    ];
    for t in [data_type::FREE, data_type::SB, data_type::JOURNAL, data_type::BTREE] {
        out.push((alloc::vec![class::DEV_DATA_TYPE, 0, t], 3));
    }
    for btree in BTREES {
        let mut r = alloc::vec![class::BTREE];
        r.extend_from_slice(&(btree as u32).to_le_bytes());
        out.push((r, 3));
    }
    let mut snapshot = alloc::vec![class::SNAPSHOT];
    snapshot.extend_from_slice(&ROOT_SNAPSHOT.to_le_bytes());
    snapshot.extend_from_slice(&(Btree::Inodes as u32).to_le_bytes());
    out.push((snapshot, 3));
    out
}

/// The root directory: mode 040755, owned by root, siphash names under
/// `hash_seed`, the root subvolume's root. Its times are the time base, so
/// zero.
fn root_inode(hash_seed: u64) -> Vec<u8> {
    let mut inode = InodeValue { hash_seed, flags: 0o40755 << 36 | 3 << 20, ..Default::default() };
    inode.set(inode_field::SUBVOL, u64::from(ROOT_SUBVOLUME));
    inode.encode()
}

/// A btree's nodes for keys of `sizes`: leaves filled in order to
/// [`capacity`], then interior levels of pointers until one root.
fn shape(sizes: impl Iterator<Item = usize>, node_bytes: usize) -> Result<Vec<Vec<core::ops::Range<usize>>>> {
    let cap = capacity(node_bytes) - node::record_bytes(core::iter::empty());
    let mut leaves = alloc::vec![0..0];
    let mut fill = 0;
    for (i, size) in sizes.enumerate() {
        let last = leaves.last_mut().unwrap();
        if fill + size > cap && last.start != last.end {
            leaves.push(i..i + 1);
            fill = size;
        } else {
            last.end = i + 1;
            fill += size;
        }
    }
    let fanout = cap / POINTER_KEY;
    let mut levels = alloc::vec![leaves];
    while levels.last().unwrap().len() > 1 {
        let below = levels.last().unwrap().len();
        levels.push((0..below).step_by(fanout).map(|i| i..(i + fanout).min(below)).collect());
        if levels.len() > 4 {
            return Err(Error::Invalid("a btree deeper than four levels"));
        }
    }
    Ok(levels)
}

/// One node as written.
struct Laid {
    btree: Btree,
    level: u8,
    seq: u64,
    sector: u64,
    min: Bpos,
    max: Bpos,
    keys: Vec<Vec<u8>>,
}

/// The nodes, `clean`'s root records, and the member's btree bitmap.
struct Tree {
    nodes: Vec<Laid>,
    roots: Vec<(Btree, u8, Vec<u8>)>,
    bitmap_shift: u8,
    bitmap: u64,
}

impl Plan {
    /// Assigns slots in btree order, leaves first, and fills the values
    /// that depend on them: backpointers, accounting, and every pointer.
    fn lay_out(mut self, g: &Geometry, seqs: &[u64]) -> Result<Tree> {
        // Each node's place and range, before its keys are final.
        struct Place {
            btree: usize,
            level: usize,
            range: core::ops::Range<usize>,
            slot: u64,
            min: Bpos,
            max: Bpos,
        }
        let mut places: Vec<Place> = Vec::new();
        let mut slot = 0;
        let mut first_of: Vec<Vec<usize>> = Vec::new();
        for (t, levels) in self.shapes.iter().enumerate() {
            let mut firsts = Vec::new();
            for (level, nodes) in levels.iter().enumerate() {
                firsts.push(places.len());
                let count = nodes.len();
                for (j, range) in nodes.iter().enumerate() {
                    let (min, max) = if level == 0 {
                        let keys = &self.keys[t];
                        let min = if j == 0 { Bpos::MIN } else { keys[range.start - 1].0.successor().expect("a key below MAX") };
                        let max = if j + 1 == count { Bpos::MAX } else { keys[range.end - 1].0 };
                        (min, max)
                    } else {
                        let below = firsts[level - 1];
                        (places[below + range.start].min, places[below + range.end - 1].max)
                    };
                    places.push(Place { btree: t, level, range: range.clone(), slot, min, max });
                    slot += 1;
                }
            }
            first_of.push(firsts);
        }

        // Backpointers and accounting, now that every node has its place.
        let bp = BTREES.iter().position(|&b| b == Btree::Backpointers).unwrap();
        for p in &places {
            let (_, sector) = g.slot(p.slot);
            let i = self.keys[bp].binary_search_by_key(&Bpos::new(0, sector << 16, 0), |(pos, _)| *pos).expect("a backpointer per node");
            let value = value::backpointer(BTREES[p.btree] as u8, p.level as u8, g.node as u32, p.max);
            self.keys[bp][i].1[40..72].copy_from_slice(&value);
        }
        self.fill_accounting(g, &places.iter().map(|p| (p.btree, p.level)).collect::<Vec<_>>());

        // Nodes from the leaves up, each level's pointers from the one
        // below.
        let mut written: Vec<Option<(NodePtr, Bpos)>> = (0..places.len()).map(|_| None).collect();
        let mut nodes = Vec::new();
        for (idx, p) in places.iter().enumerate() {
            let keys: Vec<Vec<u8>> = if p.level == 0 {
                self.keys[p.btree][p.range.clone()].iter().map(|(_, b)| b.clone()).collect()
            } else {
                let below = first_of[p.btree][p.level - 1];
                p.range
                    .clone()
                    .map(|c| {
                        let (ptr, max) = written[below + c].expect("children laid out first");
                        Key::new(kind::BTREE_PTR_V2, max).encode(&ptr.encode())
                    })
                    .collect()
            };
            let (_, sector) = g.slot(p.slot);
            let bytes = node::record_bytes(keys.iter().map(Vec::len)).next_multiple_of(BLOCK);
            assert!(bytes <= self.node_bytes, "a node past its size");
            let ptr = NodePtr {
                seq: seqs[p.slot as usize],
                written: (bytes / SECTOR) as u16,
                min: p.min,
                ptr: Ptr { sector, dev: 0, gen: 0, cached: false, unwritten: false },
            };
            written[idx] = Some((ptr, p.max));
            nodes.push(Laid { btree: BTREES[p.btree], level: p.level as u8, seq: ptr.seq, sector, min: p.min, max: p.max, keys });
        }
        let roots = (0..BTREES.len())
            .map(|t| {
                let top = self.shapes[t].len() - 1;
                let (ptr, max) = written[first_of[t][top]].unwrap();
                (BTREES[t], top as u8, Key::new(kind::BTREE_PTR_V2, max).encode(&ptr.encode()))
            })
            .collect();

        // The bitmap's shift covers the last node; each node's regions are set.
        let end = nodes.iter().map(|n| n.sector + g.node).max().unwrap_or(0);
        let bitmap_shift = (64 - (end.max(64) - 1).leading_zeros()).saturating_sub(6) as u8;
        let mut bitmap = 0u64;
        for n in &nodes {
            for i in n.sector >> bitmap_shift..(n.sector + g.node).div_ceil(1 << bitmap_shift) {
                bitmap |= 1 << i;
            }
        }
        Ok(Tree { nodes, roots, bitmap_shift, bitmap })
    }

    /// Each class's counters, in [`accounting_classes`]' order.
    fn fill_accounting(&mut self, g: &Geometry, nodes: &[(usize, usize)]) {
        let n = nodes.len() as u64;
        let node = g.node;
        let btree: Vec<(u64, u64)> = g.btree_buckets(n).collect();
        let sb_sectors: u64 = g.sb.iter().map(|&(_, s)| s).sum();
        let sb_frag: u64 = g.sb.iter().map(|&(_, s)| g.bucket - s).sum();
        let btree_frag: u64 = btree.iter().map(|&(_, k)| g.bucket - k * node).sum();
        let used = g.sb.len() as u64 + g.journal.1 + btree.len() as u64;
        let inode_bytes = {
            let inodes = BTREES.iter().position(|&b| b == Btree::Inodes).unwrap();
            self.keys[inodes][0].1.len() as u64
        };
        let mut values: Vec<Vec<u64>> = alloc::vec![
            alloc::vec![1],
            alloc::vec![n * node],
            alloc::vec![g.nbuckets - used, 0, 0],
            alloc::vec![g.sb.len() as u64, sb_sectors, sb_frag],
            alloc::vec![g.journal.1, g.journal.1 * g.bucket, 0],
            alloc::vec![btree.len() as u64, n * node, btree_frag],
        ];
        for t in 0..BTREES.len() {
            let count = nodes.iter().filter(|&&(b, _)| b == t).count() as u64;
            let interior = nodes.iter().filter(|&&(b, l)| b == t && l > 0).count() as u64;
            values.push(alloc::vec![count * node, count, interior]);
        }
        values.push(alloc::vec![1, inode_bytes, 0]);

        let acct = BTREES.iter().position(|&b| b == Btree::Accounting).unwrap();
        for ((record, _), v) in accounting_classes().iter().zip(values) {
            let pos = value::accounting_pos(record);
            let i = self.keys[acct].binary_search_by_key(&pos, |(p, _)| *p).expect("a key per class");
            self.keys[acct][i].1[40..].copy_from_slice(&value::counters(&v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_device_lays_out_as_format_does() {
        // Upstream's own 32 MiB layout: 65 536 sectors in 32-sector buckets, copies at 8,
        // 2 056 and 63 488; buckets 0–127 hold 32 superblock sectors, 128
        // holds 8, 1 984–2 047 hold 32.
        let g = Geometry::new(65536).unwrap();
        assert_eq!((g.bucket, g.nbuckets, g.node), (32, 2048, 32));
        assert_eq!(g.layout.offsets, [8, 2056, 63488]);
        assert_eq!(g.sb.len(), 129 + 64);
        assert_eq!(g.sb[128], (128, 8));
        assert!(g.sb.iter().filter(|&&(b, _)| b != 128).all(|&(_, s)| s == 32));
        assert_eq!(g.journal, (129, 16));
    }

    #[test]
    fn a_large_device_takes_two_mib_buckets_and_256_kib_nodes() {
        // 171 GiB, as `format` lays out a partition that size.
        let g = Geometry::new(171 << 21).unwrap();
        assert_eq!((g.bucket, g.node), (4096, 512));
        assert_eq!(g.journal.1, g.nbuckets / 128);
    }

    #[test]
    fn leaves_fill_to_two_thirds_and_one_root_spans_them() {
        let levels = shape(core::iter::repeat_n(104, 300), 16384).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 3);
        assert_eq!(levels[1], vec![0..3]);
        let empty = shape(core::iter::empty(), 16384).unwrap();
        assert_eq!(empty, vec![vec![0..0]], "an empty btree still has its root");
    }
}
