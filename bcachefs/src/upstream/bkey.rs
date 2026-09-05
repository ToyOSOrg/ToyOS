//! Keys: the position they sort by, the per-node packing, and the unpacking
//! `__bch2_bkey_unpack_key` defines.

use super::raw::Raw;
use super::UpstreamError;

/// `BKEY_U64s`: the unpacked key is five u64s, and its value follows them.
pub const BKEY_U64S: usize = 5;
pub const BKEY_BYTES: usize = BKEY_U64S * 8;
/// `KEY_PACKED_BITS_START`: the three header bytes a packed key keeps
/// unpacked, below every field bit.
const PACKED_BITS_START: u32 = 24;
const NR_FIELDS: usize = 6;
const FIELD_INODE: usize = 0;
const FIELD_OFFSET: usize = 1;
const FIELD_SNAPSHOT: usize = 2;
const FIELD_SIZE: usize = 3;

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

/// A btree node's key format: how many bits each field is packed into, and
/// what is added back to each on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BkeyFormat {
    pub key_u64s: u8,
    bits_per_field: [u8; NR_FIELDS],
    field_offset: [u64; NR_FIELDS],
}

pub const FORMAT_BYTES: usize = 56;

impl BkeyFormat {
    /// `BKEY_FORMAT_CURRENT`: what a key outside any btree node — a journal
    /// entry's, a clean section's — is written in.
    pub const fn unpacked() -> Self {
        Self {
            key_u64s: BKEY_U64S as u8,
            bits_per_field: [64, 64, 32, 32, 32, 64],
            field_offset: [0; NR_FIELDS],
        }
    }

    /// Read a node's format, refusing exactly what `bch2_bkey_format_invalid`
    /// refuses.
    ///
    /// **A field wider than the unpacked one it fills is the refusal that
    /// matters here**: without it a 64-bit packed snapshot unpacks into a
    /// 32-bit field, and a key from any snapshot is served as one from the
    /// root subvolume. `key_u64s` is required to be exactly the words the
    /// fields need, so a format cannot carry slack a key could hide in.
    pub fn read(raw: &Raw<'_>, off: usize) -> Result<Self, UpstreamError> {
        let key_u64s = raw.u8(off)?;
        let nr_fields = raw.u8(off + 1)?;
        if nr_fields as usize != NR_FIELDS {
            return Err(UpstreamError::Refused("btree node's key format has the wrong field count"));
        }
        let mut bits_per_field = [0u8; NR_FIELDS];
        let mut field_offset = [0u64; NR_FIELDS];
        let mut total = PACKED_BITS_START;
        for (i, bits) in bits_per_field.iter_mut().enumerate() {
            *bits = raw.u8(off + 2 + i)?;
            field_offset[i] = raw.u64(off + 8 + i * 8)?;
            if field_overflows(*bits, field_offset[i], UNPACKED_BITS[i]) {
                return Err(UpstreamError::Refused("btree node's key format packs a field wider than the one it fills"));
            }
            total += *bits as u32;
        }
        // No separate bound against an unpacked key's length: every field is
        // capped at the width it unpacks into, so `total` cannot pass 312 bits
        // and this equality already forces five words or fewer.
        if key_u64s as u32 != total.div_ceil(64) {
            return Err(UpstreamError::Refused("btree node's key format is not as many words as its fields need"));
        }
        Ok(Self { key_u64s, bits_per_field, field_offset })
    }
}

/// `BKEY_FORMAT_CURRENT`'s widths: what each packed field is unpacked into.
const UNPACKED_BITS: [u8; NR_FIELDS] = [64, 64, 32, 32, 32, 64];

/// `bch2_bkey_format_field_overflows`: whether this field could unpack to a
/// value the unpacked key cannot hold.
fn field_overflows(bits: u8, offset: u64, unpacked_bits: u8) -> bool {
    if bits > unpacked_bits {
        return true;
    }
    if bits == unpacked_bits && offset != 0 {
        return true;
    }
    let unpacked_mask = !((!0u64 << 1) << (unpacked_bits - 1));
    let field_mask = if bits == 0 { 0 } else { !((!0u64 << (bits - 1)) << 1) };
    (offset.wrapping_add(field_mask) & unpacked_mask) < offset
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
            KEY_FORMAT_LOCAL_BTREE => format.key_u64s as usize,
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
            unpack(&whole, format)?
        };

        Ok(Self { u64s, kind, size, pos, val_at: key_u64s * 8, base: 0 })
    }
}

