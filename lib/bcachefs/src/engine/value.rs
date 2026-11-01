//! Values: what follows a key, by key type.

use alloc::vec::Vec;

use super::bkey::Bpos;
use super::{field, le16, le32, le64, put16, put32, put64, Error, Result};

/// The inode fields' varint: `n` bytes for a value of up to 7·`n` bits, `n` at most 8,
/// or 0xff and eight bytes.
pub(crate) fn varint(out: &mut Vec<u8>, v: u64) {
    let n = (64 - (v | 1).leading_zeros()).div_ceil(7);
    if n >= 9 {
        out.push(0xff);
        out.extend_from_slice(&v.to_le_bytes());
    } else {
        let encoded = (v << n) | ((1 << (n - 1)) - 1);
        out.extend_from_slice(&encoded.to_le_bytes()[..n as usize]);
    }
}

/// The varint at the start of `b` and its length.
pub(crate) fn read_varint(b: &[u8]) -> Result<(u64, usize)> {
    let first = *b.first().ok_or(Error::Corrupt("varint past the end of its value"))?;
    let n = first.trailing_ones() as usize + 1;
    if n == 9 {
        let v = b.get(1..9).ok_or(Error::Corrupt("varint past the end of its value"))?;
        return Ok((u64::from_le_bytes(v.try_into().unwrap()), 9));
    }
    let bytes = b.get(..n).ok_or(Error::Corrupt("varint past the end of its value"))?;
    let mut word = [0u8; 8];
    word[..n].copy_from_slice(bytes);
    Ok((u64::from_le_bytes(word) >> n, n))
}

/// What an inode_v3 value holds. Times are in the volume's
/// units since its time base, as stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InodeValue {
    pub journal_seq: u64,
    pub hash_seed: u64,
    pub flags: u64,
    pub sectors: u64,
    pub size: u64,
    /// The 28 packed fields, field `n` at index `n - 1`.
    pub fields: [u64; INODE_FIELDS],
}

pub(crate) const INODE_FIELDS: usize = 28;
/// Each packed field's width in bits; 96 is two varints, low then high.
const INODE_WIDTH: [u32; INODE_FIELDS] =
    [96, 96, 96, 96, 32, 32, 32, 32, 32, 8, 8, 32, 8, 8, 16, 16, 16, 16, 16, 64, 64, 32, 32, 8, 32, 8, 8, 8];
/// A 1.39 writer's fields-start, in u64s.
const FIELDS_START: u64 = 6;

/// Packed field numbers this crate reads or writes.
pub(crate) mod inode_field {
    pub const ATIME: usize = 1;
    pub const CTIME: usize = 2;
    pub const MTIME: usize = 3;
    pub const OTIME: usize = 4;
    pub const UID: usize = 5;
    pub const GID: usize = 6;
    pub const NLINK: usize = 7;
    pub const GENERATION: usize = 8;
    pub const RDEV: usize = 9;
    pub const DIR: usize = 20;
    pub const DIR_OFFSET: usize = 21;
    pub const SUBVOL: usize = 22;
    pub const PARENT_SUBVOL: usize = 23;
    pub const DEPTH: usize = 25;
    /// The per-inode options, stored biased by one: 0 is unset.
    pub const OPTIONS: [usize; 12] = [10, 11, 12, 13, 14, 15, 16, 17, 18, 24, 26, 27];
}

/// File-type bits of a mode.
pub(crate) mod mode {
    pub const S_IFMT: u16 = 0o170000;
    pub const S_IFDIR: u16 = 0o040000;
    pub const S_IFREG: u16 = 0o100000;
    pub const S_IFLNK: u16 = 0o120000;
}

/// Checksum types, of metadata and of data alike.
pub(crate) mod csum_type {
    pub const NONE: u8 = 0;
    /// CRC-32C with its inversions: metadata's type.
    pub const CRC32C_NONZERO: u8 = 1;
    pub const CRC64_NONZERO: u8 = 2;
    pub const CHACHA_POLY_80: u8 = 3;
    pub const CHACHA_POLY_128: u8 = 4;
    /// CRC-32C with no inversions: data's type.
    pub const CRC32C: u8 = 5;
    pub const CRC64: u8 = 6;
    pub const XXHASH: u8 = 7;

