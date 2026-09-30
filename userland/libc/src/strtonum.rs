//! The one reader of numbers out of C strings, narrow and wide: the grammar of
//! `strtol` and `strtod`, their `endptr`, and their `ERANGE`. A decimal
//! floating-point number is rounded by `core`'s parser, correctly. It reads
//! and sets nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

use alloc::vec::Vec;

/// A number read out of a C string.
pub(crate) struct Read<T> {
    pub(crate) value: T,
    /// How many code units it spans: 0 when there is no number.
    pub(crate) end: usize,
    pub(crate) refused: Option<Refusal>,
}

/// What C's readers tell `errno`.
pub(crate) enum Refusal {
    /// A base C does not define: `EINVAL`.
    Base,
    /// A value outside the type: `ERANGE`.
    Range,
}

/// A code unit of a C string: `char` or `wchar_t`.
pub(crate) trait Unit: Copy {
    fn code(self) -> u32;
}

impl Unit for u8 {
    fn code(self) -> u32 {
        u32::from(self)
    }
}

impl Unit for crate::arch::WChar {
    fn code(self) -> u32 {
        self as u32
    }
}

fn is_space(c: u32) -> bool {
    c == 0x20 || (0x09..=0x0d).contains(&c)
}

fn digit(c: u32) -> Option<u32> {
    match c {
        0x30..=0x39 => Some(c - 0x30),
        0x41..=0x5a => Some(c - 0x41 + 10),
        0x61..=0x7a => Some(c - 0x61 + 10),
        _ => None,
    }
}

fn lower(c: u32) -> u32 {
    if (0x41..=0x5a).contains(&c) { c + 0x20 } else { c }
}

/// The code unit `i` places into `s`.
unsafe fn at<U: Unit>(s: *const U, i: usize) -> u32 {
    unsafe { *s.add(i) }.code()
}

/// Whether `s + i` begins with `word`, ignoring ASCII case.
unsafe fn starts<U: Unit>(s: *const U, i: usize, word: &[u8]) -> bool {
    word.iter().enumerate().all(|(k, &b)| unsafe { lower(at(s, i + k)) } == u32::from(b))
}

/// Past the whitespace and the sign: where the number starts, and whether it
/// is negative.
unsafe fn lead<U: Unit>(s: *const U) -> (usize, bool) {
    let mut i = 0;
    while is_space(unsafe { at(s, i) }) {
        i += 1;
    }
    match unsafe { at(s, i) } {
        0x2d => (i + 1, true),
        0x2b => (i + 1, false),
        _ => (i, false),
    }
}

/// An integer as `strtol`'s grammar reads it.
pub(crate) struct Int {
    pub(crate) negative: bool,
    /// Its magnitude, `None` when it does not fit in 64 bits.
    pub(crate) magnitude: Option<u64>,
    /// How many code units it spans: 0 when there is no number.
    pub(crate) end: usize,
}

/// Read an integer in `base` (0 for C's prefixes) from `s`. `None` is a base
/// C refuses.
pub(crate) unsafe fn int<U: Unit>(s: *const U, base: i32) -> Option<Int> {
    if base < 0 || base == 1 || base > 36 {
        return None;
    }
    let (mut i, negative) = unsafe { lead(s) };
    let mut base = base as u32;
    let hex_prefix = unsafe { at(s, i) == 0x30 && lower(at(s, i + 1)) == 0x78 && digit(at(s, i + 2)).is_some_and(|d| d < 16) };
    if (base == 0 || base == 16) && hex_prefix {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if unsafe { at(s, i) } == 0x30 { 8 } else { 10 };
    }
    let start = i;
    let mut magnitude = Some(0u64);
    while let Some(d) = digit(unsafe { at(s, i) }).filter(|&d| d < base) {
        magnitude = magnitude.and_then(|m| m.checked_mul(u64::from(base))).and_then(|m| m.checked_add(u64::from(d)));
        i += 1;
    }
    let end = if i == start { 0 } else { i };
    Some(Int { negative, magnitude, end })
}

/// `strtol`'s answer.
pub(crate) unsafe fn signed<U: Unit>(s: *const U, base: i32) -> Read<i64> {
    let Some(n) = (unsafe { int(s, base) }) else {
        return Read { value: 0, end: 0, refused: Some(Refusal::Base) };
    };
    let (value, refused) = match (n.magnitude, n.negative) {
        (Some(m), false) if m <= i64::MAX as u64 => (m as i64, None),
        (Some(m), true) if m <= i64::MIN.unsigned_abs() => ((m as i64).wrapping_neg(), None),
        (_, negative) => (if negative { i64::MIN } else { i64::MAX }, Some(Refusal::Range)),
    };
    Read { value, end: n.end, refused }
}

