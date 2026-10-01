//! Where the window puts everything, and at which cell size.
//!
//! **Nothing here is a fixed pixel.** [`Layout`] is computed from the surface
//! every frame, so the window resizes: the eight columns and four rows divide
//! what there is and carry the remainder a pixel at a time, and the type is
//! chosen per region from the four cell sizes `build.rs` bakes.

use font::Font;

use crate::app::Mode;

/// The size the window opens at.
pub const OPEN_W: u32 = 600;
pub const OPEN_H: u32 = 440;

/// Below this the keys stop being keys. Asked of the window as a minimum, and
/// applied to the layout as a floor as well, because a compositor is free to
/// ignore the request.
pub const MIN_W: i32 = 460;
pub const MIN_H: i32 = 360;

const COLS: usize = 8;
const ROWS: usize = 4;
/// Columns in the scientific block; the pad is the rest.
const LEFT_COLS: usize = 3;

/// The longest key face, in characters — what the key type has to fit.
pub const KEY_CHARS: i32 = 3;

/// Half-open, like every other rectangle in this repository.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn inset(&self, by: i32) -> Rect {
        Rect { x: self.x + by, y: self.y + by, w: self.w - 2 * by, h: self.h - 2 * by }
    }
}

/// Divide `total` into `gaps.len() + 1` tracks separated by those gaps.
///
/// The remainder is handed out a pixel at a time to the leading tracks rather
/// than left at one end, so the last track ends exactly where the space does at
/// every window size — which is the whole of what "the keys still line up"
/// means once nothing is a constant.
fn tracks(start: i32, total: i32, gaps: &[i32]) -> Vec<(i32, i32)> {
    let n = gaps.len() as i32 + 1;
    let total = total.max(n);
    let mut gaps: Vec<i32> = gaps.to_vec();
    let mut gap_sum: i32 = gaps.iter().sum();
    // **Gaps give way before tracks do.** A one-pixel key is still a key; a gap
    // that pushes the last key past the end of the row is not a gap. Without
    // this the eight columns overflowed their strip on any window narrow enough
    // that the seams cost more than the keys.
    if gap_sum > total - n {
        let each = (total - n).max(0) / gaps.len().max(1) as i32;
        for g in gaps.iter_mut() {
            *g = (*g).min(each);
        }
        gap_sum = gaps.iter().sum();
    }
    let avail = total - gap_sum;
    let size = avail / n;
    let extra = avail % n;
    let mut out = Vec::with_capacity(n as usize);
    let mut at = start;
    for i in 0..n {
        let w = size + i32::from(i < extra);
        out.push((at, w));
        at += w + gaps.get(i as usize).copied().unwrap_or(0);
    }
    out
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi)
}

/// Where everything goes, for one surface size.
pub struct Layout {
    pub tabs: [Rect; 2],
    /// The right end of the tab strip, where the mode's own note sits.
    pub strip: Rect,
    pub display: Rect,
    pub message: Rect,
    pub panel: Rect,
    pub keys: [Rect; COLS * ROWS],
    pub pad: i32,
}

