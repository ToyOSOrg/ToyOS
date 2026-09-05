//! Btree nodes: a node is a log of checksummed bsets, and reading one means
//! validating each in turn and merging what they say.

use alloc::vec;
use alloc::vec::Vec;

use crate::block_io::{BlockBuf, BlockIO, BlockNum, BLOCK_SIZE};

use super::bkey::{Bpos, BkeyFormat, Key, BKEY_BYTES, BPOS_BYTES, FORMAT_BYTES};
use super::csum::CsumType;
use super::raw::{bits, Raw};
use super::sb::{Superblock, BSET_MAGIC, SECTOR};
use super::UpstreamError;

/// `BTREE_MAX_DEPTH`, and therefore the deepest descent this reader will make.
pub const MAX_DEPTH: u8 = 4;

const NODE_MAGIC: usize = 16;
const NODE_FLAGS: usize = 24;
const NODE_MIN_KEY: usize = 32;
const NODE_MAX_KEY: usize = NODE_MIN_KEY + BPOS_BYTES;
/// After `max_key` comes the unused `_ptr`, then the format.
const NODE_FORMAT: usize = NODE_MAX_KEY + BPOS_BYTES + 8;
const NODE_BSET: usize = NODE_FORMAT + FORMAT_BYTES;
/// `offsetof(struct btree_node_entry, keys)`.
const ENTRY_BSET: usize = 16;

const BSET_SEQ: usize = 0;
const BSET_FLAGS: usize = 16;
const BSET_U64S: usize = 22;
const BSET_KEYS: usize = 24;

/// A pointer at a btree node: `bch_btree_ptr_v2`, the only kind a volume at
/// this metadata version writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtreePtr {
    pub seq: u64,
    /// Sectors of the node actually written, or zero meaning "the whole node".
    pub sectors_written: u16,
    pub min_key: Bpos,
    /// Where the node is, in 512-byte sectors from the start of the device.
    pub sector: u64,
    pub dev: u8,
}

impl BtreePtr {
    /// Decode the value of a `KEY_TYPE_btree_ptr_v2` key.
    pub fn read(val: &Raw<'_>) -> Result<Self, UpstreamError> {
        let seq = val.u64(8)?;
        let sectors_written = val.u16(16)?;
        let min_key = Bpos::read(val, 20)?;
        // The first entry is this reader's pointer; a replica list it does not
        // implement is refused where replicas are, in the superblock.
        let entry = val.u64(40)?;
        if entry & 1 == 0 {
            return Err(UpstreamError::Refused("btree pointer's first entry is not a device pointer"));
        }
        Ok(Self {
            seq,
            sectors_written,
            min_key,
            sector: bits(entry, 4, 48),
            dev: bits(entry, 48, 56) as u8,
        })
    }
}

/// One validated bset's keys, as a byte range inside the node.
struct Bset {
    at: usize,
    bytes: usize,
}

/// A btree node this reader has checked: every bset's checksum matched, and
/// every key in it fits the node.
pub struct Node {
    bytes: Vec<u8>,
    pub format: BkeyFormat,
    pub level: u8,
    pub btree_id: u32,
    pub min_key: Bpos,
    pub max_key: Bpos,
    bsets: Vec<Bset>,
}

impl Node {
    /// Read the node `ptr` names and validate it against what the caller
    /// expects to find there.
    ///
    /// `seq`, `btree_id` and `level` are checked against the pointer rather
    /// than trusted from the node, because a node the allocator has since
    /// reused is otherwise a well-formed node of the wrong tree.
    pub fn read(
        io: &dyn BlockIO,
        sb: &Superblock,
        ptr: &BtreePtr,
        btree_id: u32,
        level: u8,
    ) -> Result<Self, UpstreamError> {
        if ptr.dev != sb.dev_idx() {
            return Err(UpstreamError::Refused("btree pointer names a device this filesystem has not got"));
        }
        let node_bytes = sb.btree_node_size();
        let byte_off = ptr
            .sector
            .checked_mul(SECTOR as u64)
            .ok_or(UpstreamError::Refused("btree node offset in bytes overflows"))?;
        if byte_off % BLOCK_SIZE as u64 != 0 {
            return Err(UpstreamError::Refused("btree node is not block aligned"));
        }
        let first_block = byte_off / BLOCK_SIZE as u64;
        let blocks = node_bytes / BLOCK_SIZE as u64;
        if first_block.checked_add(blocks).is_none_or(|end| end > io.block_count()) {
            return Err(UpstreamError::Refused("btree node runs past the end of the device"));
        }

        let mut bytes = vec![0u8; node_bytes as usize];
        let mut buf = BlockBuf::zeroed();
        for i in 0..blocks as usize {
            let block = BlockNum::new(first_block + i as u64);
            io.read_block(block, &mut buf).map_err(|e| UpstreamError::Device(block, e))?;
            bytes[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE].copy_from_slice(buf.as_bytes());
        }

        Self::parse(bytes, sb, ptr, btree_id, level)
    }

