//! A monospace font as one cell of 8-bit coverage per glyph, drawn into a
//! pixel buffer.
//!
//! [`Font::from_ttf`] rasterizes a TTF into cells of any size, which is how a
//! program draws text at a display's scale; the build runs the same producer
//! for the 8x16 rasters it ships, which [`Font::from_prebuilt`] reads.

#[cfg(feature = "ttf")]
use std::collections::BTreeSet;

/// The desktop's text cell at scale 1, in pixels: every glyph of its
/// monospace font fits one.
pub const CELL: (usize, usize) = (8, 16);

#[derive(Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub trait Canvas {
    fn put_pixel(&self, x: usize, y: usize, color: Color);
}

pub struct Font {
    width: usize,
    height: usize,
    /// Sorted codepoints for binary search lookup.
    codepoints: Vec<u32>,
    /// Flattened glyph bitmaps: `codepoints.len() * width * height` alpha bytes.
    data: Vec<u8>,
}

impl Font {
    /// `ttf`'s glyphs for [`codepoints`], each in a cell of `width` by
    /// `height` pixels.
    #[cfg(feature = "ttf")]
    pub fn from_ttf(ttf: &[u8], width: usize, height: usize) -> Result<Self, &'static str> {
        let font = fontdue::Font::from_bytes(ttf, fontdue::FontSettings::default())?;
        let codepoints: Vec<u32> = codepoints(&font).into_iter().collect();
        let data = rasterize_cells(&font, &codepoints, width, height);
        Ok(Self { width, height, codepoints, data })
    }

    /// The format [`from_prebuilt`](Self::from_prebuilt) reads.
    pub fn prebuilt(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.codepoints.len() * 4 + self.data.len());
        out.extend((self.width as u16).to_le_bytes());
        out.extend((self.height as u16).to_le_bytes());
        out.extend((self.codepoints.len() as u32).to_le_bytes());
        for &cp in &self.codepoints {
            out.extend(cp.to_le_bytes());
        }
        out.extend(&self.data);
        out
    }

    /// The cell of `ch`'s coverage, row by row, if the font has it.
    pub fn coverage(&self, ch: char) -> Option<&[u8]> {
        let idx = self.codepoints.binary_search(&(ch as u32)).ok()?;
        let cell = self.width * self.height;
        Some(&self.data[idx * cell..][..cell])
    }

    /// Load a pre-rasterized font from the binary format produced at build time.
    ///
    /// Format: [u16 width][u16 height][u32 glyph_count][codepoints...][alpha data...]
    pub fn from_prebuilt(bytes: &[u8]) -> Self {
        let width = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        let height = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        let glyph_count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;

        let cp_start = 8;
        let cp_end = cp_start + glyph_count * 4;
        let mut codepoints = Vec::with_capacity(glyph_count);
        for i in 0..glyph_count {
            let off = cp_start + i * 4;
            codepoints.push(u32::from_le_bytes([
                bytes[off],
                bytes[off + 1],
                bytes[off + 2],
                bytes[off + 3],
            ]));
        }

        let data = bytes[cp_end..].to_vec();

        Self { width, height, codepoints, data }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn draw_char(
        &self,
        canvas: &impl Canvas,
        x: usize,
        y: usize,
        ch: char,
        fg: Color,
        bg: Color,
    ) {
        let idx = self
            .codepoints
            .binary_search(&(ch as u32))
            .unwrap_or_else(|_| {
                self.codepoints
                    .binary_search(&0x3F) // '?'
                    .unwrap_or(0)
            });

        let base = idx * self.width * self.height;

        for row in 0..self.height {
            for col in 0..self.width {
                let alpha = self.data[base + row * self.width + col] as u16;
                let inv = 255 - alpha;
                let r = (fg.r as u16 * alpha + bg.r as u16 * inv) / 255;
                let g = (fg.g as u16 * alpha + bg.g as u16 * inv) / 255;
                let b = (fg.b as u16 * alpha + bg.b as u16 * inv) / 255;
                canvas.put_pixel(
                    x + col,
                    y + row,
                    Color { r: r as u8, g: g as u8, b: b as u8 },
                );
            }
        }
    }

    pub fn draw_string(
        &self,
        canvas: &impl Canvas,
        x: usize,
        y: usize,
        text: &str,
        fg: Color,
        bg: Color,
    ) {
        let mut cx = x;
        for ch in text.chars() {
            self.draw_char(canvas, cx, y, ch, fg, bg);
            cx += self.width;
        }
    }
}

#[cfg(feature = "ttf")]
/// Every codepoint a raster carries: Latin-1, box drawing and block elements,
/// and what each shipped layout types at every level or composes with a dead
/// key, where the font has a glyph for it.
///
/// A layout can ask for a character the font has no glyph for — a handful of
/// rare dead-key compositions and symbols do. Left out rather than rasterized
/// blank, so it falls back to `?` exactly as an uncovered codepoint always
/// has, instead of silently drawing nothing.
fn codepoints(font: &fontdue::Font) -> BTreeSet<u32> {
    use toyos_keymap::{Key, LAYOUTS, LEVELS};

    let mut out: BTreeSet<u32> = (0u32..=255).collect();
    out.extend(0x2500u32..=0x257F); // Box Drawing
    out.extend(0x2580u32..=0x259F); // Block Elements

    let mut typed = BTreeSet::new();
    let mut add_entry = |entry: &toyos_keymap::KeyEntry| {
        for level in 0..LEVELS {
            if let Key::Chars(s) = entry.level(level) {
                typed.extend(s.chars());
            }
        }
    };
    for layout in LAYOUTS {
        for usage in toyos_keymap::FIRST_USAGE..=toyos_keymap::LAST_USAGE {
            if let Some(entry) = layout.entry(usage) {
                add_entry(entry);
            }
        }
        add_entry(&layout.iso_key);
    }
    typed.extend(toyos_keymap::composed_chars());
    out.extend(typed.into_iter().filter(|&c| font.has_glyph(c)).map(|c| c as u32));
    out
}

