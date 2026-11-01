//! A btree node: a first record with the node header and a bset, then
//! later records each holding a bset, every one checksummed or MACed.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::bkey::{Bpos, Format, Key};
use super::crypt::{MasterKey, Nonce};
use super::superblock::Superblock;
use super::value::{csum_type as csum, NodePtr};
use super::{field, kind, le16, le32, le64, put16, put32, put64, Btree, Error, Result, SECTOR, VERSION};
use crate::crc32c::crc32c;

/// The node magic's XOR with the internal UUID's first eight bytes.
const NODE_MAGIC: u64 = 0x9013_5c78_b99e_07f5;
/// Bytes before the first bset's keys: header, format and bset header.
pub(crate) const FIRST_KEYS: usize = 160;
/// Bytes before a later bset's keys: checksum and bset header.
pub(crate) const LATER_KEYS: usize = 40;
/// Node flags bit 8, set on every node.
const NEW_EXTENT_OVERWRITE: u64 = 1 << 8;

/// Every node's magic on the volume of `internal_uuid`.
pub(crate) fn magic(internal_uuid: &[u8; 16]) -> u64 {
    u64::from_le_bytes(internal_uuid[..8].try_into().unwrap()) ^ NODE_MAGIC
}

/// What a node is expected to be: where its parent pointer says it is, and
/// what it says it holds.
pub(crate) struct Expect {
    pub btree: Btree,
    pub level: u8,
    pub seq: u64,
    pub min: Bpos,
    pub max: Bpos,
}

/// A node, its bsets merged.
pub(crate) struct Node {
    /// Live keys in position order, each with its value.
    pub keys: Vec<(Key, Vec<u8>)>,
    /// Each bset's journal sequence, in the order written.
    pub bset_seqs: Vec<u64>,
}

/// One record's place in the node and its parts.
struct Record {
    /// Byte offset in the node.
    at: usize,
    /// Byte offset of the bset header.
    bset: usize,
    /// Byte offset of the keys.
    keys: usize,
    /// Byte offset past the keys.
    end: usize,
}

/// The node `bytes`, its parent pointer's written sectors of it, checked
/// against `expect`; `key` for an encrypted volume.
pub(crate) fn decode(sb: &Superblock, key: Option<&MasterKey>, expect: &Expect, mut bytes: Vec<u8>) -> Result<Node> {
    let block = sb.block_bytes();
    if bytes.len() < FIRST_KEYS || le64(&bytes, 16) != magic(&sb.header.internal_uuid) {
        return Err(Error::Corrupt("btree node without its magic"));
    }
    let mut bsets: Vec<Vec<(Key, Vec<u8>)>> = Vec::new();
    let mut bset_seqs = Vec::new();
    let mut format = Format::UNPACKED;
    let mut at = 0;
    while at < bytes.len() {
        let first = at == 0;
        let bset = if first { 136 } else { at + 16 };
        if bset + 24 > bytes.len() {
            return Err(Error::Corrupt("bset header past the node's written sectors"));
        }
        // Every record up to the pointer's written sectors is the node's: a
        // record of another node there is one cut short.
        if le64(&bytes, bset) != expect.seq {
            return Err(Error::Corrupt(if first { "btree node of another sequence than its pointer's" } else { "btree node shorter than its pointer's written sectors" }));
        }
        let keys = if first { FIRST_KEYS } else { at + LATER_KEYS };
        let end = keys + 8 * usize::from(le16(&bytes, bset + 22));
        let next = end.next_multiple_of(block);
        if next > bytes.len() {
            return Err(Error::Corrupt("bset past the node's written sectors"));
        }
        let record = Record { at, bset, keys, end };
        let journal_seq = le64(&bytes, bset + 8);
        let flags = le32(&bytes, bset + 16);
        check_bset(sb, flags, le16(&bytes, bset + 20), at)?;
        let csum_type = field(u64::from(flags), 0, 4) as u8;
        // An encrypted volume MACs all its metadata.
        if sb.encrypted() && csum_type != csum::CHACHA_POLY_128 {
            return Err(Error::Corrupt("unauthenticated btree node on an encrypted volume"));
        }
        open(key, expect.seq, journal_seq, csum_type, &record, &mut bytes)?;
        if first {
            format = parse_header(&bytes, expect)?;
        } else if end == keys {
            return Err(Error::Corrupt("later bset with no keys"));
        }
        if sb.blacklisted(journal_seq) {
            if first {
                return Err(Error::Corrupt("first bset's journal sequence blacklisted"));
            }
        } else {
            bsets.push(parse_keys(&format, &bytes[keys..end], expect)?);
            bset_seqs.push(journal_seq);
        }
        at = next;
    }
    Ok(Node { keys: merge(bsets), bset_seqs })
}