    fn parse(
        bytes: Vec<u8>,
        sb: &Superblock,
        ptr: &BtreePtr,
        btree_id: u32,
        level: u8,
    ) -> Result<Self, UpstreamError> {
        let node = Raw::new(&bytes, "btree node ends early");
        let uuid_lo = u64::from_le_bytes(sb.uuid()[..8].try_into().expect("eight uuid bytes"));
        if node.u64(NODE_MAGIC)? != uuid_lo ^ BSET_MAGIC {
            return Err(UpstreamError::Refused("btree node's magic is not this filesystem's"));
        }
        let flags = node.u64(NODE_FLAGS)?;
        let node_id = bits(flags, 0, 4) | (bits(flags, 9, 25) << 4);
        if node_id != btree_id as u64 {
            return Err(UpstreamError::Refused("btree node belongs to a different btree"));
        }
        if bits(flags, 4, 8) != level as u64 {
            return Err(UpstreamError::Refused("btree node is at a different level than its pointer"));
        }
        let min_key = Bpos::read(&node, NODE_MIN_KEY)?;
        let max_key = Bpos::read(&node, NODE_MAX_KEY)?;
        if min_key > max_key {
            return Err(UpstreamError::Refused("btree node's key range runs backwards"));
        }
        let format = BkeyFormat::read(&node, NODE_FORMAT)?;

        let node_sectors = bytes.len() / SECTOR;
        let limit = match ptr.sectors_written {
            0 => node_sectors,
            n => (n as usize).min(node_sectors),
        };
        let first_seq = node.u64(NODE_BSET + BSET_SEQ)?;
        if first_seq != ptr.seq {
            return Err(UpstreamError::Refused("btree node's sequence number is not its pointer's"));
        }

        let mut bsets = Vec::new();
        let mut written = 0usize;
        while written < limit {
            let (head, bset_at, data_at) = if written == 0 {
                (0, NODE_BSET, NODE_BSET + BSET_KEYS)
            } else {
                let head = written * SECTOR;
                (head, head + ENTRY_BSET, head + ENTRY_BSET + BSET_KEYS)
            };
            let bset = node.sub(bset_at, BSET_KEYS, "btree node ends before a bset header")?;
            if written != 0 && bset.u64(BSET_SEQ)? != first_seq {
                break;
            }
            let bset_flags = bset.u32(BSET_FLAGS)? as u64;
            if bits(bset_flags, 4, 5) != 0 {
                return Err(UpstreamError::Refused("bset was written big-endian"));
            }
            if written != 0 && bits(bset_flags, 16, 32) != written as u64 {
                return Err(UpstreamError::Refused("bset does not sit where it says it does"));
            }
            let csum_type = CsumType::from_disk(bits(bset_flags, 0, 4))?;
            if csum_type == CsumType::None {
                return Err(UpstreamError::Refused("bset carries no checksum"));
            }

            let key_bytes = (bset.u16(BSET_U64S)? as usize)
                .checked_mul(8)
                .ok_or(UpstreamError::Refused("bset's key length overflows"))?;
            let end = data_at
                .checked_add(key_bytes)
                .ok_or(UpstreamError::Refused("bset ends past any address"))?;
            if end > bytes.len() {
                return Err(UpstreamError::Refused("bset runs past the end of its btree node"));
            }
            let stored = (node.u64(head)?, node.u64(head + 8)?);
            if !csum_type.verify(&bytes[head + 16..end], stored) {
                return Err(UpstreamError::Refused("bset checksum does not match its bytes"));
            }

            bsets.push(Bset { at: data_at, bytes: key_bytes });
            let sectors = (end - head).div_ceil(BLOCK_SIZE) * (BLOCK_SIZE / SECTOR);
            if sectors == 0 {
                return Err(UpstreamError::Refused("bset occupies no sectors"));
            }
            written += sectors;
        }

        if bsets.is_empty() {
            return Err(UpstreamError::Refused("btree node holds no bset at all"));
        }
        Ok(Self { bytes, format, level, btree_id, min_key, max_key, bsets })
    }

