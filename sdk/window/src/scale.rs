//! A display scale as the window protocol carries it, and the one rounding
//! every party converts pixels with.
//!
//! **Rounding is half away from zero, in integers:** `l` logical pixels at a
//! scale of `n/120` are `sign(l) * ((|l| * n + 60) / 120)` physical pixels,
//! and every other conversion between two scales rounds the same way. A
//! rectangle converts by its corners, never by its origin and extent, so two
//! rectangles that touch before a conversion touch after it.

use core::fmt;
use core::str::FromStr;

/// How many physical pixels one logical pixel is, in 120ths: the denominator
/// Wayland's fractional-scale-v1 protocol publishes, so 1.25, 1.5 and 1.75 are
/// exact and integer scales take the same path. From 1 to 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scale(u16);

/// The denominator of every [`Scale`].
pub const DENOMINATOR: u32 = 120;

impl Scale {
    pub const ONE: Self = Self(120);
    /// The largest scale the protocol carries.
    pub const MAX: Self = Self(480);

    /// The scale a peer sent as `n/120`: `None` outside 1 to 4.
    pub const fn from_wire(n: u32) -> Option<Self> {
        if n >= Self::ONE.0 as u32 && n <= Self::MAX.0 as u32 {
            Some(Self(n as u16))
        } else {
            None
        }
    }

    /// The numerator over [`DENOMINATOR`].
    pub const fn wire(self) -> u32 {
        self.0 as u32
    }

    /// As a factor, for a client whose toolkit counts in `f64`.
    pub fn factor(self) -> f64 {
        f64::from(self.0) / f64::from(DENOMINATOR)
    }

    /// `l` logical pixels in physical pixels.
    pub const fn px(self, l: i32) -> i32 {
        round_ratio(l, self.0 as u32, DENOMINATOR)
    }

    /// [`px`](Self::px) for an extent, which is never negative.
    pub const fn px_u32(self, l: u32) -> u32 {
        let l = if l > i32::MAX as u32 { i32::MAX } else { l as i32 };
        self.px(l) as u32
    }

    /// The desktop's text cell ([`font::CELL`]) at this scale.
    pub const fn cell(self) -> (usize, usize) {
        (self.px_u32(font::CELL.0 as u32) as usize, self.px_u32(font::CELL.1 as u32) as usize)
    }

    /// `p` physical pixels at this scale, as physical pixels at `to`.
    pub const fn rescale(self, p: i32, to: Self) -> i32 {
        round_ratio(p, to.0 as u32, self.0 as u32)
    }
}

/// `v * num / den`, rounded half away from zero, saturating at `i32`'s range:
/// the one rounding every conversion here makes. `den` is not zero.
pub const fn round_ratio(v: i32, num: u32, den: u32) -> i32 {
    let magnitude = (v.unsigned_abs() as u64 * num as u64 + den as u64 / 2) / den as u64;
    let magnitude = if magnitude > i32::MAX as u64 { i32::MAX } else { magnitude as i32 };
    if v < 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// As a decimal where two places hold it exactly — `1`, `1.25`, `1.5` — and
/// as `n/120` where they do not.
impl fmt::Display for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = u32::from(self.0);
        if n * 100 % DENOMINATOR != 0 {
            return write!(f, "{n}/{DENOMINATOR}");
        }
        let hundredths = n * 100 / DENOMINATOR;
        match (hundredths / 100, hundredths % 100) {
            (whole, 0) => write!(f, "{whole}"),
            (whole, frac) if frac % 10 == 0 => write!(f, "{whole}.{}", frac / 10),
            (whole, frac) => write!(f, "{whole}.{frac:02}"),
        }
    }
}

/// Why a decimal is not a [`Scale`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not digits, optionally a point and at most four more digits.
    NotADecimal,
    /// No whole number of 120ths: `1.1` is 132/120, `1.333` is no scale.
    NotIn120ths,
    /// Below 1 or above 4.
    OutOfRange,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADecimal => write!(f, "not a decimal number"),
            Self::NotIn120ths => write!(f, "not a whole number of 120ths"),
            Self::OutOfRange => write!(f, "not from 1 to 4"),
        }
    }
}

impl FromStr for Scale {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || (s.contains('.') && !digits(frac)) || whole.len() > 2 || frac.len() > 4 {
            return Err(ParseError::NotADecimal);
        }
        let ten_k = 10u32.pow(frac.len() as u32);
        let whole: u32 = whole.parse().map_err(|_| ParseError::NotADecimal)?;
        let frac: u32 = if frac.is_empty() { 0 } else { frac.parse().map_err(|_| ParseError::NotADecimal)? };
        let scaled = (whole * ten_k + frac) * DENOMINATOR;
        if !scaled.is_multiple_of(ten_k) {
            return Err(ParseError::NotIn120ths);
        }
        Self::from_wire(scaled / ten_k).ok_or(ParseError::OutOfRange)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn at(n: u32) -> Scale {
        match Scale::from_wire(n) {
            Some(s) => s,
            None => panic!("a scale from 1 to 4"),
        }
    }

