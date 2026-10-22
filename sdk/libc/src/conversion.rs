//! printf's conversion specifications (C17 §7.21.6.1): reading one, and writing
//! an integer or a floating-point value under its flags, width and precision.
//! It reads and sets nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

use alloc::string::String;
use core::fmt::Write;

pub(crate) trait Sink {
    fn put(&mut self, b: u8);
}

#[derive(Default)]
pub(crate) struct Spec {
    pub(crate) left: bool,
    pub(crate) zero: bool,
    pub(crate) plus: bool,
    pub(crate) space: bool,
    pub(crate) alt: bool,
    pub(crate) width: usize,
    pub(crate) precision: Option<usize>,
    /// `l`, `ll`, `j`, `z` or `t`: the argument is 64 bits wide.
    pub(crate) wide: bool,
    pub(crate) conversion: u8,
}

/// The specification `fmt` begins with, just past its `%`, and how many bytes
/// it spans; `None` when `fmt` ends inside it. `star` reads the argument of a
/// `*` width or precision.
pub(crate) fn spec(fmt: &[u8], mut star: impl FnMut() -> i32) -> Option<(Spec, usize)> {
    let mut s = Spec::default();
    let mut i = 0;
    while let Some(&b) = fmt.get(i) {
        match b {
            b'-' => s.left = true,
            b'0' => s.zero = true,
            b'+' => s.plus = true,
            b' ' => s.space = true,
            b'#' => s.alt = true,
            _ => break,
        }
        i += 1;
    }
    if fmt.get(i) == Some(&b'*') {
        let width = star();
        s.left |= width < 0;
        s.width = width.unsigned_abs() as usize;
        i += 1;
    } else {
        s.width = decimal(fmt, &mut i);
    }
    if fmt.get(i) == Some(&b'.') {
        i += 1;
        s.precision = if fmt.get(i) == Some(&b'*') {
            i += 1;
            // A negative precision argument is taken as if it were omitted.
            usize::try_from(star()).ok()
        } else {
            Some(decimal(fmt, &mut i))
        };
    }
    match fmt.get(i) {
        Some(b'l') => {
            i += if fmt.get(i + 1) == Some(&b'l') { 2 } else { 1 };
            s.wide = true;
        }
        Some(b'h') => i += if fmt.get(i + 1) == Some(&b'h') { 2 } else { 1 },
        Some(b'z' | b't' | b'j') => {
            s.wide = true;
            i += 1;
        }
        // Read as a double: issues/libc-reads-a-long-double-as-a-double.md.
        Some(b'L') => i += 1,
        _ => {}
    }
    s.conversion = *fmt.get(i)?;
    Some((s, i + 1))
}

fn decimal(fmt: &[u8], i: &mut usize) -> usize {
    let mut n = 0usize;
    while let Some(d) = fmt.get(*i).filter(|b| b.is_ascii_digit()) {
        n = n.saturating_mul(10).saturating_add(usize::from(d - b'0'));
        *i += 1;
    }
    n
}

/// `v`'s digits in `base`, `0` for zero.
pub(crate) fn digits(mut v: u64, base: u64, upper: bool, buf: &mut [u8; 22]) -> &[u8] {
    let set = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = set[(v % base) as usize];
        v /= base;
        if v == 0 {
            return &buf[at..];
        }
    }
}

/// `d` or `i`.
pub(crate) fn signed(out: &mut impl Sink, s: &Spec, v: i64) {
    let sign: &[u8] = if v < 0 {
        b"-"
    } else if s.plus {
        b"+"
    } else if s.space {
        b" "
    } else {
        b""
    };
    integer(out, s, sign, v.unsigned_abs(), 10, false);
}

/// `u`, `o`, `x` or `X`.
pub(crate) fn unsigned(out: &mut impl Sink, s: &Spec, v: u64) {
    let (base, prefix): (u64, &[u8]) = match s.conversion {
        b'o' => (8, b""),
        b'x' => (16, if s.alt && v != 0 { b"0x" } else { b"" }),
        b'X' => (16, if s.alt && v != 0 { b"0X" } else { b"" }),
        _ => (10, b""),
    };
    integer(out, s, prefix, v, base, s.conversion == b'X');
}

fn integer(out: &mut impl Sink, s: &Spec, prefix: &[u8], v: u64, base: u64, upper: bool) {
    let mut buf = [0; 22];
    let body = if v == 0 && s.precision == Some(0) { &[][..] } else { digits(v, base, upper, &mut buf) };
    let mut zeros = s.precision.unwrap_or(0).saturating_sub(body.len());
    // `#o` raises the precision just far enough that the first digit is a 0.
    if s.alt && base == 8 && zeros == 0 && body.first() != Some(&b'0') {
        zeros = 1;
    }
    pad(out, s, prefix, zeros, body, s.precision.is_none());
}

/// No double's exact decimal expansion has more fractional or significant
/// digits than this, so any further requested digit is a zero.
const EXACT_DIGITS: usize = 1074;

