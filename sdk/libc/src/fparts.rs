//! A double's parts: `modf`'s integral and fractional parts, the integer
//! `round` and `lround` take from them, and `logb`'s exponent, read off its
//! bits. It reads and sets nothing but what it is
//! handed, so the host holds it to its own C library's (`toyos-libc-copies`).

const MANTISSA_BITS: u32 = 52;
const EXPONENT_MASK: u64 = 0x7ff;
const BIAS: i32 = 1023;

/// `x`'s unbiased exponent field, `-1023` for zero and subnormals and `1024`
/// for infinities and NaNs.
fn exponent(x: f64) -> i32 {
    ((x.to_bits() >> MANTISSA_BITS) & EXPONENT_MASK) as i32 - BIAS
}

/// `modf`: `x`'s integral part and its fractional part, each with `x`'s sign;
/// an infinity's fraction is a zero and a NaN's parts are both the NaN.
pub(crate) fn modf(x: f64) -> (f64, f64) {
    let e = exponent(x);
    if x.is_nan() {
        return (x, x);
    }
    let zero = f64::from_bits(x.to_bits() & (1 << 63));
    if e >= MANTISSA_BITS as i32 {
        // No fraction bit left, infinities with them.
        return (x, zero);
    }
    if e < 0 {
        return (zero, x);
    }
    let fraction_bits = (1u64 << (MANTISSA_BITS as i32 - e)) - 1;
    if x.to_bits() & fraction_bits == 0 {
        return (x, zero);
    }
    let int = f64::from_bits(x.to_bits() & !fraction_bits);
    // Exact: both have `x`'s sign and `int` shares every bit of `x` above
    // the fraction's.
    (int, x - int)
}

/// `round`: `x` to the nearest integer, a half away from zero, keeping its
/// sign; an integer, an infinity or a NaN is itself.
pub(crate) fn round(x: f64) -> f64 {
    let (int, fraction) = modf(x);
    if fraction.abs() < 0.5 {
        return int;
    }
    // Exact: below 2^52 every integer and its successor are doubles, and at or
    // above it `modf` answers no fraction.
    if x.is_sign_negative() { int - 1.0 } else { int + 1.0 }
}

/// `lround` of a value whose rounding is no `long`, a NaN among them: POSIX's
/// domain error, `errno` `EDOM`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Domain;

/// `lround`: [`round`] as a `long`.
pub(crate) fn lround(x: f64) -> Result<i64, Domain> {
    // `long`'s range as doubles, `[-2^63, 2^63)`; a NaN is in neither half.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let rounded = round(x);
    if (-LIMIT..LIMIT).contains(&rounded) { Ok(rounded as i64) } else { Err(Domain) }
}

/// `logb` at a zero: POSIX's pole error, `-inf` with `errno` `ERANGE`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pole;

/// `logb`: the exponent of `x` as a double, the one a normalized `x` would
/// have for a subnormal; `+inf` for an infinity, the NaN for a NaN.
pub(crate) fn logb(x: f64) -> Result<f64, Pole> {
    if x.is_nan() {
        return Ok(x);
    }
    if x.is_infinite() {
        return Ok(f64::INFINITY);
    }
    if x == 0.0 {
        return Err(Pole);
    }
    let e = exponent(x);
    if e > -BIAS {
        return Ok(f64::from(e));
    }
    // Subnormal: `mantissa * 2^-1074`, whose top set bit is the exponent.
    let mantissa = x.to_bits() & ((1u64 << MANTISSA_BITS) - 1);
    Ok(f64::from(63 - mantissa.leading_zeros() as i32 - 1074))
}
