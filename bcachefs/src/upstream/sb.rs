//! The superblock: where it is, what makes it this device's, and the options
//! and feature bits that decide whether a mount may proceed.

use alloc::vec;
use alloc::vec::Vec;

use crate::block_io::{BlockBuf, BlockIO, BlockNum, BLOCK_SIZE};

use super::csum::CsumType;
use super::raw::{bits, Raw};
use super::UpstreamError;

pub const SECTOR: usize = 512;
/// `BCH_SB_SECTOR` and `BCH_SB_LAYOUT_SECTOR`.
pub const SB_SECTOR: u64 = 8;
pub const LAYOUT_SECTOR: u64 = 7;
/// `BCHFS_MAGIC`, as the sixteen bytes it occupies on disk.
pub const BCHFS_MAGIC: [u8; 16] = [
    0xc6, 0x85, 0x73, 0xf6, 0x66, 0xce, 0x90, 0xa9, 0xd9, 0x6a, 0x60, 0xcf, 0x80, 0x3d, 0xf7, 0xef,
];
pub const BSET_MAGIC: u64 = 0x9013_5c78_b99e_07f5;

/// `bcachefs_metadata_version_max - 1` at the pinned commit: what `bcachefs
/// format` writes today, and the only version this reader claims to know.
pub const VERSION_CURRENT: u16 = 1063;
/// `bcachefs_metadata_version_major_minor`: below it the format predates the
/// single major.minor scheme and nothing here applies.
pub const VERSION_MIN_SUPPORTED: u16 = 1024;

const SB_VERSION: usize = 16;
const SB_VERSION_MIN: usize = 18;
const SB_MAGIC: usize = 24;
const SB_UUID: usize = 40;
const SB_OFFSET: usize = 104;
const SB_SEQ: usize = 112;
const SB_BLOCK_SIZE: usize = 120;
const SB_DEV_IDX: usize = 122;
const SB_NR_DEVICES: usize = 123;
const SB_U64S: usize = 124;
const SB_FLAGS: usize = 144;
const SB_FEATURES: usize = 208;
const SB_LAYOUT: usize = 240;
/// `offsetof(struct bch_sb, _data)`: the fixed header, layout included.
const SB_FIELDS_START: usize = 752;

const LAYOUT_BYTES: usize = 512;
const LAYOUT_MAX_SUPERBLOCKS: usize = 61;
/// `BCH_SB_LAYOUT_SIZE_BITS_MAX`: 512 << 16 is 32 MB, the largest a superblock
/// may reserve and therefore the largest this reader will ever allocate for one.
const LAYOUT_SIZE_BITS_MAX: u8 = 16;

/// Section types this reader looks for; the rest are stepped over.
const FIELD_MEMBERS_V1: u32 = 1;
const FIELD_CLEAN: u32 = 6;
const FIELD_MEMBERS_V2: u32 = 11;
const FIELD_EXTENT_TYPE_U64S: u32 = 16;
/// `bch_sb_field_clean`'s flags, clocks and `journal_seq`, before its entries.
const CLEAN_ENTRIES_AT: usize = 16;

/// Feature bits `BCH_SB_FEATURES()` numbers, in the two words `features[2]`.
const FEATURE_LZ4: u32 = 0;
const FEATURE_GZIP: u32 = 1;
const FEATURE_ZSTD: u32 = 2;
const FEATURE_EC: u32 = 4;
const FEATURE_INCOMPRESSIBLE: u32 = 10;
const FEATURE_CASEFOLDING: u32 = 20;
const FEATURE_NO_DEFAULT_SB: u32 = 23;

/// The features a volume `bcachefs format` writes carries, and which this
/// reader therefore has to tolerate. A bit outside this set is refused by
/// number, because a feature nobody has read is a feature nobody can honour.
const FEATURES_KNOWN: u64 = (1 << 5) // journal_seq_blacklist_v3
    | (1 << 6)  // reflink
    | (1 << 7)  // new_siphash
    | (1 << 8)  // inline_data
    | (1 << 9)  // new_extent_overwrite
    | (1 << 11) // btree_ptr_v2
    | (1 << 12) // extents_above_btree_updates
    | (1 << 13) // btree_updates_journalled
    | (1 << 14) // reflink_inline_data
    | (1 << 15) // new_varint
    | (1 << 16) // journal_no_flush
    | (1 << 17) // alloc_v2
    | (1 << 18) // extents_across_btree_nodes
    | (1 << 19) // incompat_version_field
    | (1 << 21) // no_alloc_info
    | (1 << 22) // small_image
    | (1 << 23); // no_default_sb, refused by name above rather than honoured