    /// The checksum or MAC bytes a type stores.
    pub fn bytes(t: u8) -> usize {
        match t {
            NONE => 0,
            CRC32C_NONZERO | CRC32C => 4,
            CRC64_NONZERO | CRC64 | XXHASH => 8,
            CHACHA_POLY_80 => 10,
            _ => 16,
        }
    }
}

/// Inode flag bits.
pub(crate) mod inode_flag {
    pub const I_SECTORS_DIRTY: u64 = 1 << 6;
    pub const UNLINKED: u64 = 1 << 7;
    pub const HAS_CHILD_SNAPSHOT: u64 = 1 << 9;
    pub const DIRENT_31_BIT: u64 = 1 << 11;
    pub const HAS_INODE_OPTS: u64 = 1 << 12;
    /// The access and default ACL flags, each needing its xattr.
    pub const ACLS: u64 = 3 << 13;
}

impl InodeValue {
    pub(crate) fn field(&self, n: usize) -> u64 {
        self.fields[n - 1]
    }

    pub(crate) fn set(&mut self, n: usize, v: u64) {
        self.fields[n - 1] = v;
    }

    pub(crate) fn mode(&self) -> u16 {
        field(self.flags, 36, 16) as u16
    }

    pub(crate) fn hash_type(&self) -> u8 {
        field(self.flags, 20, 4) as u8
    }

    pub(crate) fn decode(v: &[u8]) -> Result<InodeValue> {
        if v.len() < 48 {
            return Err(Error::Corrupt("inode value shorter than 48 bytes"));
        }
        let flags = le64(v, 16);
        let (count, start) = (field(flags, 24, 7) as usize, field(flags, 31, 5));
        if start < FIELDS_START || start as usize * 8 > v.len() || field(flags, 20, 4) >= 4 {
            return Err(Error::Corrupt("inode fields start or hash type out of range"));
        }
        if count > INODE_FIELDS {
            return Err(Error::Corrupt("inode with more fields than the format has"));
        }
        let mut fields = [0u64; INODE_FIELDS];
        let mut at = start as usize * 8;
        for (i, field) in fields.iter_mut().enumerate().take(count) {
            let (lo, n) = read_varint(&v[at..])?;
            at += n;
            let width = INODE_WIDTH[i];
            if width == 96 {
                let (hi, n) = read_varint(&v[at..])?;
                at += n;
                if hi != 0 {
                    return Err(Error::Corrupt("inode time with a high part"));
                }
            } else if width < 64 && lo >> width != 0 {
                return Err(Error::Corrupt("inode field wider than the format allows"));
            }
            *field = lo;
        }
        Ok(InodeValue { journal_seq: le64(v, 0), hash_seed: le64(v, 8), flags, sectors: le64(v, 24), size: le64(v, 32), fields })
    }

    /// Every field to the last nonzero one, zero-padded to u64s.
    /// `flags`' field count and fields-start are this encoding's.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let count = self.fields.iter().rposition(|&f| f != 0).map_or(0, |i| i + 1);
        let flags = self.flags & !(0x7f << 24) & !(0x1f << 31) | (count as u64) << 24 | FIELDS_START << 31;
        let mut v = alloc::vec![0u8; 48];
        put64(&mut v, 0, self.journal_seq);
        put64(&mut v, 8, self.hash_seed);
        put64(&mut v, 16, flags);
        put64(&mut v, 24, self.sectors);
        put64(&mut v, 32, self.size);
        for (i, &f) in self.fields[..count].iter().enumerate() {
            varint(&mut v, f);
            if INODE_WIDTH[i] == 96 {
                varint(&mut v, 0);
            }
        }
        v.resize(v.len().next_multiple_of(8), 0);
        v
    }
}

/// A directory entry's type: the target's mode type, or a subvolume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DirentType {
    Fifo = 1,
    Char = 2,
    Directory = 4,
    Block = 6,
    Regular = 8,
    Symlink = 10,
    Socket = 12,
    Subvolume = 16,
}

impl DirentType {
    /// The type of a node of `mode`; `None` for a mode of no file type.
    pub(crate) fn of_mode(mode: u16) -> Option<DirentType> {
        Self::from_bits((mode >> 12) as u8).filter(|&t| t != DirentType::Subvolume)
    }

    pub(crate) fn bits(self) -> u8 {
        self as u8
    }