    /// Every key the node holds, newest bset last.
    ///
    /// The caller merges: a key in a later bset overrides one at the same
    /// position in an earlier one, which is what makes a node a log.
    pub fn keys(&self) -> Result<Vec<(usize, Key)>, UpstreamError> {
        let mut out = Vec::new();
        for (generation, bset) in self.bsets.iter().enumerate() {
            let mut at = bset.at;
            let end = bset.at + bset.bytes;
            while at < end {
                let window = Raw::new(&self.bytes[at..end], "key runs past the end of its bset");
                let key = Key::read(&window, &self.format)?;
                if key.u64s == 0 {
                    return Err(UpstreamError::Refused("bset holds a key of no length"));
                }
                out.push((generation, key.with_base(at)));
                at += key.u64s as usize * 8;
            }
        }
        Ok(out)
    }

    /// The bytes of a key's value, given the key [`Node::keys`] handed back.
    pub fn value(&self, key: &Key) -> Result<Raw<'_>, UpstreamError> {
        let at = key.base + key.val_at;
        let len = key.val_bytes();
        Raw::new(&self.bytes, "key's value runs past its node").sub(at, len, "key's value runs past its node")
    }
}

/// The unpacked-key length, for a caller sizing a value window.
pub const UNPACKED_KEY_BYTES: usize = BKEY_BYTES;

/// `BCH_JSET_ENTRY_btree_root`.
const JSET_ENTRY_BTREE_ROOT: u8 = 1;
const JSET_ENTRY_HEADER: usize = 8;

/// The btree roots a cleanly-unmounted filesystem is opened from.
///
/// A volume whose journal still holds updates has no business being read by
/// this half of the crate: replay is a write, so a dirty volume is refused in
/// [`Superblock::clean`] rather than replayed here.
pub fn clean_roots(sb: &Superblock) -> Result<Vec<(u32, u8, BtreePtr)>, UpstreamError> {
    if !sb.is_clean() {
        return Err(UpstreamError::Refused("volume was not unmounted cleanly: it needs journal replay"));
    }
    let entries = sb.clean()?;
    let format = BkeyFormat::unpacked();
    let mut roots = Vec::new();
    let mut at = 0usize;

    while at + JSET_ENTRY_HEADER <= entries.len() {
        let u64s = entries.u16(at)? as usize;
        let btree_id = entries.u8(at + 2)? as u32;
        let level = entries.u8(at + 3)?;
        let kind = entries.u8(at + 4)?;
        let bytes = JSET_ENTRY_HEADER
            .checked_add(u64s.checked_mul(8).ok_or(SHORT_ENTRY)?)
            .ok_or(SHORT_ENTRY)?;
        if at + bytes > entries.len() {
            return Err(SHORT_ENTRY);
        }
        if kind == JSET_ENTRY_BTREE_ROOT && u64s != 0 {
            if level >= MAX_DEPTH {
                return Err(UpstreamError::Refused("btree root is deeper than the format allows"));
            }
            let payload = entries.sub(at + JSET_ENTRY_HEADER, u64s * 8, "btree root ends early")?;
            let key = Key::read(&payload, &format)?;
            if key.kind == super::bkey::TYPE_BTREE_PTR_V2 {
                let val = payload.sub(key.val_at, key.val_bytes(), "btree root's value ends early")?;
                roots.push((btree_id, level, BtreePtr::read(&val)?));
            }
        }
        if bytes == 0 {
            return Err(SHORT_ENTRY);
        }
        at += bytes;
    }
    Ok(roots)
}