/// One member device, as the members section describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub uuid: [u8; 16],
    pub nbuckets: u64,
    /// Sectors, and the unit every bucket index is multiplied by.
    pub bucket_size: u16,
    pub first_bucket: u16,
}

/// A superblock this reader has agreed to mount from.
pub struct Superblock {
    bytes: Vec<u8>,
}

impl Superblock {
    fn raw(&self) -> Raw<'_> {
        Raw::new(&self.bytes, "superblock ends early")
    }

    pub fn version(&self) -> u16 {
        self.raw().u16(SB_VERSION).unwrap_or(0)
    }

    pub fn version_min(&self) -> u16 {
        self.raw().u16(SB_VERSION_MIN).unwrap_or(0)
    }

    /// The immutable filesystem UUID; the bset and jset magics are derived from
    /// its first eight bytes.
    pub fn uuid(&self) -> [u8; 16] {
        self.raw().uuid(SB_UUID).unwrap_or([0; 16])
    }

    /// Filesystem block size in bytes.
    pub fn block_size(&self) -> u32 {
        self.raw().u16(SB_BLOCK_SIZE).unwrap_or(0) as u32 * SECTOR as u32
    }

    pub fn dev_idx(&self) -> u8 {
        self.raw().u8(SB_DEV_IDX).unwrap_or(0)
    }

    pub fn nr_devices(&self) -> u8 {
        self.raw().u8(SB_NR_DEVICES).unwrap_or(0)
    }

    fn flags(&self, word: usize) -> u64 {
        self.raw().u64(SB_FLAGS + word * 8).unwrap_or(0)
    }

    fn features(&self, word: usize) -> u64 {
        self.raw().u64(SB_FEATURES + word * 8).unwrap_or(0)
    }

    pub fn is_clean(&self) -> bool {
        bits(self.flags(0), 1, 2) == 1
    }

    /// The btree node size in bytes: every node read is exactly this long.
    pub fn btree_node_size(&self) -> u64 {
        bits(self.flags(0), 12, 28) * SECTOR as u64
    }

    pub fn metadata_csum(&self) -> Result<CsumType, UpstreamError> {
        csum_from_opt(bits(self.flags(0), 40, 44))
    }

    pub fn data_csum(&self) -> Result<CsumType, UpstreamError> {
        csum_from_opt(bits(self.flags(0), 44, 48))
    }

    /// Walk the variable-length sections, handing each `(type, payload)`.
    ///
    /// A section whose `u64s` does not advance is the end of the list rather
    /// than a loop: upstream's terminator is a zero-length field.
    fn sections(&self) -> Sections<'_> {
        Sections { raw: self.raw(), at: SB_FIELDS_START }
    }

    fn section(&self, want: u32) -> Option<Raw<'_>> {
        self.sections().find(|(ty, _)| *ty == want).map(|(_, payload)| payload)
    }

    /// This device's member entry, which is where a bucket becomes a sector.
    pub fn member(&self, idx: u8) -> Result<Member, UpstreamError> {
        let (section, first, stride) = if let Some(v2) = self.section(FIELD_MEMBERS_V2) {
            let stride = v2.u16(0)? as usize;
            (v2, 8, stride)
        } else if let Some(v1) = self.section(FIELD_MEMBERS_V1) {
            (v1, 0, MEMBER_V1_BYTES)
        } else {
            return Err(UpstreamError::Refused("superblock has no members section"));
        };
        if stride < MEMBER_MIN_BYTES {
            return Err(UpstreamError::Refused("members section entry is too small to be one"));
        }
        let at = first + (idx as usize)
            .checked_mul(stride)
            .ok_or(UpstreamError::Refused("member index times entry size overflows"))?;
        let m = section.sub(at, stride, "members section ends before this device")?;
        Ok(Member {
            uuid: m.uuid(0)?,
            nbuckets: m.u64(16)?,
            first_bucket: m.u16(24)?,
            bucket_size: m.u16(26)?,
        })
    }

    /// The clean section's journal entries, which carry the btree roots a
    /// cleanly-unmounted filesystem is opened from.
    pub fn clean(&self) -> Result<Raw<'_>, UpstreamError> {
        let clean = self
            .section(FIELD_CLEAN)
            .ok_or(UpstreamError::Refused("volume has no clean section: it needs journal replay"))?;
        // A section shorter than its own fixed part is a subtraction that
        // panics under the kernel's overflow checks, not a short read.
        let entries = clean
            .bytes()
            .len()
            .checked_sub(CLEAN_ENTRIES_AT)
            .ok_or(UpstreamError::Refused("clean section is shorter than its own header"))?;
        clean.sub(CLEAN_ENTRIES_AT, entries, "clean section ends before its entries")
    }

    /// How many u64s each `bch_extent_entry` type occupies.
    ///
    /// **A size that is wrong by one word makes the walker step onto a pointer
    /// and serve arbitrary device blocks as file contents.** The sizes for the
    /// types this reader decodes are compiled in, because they are what its
    /// own field offsets assume; the superblock's `extent_type_u64s` section
    /// supplies the rest, so an entry type written after this was is stepped
    /// over rather than guessed at. A section that disagrees with a compiled-in
    /// size is refused: one of the two is describing another format.
    pub fn extent_entry_u64s(&self) -> Result<[u8; EXTENT_TYPES_MAX], UpstreamError> {
        let section = self.section(FIELD_EXTENT_TYPE_U64S).ok_or(UpstreamError::Refused(
            "volume has no extent_type_u64s section: this reader cannot size an extent's entries",
        ))?;
        let mut sizes = EXTENT_ENTRY_U64S_KNOWN;
        for (kind, size) in sizes.iter_mut().enumerate() {
            let Ok(stated) = section.u8(kind) else { break };
            if stated == 0 {
                break;
            }
            if kind < EXTENT_ENTRY_TYPES_KNOWN && stated != *size {
                return Err(UpstreamError::Refused(
                    "volume sizes an extent entry this reader decodes differently than it does",
                ));
            }
            *size = stated;
        }
        Ok(sizes)
    }

    /// Refuse, by name, every format feature this reader does not implement.
    ///
    /// A feature bit silently ignored is a mount that returns the wrong bytes,
    /// so the unknown half of `features[0]` is refused by number too.
    fn check_supported(&self, device_sectors: u64) -> Result<(), UpstreamError> {
        let refuse = |why| Err(UpstreamError::Refused(why));

        if self.version() > VERSION_CURRENT {
            return refuse("volume is a newer metadata version than this reader implements");
        }
        if self.version_min() < VERSION_MIN_SUPPORTED {
            return refuse("volume still holds metadata older than version 1.0");
        }
        if bits(self.flags(0), 62, 63) != 0 {
            return refuse("volume was written big-endian");
        }
        if self.block_size() as usize != BLOCK_SIZE {
            return refuse("volume's block size is not the 4096 bytes this crate reads");
        }
        if !self.btree_node_size().is_multiple_of(BLOCK_SIZE as u64) || self.btree_node_size() == 0 {
            return refuse("btree node size is not a whole number of blocks");
        }
        if self.nr_devices() != 1 {
            return refuse("volume has more than one member device");
        }
        if self.dev_idx() != 0 {
            return refuse("volume's superblock is not device 0 of its filesystem");
        }
        if bits(self.flags(1), 10, 14) != 0 {
            return refuse("volume is encrypted");
        }
        if bits(self.flags(1), 4, 8) != 0 || bits(self.flags(4), 56, 60) != 0 {
            return refuse("volume has compression enabled");
        }
        if bits(self.flags(2), 0, 4) != 0 || bits(self.flags(4), 60, 64) != 0 {
            return refuse("volume has background compression enabled");
        }
        // Wanted and required, both kinds: a zero is as much "not one replica"
        // as a two is, and this reader reads exactly one copy of everything.
        for (word, start, end) in [(0, 48, 52), (0, 52, 56), (1, 20, 24), (1, 24, 28)] {
            if bits(self.flags(word), start, end) != 1 {
                return refuse("volume does not ask for exactly one replica of everything");
            }
        }
        // A volume that checksums nothing would have this reader verifying
        // every extent against a checksum nobody computed.
        if self.metadata_csum()? == CsumType::None || self.data_csum()? == CsumType::None {
            return refuse("volume checksums its metadata or its data with nothing");
        }
        if bits(self.flags(3), 0, 16) != 0 {
            return refuse("volume has erasure coding enabled");
        }
        if bits(self.flags(6), 22, 23) != 0 {
            return refuse("volume is casefolded");
        }
        if bits(self.flags(3), 63, 64) != 0 {
            return refuse("volume is a multi-device filesystem");
        }
        for (bit, why) in [
            (FEATURE_LZ4, "volume carries lz4-compressed data"),
            (FEATURE_GZIP, "volume carries gzip-compressed data"),
            (FEATURE_ZSTD, "volume carries zstd-compressed data"),
            (FEATURE_INCOMPRESSIBLE, "volume carries compression metadata"),
            (FEATURE_EC, "volume carries erasure-coded stripes"),
            (FEATURE_CASEFOLDING, "volume carries casefolded directories"),
            (FEATURE_NO_DEFAULT_SB, "volume keeps no superblock at the sector this reader looks at"),
        ] {
            if self.features(0) & (1u64 << bit) != 0 {
                return refuse(why);
            }
        }
        if self.features(0) & !FEATURES_KNOWN != 0 {
            return refuse("volume sets a feature bit this reader has never read");
        }
        if self.features(1) != 0 {
            return refuse("volume sets a feature bit past the first word");
        }

        let member = self.member(self.dev_idx())?;
        if member.bucket_size == 0 {
            return refuse("member's bucket size is zero");
        }
        let sectors = member
            .nbuckets
            .checked_mul(member.bucket_size as u64)
            .ok_or(UpstreamError::Refused("member's bucket count times bucket size overflows"))?;
        if sectors > device_sectors {
            return refuse("member claims more sectors than the device has");
        }
        if member.first_bucket as u64 >= member.nbuckets {
            return refuse("member's first bucket is past its last");
        }
        Ok(())
    }
}