    fn from_bits(bits: u8) -> Option<DirentType> {
        use DirentType::*;
        [Fifo, Char, Directory, Block, Regular, Symlink, Socket, Subvolume].into_iter().find(|&t| t.bits() == bits)
    }
}

/// The longest name a dirent may be given.
pub(crate) const MAX_NAME: usize = 512;

/// A dirent value naming `target`, non-casefold: the name runs to the
/// value's end, zero-padded to a word.
pub(crate) fn dirent(target: u64, kind: DirentType, name: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![0u8; (9 + name.len()).next_multiple_of(8)];
    put64(&mut v, 0, target);
    v[8] = kind.bits();
    v[9..9 + name.len()].copy_from_slice(name);
    v
}

/// A dirent value, non-casefold layout.
pub(crate) struct Dirent<'a> {
    pub target: u64,
    pub kind: DirentType,
    pub name: &'a [u8],
}

impl<'a> Dirent<'a> {
    pub(crate) fn decode(v: &'a [u8]) -> Result<Dirent<'a>> {
        if v.len() < 16 {
            return Err(Error::Corrupt("dirent value shorter than 16 bytes"));
        }
        if v[8] & 0x80 != 0 {
            return Err(Error::Unsupported("casefolded dirent"));
        }
        let kind = DirentType::from_bits(v[8] & 0x1f).ok_or(Error::Corrupt("dirent of no known type"))?;
        let name = &v[9..];
        let end = name.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        let name = &name[..end];
        if name.is_empty() || name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
            return Err(Error::Corrupt("dirent name empty, holding NUL or '/', or a dot entry"));
        }
        Ok(Dirent { target: le64(v, 0), kind, name })
    }
}

/// One pointer into the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ptr {
    pub sector: u64,
    pub dev: u8,
    pub gen: u8,
    pub cached: bool,
    pub unwritten: bool,
}

impl Ptr {
    fn decode(w: u64) -> Ptr {
        Ptr {
            cached: w & 2 != 0,
            unwritten: w & 8 != 0,
            sector: field(w, 4, 44),
            dev: field(w, 48, 8) as u8,
            gen: field(w, 56, 8) as u8,
        }
    }

    pub(crate) fn encode(self) -> u64 {
        assert!(self.sector >> 44 == 0, "a sector past the pointer's 44 bits");
        1 | u64::from(self.cached) << 1 | u64::from(self.unwritten) << 3 | self.sector << 4 | u64::from(self.dev) << 48 | u64::from(self.gen) << 56
    }
}

/// A checksum entry, sizes in sectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Crc {
    pub compressed: u32,
    pub uncompressed: u32,
    pub offset: u32,
    pub nonce: u32,
    pub csum_type: u8,
    pub compression: u8,
    /// Its checksum or MAC, as many bytes as the entry holds.
    pub csum: [u8; 16],
    pub csum_len: usize,
}

/// An extent value's single pointer and the checksum entry before it.
pub(crate) struct Extent {
    pub crc: Option<Crc>,
    pub ptr: Ptr,
}

/// The most sectors one crc32 entry, and one crc64 entry, describes.
const CRC32_SECTORS: u32 = 128;
const CRC64_SECTORS: u32 = 512;

