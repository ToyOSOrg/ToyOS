//! Keys: the position they sort by, the per-node packing, and unpacking.
//!
//! A packed key's first `key_u64s` words, read little-endian, are one integer.
//! Its three header bytes are the low 24 bits; the node's `bkey_format` lays
//! the six fields end to end below the integer's top bit, `INODE` highest and
//! `VERSION_LO` lowest, each `bits_per_field` wide, and each unpacks to its
//! packed value plus its `field_offset`. The bits between the header and the
//! lowest field mean nothing, and a field crosses word boundaries freely.

use super::raw::Raw;
use super::UpstreamError;

/// `BKEY_U64s`: the unpacked key is five u64s, and its value follows them.
pub const BKEY_U64S: usize = 5;
pub const BKEY_BYTES: usize = BKEY_U64S * 8;
/// `KEY_PACKED_BITS_START`: the three header bytes a packed key keeps
/// unpacked, below every field bit.
const PACKED_BITS_START: u32 = 24;
const NR_FIELDS: usize = 6;

pub const KEY_FORMAT_LOCAL_BTREE: u8 = 0;
pub const KEY_FORMAT_CURRENT: u8 = 1;

/// Value types this reader decodes or steps over, `BCH_BKEY_TYPES()`.
pub const TYPE_DELETED: u8 = 0;
pub const TYPE_WHITEOUT: u8 = 1;
pub const TYPE_ERROR: u8 = 2;
pub const TYPE_HASH_WHITEOUT: u8 = 4;
pub const TYPE_EXTENT: u8 = 6;
pub const TYPE_RESERVATION: u8 = 7;
pub const TYPE_DIRENT: u8 = 10;
pub const TYPE_INLINE_DATA: u8 = 17;
pub const TYPE_BTREE_PTR_V2: u8 = 18;
pub const TYPE_SUBVOLUME: u8 = 21;
pub const TYPE_INODE_V3: u8 = 29;
pub const TYPE_EXTENT_WHITEOUT: u8 = 36;

/// A key's position: the btree's sort order, low to high, is exactly this
/// tuple's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Bpos {
    pub inode: u64,
    pub offset: u64,
    pub snapshot: u32,
}

impl Bpos {
    pub const MIN: Self = Self { inode: 0, offset: 0, snapshot: 0 };

    pub fn new(inode: u64, offset: u64, snapshot: u32) -> Self {
        Self { inode, offset, snapshot }
    }

    /// Read the on-disk `struct bpos` at `off`; its words are stored low to
    /// high, which is the reverse of how they compare.
    pub fn read(raw: &Raw<'_>, off: usize) -> Result<Self, UpstreamError> {
        Ok(Self {
            snapshot: raw.u32(off)?,
            offset: raw.u64(off + 4)?,
            inode: raw.u64(off + 12)?,
        })
    }
}

pub const BPOS_BYTES: usize = 20;

/// One field of a `bkey_format`: its packed width, and what unpacking adds to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FieldPacking {
    bits: u8,
    offset: u64,
}

impl FieldPacking {
    /// A field packed at its unpacked width, with nothing added back.
    const fn whole(bits: u8) -> Self {
        Self { bits, offset: 0 }
    }

    /// Whether every value this field can unpack to, `offset + 2^bits - 1` at
    /// the most, fits in the unpacked field it fills.
    fn fits(self, unpacked: Unpacked) -> bool {
        // A span past `u128` is past any unpacked field.
        let top = 2u128
            .checked_pow(self.bits.into())
            .and_then(|span| span.checked_add(self.offset.into()));
        top.is_some_and(|top| top <= 2u128.pow(unpacked as u32))
    }
}

/// The width of a member of the unpacked `struct bkey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unpacked {
    U32 = 32,
    U64 = 64,
}

/// A btree node's key format: how many bits each field is packed into, and
/// what is added back to each on the way out.
///
/// **Holding one is proof it is valid**: the only way to make one from disk
/// bytes is `TryFrom<StoredFormat>`, so unpacking never meets a field wider
/// than the one it fills or a key length its fields do not add up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BkeyFormat {
    key_u64s: u8,
    fields: [FieldPacking; NR_FIELDS],
}

pub const FORMAT_BYTES: usize = 56;