/// `f`, `F`, `e`, `E`, `g`, `G`, `a` or `A`.
pub(crate) fn float(out: &mut impl Sink, s: &Spec, v: f64) {
    let upper = s.conversion.is_ascii_uppercase();
    let sign = if v.is_sign_negative() {
        "-"
    } else if s.plus {
        "+"
    } else if s.space {
        " "
    } else {
        ""
    };
    let a = v.abs();
    let mut prefix = String::from(sign);
    if !a.is_finite() {
        let body: &[u8] = match (a.is_nan(), upper) {
            (true, false) => b"nan",
            (true, true) => b"NAN",
            (false, false) => b"inf",
            (false, true) => b"INF",
        };
        return pad(out, s, prefix.as_bytes(), 0, body, false);
    }
    let mut body = String::new();
    match s.conversion | 0x20 {
        b'f' => fixed(&mut body, a, s.precision.unwrap_or(6), s.alt),
        b'e' => exponential(&mut body, a, s.precision.unwrap_or(6), s.alt),
        b'g' => general(&mut body, a, s.precision.unwrap_or(6).max(1), s.alt),
        b'a' => {
            prefix.push_str("0x");
            hexadecimal(&mut body, a, s.precision, s.alt);
        }
        other => unreachable!("{other} is no floating-point conversion"),
    }
    if upper {
        prefix.make_ascii_uppercase();
        body.make_ascii_uppercase();
    }
    pad(out, s, prefix.as_bytes(), 0, body.as_bytes(), true);
}

fn fixed(body: &mut String, a: f64, precision: usize, alt: bool) {
    let exact = precision.min(EXACT_DIGITS);
    let _ = write!(body, "{a:.exact$}");
    if alt && precision == 0 {
        body.push('.');
    }
    zeros(body, precision - exact);
}

fn exponential(body: &mut String, a: f64, precision: usize, alt: bool) {
    let exact = precision.min(EXACT_DIGITS);
    let _ = write!(body, "{a:.exact$e}");
    let exponent = body.split_off(body.rfind('e').expect("an exponent"));
    let exponent: i32 = exponent[1..].parse().expect("a decimal exponent");
    if alt && precision == 0 {
        body.push('.');
    }
    zeros(body, precision - exact);
    let sign = if exponent < 0 { '-' } else { '+' };
    let _ = write!(body, "e{sign}{:02}", exponent.unsigned_abs());
}

/// `g` with precision `p` is `e`'s style when the exponent `x` it would have
/// is below -4 or at least `p`, and otherwise `f`'s with precision `p - 1 - x`.
fn general(body: &mut String, a: f64, p: usize, alt: bool) {
    exponential(body, a, p - 1, alt);
    let x: i64 = body[body.rfind('e').expect("an exponent") + 1..].parse().expect("a decimal exponent");
    if (-4..p as i64).contains(&x) {
        body.clear();
        fixed(body, a, (p as i64 - 1 - x) as usize, alt);
    }
    if !alt {
        let exponent = body.split_off(body.find('e').unwrap_or(body.len()));
        if body.contains('.') {
            let kept = body.trim_end_matches('0').trim_end_matches('.').len();
            body.truncate(kept);
        }
        body.push_str(&exponent);
    }
}

/// `a` after its `0x`: the exact value, or with a precision rounded to nearest
/// with ties to even. A subnormal's leading digit is 0.
fn hexadecimal(body: &mut String, a: f64, precision: Option<usize>, alt: bool) {
    const FRACTION_DIGITS: usize = 13;
    let bits = a.to_bits();
    let biased = (bits >> (4 * FRACTION_DIGITS)) as i32;
    let fraction = bits & ((1 << (4 * FRACTION_DIGITS)) - 1);
    let (lead, exponent) = match biased {
        0 if fraction == 0 => (0, 0),
        0 => (0, -1022),
        _ => (1, biased - 1023),
    };
    let exact = lead << (4 * FRACTION_DIGITS) | fraction;
    let shown = precision.unwrap_or(FRACTION_DIGITS - fraction.trailing_zeros().min(52) as usize / 4);
    let kept = shown.min(FRACTION_DIGITS);
    let dropped = 4 * (FRACTION_DIGITS - kept) as u32;
    let mut significand = exact >> dropped;
    if dropped > 0 {
        let rest = exact & ((1 << dropped) - 1);
        let half = 1 << (dropped - 1);
        if rest > half || (rest == half && significand & 1 == 1) {
            significand += 1;
        }
    }
    // A carry out of the fraction makes the leading digit 2.
    let _ = write!(body, "{:x}", significand >> (4 * kept));
    if shown > 0 || alt {
        body.push('.');
    }
    if kept > 0 {
        let _ = write!(body, "{:0kept$x}", significand & ((1 << (4 * kept)) - 1));
    }
    zeros(body, shown - kept);
    let sign = if exponent < 0 { '-' } else { '+' };
    let _ = write!(body, "p{sign}{}", exponent.unsigned_abs());
}

fn zeros(body: &mut String, n: usize) {
    body.extend(core::iter::repeat_n('0', n));
}

/// `prefix`, `zeros` zeros and `body`, padded to the width: with zeros after
/// the prefix when the `0` flag asks and `zero_fill` allows, else with spaces.
fn pad(out: &mut impl Sink, s: &Spec, prefix: &[u8], zeros: usize, body: &[u8], zero_fill: bool) {
    let fill = s.width.saturating_sub(prefix.len() + zeros + body.len());
    let (before, zeros, after) = if s.left {
        (0, zeros, fill)
    } else if s.zero && zero_fill {
        (0, zeros + fill, 0)
    } else {
        (fill, zeros, 0)
    };
    (0..before).for_each(|_| out.put(b' '));
    prefix.iter().for_each(|&b| out.put(b));
    (0..zeros).for_each(|_| out.put(b'0'));
    body.iter().for_each(|&b| out.put(b));
    (0..after).for_each(|_| out.put(b' '));
}