impl Extent {
    /// An unencrypted, uncompressed extent of `sectors` at `ptr`, its
    /// checksum `csum` of `csum_type` in the smallest entry that holds it:
    /// a crc32 entry up to 128 sectors, else a crc64 one.
    pub(crate) fn encode(sectors: u32, csum_type: u8, csum: u32, ptr: Ptr) -> Vec<u8> {
        assert!((1..=CRC64_SECTORS).contains(&sectors), "an extent of {sectors} sectors");
        let n = u64::from(sectors - 1);
        let ty = u64::from(csum_type);
        let words: Vec<u64> = if sectors <= CRC32_SECTORS {
            alloc::vec![0b10 | n << 2 | n << 9 | ty << 24 | u64::from(csum) << 32, ptr.encode()]
        } else {
            alloc::vec![0b100 | n << 3 | n << 12 | ty << 40, u64::from(csum), ptr.encode()]
        };
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Bytes of checksum the smallest entry for this extent's checksum
    /// type and size holds (A5.34): 4 in crc32, 10 in crc64, 16 in crc128.
    pub(crate) fn smallest_entry(crc: &Crc) -> usize {
        let bytes = csum_type::bytes(crc.csum_type);
        if bytes <= 4 && crc.uncompressed <= CRC32_SECTORS && crc.nonce == 0 {
            4
        } else if bytes <= 10 && crc.uncompressed <= CRC64_SECTORS && crc.nonce <= 1023 {
            10
        } else {
            16
        }
    }

    /// The entry list, as far as a one-device volume with no erasure
    /// coding can hold.
    pub(crate) fn decode(v: &[u8]) -> Result<Extent> {
        let words: Vec<u64> = v.as_chunks::<8>().0.iter().map(|w| u64::from_le_bytes(*w)).collect();
        let (mut crc, mut ptr, mut i) = (None, None, 0);
        while i < words.len() {
            let w = words[i];
            let size = match w.trailing_zeros() {
                0 => {
                    if ptr.replace(Ptr::decode(w)).is_some() {
                        return Err(Error::Unsupported("extent with more than one pointer"));
                    }
                    1
                }
                1 => {
                    crc = Some(once(crc, ptr.is_some(), crc32(w))?);
                    1
                }
                2 => {
                    let w1 = *words.get(i + 1).ok_or(Error::Corrupt("crc64 entry past the extent's end"))?;
                    crc = Some(once(crc, ptr.is_some(), crc64(w, w1))?);
                    2
                }
                3 => {
                    let rest = words.get(i + 1..i + 3).ok_or(Error::Corrupt("crc128 entry past the extent's end"))?;
                    crc = Some(once(crc, ptr.is_some(), crc128(w, rest[0], rest[1]))?);
                    3
                }
                4 => return Err(Error::Unsupported("erasure-coded extent")),
                // Flags, the first of which marks the data lost.
                6 if w >> 7 & 1 != 0 => return Err(Error::Corrupt("extent poisoned")),
                // Old rebalance, flags, reconcile and its back-pointer: one
                // word each, background work this reader has no part in.
                5..=8 => 1,
                _ => return Err(Error::Corrupt("extent entry of no known type")),
            };
            i += size;
        }
        let ptr = ptr.ok_or(Error::Corrupt("extent without a pointer"))?;
        Ok(Extent { crc, ptr })
    }
}

/// The extent's checksum entry, which precedes its one pointer.
fn once(had: Option<Crc>, after_ptr: bool, crc: Crc) -> Result<Crc> {
    match (had, after_ptr) {
        (None, false) => Ok(crc),
        (_, true) => Err(Error::Corrupt("checksum entry after the extent's pointer")),
        (Some(_), false) => Err(Error::Corrupt("two checksum entries for one pointer")),
    }
}

fn crc32(w: u64) -> Crc {
    let mut csum = [0u8; 16];
    csum[..4].copy_from_slice(&((w >> 32) as u32).to_le_bytes());
    Crc {
        compressed: field(w, 2, 7) as u32 + 1,
        uncompressed: field(w, 9, 7) as u32 + 1,
        offset: field(w, 16, 7) as u32,
        nonce: 0,
        csum_type: field(w, 24, 4) as u8,
        compression: field(w, 28, 4) as u8,
        csum,
        csum_len: 4,
    }
}

fn crc64(w: u64, w1: u64) -> Crc {
    let mut csum = [0u8; 16];
    csum[..8].copy_from_slice(&w1.to_le_bytes());
    csum[8..10].copy_from_slice(&((w >> 48) as u16).to_le_bytes());
    Crc {
        compressed: field(w, 3, 9) as u32 + 1,
        uncompressed: field(w, 12, 9) as u32 + 1,
        offset: field(w, 21, 9) as u32,
        nonce: field(w, 30, 10) as u32,
        csum_type: field(w, 40, 4) as u8,
        compression: field(w, 44, 4) as u8,
        csum,
        csum_len: 10,
    }
}

fn crc128(w: u64, w1: u64, w2: u64) -> Crc {
    let mut csum = [0u8; 16];
    csum[..8].copy_from_slice(&w1.to_le_bytes());
    csum[8..].copy_from_slice(&w2.to_le_bytes());
    Crc {
        compressed: field(w, 4, 13) as u32 + 1,
        uncompressed: field(w, 17, 13) as u32 + 1,
        offset: field(w, 30, 13) as u32,
        nonce: field(w, 43, 13) as u32,
        csum_type: field(w, 56, 4) as u8,
        compression: field(w, 60, 4) as u8,
        csum,
        csum_len: 16,
    }
}

/// A btree_ptr_v2 value: a child node and where it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodePtr {
    pub seq: u64,
    /// Sectors of the child that are valid.
    pub written: u16,
    pub min: Bpos,
    pub ptr: Ptr,
}