impl Layout {
    pub fn new(width: i32, height: i32) -> Layout {
        let w = width.max(MIN_W);
        let h = height.max(MIN_H);
        let margin = clamp(w.min(h) / 28, 8, 22);
        let cw = w - 2 * margin;
        let ch = h - 2 * margin;

        let vgap = clamp(ch / 45, 4, 12);
        let tab_h = clamp(ch * 10 / 100, 24, 44);
        let msg_h = clamp(ch * 7 / 100, 14, 26);
        let rest = (ch - tab_h - msg_h - 3 * vgap).max(4 * ROWS as i32);
        let grid_h = rest * 3 / 5;
        let display_h = rest - grid_h;

        let tab_y = margin;
        let display_y = tab_y + tab_h + vgap;
        let message_y = display_y + display_h + vgap;
        let grid_y = message_y + msg_h + vgap;

        let tab_w = clamp(cw / 7, 52, 96);
        let tab_gap = clamp(cw / 80, 4, 10);
        let tabs = [
            Rect { x: margin, y: tab_y, w: tab_w, h: tab_h },
            Rect { x: margin + tab_w + tab_gap, y: tab_y, w: tab_w, h: tab_h },
        ];

        let gap = clamp(cw / 80, 4, 9);
        // The scientific block and the pad read as two, so the seam between
        // them is wider than the seams inside either.
        let block = gap * 3;
        let col_gaps: Vec<i32> =
            (0..COLS - 1).map(|i| if i == LEFT_COLS - 1 { block } else { gap }).collect();
        let cols = tracks(margin, cw, &col_gaps);
        let rows = tracks(grid_y, grid_h, &vec![gap; ROWS - 1]);

        let mut keys = [Rect { x: 0, y: 0, w: 0, h: 0 }; COLS * ROWS];
        for (i, key) in keys.iter_mut().enumerate() {
            let (x, kw) = cols[i % COLS];
            let (y, kh) = rows[i / COLS];
            *key = Rect { x, y, w: kw, h: kh };
        }

        // The keys sit on a panel that stands a little proud of them. Never
        // more than half the vertical gap, or on a short window the panel
        // reaches up into the message line.
        let outset = clamp(vgap / 2, 2, gap);
        Layout {
            tabs,
            strip: Rect { x: margin, y: tab_y, w: cw, h: tab_h },
            display: Rect { x: margin, y: display_y, w: cw, h: display_h },
            message: Rect { x: margin, y: message_y, w: cw, h: msg_h },
            panel: Rect {
                x: margin - outset,
                y: grid_y - outset,
                w: cw + 2 * outset,
                h: grid_h + 2 * outset,
            },
            keys,
            pad: clamp(cw / 60, 6, 14),
        }
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Target> {
        for (i, mode) in [Mode::Calc, Mode::Prog].into_iter().enumerate() {
            if self.tabs[i].contains(x, y) {
                return Some(Target::Tab(mode));
            }
        }
        self.keys.iter().position(|k| k.contains(x, y)).map(Target::Key)
    }
}

/// What the pointer is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Tab(Mode),
    Key(usize),
}

/// The four cell sizes, largest first. A result that will not fit at one is
/// drawn at the next; nothing is ever cut to make it fit.
pub struct Fonts {
    scaled: [Font; 4],
}

impl Fonts {
    pub fn load() -> Fonts {
        Fonts {
            scaled: [
                Font::from_prebuilt(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/JetBrainsMono-Regular-12x24.font"
                ))),
                Font::from_prebuilt(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/JetBrainsMono-Regular-10x20.font"
                ))),
                Font::from_prebuilt(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/JetBrainsMono-Regular-8x16.font"
                ))),
                Font::from_prebuilt(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/JetBrainsMono-Regular-6x12.font"
                ))),
            ],
        }
    }

    pub fn smallest(&self) -> &Font {
        &self.scaled[3]
    }

    /// The largest cell that puts `chars` characters inside `w` by `h`.
    pub fn fitting(&self, chars: i32, w: i32, h: i32) -> &Font {
        self.scaled
            .iter()
            .find(|f| chars * f.width() as i32 <= w && f.height() as i32 <= h)
            .unwrap_or_else(|| self.smallest())
    }

    /// The largest cell that draws `text` whole in as few rows as it can, up to
    /// `lines`. Shrinking comes first and wrapping second, and if the smallest
    /// cell still needs more rows than that it gets them: the alternative is
    /// cutting digits off a number, which this never does.
    pub fn fit(&self, text: &str, width: i32, lines: usize) -> (&Font, Vec<String>) {
        let count = text.chars().count();
        for allowed in 1..=lines {
            for f in &self.scaled {
                let per = (width / f.width() as i32).max(1) as usize;
                if count.div_ceil(per) <= allowed {
                    return (f, wrap(text, per));
                }
            }
        }
        let f = self.smallest();
        let per = (width / f.width() as i32).max(1) as usize;
        (f, wrap(text, per))
    }
}

fn wrap(text: &str, per: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(per).map(|c| c.iter().collect()).collect()
}

