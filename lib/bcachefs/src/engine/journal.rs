//! The journal: entries (`jset`s) back to back in a ring of buckets, each a
//! run of records, checksummed, or encrypted and MACed; and replay, which
//! takes the window the newest flush entry names and lays its records over
//! the btrees.
//!
//! **Every entry is read whole before it is trusted.** [`scan`] keeps an
//! entry's records only when its checksum or MAC holds, and
//! [`Scan::window`] refuses a window holding any other.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use toyos_blockdev::BlockDev;

use super::bkey::{Bpos, Format, Key, Version, KEY_U64S};
use super::crypt::{MasterKey, Nonce};
use super::node::check_kind;
use super::read::read_bytes;
use super::superblock::{entries, entry_type, Entry, Root, Superblock, KEY_VERSION};
use super::value::csum_type as csum;
use super::{kind, le32, le64, Btree, Error, Result, SECTOR, VERSION};
use crate::crc32c::crc32c;

/// Bytes of an entry's header, before its records.
pub(crate) const HEADER: usize = 56;
/// The journal magic's XOR with the internal UUID's first eight bytes.
const MAGIC: u64 = 0x2452_35c1_a362_5032;
/// Flags bit 5: written without a flush, so it names no last sequence.
const NO_FLUSH: u32 = 1 << 5;
/// The first byte an encrypted entry encrypts.
const ENCRYPTED_FROM: usize = 44;

fn magic(internal_uuid: &[u8; 16]) -> u64 {
    u64::from_le_bytes(internal_uuid[..8].try_into().unwrap()) ^ MAGIC
}

/// One entry as written: its sequence, the oldest sequence it keeps in the
/// replay window, and its records.
#[cfg(test)]
pub(crate) struct Jset<'a> {
    pub seq: u64,
    pub last_seq: u64,
    /// The records, back to back, as [`record`] lays them out.
    pub area: &'a [u8],
}

/// Appends a record to an entry area.
#[cfg(test)]
pub(crate) fn record(area: &mut Vec<u8>, kind_: u8, btree: u8, level: u8, payload: &[u8]) {
    assert!(payload.len().is_multiple_of(8) && u16::try_from(payload.len() / 8).is_ok(), "a record of {} bytes", payload.len());
    let mut header = [0u8; 8];
    header[..2].copy_from_slice(&((payload.len() / 8) as u16).to_le_bytes());
    header[2] = btree;
    header[3] = level;
    header[4] = kind_;
    area.extend_from_slice(&header);
    area.extend_from_slice(payload);
}

/// The bytes of `entry`, padded to whole blocks, checksummed for an
/// unencrypted volume or encrypted and MACed under `key`. A flush entry,
/// always: v1 writes no other.
#[cfg(test)]
pub(crate) fn encode(internal_uuid: &[u8; 16], block: usize, key: Option<&MasterKey>, entry: &Jset) -> Result<Vec<u8>> {
    use super::{put32, put64};
    let end = HEADER + entry.area.len();
    let mut b = alloc::vec![0u8; end.next_multiple_of(block)];
    put64(&mut b, 16, magic(internal_uuid));
    put64(&mut b, 24, entry.seq);
    put32(&mut b, 32, u32::from(VERSION));
    let csum_type = if key.is_some() { csum::CHACHA_POLY_128 } else { csum::CRC32C_NONZERO };
    put32(&mut b, 36, u32::from(csum_type));
    put32(&mut b, 40, (entry.area.len() / 8) as u32);
    put64(&mut b, 48, entry.last_seq);
    b[HEADER..end].copy_from_slice(entry.area);
    match key {
        None => {
            let crc = crc32c(&b[16..end]);
            put32(&mut b, 0, crc);
        }
        Some(key) => {
            let nonce = Nonce::journal(entry.seq);
            key.xor(nonce, &mut b[ENCRYPTED_FROM..end])?;
            let tag = key.mac(nonce, &b[16..end])?;
            b[..16].copy_from_slice(&tag);
        }
    }
    Ok(b)
}

/// An entry found in the ring.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub seq: u64,
    /// Zero for an entry written without a flush.
    pub last_seq: u64,
    pub flush: bool,
    /// Whether its checksum or MAC holds.
    pub good: bool,
    /// Its bucket, as an index into the ring, and its first sector there.
    pub bucket: usize,
    pub offset: u64,
    /// Sectors it occupies, its last block's padding included.
    pub sectors: u64,
    /// Its records, decrypted; empty when it is not `good`.
    pub area: Vec<u8>,
}

