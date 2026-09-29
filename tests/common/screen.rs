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

/// Foreground threshold on the brightest channel. The renderer draws white
/// (0xFF) or alert red (0xFF,0x50,0x50) over a dark red (0x60,0,0) or black
/// fill, so anything at or above this is text and anything below is
/// background, with 0x30 of margin on both sides.
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

    /// Every cell row as `/system/bin/console` drew it, right-trimmed, blanks kept.
    pub fn console_rows(&self, font: &ConsoleFont) -> Vec<String> {
        let mut rows: Vec<String> = Vec::new();
        for cy in 0..self.height / GLYPH_H {
            let mut row = String::new();
            for cx in 0..self.width / GLYPH_W {
                let mut cell = [0u8; CELL];
                for r in 0..GLYPH_H {
                    for c in 0..GLYPH_W {
                        let p = self.pixels[(cy * GLYPH_H + r) * self.width + cx * GLYPH_W + c];
                        cell[r * GLYPH_W + c] = p[0].max(p[1]).max(p[2]);
                    }
                }
                row.push(font.lookup(&cell));
            }
            rows.push(row.trim_end().to_string());
        }
        rows
    }

    /// [`Ppm::console_rows`] joined, trailing blank rows dropped — the console's
    /// counterpart to [`Ppm::text`].
    pub fn console_text(&self, font: &ConsoleFont) -> String {
        let mut rows = self.console_rows(font);
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

    /// How many rows of text are on the panel, counted without decoding a
    /// glyph: no firmware font is committed here, so a row one drew is
    /// invisible to [`Ppm::text`].
    ///
    /// A band taller than the pitch is a logo or two rows that touch, and
    /// neither is one row of text; the pitch is the median distance between
    /// band tops, because a stray scanline sets a minimum of one and drags a
    /// mean as well. A panel with fewer than two bands has no pitch to take,
    /// and is refused rather than counted as none.
    pub fn text_row_bands(&self) -> Result<usize, String> {
        let mut bands: Vec<(usize, usize)> = Vec::new();
        let mut top = None;
        for y in 0..self.height {
            let lit = (0..self.width)
                .any(|x| self.pixels[y * self.width + x].iter().any(|c| *c >= FG_THRESHOLD));
            match (lit, top) {
                (true, None) => top = Some(y),
                (false, Some(from)) => {
                    bands.push((from, y));
                    top = None;
                }
                _ => {}
            }
        }
        if let Some(from) = top {
            bands.push((from, self.height));
        }
        let mut gaps: Vec<usize> = bands.windows(2).map(|pair| pair[1].0 - pair[0].0).collect();
        if gaps.is_empty() {
            return Err(format!(
                "the panel carries {} band(s) of lit scanlines, too few to take a row pitch from",
                bands.len()
            ));
        }
        gaps.sort_unstable();
        let pitch = gaps[gaps.len() / 2];
        let rows = bands.iter().filter(|(from, to)| to - from <= pitch).count();
        if rows == 0 {
            return Err(format!(
                "every one of the panel's {} band(s) is taller than the {pitch}-pixel pitch, so \
                 none of them is a row of text",
                bands.len()
            ));
        }
        Ok(rows)
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

    /// Whether every pixel matches `other`.
    pub fn identical_to(&self, other: &Ppm) -> bool {
        self.width == other.width && self.height == other.height && self.pixels == other.pixels
    }
}

/// Cells of the console's font, in the alpha values it blits.
const CELL: usize = GLYPH_W * GLYPH_H;

/// The font `/system/bin/console` and `/system/bin/terminal` draw with — 8x16 anti-aliased
/// alpha, not the kernel's 1-bit table.
///
/// The two decoders exist for the same reason and read the same way: a glyph on
/// screen is a bit-exact function of the table the drawer used, so decoding
/// against *that* table makes a screen assertion an ordinary string assertion.
/// The table is rebuilt here by [`toyos_build::assets::console_font`], the same
/// producer that puts it on ROOT.
///
/// Exact, not nearest-match, and that is a property of the blend rather than a
/// tolerance: `font::Font::draw_char` computes `(fg*a + bg*(255-a))/255` per
/// channel, so white on black is `a` and black on white — the cursor cell — is
/// its complement. Both are looked up.
///
/// **It is also the discriminator that keeps the console tests non-vacuous.**
/// A boot checkpoint paints the same kernel log lines from the same ring, in
/// `font8x16.bin`. Those cells are the *thresholded* form of these, so they
/// decode to [`UNKNOWN`] here and these decode to `UNKNOWN` there: "the console
/// rendered the log" and "the console never ran and the kernel's paint is still
/// up" cannot be confused for one another.
pub struct ConsoleFont {
    pub(crate) by_cell: HashMap<[u8; CELL], char>,
}

impl ConsoleFont {
    pub fn load() -> ConsoleFont {
        let raw = toyos_build::assets::console_font(&super::compile::repo_root());
        let width = u16::from_le_bytes([raw[0], raw[1]]) as usize;
        let height = u16::from_le_bytes([raw[2], raw[3]]) as usize;
        assert_eq!(
            (width, height),
            (GLYPH_W, GLYPH_H),
            "the console font is not the 8x16 cell this decoder grids for"
        );
        let count = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
        let alpha = 8 + count * 4;

        let mut by_cell: HashMap<[u8; CELL], char> = HashMap::new();
        let mut ascii_clash: Vec<(char, char)> = Vec::new();
        for i in 0..count {
            let cp = u32::from_le_bytes([
                raw[8 + i * 4],
                raw[9 + i * 4],
                raw[10 + i * 4],
                raw[11 + i * 4],
            ]);
            // C0 and C1 have no glyph and all rasterize blank, which would make
            // a space decode as whichever control code sorted first.
            if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
                continue;
            }
            let Some(ch) = char::from_u32(cp) else { continue };
            let mut cell = [0u8; CELL];
            cell.copy_from_slice(&raw[alpha + i * CELL..alpha + (i + 1) * CELL]);
            // Lowest codepoint wins, so U+00A0 does not take the blank cell
            // away from a space. A clash *inside* printable ASCII would make
            // every assertion in the suite ambiguous, so it is refused here
            // rather than decoded into whichever codepoint sorted first.
            if let Some(&first) = by_cell.get(&cell) {
                if (0x20..0x7F).contains(&cp) && (0x20..0x7F).contains(&(first as u32)) {
                    ascii_clash.push((first, ch));
                }
                continue;
            }
            by_cell.insert(cell, ch);
        }
        assert!(
            ascii_clash.is_empty(),
            "the console font rasterizes these printable ASCII pairs identically at \
             8x16, so a decoded screen cannot say which was drawn: {ascii_clash:?}"
        );
        ConsoleFont { by_cell }
    }

    fn lookup(&self, cell: &[u8; CELL]) -> char {
        if let Some(&ch) = self.by_cell.get(cell) {
            return ch;
        }
        // The cursor cell, drawn with foreground and background swapped.
        let mut inverted = [0u8; CELL];
        for (dst, &src) in inverted.iter_mut().zip(cell.iter()) {
            *dst = 255 - src;
        }
        *self.by_cell.get(&inverted).unwrap_or(&UNKNOWN)
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
