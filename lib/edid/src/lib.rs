//! The panel's image size out of a display's E-EDID base block.
//!
//! Every `§` and table here is VESA's *Enhanced EDID Standard*, Release A
//! Rev. 2 (EDID 1.4). The bytes come from outside the trust of whoever reads
//! them, firmware or a device, so a block that is short, has no header or does
//! not sum to zero is refused by name. Nothing but the size is read: a mode
//! comes from the device or the firmware, never from here.

#![no_std]

/// One EDID block: the base block, or one extension.
pub const BLOCK: usize = 128;

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const SCREEN_W_CM: usize = 0x15;
const SCREEN_H_CM: usize = 0x16;
/// The first of §3.10's four 18-byte descriptors, which carries the
/// preferred timing.
const FIRST_DESCRIPTOR: usize = 0x36;

/// Why a block was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Fewer bytes than one block.
    Short { len: usize },
    /// The first eight bytes are not §3.3's header.
    Header,
    /// The block's bytes do not sum to zero modulo 256 (§3.11).
    Checksum { sum: u8 },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short { len } => write!(f, "{len} byte(s) are less than one {BLOCK}-byte block"),
            Self::Header => write!(f, "the block does not open with the EDID header"),
            Self::Checksum { sum } => write!(f, "the block sums to {sum:#04x}, not zero"),
        }
    }
}

/// What a block says of the panel's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageSize {
    Millimetres { width: u32, height: u32 },
    /// Table 3.12: one of bytes 0x15/0x16 zero makes the other an aspect
    /// ratio, which says nothing of size.
    AspectRatio,
    /// Table 3.12: both zero, "unknown or undefined".
    Unknown,
}

/// A base block that has its header and sums to zero.
#[derive(Clone, Copy)]
pub struct BaseBlock<'a>(&'a [u8; BLOCK]);

impl<'a> BaseBlock<'a> {
    /// The first [`BLOCK`] bytes of `bytes`; whatever follows is extensions,
    /// which nothing here reads.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Refusal> {
        let block: &[u8; BLOCK] = bytes
            .get(..BLOCK)
            .and_then(|b| b.try_into().ok())
            .ok_or(Refusal::Short { len: bytes.len() })?;
        if block[..HEADER.len()] != HEADER {
            return Err(Refusal::Header);
        }
        let sum = block.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
        if sum != 0 {
            return Err(Refusal::Checksum { sum });
        }
        Ok(Self(block))
    }

    /// The panel's size: the first descriptor's, where it is a detailed
    /// timing that states one (Table 3.21 notes 11 and 12); otherwise bytes
    /// 0x15/0x16 in centimetres (Table 3.12). A timing's 0 mm is "undefined"
    /// (note 17), so it falls through to the base block.
    pub fn image_size(&self) -> ImageSize {
        let d = &self.0[FIRST_DESCRIPTOR..FIRST_DESCRIPTOR + 18];
        // §3.10: a detailed timing (§3.10.2) has a nonzero pixel clock; a
        // display descriptor (§3.10.3) opens with two zero bytes.
        if d[0] != 0 || d[1] != 0 {
            let width = u32::from(d[12]) | u32::from(d[14] >> 4) << 8;
            let height = u32::from(d[13]) | u32::from(d[14] & 0x0f) << 8;
            if width != 0 && height != 0 {
                return ImageSize::Millimetres { width, height };
            }
        }
        match (self.0[SCREEN_W_CM], self.0[SCREEN_H_CM]) {
            (0, 0) => ImageSize::Unknown,
            (0, _) | (_, 0) => ImageSize::AspectRatio,
            (w, h) => ImageSize::Millimetres { width: u32::from(w) * 10, height: u32::from(h) * 10 },
        }
    }
}

#[cfg(test)]
mod tests;