const SHORT_ENTRY: UpstreamError =
    UpstreamError::Refused("clean section's entry list ends inside an entry");

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::upstream::bkey::{BKEY_U64S, KEY_FORMAT_CURRENT, TYPE_DIRENT};
    use crate::upstream::sb::fixture;

    const NODE_SECTORS: usize = 512;
    const BTREE: u32 = 2;
    const SEQ: u64 = 0x0123_4567_89ab_cdef;

    fn superblock() -> Superblock {
        super::super::sb::read(&fixture::device(&fixture::valid())).expect("the crafted superblock")
    }

    /// One unpacked key with an eight-byte value, as a bset holds it.
    fn key(offset: u64) -> Vec<u8> {
        let mut out = vec![0u8; BKEY_U64S * 8 + 8];
        out[0] = BKEY_U64S as u8 + 1;
        out[1] = KEY_FORMAT_CURRENT;
        out[2] = TYPE_DIRENT;
        out[20..24].copy_from_slice(&0u32.to_le_bytes());
        out[24..32].copy_from_slice(&offset.to_le_bytes());
        out[32..40].copy_from_slice(&4096u64.to_le_bytes());
        out
    }

    /// A node with one bset holding `offsets.len()` keys, checksummed as
    /// upstream checksums the first bset of a node.
    fn node(sb: &Superblock, offsets: &[u64]) -> Vec<u8> {
        let mut bytes = vec![0u8; NODE_SECTORS * SECTOR];
        let uuid_lo = u64::from_le_bytes(sb.uuid()[..8].try_into().unwrap());
        bytes[NODE_MAGIC..NODE_MAGIC + 8].copy_from_slice(&(uuid_lo ^ BSET_MAGIC).to_le_bytes());
        let flags = ((BTREE as u64) & 0xF) | (((BTREE as u64) >> 4) << 9);
        bytes[NODE_FLAGS..NODE_FLAGS + 8].copy_from_slice(&flags.to_le_bytes());
        // max_key is SPOS_MAX, so the node's range holds every key below.
        bytes[NODE_MAX_KEY..NODE_MAX_KEY + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[NODE_MAX_KEY + 4..NODE_MAX_KEY + 12].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[NODE_MAX_KEY + 12..NODE_MAX_KEY + 20].copy_from_slice(&u64::MAX.to_le_bytes());
        // The identity key format, so a key can be written unpacked.
        bytes[NODE_FORMAT] = BKEY_U64S as u8;
        bytes[NODE_FORMAT + 1] = 6;
        bytes[NODE_FORMAT + 2..NODE_FORMAT + 8].copy_from_slice(&[64, 64, 32, 32, 32, 64]);

        let mut keys = Vec::new();
        for offset in offsets {
            keys.extend_from_slice(&key(*offset));
        }
        bytes[NODE_BSET + BSET_SEQ..NODE_BSET + BSET_SEQ + 8].copy_from_slice(&SEQ.to_le_bytes());
        // Checksum type crc32c, which is what `bcachefs format` writes.
        bytes[NODE_BSET + BSET_FLAGS..NODE_BSET + BSET_FLAGS + 4].copy_from_slice(&5u32.to_le_bytes());
        let u64s = (keys.len() / 8) as u16;
        bytes[NODE_BSET + BSET_U64S..NODE_BSET + BSET_U64S + 2].copy_from_slice(&u64s.to_le_bytes());
        let at = NODE_BSET + BSET_KEYS;
        bytes[at..at + keys.len()].copy_from_slice(&keys);
        reseal(&mut bytes);
        bytes
    }

    fn reseal(bytes: &mut [u8]) {
        let u64s = u16::from_le_bytes(
            bytes[NODE_BSET + BSET_U64S..NODE_BSET + BSET_U64S + 2].try_into().unwrap(),
        ) as usize;
        let end = (NODE_BSET + BSET_KEYS + u64s * 8).min(bytes.len());
        let (lo, hi) = CsumType::Crc32c.digest(&bytes[16..end]);
        bytes[0..8].copy_from_slice(&lo.to_le_bytes());
        bytes[8..16].copy_from_slice(&hi.to_le_bytes());
    }

    fn ptr() -> BtreePtr {
        BtreePtr {
            seq: SEQ,
            sectors_written: NODE_SECTORS as u16,
            min_key: Bpos::MIN,
            sector: 0,
            dev: 0,
        }
    }

    fn parse(sb: &Superblock, bytes: Vec<u8>) -> Result<Node, UpstreamError> {
        Node::parse(bytes, sb, &ptr(), BTREE, 0)
    }

    fn refusal(sb: &Superblock, bytes: Vec<u8>) -> &'static str {
        match parse(sb, bytes) {
            Err(UpstreamError::Refused(why)) => why,
            Err(other) => panic!("expected a refusal, got {other:?}"),
            Ok(_) => panic!("this btree node should not have been accepted"),
        }
    }

    /// The builder makes a node this reader accepts, which is what makes each
    /// mutation below a measurement of the field it moved.
    #[test]
    fn the_crafted_node_is_accepted() {
        let sb = superblock();
        let node = parse(&sb, node(&sb, &[8, 16, 24])).expect("the crafted node");
        let keys = node.keys().expect("its keys");
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].1.pos, Bpos::new(4096, 8, 0));
        assert_eq!(keys[2].1.pos, Bpos::new(4096, 24, 0));
    }

    /// **`u64s` is the defect class again**: a bset's key length is a number
    /// off the disk that decides how far the key walk goes.
    #[test]
    fn a_bset_longer_than_its_node_is_refused() {
        let sb = superblock();
        let mut bytes = node(&sb, &[8]);
        bytes[NODE_BSET + BSET_U64S..NODE_BSET + BSET_U64S + 2]
            .copy_from_slice(&u16::MAX.to_le_bytes());
        reseal(&mut bytes);
        assert_eq!(refusal(&sb, bytes), "bset runs past the end of its btree node");
    }

    /// A node that is well formed but belongs to another tree, another level
    /// or another generation is a node the allocator has since reused.
    #[test]
    fn a_node_that_is_not_the_one_asked_for_is_refused() {
        let sb = superblock();

        let mut wrong_magic = node(&sb, &[8]);
        wrong_magic[NODE_MAGIC] ^= 0xFF;
        reseal(&mut wrong_magic);
        assert_eq!(refusal(&sb, wrong_magic), "btree node's magic is not this filesystem's");

        let mut wrong_btree = node(&sb, &[8]);
        wrong_btree[NODE_FLAGS] = 7;
        reseal(&mut wrong_btree);
        assert_eq!(refusal(&sb, wrong_btree), "btree node belongs to a different btree");

        let mut wrong_level = node(&sb, &[8]);
        wrong_level[NODE_FLAGS] |= 1 << 4;
        reseal(&mut wrong_level);
        assert_eq!(refusal(&sb, wrong_level), "btree node is at a different level than its pointer");

        let mut wrong_seq = node(&sb, &[8]);
        wrong_seq[NODE_BSET + BSET_SEQ] ^= 0xFF;
        reseal(&mut wrong_seq);
        assert_eq!(refusal(&sb, wrong_seq), "btree node's sequence number is not its pointer's");
    }

    /// A bset whose bytes were changed after it was written is refused, and a
    /// bset that declares no checksum at all is refused rather than trusted.
    #[test]
    fn a_bset_this_filesystem_did_not_write_is_refused() {
        let sb = superblock();

        // Moved and not resealed: the checksum is the only thing that objects.
        let mut torn = node(&sb, &[8, 16]);
        torn[NODE_BSET + BSET_KEYS + 24] ^= 0xFF;
        assert_eq!(refusal(&sb, torn), "bset checksum does not match its bytes");

        let mut unchecked = node(&sb, &[8]);
        unchecked[NODE_BSET + BSET_FLAGS] = 0;
        reseal(&mut unchecked);
        assert_eq!(refusal(&sb, unchecked), "bset carries no checksum");

        let mut big_endian = node(&sb, &[8]);
        big_endian[NODE_BSET + BSET_FLAGS] = 5 | (1 << 4);
        reseal(&mut big_endian);
        assert_eq!(refusal(&sb, big_endian), "bset was written big-endian");
    }

    /// A key whose length runs past the bset it is in is refused rather than
    /// read out of whatever follows it.
    #[test]
    fn a_key_longer_than_its_bset_is_refused() {
        let sb = superblock();
        // The last key of two, one word longer than the bset has left.
        let mut ragged = node(&sb, &[8, 16]);
        ragged[NODE_BSET + BSET_KEYS + BKEY_U64S * 8 + 8] += 1;
        reseal(&mut ragged);
        let parsed = parse(&sb, ragged).expect("the node still parses; its keys do not");
        assert_eq!(
            parsed.keys().err(),
            Some(UpstreamError::Refused("key runs past the end of its bset"))
        );

        let mut zero_length = node(&sb, &[8, 16]);
        zero_length[NODE_BSET + BSET_KEYS] = 0;
        reseal(&mut zero_length);
        let parsed = parse(&sb, zero_length).expect("the node still parses");
        assert_eq!(
            parsed.keys().err(),
            Some(UpstreamError::Refused("key is shorter than the format it names"))
        );
    }
}