/// Every character outside ASCII that the panel can put on the screen.
///
/// The same set `build.rs` bakes beyond Latin-1, plus the Latin-1 signs the
/// font always carries. A glyph nothing baked draws as `?`, which is a button
/// with a wrong face on it and nothing that fails — so the test below is what
/// makes the two lists agree.
#[cfg(test)]
const DRAWABLE_NON_ASCII: &[char] = &['\u{00B1}', '\u{00D7}', '\u{00F7}', 'π', '←', '−', '√', '≈'];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{CALC_BUTTONS, PROG_BUTTONS};
    use crate::error::EvalError;
    use crate::num::APPROX;
    use crate::prog::Base;

    /// Sizes the layout has to hold: the one it opens at, its own floor, one
    /// below that floor, a tall narrow one, a wide short one, and a screen.
    const SIZES: &[(i32, i32)] = &[
        (OPEN_W as i32, OPEN_H as i32),
        (MIN_W, MIN_H),
        (320, 240),
        (480, 900),
        (1400, 400),
        (1920, 1080),
    ];

    /// At every size the keys tile their strip: inside the panel, no two
    /// overlapping, and the last column ending exactly where the space does.
    #[test]
    fn the_keys_tile_the_panel_at_every_size() {
        for &(w, h) in SIZES {
            let l = Layout::new(w, h);
            let (fw, fh) = (w.max(MIN_W), h.max(MIN_H));
            for (i, a) in l.keys.iter().enumerate() {
                assert!(a.w > 0 && a.h > 0, "key {i} is empty at {w}x{h}");
                assert!(a.x >= l.panel.x, "key {i} left of the panel at {w}x{h}");
                assert!(a.x + a.w <= l.panel.x + l.panel.w, "key {i} past the panel at {w}x{h}");
                assert!(a.y + a.h <= fh, "key {i} past the bottom at {w}x{h}");
                for (j, b) in l.keys.iter().enumerate().skip(i + 1) {
                    let apart = a.x + a.w <= b.x
                        || b.x + b.w <= a.x
                        || a.y + a.h <= b.y
                        || b.y + b.h <= a.y;
                    assert!(apart, "keys {i} and {j} overlap at {w}x{h}");
                }
            }
            // The row spans the content exactly, remainder pixels and all.
            let first = l.keys[0];
            let last = l.keys[COLS - 1];
            assert_eq!(first.x, l.display.x, "the grid and the display disagree at {w}x{h}");
            assert_eq!(
                last.x + last.w,
                l.display.x + l.display.w,
                "the grid stops short of the display at {w}x{h}"
            );
            let bottom = l.keys[COLS * ROWS - 1];
            assert!(bottom.y + bottom.h <= fh, "the grid runs off the bottom at {w}x{h}");
            // And the strips above it are in order and do not overlap.
            assert!(l.strip.y + l.strip.h <= l.display.y, "tabs into the display at {w}x{h}");
            assert!(l.display.y + l.display.h <= l.message.y, "display into the message at {w}x{h}");
            assert!(l.message.y + l.message.h <= l.panel.y, "message into the keys at {w}x{h}");
            assert!(l.panel.x >= 0 && l.panel.x + l.panel.w <= fw, "the panel escapes at {w}x{h}");
        }
    }

    #[test]
    fn hit_testing_follows_the_layout_at_every_size() {
        for &(w, h) in SIZES {
            let l = Layout::new(w, h);
            for i in 0..COLS * ROWS {
                let k = l.keys[i];
                assert_eq!(l.hit(k.x + k.w / 2, k.y + k.h / 2), Some(Target::Key(i)));
            }
            assert_eq!(l.hit(l.tabs[0].x + 2, l.tabs[0].y + 2), Some(Target::Tab(Mode::Calc)));
            assert_eq!(l.hit(l.tabs[1].x + 2, l.tabs[1].y + 2), Some(Target::Tab(Mode::Prog)));
            assert_eq!(l.hit(l.display.x, l.display.y + 2), None);
            assert_eq!(l.hit(-1, -1), None);
            // The seam between the two blocks belongs to neither.
            let seam = l.keys[LEFT_COLS - 1];
            assert_eq!(l.hit(seam.x + seam.w + 1, seam.y + seam.h / 2), None);
        }
    }

    /// A forty-digit answer is drawn whole at some cell size, which is the
    /// whole point of carrying four of them.
    #[test]
    fn the_longest_answer_is_never_cut() {
        let fonts = Fonts::load();
        let longest = format!("{APPROX}-1.{}e-100", "9".repeat(39));
        for &(w, h) in SIZES {
            let inner = Layout::new(w, h).display.inset(Layout::new(w, h).pad);
            let (_, lines) = fonts.fit(&longest, inner.w, 2);
            assert_eq!(lines.concat(), longest, "digits went missing at {w}x{h}");
            assert!(lines.len() <= 2, "the answer needed {} lines at {w}x{h}", lines.len());
        }
        // One that genuinely cannot fit is wrapped rather than shortened.
        let absurd = "8".repeat(400);
        let (_, lines) = fonts.fit(&absurd, 200, 2);
        assert_eq!(lines.concat(), absurd);
    }

    /// Nothing the panel can draw names a glyph the font does not carry.
    #[test]
    fn every_face_the_panel_shows_was_baked() {
        let mut faces: Vec<String> = Vec::new();
        for layout in [&CALC_BUTTONS, &PROG_BUTTONS] {
            faces.extend(layout.iter().map(|b| b.label.to_string()));
        }
        faces.extend(["Calc", "Prog", "RAD", "DEG"].map(String::from));
        faces.extend([Base::Hex, Base::Dec, Base::Bin].map(|b| b.label().to_string()));
        faces.push(APPROX.to_string());
        for error in [
            EvalError::Parse("× needs a value before it".into()),
            EvalError::DivisionByZero,
            EvalError::NegativeRoot,
            EvalError::LogOfNonPositive,
            EvalError::ZeroToNonPositivePower,
            EvalError::NegativeBaseFractionalExponent,
            EvalError::Overflow,
            EvalError::ArgumentTooLarge,
            EvalError::NotAnInteger,
            EvalError::OutOfRange,
            EvalError::NegativeShift,
            EvalError::TooDeep,
            EvalError::TooLong,
        ] {
            faces.push(error.message());
        }
        for face in &faces {
            for ch in face.chars() {
                let baked = ch.is_ascii_graphic() || ch == ' ' || DRAWABLE_NON_ASCII.contains(&ch);
                assert!(baked, "{ch:?} (U+{:04X}) in {face:?} is not in the baked font", ch as u32);
            }
        }
        // The message strip is one row tall and draws the first line it is
        // given, so a refusal too long for the narrowest window would be a
        // sentence cut in half. Every one of them fits.
        let strip = Layout::new(MIN_W, MIN_H).message;
        let cell = Fonts::load().smallest().width() as i32;
        for face in faces.iter().filter(|f| f.contains(' ')) {
            let width = face.chars().count() as i32 * cell;
            assert!(width <= strip.w, "{face:?} is {width}px in a {}px message strip", strip.w);
        }
        // And the set is not idle: every character in it is on a face, so a
        // codepoint that stops being drawn stops being baked.
        for &ch in DRAWABLE_NON_ASCII {
            assert!(faces.iter().any(|f| f.contains(ch)), "{ch:?} is baked and nothing draws it");
        }
    }

    /// No key face is wider than the cell it is drawn in, at any size.
    #[test]
    fn every_key_face_fits_its_key() {
        let fonts = Fonts::load();
        let widest = CALC_BUTTONS
            .iter()
            .chain(PROG_BUTTONS.iter())
            .map(|b| b.label.chars().count())
            .max()
            .expect("both layouts have keys");
        assert!(widest as i32 <= KEY_CHARS, "a key face is {widest} characters wide");
        for &(w, h) in SIZES {
            let key = Layout::new(w, h).keys[0];
            let f = fonts.fitting(KEY_CHARS, key.w - 8, key.h - 8);
            assert!(
                KEY_CHARS * f.width() as i32 <= key.w,
                "a three-character face does not fit a {}x{} key at {w}x{h}",
                key.w,
                key.h
            );
        }
    }

    /// The tracks a row divides into span it exactly, whatever the remainder.
    #[test]
    fn tracks_never_lose_a_pixel() {
        for total in 40..400 {
            for gap in [0, 1, 4, 9] {
                let gaps = vec![gap; COLS - 1];
                let out = tracks(7, total, &gaps);
                assert_eq!(out.len(), COLS);
                let last = out[COLS - 1];
                assert_eq!(last.0 + last.1, 7 + total, "total={total} gap={gap}");
                let spread = out.iter().map(|t| t.1).max().unwrap()
                    - out.iter().map(|t| t.1).min().unwrap();
                assert!(spread <= 1, "tracks differ by {spread} at total={total} gap={gap}");
            }
        }
    }
}