/// The largest entry type this reader will size, one past `BCH_EXTENT_ENTRY_MAX`
/// so the superblock can name a type written after this code was.
pub const EXTENT_TYPES_MAX: usize = 16;
/// `BCH_EXTENT_ENTRY_MAX`: how many of those this reader has read the layout of.
pub const EXTENT_ENTRY_TYPES_KNOWN: usize = 9;
/// `extent_entry_u64s_known`, as `sizeof` gives it at the pinned commit: ptr,
/// crc32, crc64, crc128, stripe_ptr, rebalance_v1, flags, reconcile,
/// reconcile_bp. The last four are each one 64-bit bitfield.
const EXTENT_ENTRY_U64S_KNOWN: [u8; EXTENT_TYPES_MAX] =
    [1, 1, 2, 3, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];

#[cfg(test)]
pub(crate) const EXTENT_ENTRY_U64S_KNOWN_FOR_TESTS: [u8; EXTENT_TYPES_MAX] = EXTENT_ENTRY_U64S_KNOWN;

const MEMBER_V1_BYTES: usize = 56;
/// Everything before `last_mount`: the fields this reader takes.
const MEMBER_MIN_BYTES: usize = 32;

fn csum_from_opt(opt: u64) -> Result<CsumType, UpstreamError> {
    match opt {
        0 => Ok(CsumType::None),
        1 => Ok(CsumType::Crc32c),
        2 => Ok(CsumType::Crc64),
        3 => Ok(CsumType::Xxhash),
        _ => Err(UpstreamError::Refused("checksum option is not one this format defines")),
    }
}