/// `BKEY_FORMAT_CURRENT`'s widths: what each packed field is unpacked into.
const UNPACKED_BITS: [Unpacked; NR_FIELDS] =
    [Unpacked::U64, Unpacked::U64, Unpacked::U32, Unpacked::U32, Unpacked::U32, Unpacked::U64];

/// A `struct bkey_format` exactly as stored, before anything in it is believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredFormat {
    key_u64s: u8,
    nr_fields: u8,
    fields: [FieldPacking; NR_FIELDS],
}

impl StoredFormat {
    fn parse(bytes: &[u8; FORMAT_BYTES]) -> Self {
        let [key_u64s, nr_fields, b0, b1, b2, b3, b4, b5, offsets @ ..] = *bytes;
        let bits = [b0, b1, b2, b3, b4, b5];
        // 48 bytes are exactly one word per field.
        let offsets = offsets.as_chunks::<8>().0;
        let fields = core::array::from_fn(|j| FieldPacking { bits: bits[j], offset: u64::from_le_bytes(offsets[j]) });
        Self { key_u64s, nr_fields, fields }
    }
}

/// Each way a stored `bkey_format` describes keys this reader cannot unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// `nr_fields` is not `BKEY_NR_FIELDS`.
    FieldCount,
    /// A field's largest value, `field_offset + 2^bits_per_field - 1`, is past
    /// what the unpacked field holds.
    FieldTooWide,
    /// `key_u64s` is not the words the header and the fields' bits fill.
    WordCount,
}

impl From<FormatError> for UpstreamError {
    fn from(err: FormatError) -> Self {
        UpstreamError::Refused(match err {
            FormatError::FieldCount => "btree node's key format has the wrong field count",
            FormatError::FieldTooWide => "btree node's key format packs a field wider than the one it fills",
            FormatError::WordCount => "btree node's key format is not as many words as its fields need",
        })
    }
}

impl TryFrom<StoredFormat> for BkeyFormat {
    type Error = FormatError;

    /// `key_u64s` is required to be exactly the words the fields need, so a
    /// format cannot carry slack a key could hide in.
    fn try_from(stored: StoredFormat) -> Result<Self, FormatError> {
        if usize::from(stored.nr_fields) != NR_FIELDS {
            return Err(FormatError::FieldCount);
        }
        if !stored.fields.iter().zip(UNPACKED_BITS).all(|(field, unpacked)| field.fits(unpacked)) {
            return Err(FormatError::FieldTooWide);
        }
        let field_bits: u32 = stored.fields.iter().map(|field| u32::from(field.bits)).sum();
        if u32::from(stored.key_u64s) != (PACKED_BITS_START + field_bits).div_ceil(64) {
            return Err(FormatError::WordCount);
        }
        Ok(Self { key_u64s: stored.key_u64s, fields: stored.fields })
    }
}

impl BkeyFormat {
    /// `BKEY_FORMAT_CURRENT`: what a key outside any btree node — a journal
    /// entry's, a clean section's — is written in.
    pub fn unpacked() -> Self {
        Self { key_u64s: BKEY_U64S as u8, fields: UNPACKED_BITS.map(|width| FieldPacking::whole(width as u8)) }
    }

    /// Read the `bkey_format` stored at `off`, refusing one whose keys could
    /// not be unpacked into `struct bkey`.
    pub fn read(raw: &Raw<'_>, off: usize) -> Result<Self, UpstreamError> {
        let bytes = raw.slice(off, FORMAT_BYTES)?.try_into().expect("a FORMAT_BYTES window");
        Ok(Self::try_from(StoredFormat::parse(bytes))?)
    }
}

/// A key, unpacked, and where its value sits inside the same window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// The key and value together, in u64s: how far the next key is.
    pub u64s: u8,
    pub kind: u8,
    /// Extent size in sectors; zero for every non-extent key.
    pub size: u32,
    pub pos: Bpos,
    /// Byte offset of the value from the start of this key.
    pub val_at: usize,
    /// Where this key starts inside the node it was read from.
    pub base: usize,
}

impl Key {
    /// The value's length in bytes.
    pub fn val_bytes(&self) -> usize {
        self.u64s as usize * 8 - self.val_at
    }

    /// Record where in the node this key was found, so its value can be located.
    pub fn with_base(mut self, base: usize) -> Self {
        self.base = base;
        self
    }