impl NodePtr {
    pub(crate) const BYTES: usize = 48;

    pub(crate) fn decode(v: &[u8]) -> Result<NodePtr> {
        if v.len() < Self::BYTES {
            return Err(Error::Corrupt("btree pointer without a device pointer"));
        }
        if v.len() > Self::BYTES {
            return Err(Error::Unsupported("btree pointer with more than one device pointer"));
        }
        let ptr = le64(v, 40);
        if ptr & 1 == 0 {
            return Err(Error::Corrupt("btree pointer whose entry is no pointer"));
        }
        let ptr = Ptr::decode(ptr);
        if le16(v, 16) == 0 || ptr.cached || ptr.unwritten {
            return Err(Error::Corrupt("btree pointer with no written sectors, or cached or unwritten"));
        }
        Ok(NodePtr { seq: le64(v, 8), written: le16(v, 16), min: Bpos::decode(&v[20..40]), ptr })
    }

    pub(crate) fn encode(&self) -> [u8; Self::BYTES] {
        let mut v = [0u8; Self::BYTES];
        put64(&mut v, 8, self.seq);
        put16(&mut v, 16, self.written);
        self.min.encode(&mut v[20..40]);
        put64(&mut v, 40, self.ptr.encode());
        v
    }
}

/// An alloc_v4 value as a v1 writer sets it: no stripes, no
/// discard, generation 0.
pub(crate) struct Alloc {
    pub data_type: u8,
    pub dirty_sectors: u32,
}

/// Data types.
pub(crate) mod data_type {
    pub const FREE: u8 = 0;
    pub const SB: u8 = 1;
    pub const JOURNAL: u8 = 2;
    pub const BTREE: u8 = 3;
    pub const USER: u8 = 4;
    pub const NEED_GC_GENS: u8 = 8;
    pub const NEED_DISCARD: u8 = 9;

    /// A bucket in one of these holds no sectors.
    pub fn empty(t: u8) -> bool {
        matches!(t, FREE | NEED_GC_GENS | NEED_DISCARD)
    }
}

/// The fields of an alloc_v4 value a check compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AllocV4 {
    pub journal_seq_nonempty: u64,
    /// Bit 0 need_discard, bit 1 needs-gen-increment.
    pub flags: u32,
    pub gen: u8,
    pub oldest_gen: u8,
    pub data_type: u8,
    pub dirty: u32,
    pub cached: u32,
    pub stripe_refcount: u32,
    pub stripe_sectors: u32,
    pub journal_seq_empty: u64,
    pub io_time_write: u64,
}

impl AllocV4 {
    /// A value at least 48 bytes long and as long as its trailing records
    /// say; fields past its end read as zero.
    pub(crate) fn decode(v: &[u8]) -> Result<AllocV4> {
        if v.len() < 48 {
            return Err(Error::Corrupt("alloc_v4 value shorter than 48 bytes"));
        }
        let flags = le32(v, 8);
        let (start, count) = (field(u64::from(flags), 2, 6) as usize, field(u64::from(flags), 8, 6) as usize);
        if start == 0 && count != 0 {
            return Err(Error::Corrupt("alloc_v4 trailing records at offset 0"));
        }
        if v.len() < (if start == 0 { 6 } else { start } + 4 * count) * 8 {
            return Err(Error::Corrupt("alloc_v4 value shorter than its trailing records"));
        }
        let get32 = |at: usize| if v.len() >= at + 4 { le32(v, at) } else { 0 };
        Ok(AllocV4 {
            journal_seq_nonempty: le64(v, 0),
            flags,
            io_time_write: le64(v, 32),
            gen: v[12],
            oldest_gen: v[13],
            data_type: v[14],
            dirty: le32(v, 16),
            cached: le32(v, 20),
            stripe_refcount: le32(v, 40),
            stripe_sectors: if start == 8 { get32(56) } else { 0 },
            journal_seq_empty: if v.len() >= 56 { le64(v, 48) } else { 0 },
        })
    }
}

