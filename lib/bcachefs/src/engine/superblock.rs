//! The superblock: its layout copy, fixed header and variable fields,
//! validated once into typed values.

use alloc::vec::Vec;

use super::bkey::{Bpos, Format, Key};
use super::value::NodePtr;
use super::{field, kind, le16, le32, le64, put16, put32, put64, Btree, Error, Result, VERSION};
use crate::crc32c::crc32c;

/// The bcachefs magic, a UUID in RFC 4122 byte order.
pub(crate) const MAGIC: [u8; 16] = [0xc6, 0x85, 0x73, 0xf6, 0x66, 0xce, 0x90, 0xa9, 0xd9, 0x6a, 0x60, 0xcf, 0x80, 0x3d, 0xf7, 0xef];
/// Where the primary copy and the layout copy start, in sectors.
pub(crate) const PRIMARY: u64 = 8;
pub(crate) const LAYOUT: u64 = 7;
/// The fixed header's bytes.
pub(crate) const HEADER: usize = 752;
/// The most a copy this reader reads may hold. Upstream's default reserve is
/// 1 MiB and its fields a few KiB; this bounds what an image can make it
/// allocate.
pub(crate) const MAX_BYTES: usize = 64 << 10;

/// The largest btree node read, 256 KiB: `format`'s largest, and
/// what bounds a node read's allocation.
pub(crate) const MAX_NODE_SECTORS: u64 = 512;

/// Feature bits this reader knows: 0 to 23 of word 0.
const KNOWN_FEATURES: u64 = (1 << 24) - 1;

/// Variable-field types.
pub(crate) mod field_type {
    pub const CRYPT: u32 = 2;
    pub const REPLICAS_V0: u32 = 3;
    pub const CLEAN: u32 = 6;
    pub const REPLICAS: u32 = 7;
    pub const BLACKLIST: u32 = 8;
    pub const JOURNAL_V2: u32 = 9;
    pub const COUNTERS: u32 = 10;
    pub const MEMBERS_V2: u32 = 11;
    pub const EXT: u32 = 13;
    pub const DOWNGRADE: u32 = 14;
    pub const RECOVERY_PASSES: u32 = 15;
    pub const EXTENT_TYPE_U64S: u32 = 16;
    pub const ERRORS_V2: u32 = 17;
}

/// `bch_sb_layout`: where every copy is and what each may occupy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub max_bits: u8,
    pub offsets: Vec<u64>,
}

impl Layout {
    pub(crate) const BYTES: usize = 512;

    pub(crate) fn decode(b: &[u8]) -> Result<Layout> {
        let (max_bits, count) = (b[17], usize::from(b[18]));
        if b[..16] != MAGIC || b[16] != 0 || max_bits > 16 || !(1..=61).contains(&count) {
            return Err(Error::Corrupt("superblock layout"));
        }
        let offsets: Vec<u64> = (0..count).map(|i| le64(b, 24 + 8 * i)).collect();
        if offsets.windows(2).any(|w| w[0].checked_add(1 << max_bits).is_none_or(|end| w[1] < end)) {
            return Err(Error::Corrupt("superblock copies overlap"));
        }
        Ok(Layout { max_bits, offsets })
    }

    pub(crate) fn encode(&self) -> [u8; Self::BYTES] {
        let mut b = [0u8; Self::BYTES];
        b[..16].copy_from_slice(&MAGIC);
        b[17] = self.max_bits;
        b[18] = self.offsets.len() as u8;
        for (i, offset) in self.offsets.iter().enumerate() {
            put64(&mut b, 24 + 8 * i, *offset);
        }
        b
    }
}

/// The fixed header's fields a reader or writer sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub version: u16,
    pub version_min: u16,
    pub internal_uuid: [u8; 16],
    pub user_uuid: [u8; 16],
    pub label: [u8; 32],
    /// The sector this copy is at.
    pub offset: u64,
    pub seq: u64,
    pub block_sectors: u16,
    pub dev_idx: u8,
    pub nr_devices: u8,
    pub time_base: u64,
    pub time_precision: u32,
    pub flags: [u64; 7],
    pub write_time: u64,
    pub features: [u64; 2],
    pub compat: [u64; 2],
    pub layout: Layout,
}

/// One variable field, as stored: its type and its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Field {
    pub kind: u32,
    pub body: Vec<u8>,
}