    /// Decode the key at the start of `raw`, unpacking it through `format`
    /// when the key says it is packed.
    pub fn read(raw: &Raw<'_>, format: &BkeyFormat) -> Result<Self, UpstreamError> {
        let u64s = raw.u8(0)?;
        let fmt_byte = raw.u8(1)?;
        let kind = raw.u8(2)?;
        let packed_format = fmt_byte & 0x7F;

        let key_u64s = match packed_format {
            KEY_FORMAT_LOCAL_BTREE => usize::from(format.key_u64s),
            KEY_FORMAT_CURRENT => BKEY_U64S,
            _ => return Err(UpstreamError::Refused("key names a format the node does not define")),
        };
        if (u64s as usize) < key_u64s {
            return Err(UpstreamError::Refused("key is shorter than the format it names"));
        }
        // Every byte the key claims has to be there before any field is read.
        let whole = raw.sub(0, u64s as usize * 8, "key runs past the end of its bset")?;

        let (size, pos) = if packed_format == KEY_FORMAT_CURRENT {
            (whole.u32(16)?, Bpos::read(&whole, 20)?)
        } else {
            PackedKey::new(&whole, format)?.unpack()?
        };

        Ok(Self { u64s, kind, size, pos, val_at: key_u64s * 8, base: 0 })
    }
}

/// A key packed in a node's format: its first `key_u64s` words, header
/// included, read little-endian as one integer.
struct PackedKey<'a> {
    bytes: &'a [u8],
    format: &'a BkeyFormat,
}

const FIELD_DOES_NOT_FIT: UpstreamError =
    UpstreamError::Refused("packed key's field does not fit the one it unpacks into");

impl<'a> PackedKey<'a> {
    /// The key part of `whole`; the value past it is not the key's.
    fn new(whole: &Raw<'a>, format: &'a BkeyFormat) -> Result<Self, UpstreamError> {
        let bytes = whole.slice(0, usize::from(format.key_u64s) * 8)?;
        Ok(Self { bytes, format })
    }

    /// Bits `[lo, lo + width)` of the key as an integer, for `width <= 64`.
    fn bit_range(&self, lo: u16, width: u8) -> u128 {
        // Nine bytes from the one holding bit `lo` cover any 64-bit field.
        let mut window = [0u8; 16];
        for (dst, src) in window.iter_mut().zip(self.bytes.iter().skip(usize::from(lo / 8))) {
            *dst = *src;
        }
        (u128::from_le_bytes(window) >> (lo % 8)) & ((1u128 << width) - 1)
    }