impl Alloc {
    pub(crate) fn encode(&self) -> [u8; 64] {
        let mut v = [0u8; 64];
        // need_discard and needs-gen-increment set, trailing records at 8.
        put32(&mut v, 8, 0x23);
        v[14] = self.data_type;
        put32(&mut v, 16, self.dirty_sectors);
        // The IO clock when the bucket filled, never below 1.
        put64(&mut v, 24, 1);
        put64(&mut v, 32, 1);
        v
    }
}

/// A backpointer value: the key holding the pointer, in `btree` at
/// `level` (a node's level plus one for a node pointer, 0 for an extent)
/// and at `pos`, and the pointer's data type, generation and sectors.
/// Bytes of a backpointer's value.
pub(crate) const BACKPOINTER_BYTES: usize = 32;
/// Bytes of a bucket_gens value: one generation of each of 256 buckets.
pub(crate) const BUCKET_GENS_BYTES: usize = 256;
/// Counters an accounting value holds at most (A7.4).
pub(crate) const MAX_COUNTERS: usize = 3;

pub(crate) fn backpointer(btree: u8, level: u8, data_type: u8, gen: u8, sectors: u32, pos: Bpos) -> [u8; BACKPOINTER_BYTES] {
    let mut v = [0u8; BACKPOINTER_BYTES];
    v[0] = btree;
    v[1] = level;
    v[2] = data_type;
    v[3] = gen;
    put32(&mut v, 8, sectors);
    pos.encode(&mut v[12..32]);
    v
}

/// A live snapshot-tree root, with its subvolume.
pub(crate) fn snapshot(subvolume: u32, tree: u32) -> [u8; 64] {
    let mut v = [0u8; 64];
    put32(&mut v, 0, SNAPSHOT_HAS_SUBVOLUME);
    put32(&mut v, 16, subvolume);
    put32(&mut v, 20, tree);
    put32(&mut v, 56, SNAPSHOT_LIVE);
    v
}

pub(crate) const SNAPSHOT_HAS_SUBVOLUME: u32 = 1 << 1;
pub(crate) const SNAPSHOT_LIVE: u32 = 0x757c_47a2;
pub(crate) const SUBVOLUME_LIVE: u32 = 0x4ad5_447e;

/// A live subvolume rooted at `inode` in `snapshot`.
pub(crate) fn subvolume(snapshot: u32, inode: u64) -> [u8; 48] {
    let mut v = [0u8; 48];
    put32(&mut v, 4, snapshot);
    put64(&mut v, 8, inode);
    put32(&mut v, 40, SUBVOLUME_LIVE);
    v
}

/// A snapshot tree: its master subvolume and root snapshot.
pub(crate) fn snapshot_tree(master_subvolume: u32, root_snapshot: u32) -> [u8; 8] {
    let mut v = [0u8; 8];
    put32(&mut v, 0, master_subvolume);
    put32(&mut v, 4, root_snapshot);
    v
}

/// An unsharded inode-allocation cursor: its generation and next number.
pub(crate) fn inode_cursor(generation: u32, next: u64) -> [u8; 16] {
    let mut v = [0u8; 16];
    put32(&mut v, 4, generation);
    put64(&mut v, 8, next);
    v
}

/// The fields of a subvolume a check compares.
pub(crate) struct Subvolume {
    pub snapshot: u32,
    pub inode: u64,
    pub state: u32,
    pub fs_path_parent: u32,
}

impl Subvolume {
    pub(crate) fn decode(v: &[u8]) -> Result<Subvolume> {
        if v.len() < 48 {
            return Err(Error::Corrupt("subvolume value shorter than 48 bytes"));
        }
        Ok(Subvolume { snapshot: le32(v, 4), inode: le64(v, 8), fs_path_parent: le32(v, 20), state: le32(v, 40) })
    }
}

/// An accounting key's position: its class record read as one
/// big-endian 160-bit integer.
pub(crate) fn accounting_pos(record: &[u8]) -> Bpos {
    let mut r = [0u8; 20];
    r[..record.len()].copy_from_slice(record);
    r.reverse();
    Bpos::decode(&r)
}

/// Accounting classes.
pub(crate) mod class {
    pub const NR_INODES: u8 = 0;
    pub const REPLICAS: u8 = 2;
    pub const DEV_DATA_TYPE: u8 = 3;
    pub const SNAPSHOT: u8 = 5;
    pub const BTREE: u8 = 6;
    pub const INUM: u8 = 8;
}