fn check_bset(sb: &Superblock, flags: u32, version: u16, at: usize) -> Result<()> {
    if flags & 1 << 4 != 0 {
        return Err(Error::Unsupported("big-endian bset"));
    }
    if flags & 1 << 5 != 0 {
        return Err(Error::Corrupt("bset with separate whiteouts"));
    }
    let offset = field(u64::from(flags), 16, 16) as usize;
    if offset != 0 && offset * SECTOR != at {
        return Err(Error::Corrupt("bset at another sector than it records"));
    }
    if version >> 10 != 1 || version > VERSION || version < sb.header.version_min {
        return Err(Error::Unsupported("bset version outside the superblock's"));
    }
    Ok(())
}

/// Verifies a record's checksum or MAC, then decrypts it in place.
fn open(key: Option<&MasterKey>, seq: u64, journal_seq: u64, csum_type: u8, r: &Record, bytes: &mut [u8]) -> Result<()> {
    match csum_type {
        csum::NONE => Ok(()),
        csum::CRC32C_NONZERO if crc32c(&bytes[r.at + 16..r.end]) == le32(bytes, r.at) => Ok(()),
        csum::CRC32C_NONZERO => Err(Error::Checksum("btree node")),
        csum::CHACHA_POLY_128 => {
            let key = key.ok_or(Error::Corrupt("MACed btree node on an unencrypted volume"))?;
            let nonce = Nonce::node(r.at as u32, seq, journal_seq);
            let tag: [u8; 16] = bytes[r.at..r.at + 16].try_into().unwrap();
            key.verify(nonce, &bytes[r.at + 16..r.end], &tag, "btree node")?;
            if r.at == 0 {
                // Node flags through the format from block 0, the
                // keys from block 2.
                key.xor(nonce, &mut bytes[24..r.bset])?;
                key.xor(nonce.plus(2), &mut bytes[r.keys..r.end])
            } else {
                key.xor(nonce, &mut bytes[r.keys..r.end])
            }
        }
        _ => Err(Error::Unsupported("btree node checksum type")),
    }
}

/// The first record's node header, against what its pointer expects.
fn parse_header(b: &[u8], expect: &Expect) -> Result<Format> {
    let flags = le64(b, 24);
    let btree = field(flags, 0, 4) | field(flags, 9, 16) << 4;
    if btree != expect.btree as u64 || field(flags, 4, 4) != u64::from(expect.level) {
        return Err(Error::Corrupt("btree node of another btree or level than its pointer's"));
    }
    if expect.btree.extents() && flags & NEW_EXTENT_OVERWRITE == 0 {
        return Err(Error::Unsupported("extents node without new_extent_overwrite"));
    }
    if Bpos::decode(&b[32..52]) != expect.min || Bpos::decode(&b[52..72]) != expect.max {
        return Err(Error::Corrupt("btree node range disagrees with its pointer"));
    }
    Format::decode(&b[80..136])
}

/// A bset's keys: sorted, inside the node's range.
fn parse_keys(format: &Format, mut b: &[u8], expect: &Expect) -> Result<Vec<(Key, Vec<u8>)>> {
    let mut keys: Vec<(Key, Vec<u8>)> = Vec::new();
    while !b.is_empty() {
        let len = usize::from(b[0]) * 8;
        if len == 0 {
            return Err(Error::Corrupt("key of no size"));
        }
        let (key, value) = format.key(b)?;
        if key.pos < expect.min || key.pos > expect.max {
            return Err(Error::Corrupt("key outside its node's range"));
        }
        check_key(expect.btree, expect.level, &key, value)?;
        if let Some((last, _)) = keys.last() {
            let ordered = last.pos < key.pos || (last.pos == key.pos && last.kind == kind::DELETED && key.kind != kind::DELETED);
            if !ordered {
                return Err(Error::Corrupt("keys out of order"));
            }
        }
        keys.push((key, value.to_vec()));
        b = &b[len..];
    }
    Ok(keys)
}