/// The member entry of the one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub uuid: [u8; 16],
    pub nbuckets: u64,
    pub first_bucket: u16,
    pub bucket_sectors: u16,
    pub bitmap_shift: u8,
    pub last_mount: u64,
    pub flags: u64,
    pub seq: u64,
    pub bitmap: u64,
    pub journal_bucket: u32,
    pub journal_offset: u32,
    /// Device name, model and serial, which ToyOS writes as zeros.
    pub identity_zero: bool,
}

impl Member {
    pub(crate) const BYTES: usize = 328;

    pub(crate) fn decode(e: &[u8]) -> Member {
        // A shorter entry reads as zero-extended.
        let mut b = [0u8; Self::BYTES];
        let n = e.len().min(Self::BYTES);
        b[..n].copy_from_slice(&e[..n]);
        Member {
            uuid: b[..16].try_into().unwrap(),
            nbuckets: le64(&b, 16),
            first_bucket: le16(&b, 24),
            bucket_sectors: le16(&b, 26),
            bitmap_shift: b[28],
            last_mount: le64(&b, 32),
            flags: le64(&b, 40),
            seq: le64(&b, 120),
            bitmap: le64(&b, 128),
            journal_bucket: le32(&b, 136),
            journal_offset: le32(&b, 140),
            identity_zero: b[144..224].iter().chain(&b[232..296]).all(|&x| x == 0),
        }
    }

    /// Writes what a mount changes into a stored entry `e`, of at least
    /// [`Member::BYTES`]: the rest, such as upstream's error counts, stays.
    pub(crate) fn patch(&self, e: &mut [u8]) {
        e[28] = self.bitmap_shift;
        put64(e, 120, self.seq);
        put64(e, 128, self.bitmap);
        put32(e, 136, self.journal_bucket);
        put32(e, 140, self.journal_offset);
    }

    pub(crate) fn encode(&self) -> [u8; Self::BYTES] {
        let mut b = [0u8; Self::BYTES];
        b[..16].copy_from_slice(&self.uuid);
        put64(&mut b, 16, self.nbuckets);
        put16(&mut b, 24, self.first_bucket);
        put16(&mut b, 26, self.bucket_sectors);
        b[28] = self.bitmap_shift;
        put64(&mut b, 32, self.last_mount);
        put64(&mut b, 40, self.flags);
        put64(&mut b, 120, self.seq);
        put64(&mut b, 128, self.bitmap);
        put32(&mut b, 136, self.journal_bucket);
        put32(&mut b, 140, self.journal_offset);
        b
    }

    /// Whether the btree bitmap covers sectors `[start, end)`.
    pub(crate) fn bitmap_covers(&self, start: u64, end: u64) -> bool {
        let shift = u32::from(self.bitmap_shift);
        if shift >= 58 || end > 64 << shift {
            return false;
        }
        (start >> shift..end.div_ceil(1 << shift)).all(|i| self.bitmap >> i & 1 != 0)
    }
}

/// Member flags.
pub(crate) mod member_flag {
    pub const DISCARD: u64 = 1 << 14;
    pub const DATA_ALLOWED: u64 = 0x1c << 15;
    /// Durability 1, stored plus one.
    pub const DURABILITY_1: u64 = 2 << 28;
    pub const FREESPACE_INITIALIZED: u64 = 1 << 30;
    pub const ROTATIONAL: u64 = 1 << 32;
    pub const ROTATIONAL_KNOWN: u64 = 1 << 33;
    pub const INIT_STATE: u64 = 0xf << 34;
}

/// A btree root from `clean`: where its root node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Root {
    pub btree: Btree,
    pub level: u8,
    /// The root's key: its position is the root node's max key.
    pub key: Key,
    pub ptr: NodePtr,
}

impl Root {
    /// A btree_root record's payload: one btree_ptr_v2 key at `Bpos::MAX`,
    /// for the root node of `btree` at `level`.
    pub(crate) fn decode(btree: u8, level: u8, payload: &[u8]) -> Result<Root> {
        let btree = Btree::from_id(u32::from(btree)).ok_or(Error::Unsupported("root of a btree this reader does not know"))?;
        let (key, value) = Format::UNPACKED.key(payload)?;
        if key.kind != kind::BTREE_PTR_V2 || key.size != 0 || usize::from(payload[0]) * 8 != payload.len() {
            return Err(Error::Corrupt("btree root record"));
        }
        if key.pos != Bpos::MAX || level > 3 {
            return Err(Error::Corrupt("btree root at a level or position no root has"));
        }
        Ok(Root { btree, level, key, ptr: NodePtr::decode(value)? })
    }
}