/// Every entry the ring holds, by sequence.
pub(crate) struct Scan {
    pub entries: BTreeMap<u64, Found>,
}

/// Reads every journal bucket in ring order (A2.9). In each bucket entries
/// are read from its start, one after the other, until a position holds no
/// entry or an entry older than the one before it: what follows is left over
/// from an earlier pass of the ring. Past an entry whose checksum fails, a
/// position without one is passed a block at a time, since a torn entry's
/// length may not be its own.
pub(crate) async fn scan(dev: &impl BlockDev, sb: &Superblock, key: Option<&MasterKey>) -> Result<Scan> {
    let bucket_sectors = u64::from(sb.member.bucket_sectors);
    let bucket_bytes = bucket_sectors as usize * SECTOR;
    let block = sb.block_bytes();
    let want = magic(&sb.header.internal_uuid);
    let mut entries: BTreeMap<u64, Found> = BTreeMap::new();
    let ring = sb.journal.iter().flat_map(|&(first, count)| first..first + count);
    for (index, bucket) in ring.enumerate() {
        let bytes = read_bytes(dev, bucket * bucket_sectors * SECTOR as u64, bucket_bytes).await?;
        let mut at = 0;
        let mut previous = None;
        let mut torn = false;
        while at + HEADER <= bytes.len() {
            if le64(&bytes, at + 16) != want {
                if !torn {
                    break;
                }
                at += block;
                continue;
            }
            let found = entry(sb, key, &bytes[at..], index, at)?;
            if previous.is_some_and(|p| found.seq < p) {
                break;
            }
            previous = Some(found.seq);
            torn |= !found.good;
            at += found.sectors as usize * SECTOR;
            match entries.get(&found.seq) {
                Some(had) if had.good && found.good && had.area != found.area => {
                    return Err(Error::Corrupt("two journal entries of one sequence"));
                }
                Some(had) if had.good || !found.good => {}
                _ => {
                    entries.insert(found.seq, found);
                }
            }
        }
    }
    Ok(Scan { entries })
}

/// The entry at the start of `b`, whose magic is the volume's, found at byte
/// `at` of ring bucket `bucket`.
fn entry(sb: &Superblock, key: Option<&MasterKey>, b: &[u8], bucket: usize, at: usize) -> Result<Found> {
    let seq = le64(b, 24);
    let version = le32(b, 32);
    if version >> 10 != 1 || version > u32::from(VERSION) {
        return Err(Error::Unsupported("journal entry version other than 1.x up to 1.39"));
    }
    let flags = le32(b, 36);
    let flush = flags & NO_FLUSH == 0;
    let block = sb.block_bytes();
    let end = (le32(b, 40) as usize).checked_mul(8).and_then(|n| n.checked_add(HEADER));
    let offset = (at / SECTOR) as u64;
    // An entry that runs past its bucket is cut there and fails its checksum.
    let Some(end) = end.filter(|&end| end <= b.len()) else {
        let sectors = (b.len() / SECTOR) as u64;
        return Ok(Found { seq, last_seq: 0, flush, good: false, bucket, offset, sectors, area: Vec::new() });
    };
    let sectors = (end.next_multiple_of(block) / SECTOR) as u64;
    let mut bytes = b[..end].to_vec();
    let good = match (flags & 0xf) as u8 {
        csum::CRC32C_NONZERO if key.is_none() => crc32c(&bytes[16..]) == le32(&bytes, 0),
        csum::CHACHA_POLY_128 => {
            let key = key.ok_or(Error::Corrupt("MACed journal entry on an unencrypted volume"))?;
            let nonce = Nonce::journal(seq);
            let tag: [u8; 16] = bytes[..16].try_into().unwrap();
            let good = key.verify(nonce, &bytes[16..], &tag, "journal entry").is_ok();
            key.xor(nonce, &mut bytes[ENCRYPTED_FROM..])?;
            good
        }
        csum::CRC32C_NONZERO => return Err(Error::Corrupt("unauthenticated journal entry on an encrypted volume")),
        _ => return Err(Error::Unsupported("journal entry checksum type")),
    };
    let last_seq = if flush { le64(&bytes, 48) } else { 0 };
    let area = if good { bytes.split_off(HEADER) } else { Vec::new() };
    Ok(Found { seq, last_seq, flush, good, bucket, offset, sectors, area })
}

/// The entries replay takes: from the newest flush entry `F`'s last
/// sequence through `F`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    /// `F`'s last sequence.
    pub first: u64,
    /// `F`'s sequence.
    pub last: u64,
    /// One past the newest sequence found, of any entry.
    pub next: u64,
}