    const ONE_HALF: Scale = at(180);
    const TWO: Scale = at(240);

    #[test]
    fn the_wire_carries_one_to_four_and_refuses_the_rest() {
        assert_eq!(Scale::from_wire(120), Some(Scale::ONE));
        assert_eq!(Scale::from_wire(480), Some(Scale::MAX));
        for absurd in [0, 1, 119, 481, 1200, u32::MAX] {
            assert_eq!(Scale::from_wire(absurd), None, "{absurd}");
        }
    }

    #[test]
    fn whole_scales_multiply() {
        for l in [-7, 0, 1, 8, 16, 641] {
            assert_eq!(Scale::ONE.px(l), l);
            assert_eq!(TWO.px(l), 2 * l);
        }
    }

    /// Half away from zero: 1.5 x 1 is 1.5, which is 2; 1.5 x 3 is 4.5,
    /// which is 5; and the negative side mirrors it. Half to even would give
    /// 2 and 4.
    #[test]
    fn a_half_rounds_away_from_zero() {
        assert_eq!(ONE_HALF.px(1), 2);
        assert_eq!(ONE_HALF.px(3), 5);
        assert_eq!(ONE_HALF.px(-1), -2);
        assert_eq!(ONE_HALF.px(-3), -5);
        assert_eq!(at(150).px(2), 3);
        assert_eq!(at(150).px(-2), -3);
        assert_eq!(at(210).px(2), 4);
    }

    /// The text cell and the chrome at the three scales the benches and
    /// QEMU run at.
    #[test]
    fn the_desktops_metrics_at_one_one_and_a_half_and_two() {
        for (scale, cell, title_bar, icon) in
            [(Scale::ONE, (8, 16), 28, 14), (ONE_HALF, (12, 24), 42, 21), (TWO, (16, 32), 56, 28)]
        {
            assert_eq!(scale.cell(), cell, "{scale}");
            assert_eq!(scale.px(28), title_bar, "{scale}");
            assert_eq!(scale.px(14), icon, "{scale}");
        }
    }

    /// Two rectangles sharing an edge share it after their corners are
    /// converted, at every quarter step, wherever the edge is.
    #[test]
    fn touching_rectangles_still_touch() {
        for n in (120..=480).step_by(30) {
            let s = at(n);
            for edge in -50..200 {
                let left = (edge - 37, edge);
                let right = (edge, edge + 11);
                assert_eq!(s.px(left.1), s.px(right.0), "{s} at {edge}");
                assert!(s.px(left.0) <= s.px(left.1) && s.px(right.0) <= s.px(right.1));
            }
        }
    }

    #[test]
    fn a_rescale_rounds_the_same_way() {
        assert_eq!(TWO.rescale(400, Scale::ONE), 200);
        assert_eq!(Scale::ONE.rescale(201, ONE_HALF), 302);
        assert_eq!(ONE_HALF.rescale(3, Scale::ONE), 2);
        assert_eq!(ONE_HALF.rescale(-3, Scale::ONE), -2);
        assert_eq!(TWO.rescale(i32::MAX, Scale::MAX), i32::MAX);
    }

    #[test]
    fn a_scale_reads_and_writes_as_a_decimal() {
        for (text, n) in [("1", 120), ("1.25", 150), ("1.5", 180), ("1.75", 210), ("2", 240), ("4", 480), ("1.1", 132)] {
            assert_eq!(text.parse::<Scale>(), Ok(at(n)), "{text}");
            assert_eq!(at(n).to_string(), text);
        }
        assert_eq!("2.50".parse::<Scale>(), Ok(at(300)));
        assert_eq!(at(121).to_string(), "121/120");
    }

    #[test]
    fn an_absurd_decimal_is_refused_by_name() {
        for (text, why) in [
            ("", ParseError::NotADecimal),
            ("auto", ParseError::NotADecimal),
            ("1.", ParseError::NotADecimal),
            (".5", ParseError::NotADecimal),
            ("-1", ParseError::NotADecimal),
            ("1.00001", ParseError::NotADecimal),
            ("99999999999", ParseError::NotADecimal),
            ("1.333", ParseError::NotIn120ths),
            ("1.001", ParseError::NotIn120ths),
            ("0.5", ParseError::OutOfRange),
            ("4.25", ParseError::OutOfRange),
            ("0", ParseError::OutOfRange),
        ] {
            assert_eq!(text.parse::<Scale>(), Err(why), "{text:?}");
        }
    }
}