/// `clean`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Clean {
    pub journal_seq: u64,
    pub roots: Vec<Root>,
    /// The key-version counter.
    pub key_version: Option<u64>,
    /// The read and write IO clocks.
    pub clocks: [u64; 2],
}

/// scrypt's cost as `crypt` stores it, each a log2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub log_n: u16,
    pub log_r: u16,
    pub log_p: u16,
}

impl Kdf {
    /// N, r and p, when r and p fit scrypt's `u32`s.
    pub(crate) fn values(self) -> Result<(u64, u32, u32)> {
        let pow = |log: u16| if log < 32 { Ok(1u32 << log) } else { Err(Error::Unsupported("scrypt r or p past 2^31")) };
        if self.log_n == 0 || self.log_n >= 64 {
            return Err(Error::Corrupt("scrypt N not a power of two from 2 to 2^63"));
        }
        Ok((1 << self.log_n, pow(self.log_r)?, pow(self.log_p)?))
    }
}

/// `crypt`: the key-derivation cost and the wrapped key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Crypt {
    pub kdf: Kdf,
    /// The key magic and master key, wrapped.
    pub wrapped: [u8; 40],
}

impl Crypt {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut b = alloc::vec![0u8; 56];
        put64(&mut b, 8, u64::from(self.kdf.log_n) | u64::from(self.kdf.log_r) << 16 | u64::from(self.kdf.log_p) << 32);
        b[16..].copy_from_slice(&self.wrapped);
        b
    }
}

/// A replicas entry: a data type and the devices it lives on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replicas {
    pub data_type: u8,
    pub required: u8,
    pub devs: Vec<u8>,
}

/// A validated superblock copy.
#[derive(Clone, Debug)]
pub struct Superblock {
    pub(crate) header: Header,
    pub(crate) fields: Vec<Field>,
    pub(crate) member: Member,
    pub(crate) clean: Option<Clean>,
    pub(crate) crypt: Option<Crypt>,
    pub(crate) blacklist: Vec<(u64, u64)>,
    pub(crate) replicas: Vec<Replicas>,
    /// The journal's buckets: each range's first bucket and count, in the
    /// listed order, whose concatenation is the journal's ring.
    pub(crate) journal: Vec<(u64, u64)>,
}

/// The `[lo, lo + len)` bits of flag word `word`.
pub(crate) fn flag(flags: &[u64; 7], word: usize, lo: u32, len: u32) -> u64 {
    field(flags[word], lo, len)
}

impl Superblock {
    /// The copy in `b`, read at `sector`, validated.
    pub(crate) fn decode(b: &[u8], sector: u64) -> Result<Superblock> {
        let header = decode_header(b)?;
        if header.offset != sector {
            return Err(Error::Corrupt("superblock copy read away from its own offset"));
        }
        let end = HEADER + 8 * le32(b, 124) as usize;
        let mut fields = Vec::new();
        let mut at = HEADER;
        while at < end {
            let words = le32(b, at) as usize;
            let len = words.checked_mul(8).filter(|&len| len >= 8 && len <= end - at).ok_or(Error::Corrupt("superblock field size"))?;
            fields.push(Field { kind: le32(b, at + 4), body: b[at + 8..at + len].to_vec() });
            at += len;
        }
        let find = |kind: u32| fields.iter().find(|f| f.kind == kind).map(|f| f.body.as_slice());
        let member = members(find(field_type::MEMBERS_V2).ok_or(Error::Unsupported("superblock without members_v2"))?, &header)?;
        let journal = find(field_type::JOURNAL_V2).map(|j| journal(j, &member)).transpose()?.unwrap_or_default();
        let clean = find(field_type::CLEAN).map(clean).transpose()?;
        let crypt = find(field_type::CRYPT).map(crypt).transpose()?;
        let blacklist = find(field_type::BLACKLIST).map(pairs).unwrap_or_default();
        // Half-open ranges, sorted and apart (A1.38).
        if blacklist.iter().any(|&(start, end)| start >= end) || blacklist.windows(2).any(|w| w[0].1 >= w[1].0) {
            return Err(Error::Corrupt("journal sequence blacklist"));
        }
        let replicas = match (find(field_type::REPLICAS_V0), find(field_type::REPLICAS)) {
            (Some(v0), _) => replicas(v0, false)?,
            (None, Some(v1)) => replicas(v1, true)?,
            (None, None) => Vec::new(),
        };
        Ok(Superblock { header, fields, member, clean, crypt, blacklist, replicas, journal })
    }