/// The counters of an accounting value.
pub(crate) fn counters(c: &[u64]) -> Vec<u8> {
    c.iter().flat_map(|c| c.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_take_seven_bits_a_byte_up_to_eight_bytes() {
        for (v, len) in [(0u64, 1), (1, 1), (127, 1), (128, 2), ((1 << 56) - 1, 8), (1 << 56, 9), (u64::MAX, 9)] {
            let mut b = Vec::new();
            varint(&mut b, v);
            assert_eq!(b.len(), len, "{v}");
            assert_eq!(read_varint(&b), Ok((v, len)), "{v}");
        }
        let mut b = Vec::new();
        varint(&mut b, 1);
        assert_eq!(b, [0x02], "one byte, low bit 0, the value above it");
        assert!(read_varint(&[0x01]).is_err(), "a two-byte varint cut short");
    }

    #[test]
    fn an_inode_value_encodes_to_its_last_nonzero_field() {
        let mut inode = InodeValue { hash_seed: 7, flags: 0o40755 << 36 | 3 << 20, ..Default::default() };
        inode.set(inode_field::SUBVOL, 1);
        let v = inode.encode();
        // 48 fixed bytes; four zero times of two bytes, 17 zero fields, then
        // the subvolume id: 26 bytes padded to 32.
        assert_eq!(v.len(), 80);
        let read = InodeValue::decode(&v).unwrap();
        assert_eq!(field(read.flags, 24, 7), 22, "the subvolume id is field 22");
        assert_eq!((read.mode(), read.hash_type(), read.field(inode_field::SUBVOL)), (0o40755, 3, 1));
        assert_eq!(read.fields, inode.fields);
    }

    #[test]
    fn a_crc64_entry_holds_an_80_bit_mac() {
        let w = 0b100 | 31 << 3 | 31 << 12 | 3 << 40 | 0xbeef << 48;
        let crc = crc64(w, 0x0706_0504_0302_0100);
        assert_eq!((crc.compressed, crc.uncompressed, crc.csum_type, crc.csum_len), (32, 32, 3, 10));
        assert_eq!(&crc.csum[..10], &[0, 1, 2, 3, 4, 5, 6, 7, 0xef, 0xbe]);
    }

    #[test]
    fn an_extent_is_its_checksum_entry_then_its_pointer() {
        let ptr = Ptr { sector: 5248, dev: 0, gen: 0, cached: false, unwritten: false }.encode();
        let crc = 0b10 | 31 << 2 | 31 << 9 | 5 << 24 | 0xdead_beef << 32;
        let value = [crc, ptr].map(u64::to_le_bytes).concat();
        let extent = Extent::decode(&value).unwrap();
        assert_eq!((extent.ptr.sector, extent.crc.unwrap().csum_type, extent.crc.unwrap().compressed), (5248, 5, 32));
        let after = [ptr, crc].map(u64::to_le_bytes).concat();
        assert!(Extent::decode(&after).is_err(), "a checksum entry after the pointer");
        let poisoned = [0b100_0000 | 1 << 7, crc, ptr].map(u64::to_le_bytes).concat();
        assert_eq!(Extent::decode(&poisoned).err(), Some(Error::Corrupt("extent poisoned")));
        let flags = [0b100_0000, crc, ptr].map(u64::to_le_bytes).concat();
        assert!(Extent::decode(&flags).is_ok(), "a flags entry without the poison bit");
    }

    #[test]
    fn an_accounting_record_is_its_position_read_big_endian() {
        // Replicas (btree, 1 device, 1 required, device 0).
        assert_eq!(accounting_pos(&[2, 3, 1, 1, 0]), Bpos::new(0x0203_0101_0000_0000, 0, 0));
        // inum 0x80000003.
        assert_eq!(accounting_pos(&[8, 3, 0, 0, 0x80]), Bpos::new(0x0803_0000_8000_0000, 0, 0));
    }

    #[test]
    fn a_node_pointer_reads_back() {
        let ptr = NodePtr { seq: 0x1234, written: 8, min: Bpos::new(0, 83, 0), ptr: Ptr { sector: 4640, dev: 0, gen: 1, cached: false, unwritten: false } };
        assert_eq!(NodePtr::decode(&ptr.encode()), Ok(ptr));
    }
}