struct Sections<'a> {
    raw: Raw<'a>,
    at: usize,
}

impl<'a> Iterator for Sections<'a> {
    type Item = (u32, Raw<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let u64s = self.raw.u32(self.at).ok()? as usize;
        let ty = self.raw.u32(self.at + 4).ok()?;
        if u64s == 0 {
            return None;
        }
        let bytes = u64s.checked_mul(8)?;
        let payload = self.raw.sub(self.at + 8, bytes.checked_sub(8)?, "section ends early").ok()?;
        self.at = self.at.checked_add(bytes)?;
        Some((ty, payload))
    }
}

/// Read `len` bytes at `byte_off` through a 4096-byte block device.
///
/// The superblock lives at sector 8 and its backups at sectors the layout
/// chooses, none of which the block size has any say over.
fn read_bytes(io: &dyn BlockIO, byte_off: u64, len: usize) -> Result<Vec<u8>, UpstreamError> {
    let first = byte_off / BLOCK_SIZE as u64;
    let skip = (byte_off % BLOCK_SIZE as u64) as usize;
    let blocks = (skip + len).div_ceil(BLOCK_SIZE);
    let last = first
        .checked_add(blocks as u64)
        .ok_or(UpstreamError::Refused("a read past the end of any device"))?;
    if last > io.block_count() {
        return Err(UpstreamError::Refused("structure runs past the end of the device"));
    }

    let mut out = vec![0u8; blocks * BLOCK_SIZE];
    let mut buf = BlockBuf::zeroed();
    for i in 0..blocks {
        let block = BlockNum::new(first + i as u64);
        io.read_block(block, &mut buf).map_err(|e| UpstreamError::Device(block, e))?;
        out[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE].copy_from_slice(buf.as_bytes());
    }
    out.drain(..skip);
    out.truncate(len);
    Ok(out)
}