    /// This copy's bytes with `offset` as its own: the copies differ only
    /// there and in the checksum.
    pub(crate) fn encode(header: &Header, fields: &[Field], offset: u64) -> Vec<u8> {
        let words: usize = fields.iter().map(|f| 1 + f.body.len().div_ceil(8)).sum();
        let mut b = alloc::vec![0u8; HEADER + 8 * words];
        put16(&mut b, 16, header.version);
        put16(&mut b, 18, header.version_min);
        b[24..40].copy_from_slice(&MAGIC);
        b[40..56].copy_from_slice(&header.internal_uuid);
        b[56..72].copy_from_slice(&header.user_uuid);
        b[72..104].copy_from_slice(&header.label);
        put64(&mut b, 104, offset);
        put64(&mut b, 112, header.seq);
        put16(&mut b, 120, header.block_sectors);
        b[122] = header.dev_idx;
        b[123] = header.nr_devices;
        put32(&mut b, 124, words as u32);
        put64(&mut b, 128, header.time_base);
        put32(&mut b, 140, header.time_precision);
        for (i, w) in header.flags.iter().enumerate() {
            put64(&mut b, 144 + 8 * i, *w);
        }
        put64(&mut b, 200, header.write_time);
        put64(&mut b, 208, header.features[0]);
        put64(&mut b, 216, header.features[1]);
        put64(&mut b, 224, header.compat[0]);
        put64(&mut b, 232, header.compat[1]);
        b[240..HEADER].copy_from_slice(&header.layout.encode());
        let mut at = HEADER;
        for f in fields {
            let words = 1 + f.body.len().div_ceil(8);
            put32(&mut b, at, words as u32);
            put32(&mut b, at + 4, f.kind);
            b[at + 8..at + 8 + f.body.len()].copy_from_slice(&f.body);
            at += 8 * words;
        }
        let csum = crc32c(&b[16..]);
        put32(&mut b, 0, csum);
        b
    }

    pub(crate) fn flag(&self, word: usize, lo: u32, len: u32) -> u64 {
        flag(&self.header.flags, word, lo, len)
    }

    pub(crate) fn clean_flag(&self) -> bool {
        self.flag(0, 1, 1) != 0
    }

    pub(crate) fn block_bytes(&self) -> usize {
        usize::from(self.header.block_sectors) * super::SECTOR
    }

    /// Btree node size in sectors.
    pub(crate) fn node_sectors(&self) -> u32 {
        self.flag(0, 12, 16) as u32
    }

    /// Encryption type 1, ChaCha20/Poly1305.
    pub(crate) fn encrypted(&self) -> bool {
        self.flag(1, 10, 4) != 0
    }

    /// The variable field of `kind`, as stored.
    pub(crate) fn field(&self, kind: u32) -> Option<&[u8]> {
        self.fields.iter().find(|f| f.kind == kind).map(|f| f.body.as_slice())
    }

    pub(crate) fn blacklisted(&self, seq: u64) -> bool {
        self.blacklist.iter().any(|&(start, end)| (start..end).contains(&seq))
    }

    /// Whether every sequence of `[start, end)` is blacklisted. Ranges are
    /// disjoint and never adjacent, so one must hold it all.
    pub(crate) fn blacklisted_span(&self, start: u64, end: u64) -> bool {
        start >= end || self.blacklist.iter().any(|&(s, e)| s <= start && end <= e)
    }

    /// The internal UUID, which seeds the node magic and the key wrap.
    pub fn internal_uuid(&self) -> [u8; 16] {
        self.header.internal_uuid
    }

    /// The user-visible UUID.
    pub fn user_uuid(&self) -> [u8; 16] {
        self.header.user_uuid
    }
}