/// A key against its node: in an interior node only pointers, of size 0,
/// each decoding; in a leaf a type its btree holds, which a read checks
/// only for the strict types, and an accounting key of one to three
/// counters.
pub(crate) fn check_key(btree: Btree, level: u8, key: &Key, value: &[u8]) -> Result<()> {
    if key.kind >= kind::UNKNOWN {
        return Err(Error::Unsupported("key type v1.39.4 does not define"));
    }
    if level > 0 && (!btree.allows(level, key.kind) || key.size != 0) {
        return Err(Error::Corrupt("interior key that is no size-0 pointer"));
    }
    if level > 0 && key.kind == kind::BTREE_PTR {
        return Err(Error::Unsupported("btree_ptr before btree_ptr_v2"));
    }
    if level > 0 && key.kind == kind::BTREE_PTR_V2 {
        NodePtr::decode(value)?;
    }
    if level == 0 && kind::strict(key.kind) && !btree.allows(0, key.kind) {
        return Err(Error::Corrupt("key of a type its btree does not hold"));
    }
    if key.kind == kind::ACCOUNTING {
        super::journal::counters(value)?;
    }
    Ok(())
}

/// Bsets merged by position, a later one winning, deleted keys dropped.
fn merge(bsets: Vec<Vec<(Key, Vec<u8>)>>) -> Vec<(Key, Vec<u8>)> {
    let mut merged = BTreeMap::new();
    for bset in bsets {
        for (key, value) in bset {
            merged.insert(key.pos, (key, value));
        }
    }
    merged.into_values().filter(|(k, _)| k.kind != kind::DELETED).collect()
}

/// A node's first and only record, written fresh.
pub(crate) struct Build<'a> {
    pub btree: Btree,
    pub level: u8,
    pub seq: u64,
    pub min: Bpos,
    pub max: Bpos,
    /// The bset's journal sequence.
    pub journal_seq: u64,
    /// Encoded unpacked keys, in position order.
    pub keys: &'a [Vec<u8>],
}

/// The first record's bytes: [`FIRST_KEYS`] plus the keys.
pub(crate) fn record_bytes(keys: impl Iterator<Item = usize>) -> usize {
    FIRST_KEYS + keys.sum::<usize>()
}

/// `build`'s record, padded to whole blocks, checksummed for an
/// unencrypted volume or encrypted and MACed under `key`.
pub(crate) fn encode(sb_magic: u64, block: usize, key: Option<&MasterKey>, build: &Build) -> Result<Vec<u8>> {
    let end = record_bytes(build.keys.iter().map(Vec::len));
    let mut b = alloc::vec![0u8; end.next_multiple_of(block)];
    put64(&mut b, 16, sb_magic);
    let id = build.btree as u64;
    put64(&mut b, 24, id & 0xf | u64::from(build.level) << 4 | NEW_EXTENT_OVERWRITE | (id >> 4) << 9);
    build.min.encode(&mut b[32..52]);
    build.max.encode(&mut b[52..72]);
    Format::UNPACKED.encode(&mut b[80..136]);
    put64(&mut b, 136, build.seq);
    put64(&mut b, 144, build.journal_seq);
    let csum_type = if key.is_some() { csum::CHACHA_POLY_128 } else { csum::CRC32C_NONZERO };
    put32(&mut b, 152, u32::from(csum_type));
    put16(&mut b, 156, VERSION);
    put16(&mut b, 158, ((end - FIRST_KEYS) / 8) as u16);
    let mut at = FIRST_KEYS;
    for k in build.keys {
        assert!(build.btree.allows(build.level, k[2]), "a key of type {} in a {:?} node at level {}", k[2], build.btree, build.level);
        assert!(build.level == 0 || le32(k, 16) == 0, "an interior key with a size");
        b[at..at + k.len()].copy_from_slice(k);
        at += k.len();
    }
    match key {
        None => {
            let crc = crc32c(&b[16..end]);
            put32(&mut b, 0, crc);
        }
        Some(key) => {
            let nonce = Nonce::node(0, build.seq, build.journal_seq);
            #[cfg(test)]
            super::crypt::oracle::record(key, nonce, &[&b[24..136], &b[FIRST_KEYS..end]].concat());
            key.xor(nonce, &mut b[24..136])?;
            key.xor(nonce.plus(2), &mut b[FIRST_KEYS..end])?;
            let tag = key.mac(nonce, &b[16..end])?;
            b[..16].copy_from_slice(&tag);
        }
    }
    Ok(b)
}

