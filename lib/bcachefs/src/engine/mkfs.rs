//! A complete clean volume: superblock copies, an empty journal and
//! `clean`, the root subvolume, snapshot and snapshot tree, a [`Tree`]'s
//! inodes, dirents and data under the root directory, the inode cursor, and
//! the allocator and accounting state all of that derives. Nothing it
//! writes names a journal sequence, so `clean`'s is 0.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use toyos_blockdev::{BlockDev, BLOCK};
use toyos_crypto::Random;

use super::bkey::{Bpos, Key};
use super::crypt::MasterKey;
use super::node::{self, Build};
use super::superblock::{field_type, member_flag, Crypt, Field, Header, Kdf, Layout, Member, Superblock, LAYOUT, MAX_NODE_SECTORS, PRIMARY};
use super::derive::{Space, Tally};
use super::tree::{Populated, Tree};
use super::value::{self, data_type, Alloc, NodePtr, Ptr};
use super::{journal, kind, put16, put64, Btree, Error, Result, SECTOR, VERSION};

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
pub(crate) const BLOCK_SECTORS: u64 = (BLOCK / SECTOR) as u64;
/// The encoded-extent maximum, 2^9 sectors, as `format` sets it: no
/// checksummed extent is larger.
const ENCODED_EXTENT_BITS: u64 = 9;
const MAX_EXTENT: u64 = 1 << ENCODED_EXTENT_BITS;
/// A backpointer's low bits that tell pieces of one extent apart.
const BACKPOINTER_SHIFT: u32 = 16;
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

/// Writes the volume holding `tree` to `dev`, every identifier and key
/// drawn from `random`, then flushes. A device too small for it is refused
/// [`Error::Small`].
pub async fn mkfs(dev: &impl BlockDev, options: &Options<'_>, tree: &Tree<'_>, random: &mut impl Random) -> Result<()> {
    let volume = Encoded::lay_out(dev.blocks() * BLOCK_SECTORS, options, tree, random)?;
    for (sector, bytes) in volume.population.extents() {
        write(dev, sector, bytes).await?;
    }
    for (sector, bytes) in &volume.nodes {
        write(dev, *sector, bytes).await?;
    }
    for &offset in &volume.header.layout.offsets {
        write(dev, offset, &Superblock::encode(&volume.header, &volume.fields, offset)).await?;
    }
    // Sectors 0–6 are zeros here; the layout copy is sector 7.
    let mut first = alloc::vec![0u8; BLOCK];
    let at = LAYOUT as usize * SECTOR;
    first[at..at + Layout::BYTES].copy_from_slice(&volume.header.layout.encode());
    write(dev, 0, &first).await?;
    dev.flush().await?;
    Ok(())
}