fn decode_header(b: &[u8]) -> Result<Header> {
    if b.len() < HEADER || b[24..40] != MAGIC {
        return Err(Error::Corrupt("no bcachefs superblock"));
    }
    let layout = Layout::decode(&b[240..HEADER])?;
    let total = HEADER + 8 * le32(b, 124) as usize;
    if total > b.len() || total as u64 > 512 << layout.max_bits {
        return Err(Error::Corrupt("superblock larger than its copy"));
    }
    let flags: [u64; 7] = core::array::from_fn(|i| le64(b, 144 + 8 * i));
    match field(flags[0], 2, 6) {
        0 => {}
        1 if crc32c(&b[16..total]) == le32(b, 0) => {}
        1 => return Err(Error::Checksum("superblock")),
        _ => return Err(Error::Unsupported("superblock checksum other than crc32c")),
    }
    let (version, version_min) = (le16(b, 16), le16(b, 18));
    let major = |v: u16| v >> 10;
    if major(version) != 1 || major(version_min) != 1 || version > VERSION || version_min > version {
        return Err(Error::Unsupported("superblock version other than 1.x up to 1.39"));
    }
    if field(flags[5], 32, 16) > u64::from(VERSION) {
        return Err(Error::Unsupported("superblock's incompatible version above 1.39"));
    }
    let features = [le64(b, 208), le64(b, 216)];
    if features[0] & !KNOWN_FEATURES != 0 || features[1] != 0 {
        return Err(Error::Unsupported("feature bit this reader does not know"));
    }
    if field(flags[0], 62, 1) != 0 {
        return Err(Error::Unsupported("big-endian metadata"));
    }
    if field(flags[1], 10, 4) > 1 {
        return Err(Error::Unsupported("encryption other than ChaCha20/Poly1305"));
    }
    let header = Header {
        version,
        version_min,
        internal_uuid: b[40..56].try_into().unwrap(),
        user_uuid: b[56..72].try_into().unwrap(),
        label: b[72..104].try_into().unwrap(),
        offset: le64(b, 104),
        seq: le64(b, 112),
        block_sectors: le16(b, 120),
        dev_idx: b[122],
        nr_devices: b[123],
        time_base: le64(b, 128),
        time_precision: le32(b, 140),
        flags,
        write_time: le64(b, 200),
        features,
        compat: [le64(b, 224), le64(b, 232)],
        layout,
    };
    if header.internal_uuid == [0; 16] || header.user_uuid == [0; 16] {
        return Err(Error::Corrupt("superblock with a zero UUID"));
    }
    if !header.block_sectors.is_power_of_two() || header.block_sectors > 128 {
        return Err(Error::Corrupt("block size"));
    }
    if !(1..=1_000_000_000).contains(&header.time_precision) {
        return Err(Error::Corrupt("time precision"));
    }
    if header.nr_devices != 1 || header.dev_idx != 0 {
        return Err(Error::Unsupported("more than one device"));
    }
    let node = field(flags[0], 12, 16);
    if node < u64::from(header.block_sectors) || node > MAX_NODE_SECTORS {
        return Err(Error::Unsupported("btree node size below a block or past 256 KiB"));
    }
    Ok(header)
}

fn members(body: &[u8], header: &Header) -> Result<Member> {
    if body.len() < 8 {
        return Err(Error::Corrupt("members_v2 without its header"));
    }
    let size = usize::from(le16(body, 0));
    let entry = body.get(8..8 + size).filter(|_| size >= 32).ok_or(Error::Corrupt("members_v2 without the device's entry"))?;
    let member = Member::decode(entry);
    let node_sectors = field(header.flags[0], 12, 16);
    let usable = member.nbuckets.checked_sub(u64::from(member.first_bucket));
    if member.nbuckets > (1 << 31) - 65
        || usable.is_none_or(|n| n < 512)
        || u64::from(member.bucket_sectors) < u64::from(header.block_sectors)
        || u64::from(member.bucket_sectors) < node_sectors
        || member.bitmap_shift >= 58
    {
        return Err(Error::Corrupt("member geometry"));
    }
    Ok(member)
}