/// Read and validate this device's superblock.
///
/// The primary at sector 8 first; a copy the layout names only when the primary
/// does not parse, because a device that refused the read is a device that is
/// not answering and not a superblock that is wrong.
pub fn read(io: &dyn BlockIO) -> Result<Superblock, UpstreamError> {
    let device_sectors = io
        .block_count()
        .checked_mul((BLOCK_SIZE / SECTOR) as u64)
        .ok_or(UpstreamError::Refused("device size in sectors overflows"))?;

    let primary = read_one(io, SB_SECTOR, device_sectors);
    let Err(primary_err) = primary else { return primary };

    let layout = read_bytes(io, LAYOUT_SECTOR * SECTOR as u64, LAYOUT_BYTES)?;
    for offset in layout_offsets(&layout)? {
        if offset == SB_SECTOR {
            continue;
        }
        if let Ok(sb) = read_one(io, offset, device_sectors) {
            return Ok(sb);
        }
    }
    Err(primary_err)
}

/// The backup superblock sectors the standalone layout at sector 7 names.
fn layout_offsets(layout: &[u8]) -> Result<Vec<u64>, UpstreamError> {
    let raw = Raw::new(layout, "superblock layout ends early");
    if raw.uuid(0)? != BCHFS_MAGIC {
        return Err(UpstreamError::Refused("no bcachefs superblock layout at sector 7"));
    }
    if raw.u8(16)? != 0 {
        return Err(UpstreamError::Refused("superblock layout is a type this reader has not read"));
    }
    if raw.u8(17)? > LAYOUT_SIZE_BITS_MAX {
        return Err(UpstreamError::Refused("superblock layout reserves more than 32 MB"));
    }
    let nr = raw.u8(18)? as usize;
    if nr == 0 || nr > LAYOUT_MAX_SUPERBLOCKS {
        return Err(UpstreamError::Refused("superblock layout names an impossible number of copies"));
    }
    (0..nr).map(|i| raw.u64(24 + i * 8)).collect()
}

fn read_one(io: &dyn BlockIO, sector: u64, device_sectors: u64) -> Result<Superblock, UpstreamError> {
    let byte_off = sector
        .checked_mul(SECTOR as u64)
        .ok_or(UpstreamError::Refused("superblock offset in bytes overflows"))?;

    // The header alone first: `u64s` decides how much more to read, and it is a
    // number the disk chose, so it is bounded before it sizes an allocation.
    let head = read_bytes(io, byte_off, SB_FIELDS_START)?;
    let raw = Raw::new(&head, "superblock header ends early");
    if raw.uuid(SB_MAGIC)? != BCHFS_MAGIC {
        return Err(UpstreamError::Refused("no bcachefs superblock here"));
    }
    if raw.u64(SB_OFFSET)? != sector {
        return Err(UpstreamError::Refused("superblock names a sector other than its own"));
    }

    let size_bits = raw.u8(SB_LAYOUT + 17)?;
    if size_bits > LAYOUT_SIZE_BITS_MAX {
        return Err(UpstreamError::Refused("superblock reserves more than 32 MB"));
    }
    let reserved = (SECTOR as u64) << size_bits;
    let u64s = raw.u32(SB_U64S)? as u64;
    let bytes = SB_FIELDS_START as u64 + u64s * 8;
    if bytes > reserved {
        return Err(UpstreamError::Refused("superblock is longer than the space it reserves"));
    }
    if byte_off / SECTOR as u64 + bytes.div_ceil(SECTOR as u64) > device_sectors {
        return Err(UpstreamError::Refused("superblock runs past the end of the device"));
    }

    let bytes = read_bytes(io, byte_off, bytes as usize)?;
    let raw = Raw::new(&bytes, "superblock ends early");
    let csum_type = CsumType::from_disk(bits(raw.u64(SB_FLAGS)?, 2, 8))?;
    let stored = (raw.u64(0)?, raw.u64(8)?);
    if !csum_type.verify(&bytes[SB_VERSION..], stored) {
        return Err(UpstreamError::Refused("superblock checksum does not match its bytes"));
    }

    let sb = Superblock { bytes };
    sb.check_supported(device_sectors)?;
    Ok(sb)
}

#[cfg(all(test, feature = "std"))]
pub(crate) mod fixture {
    use super::*;
    use crate::block_io::VecBlockIO;

    pub(crate) const DEVICE_BLOCKS: u64 = 512;
    const NBUCKETS: u64 = 512;
    const BUCKET_SECTORS: u16 = 8;

