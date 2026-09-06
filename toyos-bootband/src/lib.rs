//! The marks a boot leaves on the panel before it can say anything.
//!
//! There is a window between `ExitBootServices` and the kernel's first log
//! record in which this machine has **no channel at all**: the firmware console
//! is gone, `println!` dereferences a system table uefi-services has already
//! nulled, the kernel's own panel is not armed, and a fault has no IDT of ours
//! to report through, so it vectors into firmware's and dead-loops. A machine
//! that stops in there holds the loader's last line forever and says nothing
//! about which half of the window it stopped in.
//!
//! So each half writes one band of solid colour straight into the scanout, with
//! no service, no allocation and no formatter in it — the loader through the
//! firmware's identity map, the kernel from its entry stub before it has a
//! stack. Which bands a photograph carries is the answer.
//!
//! **They stay for the life of the boot.** The panel's glyph grid starts below
//! them ([`ROWS`]), so the first repaint does not wipe the only account of how
//! the machine got that far. Every colour here is under the brightness the
//! harness's decoder calls ink (`tests/common/screen.rs`'s `FG_THRESHOLD`,
//! `0x90`), asserted below, so the bands cost the text grid three rows and
//! change no character in it.

#![no_std]
#![forbid(unsafe_code)]

/// Pixel rows one band occupies: exactly one text row, so the grid below the
/// bands still starts on the 16-row boundary every decoder of this panel counts in.
pub const BAND_ROWS: usize = 16;

/// One band: which of the three it is, and what colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Band {
    index: usize,
    red: u8,
    green: u8,
    blue: u8,
}

impl Band {
    /// The first pixel row this band paints.
    pub const fn first_row(self) -> usize {
        self.index * BAND_ROWS
    }

    /// One past the last.
    pub const fn end_row(self) -> usize {
        self.first_row() + BAND_ROWS
    }

    /// The 32-bit pixel this band is, on a scanout of `format`: 0 is RGB and 1
    /// is BGR, which is `KernelArgs::gop_pixel_format`'s own encoding.
    pub const fn pixel(self, format: u32) -> u32 {
        let (r, g, b) = (self.red as u32, self.green as u32, self.blue as u32);
        if format == 0 {
            r | (g << 8) | (b << 16)
        } else {
            b | (g << 8) | (r << 16)
        }
    }

    /// The colour a reader of the panel compares against, in the order a
    /// screendump gives it.
    pub const fn rgb(self) -> [u8; 3] {
        [self.red, self.green, self.blue]
    }
}

/// The loader is about to call `ExitBootServices`. Alone on the panel, it means
/// that call never came back — which is a reset or a hang inside firmware, and
/// not this tree's code at all.
pub const EXITING: Band = Band { index: 0, red: 0x00, green: 0x00, blue: 0x80 };

/// It came back, the memory map is copied, and the loader is about to switch
/// `cr3` and jump. Without [`KERNEL`] below it, the jump is what did not arrive.
pub const EXITED: Band = Band { index: 1, red: 0x00, green: 0x80, blue: 0x00 };

/// The kernel's entry stub ran — before its stack switch, so this band is on
/// the panel even if the stack it is about to take is not mapped.
pub const KERNEL: Band = Band { index: 2, red: 0x00, green: 0x80, blue: 0x80 };

/// Every band, in the order they are painted and the order they sit on the panel.
pub const BANDS: [Band; 3] = [EXITING, EXITED, KERNEL];

/// Pixel rows the bands take off the top of the panel, which is where the
/// kernel's text grid begins.
pub const ROWS: usize = BANDS.len() * BAND_ROWS;

const _: () = {
    // Under the brightness `tests/common/screen.rs` calls ink, so a decoder
    // reads three blank rows and no glyph changes.
    let mut i = 0;
    while i < BANDS.len() {
        let b = BANDS[i];
        assert!(b.red < 0x90 && b.green < 0x90 && b.blue < 0x90);
        assert!(b.index == i);
        i += 1;
    }
    // A whole number of text rows, or the grid below them is off its boundary.
    assert!(ROWS.is_multiple_of(BAND_ROWS));
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The two formats put the same band on the wire two ways round, and a
    /// loader and a kernel reading one field must agree about which.
    #[test]
    fn a_band_is_the_same_colour_in_both_pixel_orders() {
        assert_eq!(EXITING.pixel(0), 0x0080_0000);
        assert_eq!(EXITING.pixel(1), 0x0000_0080);
        assert_eq!(KERNEL.pixel(0), 0x0080_8000);
        assert_eq!(KERNEL.pixel(1), 0x0000_8080);
    }

    /// Three bands, in order, touching, none overlapping, all inside `ROWS`.
    #[test]
    fn the_bands_tile_the_rows_they_claim() {
        assert_eq!(EXITING.first_row(), 0);
        assert_eq!(EXITING.end_row(), EXITED.first_row());
        assert_eq!(EXITED.end_row(), KERNEL.first_row());
        assert_eq!(KERNEL.end_row(), ROWS);
    }

    /// No two of them can be told apart by a reader that cannot tell them apart.
    #[test]
    fn no_two_bands_are_one_colour() {
        for (i, a) in BANDS.iter().enumerate() {
            for b in &BANDS[i + 1..] {
                assert_ne!(a.rgb(), b.rgb());
            }
        }
    }
}