/// `strtoul`'s answer: a negative number is negated in the unsigned type, as
/// C says.
pub(crate) unsafe fn unsigned<U: Unit>(s: *const U, base: i32) -> Read<u64> {
    let Some(n) = (unsafe { int(s, base) }) else {
        return Read { value: 0, end: 0, refused: Some(Refusal::Base) };
    };
    let (value, refused) = match n.magnitude {
        Some(m) if n.negative => (m.wrapping_neg(), None),
        Some(m) => (m, None),
        None => (u64::MAX, Some(Refusal::Range)),
    };
    Read { value, end: n.end, refused }
}

/// The IEEE formats `strtof` and `strtod` round to.
pub(crate) trait Float: Copy + core::str::FromStr + core::ops::Neg<Output = Self> {
    const INFINITY: Self;
    const NAN: Self;
    /// Significand bits, the leading one included.
    const PRECISION: u32;
    const MIN_EXP: i64;
    const MAX_EXP: i64;
    fn from_parts(biased_exponent: u64, fraction: u64) -> Self;
    fn is_zero_or_subnormal(self) -> bool;
    fn is_infinite(self) -> bool;
}

impl Float for f64 {
    const INFINITY: Self = f64::INFINITY;
    const NAN: Self = f64::NAN;
    const PRECISION: u32 = 53;
    const MIN_EXP: i64 = -1022;
    const MAX_EXP: i64 = 1023;
    fn from_parts(biased_exponent: u64, fraction: u64) -> Self {
        f64::from_bits(biased_exponent << 52 | fraction)
    }
    fn is_zero_or_subnormal(self) -> bool {
        !self.is_normal() && self.is_finite()
    }
    fn is_infinite(self) -> bool {
        f64::is_infinite(self)
    }
}

impl Float for f32 {
    const INFINITY: Self = f32::INFINITY;
    const NAN: Self = f32::NAN;
    const PRECISION: u32 = 24;
    const MIN_EXP: i64 = -126;
    const MAX_EXP: i64 = 127;
    fn from_parts(biased_exponent: u64, fraction: u64) -> Self {
        f32::from_bits((biased_exponent << 23 | fraction) as u32)
    }
    fn is_zero_or_subnormal(self) -> bool {
        !self.is_normal() && self.is_finite()
    }
    fn is_infinite(self) -> bool {
        f32::is_infinite(self)
    }
}

/// Read a floating-point number from `s` as `strtod` does, rounded to `F`.
pub(crate) unsafe fn float<U: Unit, F: Float>(s: *const U) -> Read<F> {
    let (i, negative) = unsafe { lead(s) };
    let sign = |x: F| if negative { -x } else { x };
    let read = |value: F, end: usize| Read { value, end, refused: None };
    if unsafe { starts(s, i, b"infinity") } {
        return read(sign(F::INFINITY), i + 8);
    }
    if unsafe { starts(s, i, b"inf") } {
        return read(sign(F::INFINITY), i + 3);
    }
    if unsafe { starts(s, i, b"nan") } {
        let mut end = i + 3;
        if unsafe { at(s, end) } == 0x28 {
            let mut k = end + 1;
            while unsafe { at(s, k) }.try_into().is_ok_and(|c: u8| c.is_ascii_alphanumeric() || c == b'_') {
                k += 1;
            }
            if unsafe { at(s, k) } == 0x29 {
                end = k + 1;
            }
        }
        return read(sign(F::NAN), end);
    }
    let hex = unsafe {
        at(s, i) == 0x30
            && lower(at(s, i + 1)) == 0x78
            && (digit(at(s, i + 2)).is_some_and(|d| d < 16)
                || (at(s, i + 2) == 0x2e && digit(at(s, i + 3)).is_some_and(|d| d < 16)))
    };
    let (value, end, nonzero) = if hex { unsafe { hex_float::<U, F>(s, i + 2) } } else { unsafe { decimal::<U, F>(s, i) } };
    if end == 0 {
        return read(F::from_parts(0, 0), 0);
    }
    let range = value.is_infinite() || (nonzero && value.is_zero_or_subnormal());
    Read { value: sign(value), end, refused: range.then_some(Refusal::Range) }
}

