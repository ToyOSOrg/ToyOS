//! Positions, key headers and the node's key format.

use super::{le32, le64, put32, put64, Error, Result};

/// A key's position, compared as the integer (inode, offset, snapshot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bpos {
    pub inode: u64,
    pub offset: u64,
    pub snapshot: u32,
}

impl Bpos {
    pub const MIN: Bpos = Bpos { inode: 0, offset: 0, snapshot: 0 };
    pub const MAX: Bpos = Bpos { inode: u64::MAX, offset: u64::MAX, snapshot: u32::MAX };

    pub const fn new(inode: u64, offset: u64, snapshot: u32) -> Self {
        Bpos { inode, offset, snapshot }
    }

    /// The next position, carrying from snapshot into offset into inode.
    /// `None` after [`Bpos::MAX`].
    pub fn successor(self) -> Option<Bpos> {
        match (self.snapshot.checked_add(1), self.offset.checked_add(1), self.inode.checked_add(1)) {
            (Some(snapshot), _, _) => Some(Bpos { snapshot, ..self }),
            (None, Some(offset), _) => Some(Bpos { offset, snapshot: 0, ..self }),
            (None, None, Some(inode)) => Some(Bpos { inode, offset: 0, snapshot: 0 }),
            (None, None, None) => None,
        }
    }

    pub(crate) fn decode(b: &[u8]) -> Bpos {
        Bpos { snapshot: le32(b, 0), offset: le64(b, 4), inode: le64(b, 12) }
    }

    pub(crate) fn encode(self, b: &mut [u8]) {
        put32(b, 0, self.snapshot);
        put64(b, 4, self.offset);
        put64(b, 12, self.inode);
    }
}

/// A key's 96-bit version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub hi: u32,
    pub lo: u64,
}

/// A key's header, unpacked: its type, version, extent size and position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub kind: u8,
    pub version: Version,
    /// An extent's length in sectors, 0 for every other key.
    pub size: u32,
    pub pos: Bpos,
}

/// u64s of an unpacked key header.
pub(crate) const KEY_U64S: usize = 5;
/// u64s a key's value takes at most, so that it unpacks into a size byte
/// (A3.16).
pub(crate) const MAX_VALUE_U64S: usize = 255 - KEY_U64S;
/// Format byte of an unpacked key.
const UNPACKED: u8 = 1;

impl Key {
    pub(crate) fn new(kind: u8, pos: Bpos) -> Self {
        Key { kind, version: Version::default(), size: 0, pos }
    }

    /// The unpacked key followed by `value`, zero-padded to whole u64s.
    pub(crate) fn encode(&self, value: &[u8]) -> alloc::vec::Vec<u8> {
        let words = KEY_U64S + value.len().div_ceil(8);
        let mut b = alloc::vec![0u8; words * 8];
        b[0] = u8::try_from(words).expect("a value no key can carry");
        b[1] = UNPACKED;
        b[2] = self.kind;
        put64(&mut b, 4, self.version.lo);
        put32(&mut b, 12, self.version.hi);
        put32(&mut b, 16, self.size);
        self.pos.encode(&mut b[20..40]);
        b[40..40 + value.len()].copy_from_slice(value);
        b
    }
}

/// Which field of a packed key, in the order a format lists them.
const FIELDS: usize = 6;
/// Each field's unpacked width in bits: inode, offset, snapshot, size,
/// version high, version low.
const WIDTH: [u32; FIELDS] = [64, 64, 32, 32, 32, 64];

/// A node's key format: how wide each packed field is and what it is
/// offset from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Format {
    key_u64s: u8,
    bits: [u8; FIELDS],
    offset: [u64; FIELDS],
}

impl Format {
    /// Every key in format 1, the unpacked one, which is valid in any node.
    pub(crate) const UNPACKED: Format = Format { key_u64s: 5, bits: [64, 64, 32, 32, 32, 64], offset: [0; FIELDS] };

    pub(crate) fn decode(b: &[u8]) -> Result<Format> {
        if b[1] as usize != FIELDS {
            return Err(Error::Corrupt("key format without six fields"));
        }
        let bits: [u8; FIELDS] = b[2..8].try_into().unwrap();
        let offset: [u64; FIELDS] = core::array::from_fn(|i| le64(b, 8 + 8 * i));
        let mut total = 24;
        for i in 0..FIELDS {
            let (bits, width) = (u32::from(bits[i]), WIDTH[i]);
            let max = if width == 64 { u64::MAX } else { (1 << width) - 1 };
            let span = if bits >= 64 { u64::MAX } else { (1 << bits) - 1 };
            if bits > width || (bits == width && offset[i] != 0) || offset[i].checked_add(span).is_none_or(|top| top > max) {
                return Err(Error::Corrupt("key format field out of range"));
            }
            total += bits;
        }
        if u32::from(b[0]) != total.div_ceil(64) {
            return Err(Error::Corrupt("key format size disagrees with its fields"));
        }
        Ok(Format { key_u64s: b[0], bits, offset })
    }

    pub(crate) fn encode(&self, b: &mut [u8]) {
        b[0] = self.key_u64s;
        b[1] = FIELDS as u8;
        b[2..8].copy_from_slice(&self.bits);
        for (i, offset) in self.offset.iter().enumerate() {
            put64(b, 8 + 8 * i, *offset);
        }
    }