/// The journal's bucket ranges, which lie on the device.
fn journal(body: &[u8], member: &Member) -> Result<Vec<(u64, u64)>> {
    let ranges = pairs(body);
    let valid = ranges.iter().all(|&(start, count)| {
        count > 0 && start > 0 && start >= u64::from(member.first_bucket) && start.checked_add(count).is_some_and(|end| end <= member.nbuckets)
    });
    // Disjoint, so together they are at most the device's buckets.
    let mut sorted = ranges.clone();
    sorted.sort_unstable();
    if !valid || sorted.windows(2).any(|w| w[0].0 + w[0].1 > w[1].0) {
        return Err(Error::Corrupt("journal buckets"));
    }
    Ok(ranges)
}

fn pairs(body: &[u8]) -> Vec<(u64, u64)> {
    body.as_chunks::<16>().0.iter().map(|p| (le64(p, 0), le64(p, 8))).collect()
}

/// A `jset_entry` record.
pub(crate) struct Entry<'a> {
    pub btree: u8,
    pub level: u8,
    pub kind: u8,
    pub payload: &'a [u8],
}

/// The records of `b`, back to back.
pub(crate) fn entries(b: &[u8]) -> Result<Vec<Entry<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let len = usize::from(le16(b, at)) * 8;
        let payload = b.get(at + 8..at + 8 + len).ok_or(Error::Corrupt("journal record past its end"))?;
        out.push(Entry { btree: b[at + 2], level: b[at + 3], kind: b[at + 4], payload });
        at += 8 + len;
    }
    Ok(out)
}

/// `jset_entry` record types.
pub(crate) mod entry_type {
    pub const BTREE_KEYS: u8 = 0;
    pub const BTREE_ROOT: u8 = 1;
    pub const BLACKLIST: u8 = 3;
    pub const BLACKLIST_V2: u8 = 4;
    pub const USAGE: u8 = 5;
    pub const CLOCK: u8 = 7;
    pub const WRITE_BUFFER_KEYS: u8 = 11;
    pub const DATETIME: u8 = 12;
}
/// The usage record's counter id for the key version.
pub(crate) const KEY_VERSION: u8 = 2;

fn clean(body: &[u8]) -> Result<Clean> {
    if body.len() < 16 {
        return Err(Error::Corrupt("clean field shorter than 16 bytes"));
    }
    let mut clean = Clean { journal_seq: le64(body, 8), roots: Vec::new(), key_version: None, clocks: [0; 2] };
    for Entry { btree, level, kind: kind_, payload } in entries(&body[16..])? {
        match kind_ {
            entry_type::BTREE_ROOT => {
                let root = Root::decode(btree, level, payload)?;
                clean.roots.retain(|r: &Root| r.btree != root.btree);
                clean.roots.push(root);
            }
            entry_type::USAGE if btree == KEY_VERSION && payload.len() >= 8 => clean.key_version = Some(le64(payload, 0)),
            entry_type::CLOCK if payload.len() == 16 && payload[0] <= 1 => clean.clocks[usize::from(payload[0])] = le64(payload, 8),
            _ => {}
        }
    }
    Ok(clean)
}

fn crypt(body: &[u8]) -> Result<Crypt> {
    if body.len() < 56 {
        return Err(Error::Corrupt("crypt field shorter than 64 bytes"));
    }
    if field(le64(body, 0), 0, 4) != 0 {
        return Err(Error::Corrupt("crypt with a key derivation other than scrypt"));
    }
    let params = le64(body, 8);
    let kdf = Kdf { log_n: field(params, 0, 16) as u16, log_r: field(params, 16, 16) as u16, log_p: field(params, 32, 16) as u16 };
    Ok(Crypt { kdf, wrapped: body[16..56].try_into().unwrap() })
}

fn replicas(body: &[u8], with_required: bool) -> Result<Vec<Replicas>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 2 <= body.len() && body[at + 1] != 0 {
        let (data_type, count) = (body[at], usize::from(body[at + 1]));
        let (required, devs) = if with_required {
            (*body.get(at + 2).ok_or(Error::Corrupt("replicas entry"))?, at + 3)
        } else {
            (1, at + 2)
        };
        let devs = body.get(devs..devs + count).ok_or(Error::Corrupt("replicas entry"))?.to_vec();
        at += devs.len() + if with_required { 3 } else { 2 };
        out.push(Replicas { data_type, required, devs });
    }
    Ok(out)
}