/// A decimal number at `s + i`: its value, its end (0 if none), and whether
/// any digit of it is nonzero.
unsafe fn decimal<U: Unit, F: Float>(s: *const U, mut i: usize) -> (F, usize, bool) {
    let mut text = Vec::new();
    let mut digits = 0;
    let mut nonzero = false;
    let mut take = |text: &mut Vec<u8>, c: u32| {
        text.push(c as u8);
        nonzero |= c != 0x30;
    };
    while digit(unsafe { at(s, i) }).is_some_and(|d| d < 10) {
        take(&mut text, unsafe { at(s, i) });
        digits += 1;
        i += 1;
    }
    if unsafe { at(s, i) } == 0x2e {
        text.push(b'.');
        i += 1;
        while digit(unsafe { at(s, i) }).is_some_and(|d| d < 10) {
            take(&mut text, unsafe { at(s, i) });
            digits += 1;
            i += 1;
        }
    }
    if digits == 0 {
        return (F::NAN, 0, false);
    }
    if lower(unsafe { at(s, i) }) == 0x65 {
        let mut k = i + 1;
        let sign = unsafe { at(s, k) };
        if sign == 0x2b || sign == 0x2d {
            k += 1;
        }
        if digit(unsafe { at(s, k) }).is_some_and(|d| d < 10) {
            text.push(b'e');
            if sign == 0x2d {
                text.push(b'-');
            }
            while digit(unsafe { at(s, k) }).is_some_and(|d| d < 10) {
                text.push(unsafe { at(s, k) } as u8);
                k += 1;
            }
            i = k;
        }
    }
    let text = core::str::from_utf8(&text).expect("only ASCII digits, '.', 'e' and '-' were taken");
    let value = text.parse::<F>().unwrap_or_else(|_| unreachable!("{text} is decimal-float syntax"));
    (value, i, nonzero)
}

/// A hexadecimal number whose digits start at `s + i`, rounded to nearest,
/// ties to even: its value, its end, and whether any digit is nonzero.
unsafe fn hex_float<U: Unit, F: Float>(s: *const U, mut i: usize) -> (F, usize, bool) {
    let mut mantissa = 0u64;
    let mut exponent = 0i64;
    let mut sticky = false;
    let mut add = |d: u32, after_point: bool, mantissa: &mut u64, exponent: &mut i64| {
        if *mantissa >> 60 == 0 {
            *mantissa = *mantissa << 4 | u64::from(d);
            if after_point {
                *exponent -= 4;
            }
        } else {
            sticky |= d != 0;
            if !after_point {
                *exponent += 4;
            }
        }
    };
    while let Some(d) = digit(unsafe { at(s, i) }).filter(|&d| d < 16) {
        add(d, false, &mut mantissa, &mut exponent);
        i += 1;
    }
    if unsafe { at(s, i) } == 0x2e {
        i += 1;
        while let Some(d) = digit(unsafe { at(s, i) }).filter(|&d| d < 16) {
            add(d, true, &mut mantissa, &mut exponent);
            i += 1;
        }
    }
    if lower(unsafe { at(s, i) }) == 0x70 {
        let mut k = i + 1;
        let negative = unsafe { at(s, k) } == 0x2d;
        if negative || unsafe { at(s, k) } == 0x2b {
            k += 1;
        }
        if digit(unsafe { at(s, k) }).is_some_and(|d| d < 10) {
            let mut power = 0i64;
            while let Some(d) = digit(unsafe { at(s, k) }).filter(|&d| d < 10) {
                power = power.saturating_mul(10).saturating_add(i64::from(d));
                k += 1;
            }
            exponent = exponent.saturating_add(if negative { -power } else { power });
            i = k;
        }
    }
    let nonzero = mantissa != 0 || sticky;
    (round::<F>(mantissa, exponent, sticky), i, nonzero)
}

/// `mantissa * 2^exponent`, plus less than one unit of `mantissa` when
/// `sticky`, rounded to `F`.
fn round<F: Float>(mantissa: u64, exponent: i64, sticky: bool) -> F {
    if mantissa == 0 {
        return F::from_parts(0, 0);
    }
    let shift = mantissa.leading_zeros();
    let mantissa = u128::from(mantissa << shift) << 64;
    // The value is 1.f * 2^top, with the leading one at bit 127 of `mantissa`.
    let top = exponent.saturating_add(63 - i64::from(shift));
    if top > F::MAX_EXP {
        return F::INFINITY;
    }
    // Bits below the kept significand: the precision's, and as many more as a
    // subnormal gives up.
    let dropped = 128 - i64::from(F::PRECISION) + (F::MIN_EXP - top).max(0);
    if dropped > 128 {
        return F::from_parts(0, 0);
    }
    let dropped = dropped as u32;
    let kept = if dropped == 128 { 0 } else { mantissa >> dropped };
    let rest = if dropped == 128 { mantissa } else { mantissa & ((1u128 << dropped) - 1) };
    // `sticky` bits lie below every bit of `rest`, which ends at bit 64.
    let half = 1u128 << (dropped - 1);
    let up = rest > half || (rest == half && (sticky || kept & 1 == 1));
    let kept = kept as u64 + u64::from(up);
    let fraction_bits = F::PRECISION - 1;
    if top < F::MIN_EXP {
        // Subnormal, or rounded up into the smallest normal.
        return F::from_parts(kept >> fraction_bits, kept & ((1 << fraction_bits) - 1));
    }
    let (kept, top) = if kept >> F::PRECISION != 0 { (kept >> 1, top + 1) } else { (kept, top) };
    if top > F::MAX_EXP {
        return F::INFINITY;
    }
    let biased = (top - F::MIN_EXP + 1) as u64;
    F::from_parts(biased, kept & ((1 << fraction_bits) - 1))
}