    /// `(size, pos)`: every field's packed value plus its `field_offset`.
    fn unpack(&self) -> Result<(u32, Bpos), UpstreamError> {
        // The format's validity puts the lowest field at bit 24 or above, so
        // `top` never passes below the header.
        let mut top = u16::from(self.format.key_u64s) * 64;
        let [inode, offset, snapshot, size, _version_hi, _version_lo] =
            self.format.fields.map(|field| {
                top -= u16::from(field.bits);
                self.bit_range(top, field.bits) + u128::from(field.offset)
            });
        let wide = |value: u128| u64::try_from(value).map_err(|_| FIELD_DOES_NOT_FIT);
        let narrow = |value: u128| u32::try_from(value).map_err(|_| FIELD_DOES_NOT_FIT);
        Ok((narrow(size)?, Bpos { inode: wide(inode)?, offset: wide(offset)?, snapshot: narrow(snapshot)? }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const FIELD_INODE: usize = 0;
    const FIELD_SNAPSHOT: usize = 2;
    const FIELD_SIZE: usize = 3;

    fn format_bytes(key_u64s: u8, bits: [u8; 6], offsets: [u64; 6]) -> Vec<u8> {
        let mut out = vec![key_u64s, 6];
        out.extend_from_slice(&bits);
        for off in offsets {
            out.extend_from_slice(&off.to_le_bytes());
        }
        out
    }

    /// The format `BKEY_FORMAT_CURRENT` is: every field at its natural width,
    /// no offsets. A key packed in it unpacks to what an unpacked key holds:
    /// its fields land on exactly the bytes of `struct bkey`.
    #[test]
    fn the_identity_format_round_trips_a_position() {
        let bits = [64u8, 64, 32, 32, 32, 64];
        let raw = format_bytes(BKEY_U64S as u8, bits, [0; 6]);
        let format = BkeyFormat::read(&Raw::new(&raw, "format"), 0).expect("a valid format");

        // Pack (inode, offset, snapshot, size, version) most significant bit
        // first, from the top of the last word down.
        let want = Bpos::new(0x1122_3344_5566_7788, 0x99AA_BBCC_DDEE_FF00, 0x1234_5678);
        let size = 0x0BAD_F00Du32;
        let mut bitstring: Vec<bool> = Vec::new();
        let mut push = |v: u64, n: u32| {
            for i in (0..n).rev() {
                bitstring.push((v >> i) & 1 == 1);
            }
        };
        push(want.inode, 64);
        push(want.offset, 64);
        push(want.snapshot as u64, 32);
        push(size as u64, 32);
        push(0, 32);
        push(0, 64);

        let mut words = [0u64; BKEY_U64S];
        let mut at = 0;
        for word in (0..BKEY_U64S).rev() {
            for bit in (0..64).rev() {
                if at < bitstring.len() && bitstring[at] {
                    words[word] |= 1u64 << bit;
                }
                at += 1;
            }
        }

        let mut key = Vec::new();
        for word in words {
            key.extend_from_slice(&word.to_le_bytes());
        }
        key[0] = BKEY_U64S as u8;
        key[1] = KEY_FORMAT_LOCAL_BTREE;
        key[2] = TYPE_DIRENT;

        let decoded = Key::read(&Raw::new(&key, "key"), &format).expect("a packed key");
        assert_eq!(decoded.pos, want);
        assert_eq!(decoded.size, size);
        assert_eq!(decoded.kind, TYPE_DIRENT);
        assert_eq!(decoded.val_at, BKEY_BYTES);
    }

    #[test]
    fn a_field_wider_than_the_one_it_fills_is_refused() {
        for (field, bits) in [(FIELD_SNAPSHOT, 64u8), (FIELD_SIZE, 33), (FIELD_INODE, 65)] {
            let mut widths = [64u8, 64, 32, 32, 32, 64];
            widths[field] = bits;
            let total: u32 = PACKED_BITS_START + widths.iter().map(|b| *b as u32).sum::<u32>();
            let raw = format_bytes(total.div_ceil(64) as u8, widths, [0; 6]);
            assert_eq!(
                BkeyFormat::read(&Raw::new(&raw, "format"), 0).err(),
                Some(UpstreamError::Refused(
                    "btree node's key format packs a field wider than the one it fills"
                )),
                "a {bits}-bit field {field} was accepted"
            );
        }

        // A field at its full width may carry no offset, or the two together
        // exceed what the unpacked field holds.
        let mut offsets = [0u64; 6];
        offsets[FIELD_SNAPSHOT] = 1;
        let full = format_bytes(BKEY_U64S as u8, [64, 64, 32, 32, 32, 64], offsets);
        assert!(BkeyFormat::read(&Raw::new(&full, "format"), 0).is_err());

        // A narrower field whose offset pushes its top past the unpacked max.
        let mut offsets = [0u64; 6];
        offsets[FIELD_SNAPSHOT] = u32::MAX as u64;
        let pushed = format_bytes(5, [64, 64, 31, 32, 32, 64], offsets);
        assert!(BkeyFormat::read(&Raw::new(&pushed, "format"), 0).is_err());
    }

    /// A key whose `u64s` is below its format's key length, or past the end of
    /// the window, is refused rather than read short.
    #[test]
    fn a_key_that_does_not_fit_is_refused() {
        let raw = format_bytes(BKEY_U64S as u8, [64, 64, 32, 32, 32, 64], [0; 6]);
        let format = BkeyFormat::read(&Raw::new(&raw, "format"), 0).expect("a valid format");

        let mut short = vec![0u8; BKEY_BYTES];
        short[0] = 4;
        short[1] = KEY_FORMAT_LOCAL_BTREE;
        assert!(Key::read(&Raw::new(&short, "key"), &format).is_err());

        let mut past = vec![0u8; BKEY_BYTES];
        past[0] = 200;
        past[1] = KEY_FORMAT_LOCAL_BTREE;
        assert!(Key::read(&Raw::new(&past, "key"), &format).is_err());

        let mut alien = vec![0u8; BKEY_BYTES];
        alien[0] = BKEY_U64S as u8;
        alien[1] = 42;
        assert!(Key::read(&Raw::new(&alien, "key"), &format).is_err());
    }

    mod vectors {
        /// `(inode, offset, snapshot, size)`.
        type Unpacked = (u64, u64, u32, u32);
        include!("bkey_vectors.rs");
    }

    fn stored(bytes: &[u8]) -> StoredFormat {
        StoredFormat::parse(bytes.first_chunk().expect("a whole format"))
    }

    /// Every format vector: valid ones read back unchanged, and each invalid
    /// one is refused for the first condition it fails.
    #[test]
    fn format_vectors() {
        for &(id, bytes, want) in vectors::FORMAT_CASES {
            let got = BkeyFormat::read(&Raw::new(bytes, "format"), 0);
            match want {
                None => {
                    let format = got.unwrap_or_else(|err| panic!("{id}: refused a valid format: {err:?}"));
                    let stored = stored(bytes);
                    assert_eq!((format.key_u64s, format.fields), (stored.key_u64s, stored.fields), "{id}");
                }
                Some(refusal) => assert_eq!(got, Err(UpstreamError::Refused(refusal)), "{id}"),
            }
        }
    }

    /// A format one byte short of its 56 is refused, not read past.
    #[test]
    fn a_truncated_format_is_refused() {
        let (_, bytes, _) = vectors::FORMAT_CASES[0];
        assert!(BkeyFormat::read(&Raw::new(&bytes[..FORMAT_BYTES - 1], "format"), 0).is_err());
        assert!(BkeyFormat::read(&Raw::new(bytes, "format"), 1).is_err());
    }

    /// `field_offset + 2^bits <= 2^unpacked`, decided over the integers for
    /// every width a byte can state.
    #[test]
    fn field_fit_vectors() {
        for &(bits, offset, unpacked, overflows) in vectors::OVERFLOW_CASES {
            let width = match unpacked {
                32 => Unpacked::U32,
                64 => Unpacked::U64,
                other => panic!("no unpacked field is {other} bits wide"),
            };
            assert_eq!(
                FieldPacking { bits, offset }.fits(width),
                !overflows,
                "{bits} bits at offset {offset:#x} into {unpacked}"
            );
        }
    }

    #[test]
    fn unpack_vectors() {
        for &(id, format, key, (inode, offset, snapshot, size)) in vectors::UNPACK_CASES {
            let format = BkeyFormat::read(&Raw::new(format, "format"), 0).expect(id);
            let got = PackedKey::new(&Raw::new(key, "key"), &format).and_then(|key| key.unpack());
            assert_eq!(got, Ok((size, Bpos::new(inode, offset, snapshot))), "{id}");
        }
    }

    /// A field whose value is past its unpacked width is refused, not
    /// truncated. No format `read` accepts reaches this, so the format is
    /// built past its constructor.
    #[test]
    fn a_field_past_its_unpacked_width_is_refused_not_truncated() {
        let narrow = FieldPacking { bits: 0, offset: u64::from(u32::MAX) + 1 };
        // The key's top bit packs a 1, which lands one past `u64::MAX`.
        let wide = FieldPacking { bits: 1, offset: u64::MAX };
        let none = FieldPacking::whole(0);
        for fields in [
            [wide, none, none, none, none, none],
            [none, wide, none, none, none, none],
            [none, none, narrow, none, none, none],
            [none, none, none, narrow, none, none],
        ] {
            let format = BkeyFormat { key_u64s: 1, fields };
            let key = [1, KEY_FORMAT_LOCAL_BTREE, TYPE_DIRENT, 0, 0, 0, 0, 0x80];
            let got = PackedKey::new(&Raw::new(&key, "key"), &format).and_then(|key| key.unpack());
            assert_eq!(got, Err(FIELD_DOES_NOT_FIT), "{fields:?}");
        }
    }

    /// The identity format read off a disk is the one `unpacked` states.
    #[test]
    fn the_identity_format_is_the_unpacked_one() {
        let (id, bytes, _) = vectors::FORMAT_CASES[0];
        assert_eq!(id, "F1");
        assert_eq!(BkeyFormat::read(&Raw::new(bytes, "format"), 0), Ok(BkeyFormat::unpacked()));
    }
}