/// A device size, in blocks, that [`mkfs`] of `tree` fits with `random`'s
/// draws. From the smallest device, each try takes the size the last
/// refusal named, so the size only grows and the volume that fits has no
/// free bucket, unless a try crossed to larger buckets, which can need less
/// than the refusal at smaller ones named. Each try draws from its own
/// clone of `random`, so the size that fits lays out exactly what [`mkfs`]
/// writes from another clone.
pub fn fit<R: Random + Clone>(options: &Options<'_>, tree: &Tree<'_>, random: &R) -> Result<u64> {
    let mut blocks = MIN_BUCKETS * MIN_BUCKET / BLOCK_SECTORS;
    loop {
        match Encoded::lay_out(blocks * BLOCK_SECTORS, options, tree, &mut random.clone()) {
            Ok(_) => return Ok(blocks),
            Err(Error::Small { blocks: needs }) => {
                assert!(needs > blocks, "a refusal at {blocks} blocks that asks for {needs}");
                blocks = needs;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A volume laid out and encoded: its extents, its nodes by first sector,
/// and the superblock every copy writes.
struct Encoded<'t, 'a> {
    population: Populated<'t, 'a>,
    nodes: Vec<(u64, Vec<u8>)>,
    header: Header,
    fields: Vec<Field>,
}

impl<'t, 'a> Encoded<'t, 'a> {
    fn lay_out(sectors: u64, options: &Options<'_>, tree: &'t Tree<'a>, random: &mut impl Random) -> Result<Self> {
        if options.label.len() > 31 {
            return Err(Error::Invalid("label longer than 31 bytes"));
        }
        // Upstream's tools cut a passphrase at NUL and strip one trailing
        // newline, so neither could unlock what such bytes wrapped.
        if options.encryption.as_ref().is_some_and(|e| e.passphrase.contains(&0) || e.passphrase.ends_with(b"\n")) {
            return Err(Error::Invalid("passphrase holding NUL or ending in a newline"));
        }
        // An encrypted extent needs a version never used before, and mkfs
        // allocates no extent versions.
        if options.encryption.is_some() && tree.has_data() {
            return Err(Error::Unsupported("file data or a symlink on an encrypted volume"));
        }
        let mut geometry = Geometry::new(sectors)?;
        let mut draw = Draw(random);
        let internal_uuid = draw.uuid()?;
        let user_uuid = draw.uuid()?;
        let member_uuid = draw.uuid()?;
        let hash_seed = draw.u64()?;
        let key = match &options.encryption {
            Some(_) => Some(MasterKey::new(draw.bytes()?)),
            None => None,
        };
        let seeds = (0..tree.len()).map(|_| draw.u64()).collect::<Result<Vec<u64>>>()?;

        let first = geometry.data_start() * geometry.bucket;
        let mut population = tree.populate(first, geometry.bucket, MAX_EXTENT, hash_seed, &seeds);
        geometry.data_buckets = (population.data_end - first).div_ceil(geometry.bucket);
        if geometry.btree_start() >= geometry.end_copy() {
            return Err(geometry.small(geometry.btree_start()));
        }
        let mut primary = root_world();
        primary.append(&mut population.keys);
        let plan = Plan::new(&geometry, &primary)?;
        let seqs = (0..plan.places.len()).map(|_| draw.nonzero()).collect::<Result<Vec<u64>>>()?;
        let laid = plan.nodes(&geometry, &seqs);

        let magic = node::magic(&internal_uuid);
        let mut nodes = Vec::new();
        for n in &laid.nodes {
            let build = Build { btree: n.btree, level: n.level, seq: n.seq, min: n.min, max: n.max, journal_seq: 0, keys: &n.keys };
            nodes.push((n.sector, node::encode(magic, BLOCK, key.as_ref(), &build)?));
        }

        let member = Member {
            uuid: member_uuid,
            nbuckets: geometry.nbuckets,
            first_bucket: 0,
            bucket_sectors: geometry.bucket as u16,
            bitmap_shift: laid.bitmap_shift,
            last_mount: options.time / 1_000_000_000,
            flags: member_flag::DATA_ALLOWED | member_flag::DURABILITY_1 | member_flag::FREESPACE_INITIALIZED | member_flag::ROTATIONAL_KNOWN,
            seq: 1,
            bitmap: laid.bitmap,
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
        let fields = fields(&geometry, &member, crypt.as_ref(), &laid.roots);
        Ok(Encoded { population, nodes, header, fields })
    }
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
        | ENCODED_EXTENT_BITS << 14 // encoded extent max, in sectors
        | 1 << 20 // metadata replicas required
        | 1 << 24; // data replicas required
    let word3 = 1000 << 30; // journal flush delay, ms
    let word4 = 100 | 1 << 32; // journal reclaim delay, ms; transaction names
    let incompat = 1 << 10 | 36; // 1.36: no user replicas in the superblock
    let word5 = u64::from(VERSION) | 30 << 16 | incompat << 32 | u64::from(VERSION) << 48;
    let word6 = 30 << 4 | 3 << 14 | u64::from(BACKPOINTER_SHIFT) << 40; // write error timeout, checksum retries, backpointer shift
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

    let clean = journal::clean_body(0, roots.iter().map(|(b, l, k)| (*b, *l, &k[..])), 0, [0; 2]);

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

pub(crate) async fn write(dev: &impl BlockDev, sector: u64, bytes: &[u8]) -> Result<()> {
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
    /// Buckets of file data, which follow the journal.
    data_buckets: u64,
}

impl Geometry {
    fn new(sectors: u64) -> Result<Geometry> {
        let wanted = (sectors / WANT_BUCKETS).max(1);
        let bucket = (1u64 << wanted.ilog2()).clamp(MIN_BUCKET, MAX_BUCKET);
        let nbuckets = sectors / bucket;
        if nbuckets < MIN_BUCKETS {
            return Err(Error::Small { blocks: MIN_BUCKETS * MIN_BUCKET / BLOCK_SECTORS });
        }
        let second = (PRIMARY + SB_SECTORS).next_multiple_of(BLOCK_SECTORS);
        let end = (sectors - SB_SECTORS) / bucket * bucket;
        if end < second + SB_SECTORS {
            return Err(Error::Invalid("device too small for three superblock copies"));
        }
        let layout = Layout { max_bits: SB_MAX_BITS, offsets: alloc::vec![PRIMARY, second, end] };
        let front = (second + SB_SECTORS).div_ceil(bucket);
        let journal = (front, (nbuckets / 128).max(8));
        Ok(Geometry { bucket, nbuckets, node: bucket.min(MAX_NODE_SECTORS), layout, journal, data_buckets: 0 })
    }

    /// The first bucket of file data, after the journal.
    fn data_start(&self) -> u64 {
        self.journal.0 + self.journal.1
    }

    /// The first bucket btree nodes go in, after the data.
    fn btree_start(&self) -> u64 {
        self.data_start() + self.data_buckets
    }

    /// The first bucket of the end superblock copy, which nothing else may
    /// reach.
    fn end_copy(&self) -> u64 {
        (self.layout.offsets[2] / self.bucket).min(self.nbuckets)
    }

    fn nodes_per_bucket(&self) -> u64 {
        self.bucket / self.node
    }

    /// The sector of node slot `i`.
    fn slot(&self, i: u64) -> u64 {
        let bucket = self.btree_start() + i / self.nodes_per_bucket();
        bucket * self.bucket + i % self.nodes_per_bucket() * self.node
    }

    /// The refusal of this device when the buckets before `bucket` are
    /// full: what it needs is those, `bucket`, and the end copy's buckets.
    fn small(&self, bucket: u64) -> Error {
        let sectors = (bucket + 1 + SB_SECTORS.div_ceil(self.bucket)) * self.bucket;
        Error::Small { blocks: sectors / BLOCK_SECTORS }
    }

    /// The device as [`derive`] sees it.
    fn space(&self) -> Space {
        let regions = core::iter::once((0, PRIMARY)).chain(self.layout.offsets.iter().map(|&o| (o, o + SB_SECTORS)));
        Space {
            bucket: self.bucket,
            nbuckets: self.nbuckets,
            first_bucket: 0,
            node: self.node,
            bp_shift: BACKPOINTER_SHIFT,
            sb: regions.collect(),
            journal: alloc::vec![self.journal],
        }
    }
}

/// A first bset fills at most two thirds of its node, as upstream splits.
pub(crate) const fn capacity(node_bytes: usize) -> usize {
    node_bytes * 2 / 3
}

/// An interior key: a `btree_ptr_v2` with one device pointer.
const POINTER_KEY: usize = 40 + NodePtr::BYTES;

/// The root subvolume, its snapshot and the snapshot tree.
fn root_world() -> Vec<(Btree, Key, Vec<u8>)> {
    alloc::vec![
        (Btree::Subvolumes, Key::new(kind::SUBVOLUME, Bpos::new(0, u64::from(ROOT_SUBVOLUME), 0)), value::subvolume(ROOT_SNAPSHOT, ROOT_INODE).to_vec()),
        (Btree::Snapshots, Key::new(kind::SNAPSHOT, Bpos::new(0, u64::from(ROOT_SNAPSHOT), 0)), value::snapshot(ROOT_SUBVOLUME, SNAPSHOT_TREE).to_vec()),
        (Btree::SnapshotTrees, Key::new(kind::SNAPSHOT_TREE, Bpos::new(0, u64::from(SNAPSHOT_TREE), 0)), value::snapshot_tree(ROOT_SUBVOLUME, ROOT_SNAPSHOT).to_vec()),
    ]
}

/// A node's place before its keys are encoded: its btree (an index into
/// [`BTREES`]), level, the range of keys or of children below it, its slot
/// and its key range.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    btree: usize,
    level: usize,
    range: core::ops::Range<usize>,
    slot: u64,
    min: Bpos,
    max: Bpos,
}

/// Every btree's keys, and where each node goes.
struct Plan {
    /// Per btree of [`BTREES`]: each key's position and encoded bytes.
    keys: Vec<Vec<(Bpos, Vec<u8>)>>,
    /// Slots in btree order, each btree's leaves first, then each level
    /// above.
    places: Vec<Place>,
    /// Per btree, the place of each level's first node.
    first_of: Vec<Vec<usize>>,
}

fn index(btree: Btree) -> usize {
    BTREES.iter().position(|&b| b == btree).expect("a btree mkfs writes")
}

impl Plan {
    /// The layout whose derived keys fill exactly its own nodes. Each
    /// layout's derived keys are counted from its node count alone, which
    /// only grows from one root per btree, so the first layout to repeat is
    /// the fixed point.
    fn new(g: &Geometry, primary: &[(Btree, Key, Vec<u8>)]) -> Result<Plan> {
        let space = g.space();
        let mut base = Tally::new(&space);
        let mut keys: Vec<Vec<(Bpos, Vec<u8>)>> = BTREES.iter().map(|_| Vec::new()).collect();
        for (btree, key, value) in primary {
            base.leaf(*btree, key, value)?;
            keys[index(*btree)].push((key.pos, key.encode(value)));
        }
        let mut places: Vec<Place> =
            (0..BTREES.len()).map(|t| Place { btree: t, level: 0, range: 0..0, slot: t as u64, min: Bpos::MIN, max: Bpos::MAX }).collect();
        loop {
            let last = g.slot(places.len() as u64 - 1) / g.bucket;
            if last >= g.end_copy() {
                return Err(g.small(last));
            }
            let all = derived(g, &base, &places, &keys);
            let (next, first_of) = place(g, &all)?;
            if next == places {
                return Ok(Plan { keys: all, places, first_of });
            }
            assert!(next.len() >= places.len(), "the layout's node count shrank as its nodes grew");
            places = next;
        }
    }

    /// The nodes, `clean`'s root records and the btree bitmap, each node
    /// given its sequence from `seqs`.
    fn nodes(self, g: &Geometry, seqs: &[u64]) -> Nodes {
        let mut written: Vec<Option<(NodePtr, Bpos)>> = alloc::vec![None; self.places.len()];
        let mut nodes = Vec::new();
        for (idx, p) in self.places.iter().enumerate() {
            let keys: Vec<Vec<u8>> = if p.level == 0 {
                self.keys[p.btree][p.range.clone()].iter().map(|(_, b)| b.clone()).collect()
            } else {
                let below = self.first_of[p.btree][p.level - 1];
                p.range
                    .clone()
                    .map(|c| {
                        let (ptr, max) = written[below + c].expect("children laid out first");
                        Key::new(kind::BTREE_PTR_V2, max).encode(&ptr.encode())
                    })
                    .collect()
            };
            let sector = g.slot(p.slot);
            let bytes = node::record_bytes(keys.iter().map(Vec::len)).next_multiple_of(BLOCK);
            assert!(bytes as u64 <= g.node * SECTOR as u64, "a node past its size");
            let ptr = NodePtr { seq: seqs[idx], written: (bytes / SECTOR) as u16, min: p.min, ptr: Ptr { sector, dev: 0, gen: 0, cached: false, unwritten: false } };
            written[idx] = Some((ptr, p.max));
            nodes.push(Laid { btree: BTREES[p.btree], level: p.level as u8, seq: ptr.seq, sector, min: p.min, max: p.max, keys });
        }
        let roots = (0..BTREES.len())
            .map(|t| {
                let top = self.first_of[t].len() - 1;
                let (ptr, max) = written[self.first_of[t][top]].expect("every btree has a root");
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
        Nodes { nodes, roots, bitmap_shift, bitmap }
    }
}

/// Every btree's keys under `places`: `primary`'s, and the allocator and
/// accounting keys the tally of them and of `places`' nodes derives.
fn derived(g: &Geometry, base: &Tally<'_>, places: &[Place], primary: &[Vec<(Bpos, Vec<u8>)>]) -> Vec<Vec<(Bpos, Vec<u8>)>> {
    let mut tally = base.clone();
    for p in places {
        let ptr = Ptr { sector: g.slot(p.slot), dev: 0, gen: 0, cached: false, unwritten: false };
        tally.node(BTREES[p.btree], p.level as u8, &ptr, p.max);
    }
    assert!(tally.findings.is_empty(), "mkfs laid out an inconsistent volume: {:?}", tally.findings);
    let d = tally.finish(&BTreeMap::new());
    assert!(d.need_discard.is_empty() && d.bucket_gens.is_empty(), "a fresh volume's buckets were used before");
    let mut keys = primary.to_vec();
    let mut put = |btree: Btree, key: Key, value: &[u8]| keys[index(btree)].push((key.pos, key.encode(value)));
    for (&b, u) in &d.buckets {
        let alloc = Alloc { data_type: u.data_type, dirty_sectors: u.dirty as u32 };
        put(Btree::Alloc, Key::new(kind::ALLOC_V4, Bpos::new(0, b, 0)), &alloc.encode());
    }
    for &(pos, size) in &d.freespace {
        put(Btree::Freespace, Key { size, ..Key::new(kind::SET, pos) }, &[]);
    }
    for &pos in &d.lru {
        put(Btree::Lru, Key::new(kind::SET, pos), &[]);
    }
    for (&pos, value) in &d.backpointers {
        put(Btree::Backpointers, Key::new(kind::BACKPOINTER, pos), value);
    }
    for (&pos, c) in &d.accounting {
        put(Btree::Accounting, Key::new(kind::ACCOUNTING, pos), &value::counters(c));
    }
    for list in &mut keys {
        list.sort_unstable_by_key(|(pos, _)| *pos);
    }
    keys
}

/// Each btree's nodes for `keys`, assigned slots in btree order, leaves
/// first, with each node's key range.
fn place(g: &Geometry, keys: &[Vec<(Bpos, Vec<u8>)>]) -> Result<(Vec<Place>, Vec<Vec<usize>>)> {
    let node_bytes = (g.node as usize) * SECTOR;
    let mut places: Vec<Place> = Vec::new();
    let mut first_of = Vec::new();
    for (t, keys) in keys.iter().enumerate() {
        let levels = shape(keys.iter().map(|(_, b)| b.len()), node_bytes)?;
        let mut firsts = Vec::new();
        for (level, nodes) in levels.iter().enumerate() {
            firsts.push(places.len());
            let count = nodes.len();
            for (j, range) in nodes.iter().enumerate() {
                let (min, max) = if level == 0 {
                    let min = if j == 0 { Bpos::MIN } else { keys[range.start - 1].0.successor().expect("a key below MAX") };
                    let max = if j + 1 == count { Bpos::MAX } else { keys[range.end - 1].0 };
                    (min, max)
                } else {
                    let below = firsts[level - 1];
                    (places[below + range.start].min, places[below + range.end - 1].max)
                };
                let slot = places.len() as u64;
                places.push(Place { btree: t, level, range: range.clone(), slot, min, max });
            }
        }
        first_of.push(firsts);
    }
    Ok((places, first_of))
}

/// A btree's nodes for keys of `sizes`: leaves, then interior levels of
/// pointers until one root, each level's entries spread evenly over the
/// fewest nodes that hold them at [`capacity`].
///
/// Evenly, so no node holds a remainder: each node of a level of several
/// holds at least about half of `cap`, less one entry. Upstream merges a
/// small node into its sibling even under `fsck -n`, and the merge
/// allocates a node, which on a volume with no free bucket waits forever.
/// Upstream's merge threshold is unspecified; this margin is the measured
/// one, not a guarantee.
fn shape(sizes: impl Iterator<Item = usize>, node_bytes: usize) -> Result<Vec<Vec<core::ops::Range<usize>>>> {
    let cap = capacity(node_bytes) - node::record_bytes(core::iter::empty());
    let sizes: Vec<usize> = sizes.collect();
    let mut levels = alloc::vec![spread(&sizes, cap)];
    while levels.last().unwrap().len() > 1 {
        let below = levels.last().unwrap().len();
        levels.push(spread(&alloc::vec![POINTER_KEY; below], cap));
        if levels.len() > 4 {
            return Err(Error::Invalid("a btree deeper than four levels"));
        }
    }
    Ok(levels)
}

/// Entries of `sizes`, each at most `cap` bytes, in consecutive nodes of
/// at most `cap` bytes, as evenly filled as the entries allow; one empty
/// node for none. The count starts at the fewest that could hold them and
/// grows until the even split fits. Each boundary leaves every later node
/// at least one entry, so no node is empty, and one node per entry always
/// fits, so the count stops there at the latest.
pub(crate) fn spread(sizes: &[usize], cap: usize) -> Vec<core::ops::Range<usize>> {
    let total: usize = sizes.iter().sum();
    let mut prefix = alloc::vec![0];
    prefix.extend(sizes.iter().scan(0, |sum, &size| {
        *sum += size;
        Some(*sum)
    }));
    let mut nodes = total.div_ceil(cap).max(1);
    loop {
        // Node k ends at the entry boundary nearest k / nodes of the total.
        let mut ranges = Vec::with_capacity(nodes);
        let mut start = 0;
        for k in 1..=nodes {
            let end = if k == nodes {
                sizes.len()
            } else {
                // In units of 1 / nodes: the boundaries either side of the
                // share, and the nearer.
                let want = total * k;
                let after = prefix.partition_point(|&p| p * nodes < want);
                let short = after.checked_sub(1).map(|b| (b, want - prefix[b] * nodes));
                let long = prefix.get(after).map(|&p| (after, p * nodes - want));
                let nearest = match (short, long) {
                    (Some((b, below)), Some((_, above))) if below < above => b,
                    (_, Some((a, _))) | (Some((a, _)), None) => a,
                    (None, None) => 0,
                };
                nearest.max(start + 1).min(sizes.len() - (nodes - k))
            };
            ranges.push(start..end);
            start = end;
        }
        if ranges.iter().all(|r| prefix[r.end] - prefix[r.start] <= cap || r.len() == 1) {
            assert!(sizes.is_empty() || ranges.iter().all(|r| !r.is_empty()), "an empty node among {} entries", sizes.len());
            return ranges;
        }
        nodes += 1;
    }
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
struct Nodes {
    nodes: Vec<Laid>,
    roots: Vec<(Btree, u8, Vec<u8>)>,
    bitmap_shift: u8,
    bitmap: u64,
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
        let space = g.space();
        let derived = Tally::new(&space).finish(&BTreeMap::new());
        let sb: Vec<(u64, u64)> = derived.buckets.iter().filter(|(_, u)| u.data_type == data_type::SB).map(|(&b, u)| (b, u.dirty)).collect();
        assert_eq!(sb.len(), 129 + 64);
        assert_eq!(sb[128], (128, 8));
        assert!(sb.iter().filter(|&&(b, _)| b != 128).all(|&(_, s)| s == 32));
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
        assert_eq!(levels[0], vec![0..100, 100..200, 200..300]);
        assert_eq!(levels[1], vec![0..3]);
        let empty = shape(core::iter::empty(), 16384).unwrap();
        assert_eq!(empty, vec![vec![0..0]], "an empty btree still has its root");
    }

    /// Three nodes hold these; without leaving each later node an entry,
    /// the nearest boundary put both 100s in one node and four nodes left
    /// one empty.
    #[test]
    fn every_later_node_keeps_an_entry() {
        assert_eq!(spread(&[1, 1, 100, 100], 100), vec![0..2, 2..3, 3..4]);
    }

    /// A remainder is spread over its level rather than left in a last
    /// node: the fills of a level's nodes differ by at most one entry, and
    /// the level takes no more nodes than its entries need.
    #[test]
    fn no_node_is_left_with_a_remainder() {
        let cap = capacity(16384) - node::record_bytes(core::iter::empty());
        for n in [105, 106, 150, 201, 299, 301, 1000, 4321] {
            let sizes: Vec<usize> = (0..n).map(|i| 40 + i % 97).collect();
            let largest = *sizes.iter().max().unwrap();
            let leaves = &shape(sizes.iter().copied(), 16384).unwrap()[0];
            let fill: Vec<usize> = leaves.iter().map(|r| sizes[r.clone()].iter().sum()).collect();
            assert!(fill.iter().all(|&f| f <= cap), "{n}: a leaf past capacity");
            assert!(fill.iter().max().unwrap() - fill.iter().min().unwrap() <= largest, "{n}: leaves of {fill:?}");
            assert_eq!(leaves.len(), sizes.iter().sum::<usize>().div_ceil(cap), "{n}: more leaves than needed");
        }
    }
}