#[cfg(feature = "ttf")]
/// Rasterize `codepoints` into `cell_width * cell_height` 8-bit coverage
/// cells, laid out one cell after another. The pixel size is the largest at
/// which every printable ASCII glyph fits its cell, so a fixed grid can be
/// blitted without per-glyph metrics.
fn rasterize_cells(font: &fontdue::Font, codepoints: &[u32], cell_width: usize, cell_height: usize) -> Vec<u8> {
    let mut px_size = cell_height as f32;
    loop {
        let lm = font.horizontal_line_metrics(px_size).expect("a font with horizontal line metrics");
        let asc = lm.ascent.ceil() as i32;
        let fits = (0x20u32..=0x7E).all(|ch| {
            let m = font.metrics(char::from_u32(ch).unwrap(), px_size);
            let glyph_top = asc - m.height as i32 - m.ymin;
            glyph_top >= 0 && (glyph_top as usize) + m.height <= cell_height && m.width <= cell_width
        });
        if fits {
            break;
        }
        px_size -= 0.25;
        assert!(px_size > 2.0, "could not find a font size that fits {cell_width}x{cell_height}");
    }

    let ascent = font.horizontal_line_metrics(px_size).expect("checked above").ascent.ceil() as i32;
    let mut data = vec![0u8; codepoints.len() * cell_width * cell_height];

    for (idx, &cp) in codepoints.iter().enumerate() {
        let Some(c) = char::from_u32(cp) else { continue };
        let (metrics, bitmap) = font.rasterize(c, px_size);
        if metrics.width == 0 || metrics.height == 0 {
            continue;
        }

        let x_offset = ((cell_width as i32 - metrics.width as i32) / 2).max(0) as usize;
        let glyph_top = ascent - metrics.height as i32 - metrics.ymin;
        let y_offset = glyph_top.max(0) as usize;
        let glyph_base = idx * cell_width * cell_height;

        for gy in 0..metrics.height {
            let cell_y = y_offset + gy;
            if cell_y >= cell_height {
                break;
            }
            for gx in 0..metrics.width {
                let cell_x = x_offset + gx;
                if cell_x >= cell_width {
                    break;
                }
                data[glyph_base + cell_y * cell_width + cell_x] = bitmap[gy * metrics.width + gx];
            }
        }
    }

    data
}

#[cfg(all(test, feature = "ttf"))]
mod tests {
    use super::*;

    fn ttf() -> Vec<u8> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/JetBrainsMono-Regular.ttf"))
            .expect("the committed JetBrains Mono")
    }

    /// The cell is what was asked for, and every printable ASCII glyph is
    /// inked inside it, at each scale's cell.
    #[test]
    fn every_scale_s_cell_holds_the_printable_glyphs() {
        let ttf = ttf();
        for (w, h) in [(8, 16), (10, 20), (12, 24), (14, 28), (16, 32)] {
            let font = Font::from_ttf(&ttf, w, h).unwrap();
            assert_eq!((font.width(), font.height()), (w, h));
            for ch in ('!'..='~').filter(|c| *c != '_' && *c != '.') {
                let cell = font.coverage(ch).unwrap_or_else(|| panic!("{ch:?} at {w}x{h}"));
                assert!(cell.iter().any(|&a| a > 128), "{ch:?} is blank at {w}x{h}");
            }
        }
    }

    /// Text drawn at twice the cell is rasterized at that size, not a doubled
    /// bitmap: a doubling leaves every pixel equal to its three neighbours in
    /// its 2x2 block, and a glyph drawn at size does not.
    #[test]
    fn a_larger_cell_is_rasterized_at_its_size() {
        let ttf = ttf();
        let one = Font::from_ttf(&ttf, 8, 16).unwrap();
        let two = Font::from_ttf(&ttf, 16, 32).unwrap();
        let doubled: Vec<u8> = (0..32)
            .flat_map(|y| (0..16).map(move |x| (x, y)))
            .map(|(x, y)| one.coverage('a').unwrap()[(y / 2) * 8 + x / 2])
            .collect();
        assert_ne!(two.coverage('a').unwrap(), &doubled[..]);
    }

    #[test]
    fn a_raster_survives_the_prebuilt_format() {
        let font = Font::from_ttf(&ttf(), 8, 16).unwrap();
        let read = Font::from_prebuilt(&font.prebuilt());
        assert_eq!((read.width(), read.height()), (8, 16));
        assert_eq!(read.coverage('g'), font.coverage('g'));
        assert_eq!(read.codepoints, font.codepoints);
    }

    #[test]
    fn a_file_that_is_not_a_font_is_refused() {
        assert!(Font::from_ttf(b"not a font", 8, 16).is_err());
    }
}