impl Scan {
    /// The replay window (A2.13). `F` is the newest flush entry, past at
    /// most one newer flush entry whose checksum fails: the write a crash
    /// tore. Every sequence of the window that is not blacklisted must be
    /// there whole. `None` when the ring holds no entry.
    pub(crate) fn window(&self, sb: &Superblock) -> Result<Option<Window>> {
        let Some(&newest) = self.entries.keys().next_back() else { return Ok(None) };
        let mut flushes = self.entries.values().rev().filter(|e| e.flush);
        let f = match flushes.next() {
            Some(e) if !e.good => flushes.next(),
            e => e,
        };
        let f = f.ok_or(Error::Corrupt("a journal with no flush entry"))?;
        if !f.good {
            return Err(Error::Checksum("journal entry"));
        }
        if f.last_seq > f.seq {
            return Err(Error::Corrupt("journal entry whose last sequence is past its own"));
        }
        for seq in f.last_seq..=f.seq {
            match self.entries.get(&seq) {
                _ if sb.blacklisted(seq) => {}
                Some(e) if e.good => {}
                Some(_) => return Err(Error::Checksum("journal entry")),
                None => return Err(Error::Corrupt("a journal entry missing from the replay window")),
            }
        }
        Ok(Some(Window { first: f.last_seq, last: f.seq, next: newest + 1 }))
    }

    /// The window's entries in sequence order, blacklisted ones left out.
    pub(crate) fn in_window<'a>(&'a self, sb: &'a Superblock, w: &Window) -> impl Iterator<Item = &'a Found> + 'a {
        self.entries.range(w.first..=w.last).map(|(_, e)| e).filter(|e| !sb.blacklisted(e.seq))
    }
}

/// A record of an entry, decoded.
pub(crate) enum Record {
    Keys { btree: Btree, level: u8, keys: Vec<(Key, Vec<u8>)> },
    Root(Root),
    KeyVersion(u64),
    /// A read or write clock.
    Clock { write: bool },
    Datetime,
    /// A record replay passes over: padding, a log, the keys an update
    /// replaced, another usage counter.
    Ignored,
}

/// The records of an entry area.
pub(crate) fn records(area: &[u8]) -> Result<Vec<Record>> {
    entries(area)?.into_iter().map(decode_record).collect()
}

fn decode_record(e: Entry<'_>) -> Result<Record> {
    let words = e.payload.len() / 8;
    let word = |i: usize| le64(e.payload, 8 * i);
    Ok(match e.kind {
        entry_type::BTREE_KEYS => {
            let btree = Btree::from_id(u32::from(e.btree)).ok_or(Error::Unsupported("journal keys of a btree this reader does not know"))?;
            if e.level > 3 {
                return Err(Error::Corrupt("journal keys above level 3"));
            }
            Record::Keys { btree, level: e.level, keys: keys(btree, e.level, e.payload)? }
        }
        entry_type::BTREE_ROOT => Record::Root(Root::decode(e.btree, e.level, e.payload)?),
        // Blacklists live in the superblock at 1.39 (A1.38); a reader that
        // honoured these would have to skip the bsets they name.
        entry_type::BLACKLIST | entry_type::BLACKLIST_V2 => return Err(Error::Unsupported("blacklist record in the journal")),
        entry_type::USAGE if words == 1 && e.btree == KEY_VERSION => Record::KeyVersion(word(0)),
        entry_type::USAGE if words == 1 => Record::Ignored,
        entry_type::CLOCK if words == 2 && e.payload[0] <= 1 => Record::Clock { write: e.payload[0] == 1 },
        entry_type::DATETIME if words >= 1 => Record::Datetime,
        entry_type::USAGE | entry_type::CLOCK | entry_type::DATETIME => return Err(Error::Corrupt("journal record of the wrong size")),
        entry_type::WRITE_BUFFER_KEYS => return Err(Error::Corrupt("write-buffer keys, which no writer puts on the medium")),
        _ => Record::Ignored,
    })
}

/// The keys of a btree_keys record: unpacked, each of a type its btree
/// holds at `level`, none of size zero.
fn keys(btree: Btree, level: u8, mut b: &[u8]) -> Result<Vec<(Key, Vec<u8>)>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let len = usize::from(b[0]) * 8;
        if len < KEY_U64S * 8 || len > b.len() || b[1] & 0x7f != 1 {
            return Err(Error::Corrupt("journal key packed, past its record or of no size"));
        }
        let (key, value) = Format::UNPACKED.key(b)?;
        check_kind(btree, level, &key)?;
        out.push((key, value.to_vec()));
        b = &b[len..];
    }
    Ok(out)
}

