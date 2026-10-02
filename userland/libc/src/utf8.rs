//! UTF-8 read one byte at a time, the one locale's multibyte encoding
//! (`wchar.rs`). **A sequence is refused at the first byte after which no
//! continuation makes it a Unicode scalar value in its shortest form**, as C's
//! `(size_t)-1` says; a prefix some continuation completes is `(size_t)-2`'s.

/// `mbstate_t`: the bits of a code point so far, the continuation bytes it
/// still needs in the low byte of `needed`, and its whole length above it.
#[repr(C)]
pub struct MbState {
    bits: u32,
    needed: u32,
}

/// What one byte does to the character being read.
pub(crate) enum Byte {
    /// It ends a character: its code point.
    Ends(u32),
    /// It leaves a prefix some continuation completes.
    Continues,
    /// No continuation completes what was read; the state is initial again.
    Refused,
}

/// The smallest code point a sequence of `len` bytes may encode.
fn shortest(len: u32) -> u32 {
    match len {
        2 => 0x80,
        3 => 0x800,
        _ => 0x10000,
    }
}

impl MbState {
    pub(crate) const INITIAL: MbState = MbState { bits: 0, needed: 0 };

    /// Whether a character has been begun and not ended.
    pub(crate) fn is_partial(&self) -> bool {
        self.needed != 0
    }

    pub(crate) fn feed(&mut self, b: u8) -> Byte {
        let b = u32::from(b);
        let (bits, len, left) = if self.needed == 0 {
            match b {
                0x00..=0x7f => return Byte::Ends(b),
                0xc2..=0xdf => (b & 0x1f, 2, 1),
                0xe0..=0xef => (b & 0x0f, 3, 2),
                0xf0..=0xf4 => (b & 0x07, 4, 3),
                _ => return self.refuse(),
            }
        } else if b & 0xc0 != 0x80 {
            return self.refuse();
        } else {
            (self.bits << 6 | (b & 0x3f), self.needed >> 8, (self.needed & 0xff) - 1)
        };
        // Every code point the bytes so far can still become: `low` to `high`.
        let span = 6 * left;
        let low = bits << span;
        let high = low | ((1 << span) - 1);
        if high < shortest(len) || low > 0x10ffff || (low >= 0xd800 && high <= 0xdfff) {
            return self.refuse();
        }
        if left == 0 {
            *self = MbState::INITIAL;
            return Byte::Ends(bits);
        }
        *self = MbState { bits, needed: len << 8 | left };
        Byte::Continues
    }

    fn refuse(&mut self) -> Byte {
        *self = MbState::INITIAL;
        Byte::Refused
    }
}