/// `get_inc_field` over all six fields, in the order `bkey_fields()` names.
///
/// Little-endian word order: the highest word of the packed key is the last
/// one, and `next_word` walks down toward the header.
fn unpack(whole: &Raw<'_>, format: &BkeyFormat) -> Result<(u32, Bpos), UpstreamError> {
    let mut word = format.key_u64s as usize - 1;
    let mut w = whole.u64(word * 8)?;
    let mut avail = 64u32;
    let mut out = [0u64; NR_FIELDS];

    for (field, slot) in out.iter_mut().enumerate() {
        let mut bits = format.bits_per_field[field] as u32;
        let mut v = 0u64;
        if bits >= avail {
            v = if bits == 0 { 0 } else { w >> (64 - bits) };
            bits -= avail;
            word = word
                .checked_sub(1)
                .ok_or(UpstreamError::Refused("packed key ran out of words"))?;
            w = whole.u64(word * 8)?;
            avail = 64;
        }
        // `bits` is never 64 here, which is what makes the paired shift safe.
        v |= (w >> 1) >> (63 - bits);
        w <<= bits;
        avail -= bits;
        *slot = v.wrapping_add(format.field_offset[field]);
    }

    // The format check above makes neither of these narrowings lossy; they are
    // refused rather than truncated so the two cannot drift apart.
    let narrow = |v: u64| {
        u32::try_from(v).map_err(|_| UpstreamError::Refused("packed key's field does not fit the one it unpacks into"))
    };
    let pos = Bpos {
        inode: out[FIELD_INODE],
        offset: out[FIELD_OFFSET],
        snapshot: narrow(out[FIELD_SNAPSHOT])?,
    };
    Ok((narrow(out[FIELD_SIZE])?, pos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn format_bytes(key_u64s: u8, bits: [u8; 6], offsets: [u64; 6]) -> Vec<u8> {
        let mut out = vec![key_u64s, 6];
        out.extend_from_slice(&bits);
        for off in offsets {
            out.extend_from_slice(&off.to_le_bytes());
        }
        out
    }

    /// The format `BKEY_FORMAT_CURRENT` is: every field at its natural width,
    /// no offsets. A key packed in it unpacks to what an unpacked key holds,
    /// which is the property `bch2_bkey_transform` rests on.
    #[test]
    fn the_identity_format_round_trips_a_position() {
        let bits = [64u8, 64, 32, 32, 32, 64];
        let raw = format_bytes(BKEY_U64S as u8, bits, [0; 6]);
        let format = BkeyFormat::read(&Raw::new(&raw, "format"), 0).expect("a valid format");

        // Pack (inode, offset, snapshot, size, version) MSB-first from the last
        // word down, exactly as `set_inc_field` writes it.
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

    /// `field_offset` is added back on the way out, which is how a node whose
    /// keys share a high inode packs them into a few bits.
    #[test]
    fn field_offset_is_added_back() {
        let bits = [8u8, 8, 0, 0, 0, 0];
        let raw = format_bytes(1, bits, [1000, 2000, 3000, 4000, 0, 0]);
        let format = BkeyFormat::read(&Raw::new(&raw, "format"), 0).expect("a valid format");

        // One word: header in bits 0..24, inode in 63..56, offset in 55..48.
        let word: u64 = (7u64 << 56) | (9u64 << 48);
        let mut key = word.to_le_bytes().to_vec();
        key[0] = 1;
        key[1] = KEY_FORMAT_LOCAL_BTREE;
        key[2] = TYPE_INODE_V3;
        let decoded = Key::read(&Raw::new(&key, "key"), &format).expect("a packed key");
        assert_eq!(decoded.pos.inode, 1007);
        assert_eq!(decoded.pos.offset, 2009);
        assert_eq!(decoded.pos.snapshot, 3000);
        assert_eq!(decoded.size, 4000);
    }

    /// **A field wider than the one it unpacks into is the refusal that keeps
    /// snapshots out.** `bits_per_field[SNAPSHOT] = 64` would let a key from
    /// any snapshot unpack, truncated, into the root subvolume's.
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

    /// A format the node could not have written is refused before it is used
    /// to read a key, because unpacking through it walks off the key.
    #[test]
    fn an_impossible_format_is_refused() {
        let too_many = format_bytes(1, [64, 64, 32, 32, 32, 64], [0; 6]);
        assert!(BkeyFormat::read(&Raw::new(&too_many, "format"), 0).is_err());

        let no_words = format_bytes(0, [0; 6], [0; 6]);
        assert!(BkeyFormat::read(&Raw::new(&no_words, "format"), 0).is_err());

        let too_many_words = format_bytes(9, [0; 6], [0; 6]);
        assert!(BkeyFormat::read(&Raw::new(&too_many_words, "format"), 0).is_err());

        let mut wrong_fields = format_bytes(5, [0; 6], [0; 6]);
        wrong_fields[1] = 5;
        assert!(BkeyFormat::read(&Raw::new(&wrong_fields, "format"), 0).is_err());

        let wide = format_bytes(5, [65, 0, 0, 0, 0, 0], [0; 6]);
        assert!(BkeyFormat::read(&Raw::new(&wide, "format"), 0).is_err());
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
}
