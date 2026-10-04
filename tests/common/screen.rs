//! Decode a QEMU screendump back into text.
//!
//! The panic console renders 1 bpp 8x16 glyphs with no anti-aliasing and no
//! scaling, so every cell on screen is a bit-exact copy of one of the 95
//! bitmaps in `kernel/src/drivers/panic_console/font8x16.bin`. This reads
//! *that same file*, so the table asserted against is by construction the
//! table the kernel blitted -- there is nothing for the two to drift on.
//!
//! Which makes screen assertions ordinary string assertions:
//! `screen.text().contains("PANIC:")`. Same discipline as the audio
//! gate: a decoded measurement, never a human looking at a picture.

use std::collections::HashMap;
use std::path::PathBuf;

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;
pub(crate) const FIRST_CH: u8 = 0x20;
const GLYPHS: usize = 95;

/// A cell that matches no glyph. Distinct from every decoded character, so an
/// assertion can never accidentally pass on undecodable pixels.
pub const UNKNOWN: char = '\u{fffd}';

/// Foreground threshold on the brightest channel. The renderer draws every
/// ink with a channel at 0x9E or above over a dark red (0x60,0,0) or black
/// fill, so anything at or above this is text and anything below is
/// background, with 0x0E and 0x30 of margin.
const FG_THRESHOLD: u8 = 0x90;

pub struct Ppm {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[u8; 3]>,
}

impl Ppm {
    /// Parse binary P6 with maxval 255, which is the only format QEMU's
    /// `screendump` emits.
    pub fn parse(bytes: &[u8]) -> Ppm {
        let mut pos = 0;
        let mut field = || {
            loop {
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                if bytes.get(pos) == Some(&b'#') {
                    while pos < bytes.len() && bytes[pos] != b'\n' {
                        pos += 1;
                    }
                    continue;
                }
                break;
            }
            let start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            String::from_utf8_lossy(&bytes[start..pos]).into_owned()
        };

        let magic = field();
        assert_eq!(magic, "P6", "screendump: expected binary PPM");
        let width: usize = field().parse().expect("ppm width");
        let height: usize = field().parse().expect("ppm height");
        let maxval: u32 = field().parse().expect("ppm maxval");
        assert_eq!(maxval, 255, "ppm: only 8-bit samples supported");
        let data = &bytes[pos + 1..];
        assert!(
            data.len() >= width * height * 3,
            "ppm: {} bytes of pixel data for {width}x{height}",
            data.len()
        );

        Ppm {
            width,
            height,
            pixels: data[..width * height * 3]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0], c[1], c[2]])
                .collect(),
        }
    }

    fn bit(&self, x: usize, y: usize) -> bool {
        let p = self.pixels[y * self.width + x];
        p[0].max(p[1]).max(p[2]) >= FG_THRESHOLD
    }

    /// Every cell row, right-trimmed, with the blank ones kept. Row `i` here
    /// is pixel rows `i * GLYPH_H ..`, which is what makes [`Ppm::row_fg`]
    /// addressable by the same index a text search returns.
    pub fn rows(&self) -> Vec<String> {
        let font = Font::load();
        let mut rows: Vec<String> = Vec::new();
        for cy in 0..self.height / GLYPH_H {
            let mut row = String::new();
            for cx in 0..self.width / GLYPH_W {
                let mut cell = [0u8; GLYPH_H];
                for (r, slot) in cell.iter_mut().enumerate() {
                    let mut bits = 0u8;
                    for c in 0..GLYPH_W {
                        if self.bit(cx * GLYPH_W + c, cy * GLYPH_H + r) {
                            bits |= 0x80 >> c;
                        }
                    }
                    *slot = bits;
                }
                row.push(font.lookup(&cell));
            }
            rows.push(row.trim_end().to_string());
        }
        rows
    }

    /// Reconstruct the text grid. Rows are right-trimmed and trailing blank
    /// rows dropped, so a mostly-empty screen decodes to a short string.
    pub fn text(&self) -> String {
        let mut rows = self.rows();
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// The colour of the first foreground pixel in cell row `cy`, or `None`
    /// for a blank row.
    ///
    /// [`Ppm::bit`] deliberately throws hue away — it has to, or a red glyph
    /// would not decode — so nothing in `text()` can tell the alert highlight
    /// from ordinary white. This is where that claim gets checked.
    pub fn row_fg(&self, cy: usize) -> Option<[u8; 3]> {
        for y in cy * GLYPH_H..(cy + 1) * GLYPH_H {
            for x in 0..self.width {
                let p = self.pixels[y * self.width + x];
                if p[0].max(p[1]).max(p[2]) >= FG_THRESHOLD {
                    return Some(p);
                }
            }
        }
        None
    }

    /// For every cell row carrying `needle`, that row and the colour of the
    /// first foreground pixel of `needle`'s own cells on it, `None` where they
    /// are blank. A row's head and its text are drawn apart, so this — and not
    /// [`Ppm::row_fg`] — is the colour of the text.
    pub fn fg_of(&self, needle: &str) -> Vec<(String, Option<[u8; 3]>)> {
        let mut found = Vec::new();
        for (cy, row) in self.rows().into_iter().enumerate() {
            let Some(at) = row.find(needle) else { continue };
            let cx = row[..at].chars().count();
            let cells = cx..cx + needle.chars().count();
            let fg = (cy * GLYPH_H..(cy + 1) * GLYPH_H)
                .flat_map(|y| (cells.start * GLYPH_W..cells.end * GLYPH_W).map(move |x| (x, y)))
                .map(|(x, y)| self.pixels[y * self.width + x])
                .find(|p| p[0].max(p[1]).max(p[2]) >= FG_THRESHOLD);
            found.push((row, fg));
        }
        found
    }

    /// The fill colour, read from the bottom-right pixel. The renderer paints
    /// at most `MAX_ROWS` rows and never the last column of a glyph cell, so
    /// this corner carries the fill and nothing else.
    pub fn fill(&self) -> [u8; 3] {
        self.pixels[self.width * self.height - 1]
    }

    /// The index of the first cell row containing `needle`.
    pub fn row_index(&self, needle: &str) -> Option<usize> {
        self.rows().iter().position(|r| r.contains(needle))
    }
}

pub struct Font {
    by_bitmap: HashMap<[u8; GLYPH_H], char>,
}

impl Font {
    pub fn path() -> PathBuf {
        super::compile::repo_root().join("kernel/src/drivers/panic_console/font8x16.bin")
    }

    pub fn load() -> Font {
        let raw = std::fs::read(Font::path()).expect("font8x16.bin not found");
        assert_eq!(raw.len(), GLYPHS * GLYPH_H, "font8x16.bin has the wrong size");
        let mut by_bitmap = HashMap::new();
        for i in 0..GLYPHS {
            let mut g = [0u8; GLYPH_H];
            g.copy_from_slice(&raw[i * GLYPH_H..(i + 1) * GLYPH_H]);
            // A duplicate would make decoding ambiguous. Nothing on the
            // generator side checks for it, so this assert is the only check
            // there is — it runs on every suite via screen_decoder.
            assert!(
                by_bitmap.insert(g, (FIRST_CH + i as u8) as char).is_none(),
                "font8x16.bin: two glyphs share a bitmap"
            );
        }
        Font { by_bitmap }
    }

    fn lookup(&self, cell: &[u8; GLYPH_H]) -> char {
        *self.by_bitmap.get(cell).unwrap_or(&UNKNOWN)
    }
}