    /// The smallest superblock this reader accepts, as bytes, so a test can
    /// move one field and name what that costs.
    pub(crate) fn valid() -> Vec<u8> {
        let members = {
            let mut section = vec![0u8; 48];
            section[0..4].copy_from_slice(&6u32.to_le_bytes());
            section[4..8].copy_from_slice(&FIELD_MEMBERS_V2.to_le_bytes());
            section[8..10].copy_from_slice(&32u16.to_le_bytes());
            section[32..40].copy_from_slice(&NBUCKETS.to_le_bytes());
            section[40..42].copy_from_slice(&0u16.to_le_bytes());
            section[42..44].copy_from_slice(&BUCKET_SECTORS.to_le_bytes());
            section
        };
        let clean = {
            let mut section = vec![0u8; 24];
            section[0..4].copy_from_slice(&3u32.to_le_bytes());
            section[4..8].copy_from_slice(&FIELD_CLEAN.to_le_bytes());
            section
        };
        let extent_types = {
            let mut section = vec![0u8; 24];
            section[0..4].copy_from_slice(&3u32.to_le_bytes());
            section[4..8].copy_from_slice(&FIELD_EXTENT_TYPE_U64S.to_le_bytes());
            section[8..8 + EXTENT_ENTRY_TYPES_KNOWN]
                .copy_from_slice(&EXTENT_ENTRY_U64S_KNOWN[..EXTENT_ENTRY_TYPES_KNOWN]);
            section
        };
        let sections = [members.as_slice(), clean.as_slice(), extent_types.as_slice()].concat();

        let mut sb = vec![0u8; SB_FIELDS_START + sections.len()];
        sb[SB_VERSION..SB_VERSION + 2].copy_from_slice(&VERSION_CURRENT.to_le_bytes());
        sb[SB_VERSION_MIN..SB_VERSION_MIN + 2].copy_from_slice(&VERSION_CURRENT.to_le_bytes());
        sb[SB_MAGIC..SB_MAGIC + 16].copy_from_slice(&BCHFS_MAGIC);
        sb[SB_OFFSET..SB_OFFSET + 8].copy_from_slice(&SB_SECTOR.to_le_bytes());
        sb[SB_BLOCK_SIZE..SB_BLOCK_SIZE + 2].copy_from_slice(&8u16.to_le_bytes());
        sb[SB_NR_DEVICES] = 1;
        let u64s = (sections.len() / 8) as u32;
        sb[SB_U64S..SB_U64S + 4].copy_from_slice(&u64s.to_le_bytes());

        // initialized, clean, csum type crc32c_nonzero, 256 KB nodes, one
        // replica of each kind wanted, each checksummed crc32c.
        let flags0 = 1 | (1 << 1) | (1 << 2) | (512u64 << 12) | (1 << 40) | (1 << 44)
            | (1 << 48) | (1 << 52);
        sb[SB_FLAGS..SB_FLAGS + 8].copy_from_slice(&flags0.to_le_bytes());
        // One replica of each kind required.
        let flags1 = (1u64 << 20) | (1 << 24);
        sb[SB_FLAGS + 8..SB_FLAGS + 16].copy_from_slice(&flags1.to_le_bytes());

        sb[SB_LAYOUT..SB_LAYOUT + 16].copy_from_slice(&BCHFS_MAGIC);
        sb[SB_LAYOUT + 17] = 11;
        sb[SB_LAYOUT + 18] = 1;
        sb[SB_LAYOUT + 24..SB_LAYOUT + 32].copy_from_slice(&SB_SECTOR.to_le_bytes());

        sb[SB_FIELDS_START..].copy_from_slice(&sections);
        reseal(&mut sb);
        sb
    }

    /// Recompute the checksum after a field has been moved, so a test measures
    /// the field's refusal rather than the checksum's.
    pub(crate) fn reseal(sb: &mut [u8]) {
        let (lo, hi) = CsumType::Crc32cNonzero.digest(&sb[SB_VERSION..]);
        sb[0..8].copy_from_slice(&lo.to_le_bytes());
        sb[8..16].copy_from_slice(&hi.to_le_bytes());
    }

    pub(crate) fn device(sb: &[u8]) -> VecBlockIO {
        let mut image = vec![0u8; DEVICE_BLOCKS as usize * BLOCK_SIZE];
        let layout_at = LAYOUT_SECTOR as usize * SECTOR;
        image[layout_at..layout_at + 16].copy_from_slice(&BCHFS_MAGIC);
        image[layout_at + 17] = 11;
        image[layout_at + 18] = 1;
        image[layout_at + 24..layout_at + 32].copy_from_slice(&SB_SECTOR.to_le_bytes());
        let at = SB_SECTOR as usize * SECTOR;
        image[at..at + sb.len()].copy_from_slice(sb);
        VecBlockIO::from_vec(image)
    }