/// A key replay lays over a node's, or its deletion.
pub(crate) type Replayed = Option<(Key, Vec<u8>)>;

/// The window's records laid over the btrees: what a reader of an unclean
/// volume sees in place of the nodes' keys.
#[derive(Default)]
pub(crate) struct Replay {
    /// Every replayed key but accounting's, by btree, level and position, the
    /// latest winning; a deletion as `None`.
    pub keys: BTreeMap<(Btree, u8, Bpos), Replayed>,
    /// Accounting deltas by position, in journal order.
    pub accounting: BTreeMap<Bpos, Vec<(Version, Vec<u64>)>>,
    /// The latest root record of each btree.
    pub roots: Vec<Root>,
    pub key_version: Option<u64>,
    /// Keys the window's btree_keys records hold, before later ones
    /// replace earlier.
    pub read: usize,
}

/// Replays the window `w` of `scan` (A2.14): roots and the key version, then
/// keys in sequence order, a later key at one btree, level and position
/// replacing an earlier.
pub(crate) fn replay(scan: &Scan, sb: &Superblock, w: &Window) -> Result<Replay> {
    let mut r = Replay::default();
    for found in scan.in_window(sb, w) {
        for record in records(&found.area)? {
            match record {
                Record::Keys { btree, level, keys } => {
                    r.read += keys.len();
                    for (key, value) in keys {
                        if btree == Btree::Accounting {
                            if key.kind != kind::ACCOUNTING {
                                return Err(Error::Corrupt("accounting record of another type"));
                            }
                            r.accounting.entry(key.pos).or_default().push((key.version, counters(&value)?));
                            continue;
                        }
                        let replayed = (key.kind != kind::DELETED).then_some((key, value));
                        r.keys.insert((btree, level, key.pos), replayed);
                    }
                }
                Record::Root(root) => {
                    r.roots.retain(|old| old.btree != root.btree);
                    r.roots.push(root);
                }
                Record::KeyVersion(v) => r.key_version = Some(v),
                Record::Clock { .. } | Record::Datetime | Record::Ignored => {}
            }
        }
    }
    r.roots.sort_by_key(|root| root.btree);
    Ok(r)
}

/// An accounting value's counters: one to three.
pub(crate) fn counters(v: &[u8]) -> Result<Vec<u64>> {
    if !(8..=24).contains(&v.len()) || !v.len().is_multiple_of(8) {
        return Err(Error::Corrupt("accounting value of other than one to three counters"));
    }
    Ok(v.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect())
}

impl Replay {
    /// The keys of a node of `btree` at `level` spanning `[min, max]`, its
    /// own `keys` with the replayed ones laid over them: a replayed key
    /// replaces the node's at its position, a replayed deletion removes it,
    /// and an accounting key adds every delta newer than its own version
    /// (A7.8), dropped when its counters come to zero.
    pub(crate) fn over(&self, btree: Btree, level: u8, min: Bpos, max: Bpos, keys: Vec<(Key, Vec<u8>)>) -> Result<Vec<(Key, Vec<u8>)>> {
        let mut merged: BTreeMap<Bpos, (Key, Vec<u8>)> = keys.into_iter().map(|(k, v)| (k.pos, (k, v))).collect();
        if btree == Btree::Accounting && level == 0 {
            for (&pos, deltas) in self.accounting.range(min..=max) {
                let stored = merged.remove(&pos);
                let (mut key, mut sums) = match stored {
                    Some((key, value)) => (key, counters(&value)?),
                    None => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
                };
                let since = key.version;
                for (version, delta) in deltas.iter().filter(|(v, _)| *v > since) {
                    if sums.len() < delta.len() {
                        sums.resize(delta.len(), 0);
                    }
                    for (sum, d) in sums.iter_mut().zip(delta) {
                        *sum = sum.wrapping_add(*d);
                    }
                    key.version = key.version.max(*version);
                }
                if sums.iter().any(|&c| c != 0) {
                    merged.insert(pos, (key, sums.iter().flat_map(|c| c.to_le_bytes()).collect()));
                }
            }
        } else {
            for ((_, _, pos), replayed) in self.keys.range((btree, level, min)..=(btree, level, max)) {
                match replayed {
                    Some(kv) => merged.insert(*pos, kv.clone()),
                    None => merged.remove(pos),
                };
            }
        }
        Ok(merged.into_values().collect())
    }
}