    /// The key whose bytes begin `b`, and its value. `b` runs to the end of
    /// its bset.
    pub(crate) fn key<'a>(&self, b: &'a [u8]) -> Result<(Key, &'a [u8])> {
        if b.len() < 8 {
            return Err(Error::Corrupt("key past the end of its bset"));
        }
        let len = usize::from(b[0]) * 8;
        let kind = b[2];
        let (key, header) = match b[1] & 0x7f {
            0 => (self.unpack(b, len)?, usize::from(self.key_u64s) * 8),
            UNPACKED => (unpacked(b, len)?, KEY_U64S * 8),
            _ => return Err(Error::Corrupt("key in an unknown format")),
        };
        let value = &b[header..len];
        if value.len() > MAX_VALUE_U64S * 8 {
            return Err(Error::Corrupt("key value past 250 u64s"));
        }
        Ok((Key { kind, ..key }, value))
    }

    fn unpack(&self, b: &[u8], len: usize) -> Result<Key> {
        let words = usize::from(self.key_u64s);
        if len < words * 8 || len > b.len() || words == 0 {
            return Err(Error::Corrupt("packed key past the end of its bset"));
        }
        let word = |i: usize| le64(b, 8 * i);
        // The key is one little-endian integer of `words` words; fields run
        // from its top bit down.
        let mut top = words as u32 * 64;
        let value: [u64; FIELDS] = core::array::from_fn(|i| {
            let bits = u32::from(self.bits[i]);
            top -= bits;
            take(&word, top, bits) + self.offset[i]
        });
        Ok(Key {
            kind: 0,
            pos: Bpos { inode: value[0], offset: value[1], snapshot: value[2] as u32 },
            size: value[3] as u32,
            version: Version { hi: value[4] as u32, lo: value[5] },
        })
    }
}

/// Bits `[lo, lo + len)` of the little-endian integer `word(0), word(1), …`.
fn take(word: &impl Fn(usize) -> u64, lo: u32, len: u32) -> u64 {
    if len == 0 {
        return 0;
    }
    let (i, shift) = ((lo / 64) as usize, lo % 64);
    let mut v = word(i) >> shift;
    if shift + len > 64 {
        v |= word(i + 1) << (64 - shift);
    }
    if len < 64 { v & ((1 << len) - 1) } else { v }
}

fn unpacked(b: &[u8], len: usize) -> Result<Key> {
    if len < KEY_U64S * 8 || len > b.len() {
        return Err(Error::Corrupt("unpacked key past the end of its bset"));
    }
    Ok(Key {
        kind: 0,
        version: Version { lo: le64(b, 4), hi: le32(b, 12) },
        size: le32(b, 16),
        pos: Bpos::decode(&b[20..40]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_carries_from_snapshot_through_offset_into_inode() {
        assert_eq!(Bpos::new(1, 2, 3).successor(), Some(Bpos::new(1, 2, 4)));
        assert_eq!(Bpos::new(1, 2, u32::MAX).successor(), Some(Bpos::new(1, 3, 0)));
        assert_eq!(Bpos::new(1, u64::MAX, u32::MAX).successor(), Some(Bpos::new(2, 0, 0)));
        assert_eq!(Bpos::MAX.successor(), None);
    }

    #[test]
    fn an_unpacked_key_reads_back() {
        let key = Key { kind: 6, version: Version { hi: 7, lo: 1 << 40 }, size: 32, pos: Bpos::new(0x8000_0003, 64, u32::MAX) };
        let bytes = key.encode(&[0xab; 9]);
        assert_eq!(bytes.len(), 56);
        let (read, value) = Format::UNPACKED.key(&bytes).unwrap();
        assert_eq!(read, key);
        assert_eq!(value, &[[0xab; 9].as_slice(), &[0; 7]].concat()[..]);
    }

    /// A packed key of one word may claim up to 254 u64s of value by its
    /// size byte; past A3.16's 250 it could not be written unpacked, and is
    /// refused where it is read.
    #[test]
    fn a_packed_key_whose_value_could_not_unpack_is_refused() {
        let mut format = [0u8; 56];
        format[0] = 1;
        format[1] = 6;
        let format = Format::decode(&format).unwrap();
        let key = |size: u8| {
            let mut b = alloc::vec![0u8; usize::from(size) * 8];
            b[..8].copy_from_slice(&(u64::from(size) | 34 << 16).to_le_bytes());
            b
        };
        assert_eq!(format.key(&key(251)).unwrap().1.len(), 250 * 8);
        assert_eq!(format.key(&key(255)).err(), Some(Error::Corrupt("key value past 250 u64s")));
    }

    /// An upstream alloc leaf's format: one word, offset 16 bits
    /// from 82 and snapshot 8 bits. The fields fill the word from its top.
    #[test]
    fn a_packed_key_reads_its_fields_from_the_top_bit_down() {
        let mut format = [0u8; 56];
        format[0] = 1;
        format[1] = 6;
        format[2..8].copy_from_slice(&[0, 16, 8, 0, 0, 0]);
        format[16..24].copy_from_slice(&82u64.to_le_bytes());
        let format = Format::decode(&format).unwrap();
        let word: u64 = 1 | (27 << 16) | (5u64 << 40) | ((1991 - 82) << 48);
        // u64s 1, format 0 (packed), type 27.
        let bytes = word.to_le_bytes();
        let (key, value) = format.key(&bytes).unwrap();
        assert_eq!((key.kind, key.pos, value.len()), (27, Bpos::new(0, 1991, 5), 0));
    }

    #[test]
    fn a_format_whose_size_disagrees_with_its_fields_is_refused() {
        let mut format = [0u8; 56];
        Format::UNPACKED.encode(&mut format);
        assert_eq!(Format::decode(&format), Ok(Format::UNPACKED));
        format[0] = 4;
        assert!(Format::decode(&format).is_err());
        Format::UNPACKED.encode(&mut format);
        format[8] = 1; // inode offset at full width
        assert!(Format::decode(&format).is_err());
    }
}