/// A later record's bytes: [`LATER_KEYS`] plus the keys.
pub(crate) fn later_record_bytes(keys: impl Iterator<Item = usize>) -> usize {
    LATER_KEYS + keys.sum::<usize>()
}

#[derive(Clone, Copy)]
/// A record appended to a node: its btree and level, the node's sequence,
/// the byte offset it goes to, its bset's journal sequence, and its keys,
/// encoded, in position order, a `deleted` key where one is removed.
pub(crate) struct Later<'a> {
    pub btree: Btree,
    pub level: u8,
    pub seq: u64,
    pub offset: usize,
    pub journal_seq: u64,
    pub keys: &'a [Vec<u8>],
}

/// `later`'s record, padded to whole blocks, checksummed or encrypted and
/// MACed as the first record is.
pub(crate) fn encode_later(block: usize, key: Option<&MasterKey>, later: &Later) -> Result<Vec<u8>> {
    let Later { btree, level, seq, offset, journal_seq, keys } = *later;
    assert!(offset > 0 && offset.is_multiple_of(block), "a later bset at byte {offset}");
    let end = later_record_bytes(keys.iter().map(Vec::len));
    let mut b = alloc::vec![0u8; end.next_multiple_of(block)];
    put64(&mut b, 16, seq);
    put64(&mut b, 24, journal_seq);
    let csum_type = if key.is_some() { csum::CHACHA_POLY_128 } else { csum::CRC32C_NONZERO };
    put32(&mut b, 32, u32::from(csum_type) | ((offset / SECTOR) as u32) << 16);
    put16(&mut b, 36, VERSION);
    put16(&mut b, 38, ((end - LATER_KEYS) / 8) as u16);
    let mut at = LATER_KEYS;
    for k in keys {
        assert!(k[2] == kind::DELETED || btree.allows(level, k[2]), "a key of type {} in a {btree:?} node at level {level}", k[2]);
        b[at..at + k.len()].copy_from_slice(k);
        at += k.len();
    }
    match key {
        None => {
            let crc = crc32c(&b[16..end]);
            put32(&mut b, 0, crc);
        }
        Some(key) => {
            let nonce = Nonce::node(offset as u32, seq, journal_seq);
            #[cfg(test)]
            super::crypt::oracle::record(key, nonce, &b[LATER_KEYS..end]);
            key.xor(nonce, &mut b[LATER_KEYS..end])?;
            let tag = key.mac(nonce, &b[16..end])?;
            b[..16].copy_from_slice(&tag);
        }
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_whose_value_the_mount_would_decode_is_checked_at_its_boundary() {
        let key = |k| Key::new(k, Bpos::MAX);
        assert!(check_key(Btree::Accounting, 0, &key(kind::ACCOUNTING), &[0; 8]).is_ok());
        assert!(matches!(check_key(Btree::Accounting, 0, &key(kind::ACCOUNTING), &[0; 4]), Err(Error::Corrupt(_))));
        assert!(matches!(check_key(Btree::Accounting, 0, &key(kind::ACCOUNTING), &[0; 32]), Err(Error::Corrupt(_))));
        assert!(matches!(check_key(Btree::Extents, 1, &key(kind::BTREE_PTR_V2), &[0; 40]), Err(Error::Corrupt(_))));
        assert!(matches!(check_key(Btree::Extents, 1, &key(kind::BTREE_PTR), &[]), Err(Error::Unsupported(_))));
    }
}