    /// One field of a superblock, moved.
    type Mutation = alloc::boxed::Box<dyn Fn(&mut Vec<u8>)>;

    pub(crate) fn refusal(sb: &[u8]) -> &'static str {
        match read(&device(sb)) {
            Err(UpstreamError::Refused(why)) => why,
            Err(other) => panic!("expected a refusal, got {other:?}"),
            Ok(_) => panic!("this superblock should not have been accepted"),
        }
    }

    /// The builder makes something this reader accepts, which is what makes
    /// every mutation below a measurement of the field it moved.
    #[test]
    fn the_crafted_superblock_is_accepted() {
        let sb = read(&device(&valid())).expect("the crafted superblock");
        assert_eq!(sb.version(), VERSION_CURRENT);
        assert!(sb.is_clean());
        assert_eq!(sb.block_size(), BLOCK_SIZE as u32);
        assert_eq!(sb.btree_node_size(), 256 * 1024);
        assert_eq!(sb.metadata_csum(), Ok(CsumType::Crc32c));
        assert_eq!(sb.member(0).expect("the member").nbuckets, NBUCKETS);
    }

    /// **`u64s` is the defect class this crate's `alloc_probe` exists for**: a
    /// length off the disk sizes the buffer the superblock is read into. A
    /// superblock claiming four gigabytes is refused, and the refusal costs
    /// nothing like four gigabytes.
    #[test]
    fn a_huge_u64s_is_refused_without_allocating_it() {
        let mut sb = valid();
        sb[SB_U64S..SB_U64S + 4].copy_from_slice(&(1u32 << 29).to_le_bytes());
        reseal(&mut sb);

        let io = device(&sb);
        let _ = crate::alloc_probe::take_peak();
        let err = read(&io).err();
        let peak = crate::alloc_probe::take_peak();

        assert_eq!(err, Some(UpstreamError::Refused("superblock is longer than the space it reserves")));
        assert!(peak < 1 << 20, "refusing a 4 GB superblock asked the allocator for {peak} bytes");
    }

    /// Each of these is a sentence a log line can print, and each is reached
    /// only because the checksum was recomputed over the moved field.
    #[test]
    fn every_unsupported_format_feature_is_refused_by_name() {
        let mut cases: Vec<(&str, Mutation)> = Vec::new();
        let flag = |word: usize, shift: u32, value: u64| -> Mutation {
            Box::new(move |sb: &mut Vec<u8>| {
                let at = SB_FLAGS + word * 8;
                let mut bits = u64::from_le_bytes(sb[at..at + 8].try_into().unwrap());
                bits |= value << shift;
                sb[at..at + 8].copy_from_slice(&bits.to_le_bytes());
            })
        };
        let feature = |bit: u32| -> Mutation {
            Box::new(move |sb: &mut Vec<u8>| {
                let mut bits = u64::from_le_bytes(sb[SB_FEATURES..SB_FEATURES + 8].try_into().unwrap());
                bits |= 1u64 << bit;
                sb[SB_FEATURES..SB_FEATURES + 8].copy_from_slice(&bits.to_le_bytes());
            })
        };
        cases.push(("volume is encrypted", flag(1, 10, 1)));
        cases.push(("volume has compression enabled", flag(1, 4, 3)));
        cases.push(("volume has background compression enabled", flag(2, 0, 3)));
        cases.push(("volume does not ask for exactly one replica of everything", flag(0, 53, 1)));
        cases.push(("volume has erasure coding enabled", flag(3, 0, 1)));
        cases.push(("volume is casefolded", flag(6, 22, 1)));
        cases.push(("volume is a multi-device filesystem", flag(3, 63, 1)));
        cases.push(("volume was written big-endian", flag(0, 62, 1)));
        cases.push(("volume carries lz4-compressed data", feature(FEATURE_LZ4)));
        cases.push(("volume carries gzip-compressed data", feature(FEATURE_GZIP)));
        cases.push(("volume carries zstd-compressed data", feature(FEATURE_ZSTD)));
        cases.push(("volume carries erasure-coded stripes", feature(FEATURE_EC)));
        cases.push(("volume carries casefolded directories", feature(FEATURE_CASEFOLDING)));
        cases.push(("volume sets a feature bit this reader has never read", feature(24)));
        cases.push((
            "volume is a newer metadata version than this reader implements",
            Box::new(|sb: &mut Vec<u8>| {
                sb[SB_VERSION..SB_VERSION + 2].copy_from_slice(&(VERSION_CURRENT + 1).to_le_bytes())
            }),
        ));
        cases.push((
            "volume has more than one member device",
            Box::new(|sb: &mut Vec<u8>| sb[SB_NR_DEVICES] = 2),
        ));
        cases.push((
            "volume's block size is not the 4096 bytes this crate reads",
            Box::new(|sb: &mut Vec<u8>| sb[SB_BLOCK_SIZE..SB_BLOCK_SIZE + 2].copy_from_slice(&1u16.to_le_bytes())),
        ));

        for (want, mutate) in cases {
            let mut sb = valid();
            mutate(&mut sb);
            reseal(&mut sb);
            assert_eq!(refusal(&sb), want, "the mutation for {want:?} was refused for another reason");
        }
    }

    /// A superblock's own bytes have to say where it is and hash to what it
    /// claims, or a copy of somebody else's disk mounts here.
    #[test]
    fn a_superblock_that_is_not_this_ones_is_refused() {
        let mut wrong_place = valid();
        wrong_place[SB_OFFSET..SB_OFFSET + 8].copy_from_slice(&9u64.to_le_bytes());
        reseal(&mut wrong_place);
        assert_eq!(refusal(&wrong_place), "superblock names a sector other than its own");

        let mut no_magic = valid();
        no_magic[SB_MAGIC] ^= 0xFF;
        reseal(&mut no_magic);
        assert_eq!(refusal(&no_magic), "no bcachefs superblock here");

        // Moved and *not* resealed: the checksum is what refuses it.
        let mut torn = valid();
        torn[SB_SEQ] ^= 0xFF;
        assert_eq!(refusal(&torn), "superblock checksum does not match its bytes");
    }

    /// A member that describes a device other than this one is where an
    /// unchecked bucket count becomes a read past the end of the disk.
    #[test]
    fn a_member_bigger_than_its_device_is_refused() {
        let sections = SB_FIELDS_START;
        let mut too_big = valid();
        too_big[sections + 32..sections + 40].copy_from_slice(&u64::MAX.to_le_bytes());
        reseal(&mut too_big);
        assert_eq!(refusal(&too_big), "member's bucket count times bucket size overflows");

        let mut over = valid();
        over[sections + 32..sections + 40].copy_from_slice(&(NBUCKETS * 64).to_le_bytes());
        reseal(&mut over);
        assert_eq!(refusal(&over), "member claims more sectors than the device has");

        let mut no_buckets = valid();
        no_buckets[sections + 42..sections + 44].copy_from_slice(&0u16.to_le_bytes());
        reseal(&mut no_buckets);
        assert_eq!(refusal(&no_buckets), "member's bucket size is zero");
    }

    /// A clean section shorter than its own fixed part reaches the read path
    /// through every mount, and the subtraction that finds its entries panics
    /// under the overflow checks the kernel and root are built with.
    #[test]
    fn a_clean_section_shorter_than_its_header_is_refused() {
        let at = SB_FIELDS_START + 48;
        for u64s in [1u32, 2] {
            let mut short = valid();
            short[at..at + 4].copy_from_slice(&u64s.to_le_bytes());
            reseal(&mut short);
            let sb = read(&device(&short)).expect("the superblock itself still parses");
            assert_eq!(
                sb.clean().err(),
                Some(UpstreamError::Refused("clean section is shorter than its own header")),
                "a clean section of {u64s} word(s) was not refused"
            );
        }
    }

    /// The two readers in this crate answer a bcachefs superblock differently:
    /// the interim ToyOS format's refuses the magic, and this one accepts it.
    #[test]
    fn the_interim_reader_rejects_bcachefs_s_magic() {
        let io = device(&valid());
        let refused = crate::Superblock::read(&io).expect_err("the interim reader must refuse this");
        assert!(
            matches!(refused, crate::FsError::BadMagic { expected, .. } if expected == *b"BCFS"),
            "the interim reader answered {refused:?} rather than refusing the magic"
        );
        // And the reverse: the upstream reader is what does accept it.
        assert!(read(&io).is_ok(), "the upstream reader must accept what it wrote the fixture for");
    }

    /// A volume that was not unmounted cleanly is refused by name, because
    /// replay is a write and this half of the crate does not do writes.
    #[test]
    fn a_volume_without_a_clean_section_is_refused_by_name() {
        let mut dirty = valid();
        // Turn the clean section into a type this reader steps over.
        let at = SB_FIELDS_START + 48;
        dirty[at + 4..at + 8].copy_from_slice(&99u32.to_le_bytes());
        reseal(&mut dirty);
        let sb = read(&device(&dirty)).expect("a superblock with no clean section still parses");
        assert_eq!(
            sb.clean().err(),
            Some(UpstreamError::Refused("volume has no clean section: it needs journal replay"))
        );
    }
}
