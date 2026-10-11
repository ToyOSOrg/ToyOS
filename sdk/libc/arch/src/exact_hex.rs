//! The value a hexadecimal floating-point number denotes, rounded to an IEEE
//! 754 binary format by IEEE 754's rule: to nearest, ties to even, with
//! subnormals. Every digit is four bits and the one rounding sees all of them,
//! so this judges what `strtod` and `strtof` must answer for hexadecimal input.

/// An IEEE 754 binary interchange format: `k` bits of storage, `p` of precision.
#[derive(Clone, Copy)]
pub(crate) struct Format {
    k: u32,
    p: u32,
}

pub(crate) const BINARY32: Format = Format { k: 32, p: 24 };
pub(crate) const BINARY64: Format = Format { k: 64, p: 53 };

/// The bits of `number` rounded to `format`. `number` is a sign, `0x` and
/// hexadecimal digits around at most one point, then optionally `p` and a
/// signed decimal exponent.
pub(crate) fn round(number: &str, format: Format) -> u64 {
    let Format { k, p } = format;
    let emax = (1i128 << (k - p - 1)) - 1;
    let emin = 1 - emax;
    let (negative, unsigned) = match number.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, number.strip_prefix('+').unwrap_or(number)),
    };
    let unsigned = unsigned.strip_prefix("0x").or_else(|| unsigned.strip_prefix("0X")).expect("a 0x prefix");
    let (digits, exponent) = unsigned.split_once(['p', 'P']).unwrap_or((unsigned, "0"));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    // The number is the integer `bits` times 2^scale.
    let bits: Vec<bool> = whole
        .chars()
        .chain(fraction.chars())
        .map(|c| c.to_digit(16).expect("a hexadecimal digit"))
        .flat_map(|d| (0..4).rev().map(move |i| d >> i & 1 == 1))
        .collect();
    let scale = exponent.parse::<i128>().expect("a decimal exponent") - 4 * fraction.len() as i128;
    let sign = u64::from(negative) << (k - 1);
    let Some(first) = bits.iter().position(|&b| b) else { return sign };
    let bits = &bits[first..];
    // `bits[i]` weighs 2^(top - i). The last bit kept weighs 2^quantum: `p`
    // bits down from the leading one, but never below the least subnormal.
    let top = scale + (bits.len() - 1) as i128;
    let quantum = (top - i128::from(p - 1)).max(emin - i128::from(p - 1));
    let kept = top - quantum + 1;
    let bit = |i: i128| usize::try_from(i).ok().and_then(|i| bits.get(i)).is_some_and(|&b| b);
    let mut significand = (0..kept).fold(0u64, |s, i| s << 1 | u64::from(bit(i)));
    let half = bit(kept);
    let sticky = bits.iter().enumerate().any(|(i, &b)| b && i as i128 > kept);
    if half && (sticky || significand & 1 == 1) {
        significand += 1;
    }
    // A carry out of the top makes one more bit of the same value.
    let (significand, quantum) =
        if significand >> p == 1 { (significand >> 1, quantum + 1) } else { (significand, quantum) };
    // The leading bit's weight is 2^(quantum + t), biased by `emax`.
    let t = p - 1;
    let magnitude = if significand >> t == 0 {
        // Subnormal or zero: the quantum is the least, the biased exponent 0.
        significand
    } else if quantum + i128::from(t) > emax {
        // Infinity: every exponent bit set, the fraction clear.
        ((2 * emax + 1) as u64) << t
    } else {
        ((quantum + i128::from(t) + emax) as u64) << t | significand & ((1 << t) - 1)
    };
    sign | magnitude
}

#[cfg(test)]
mod tests {
    use super::{round, BINARY32, BINARY64};
    use crate::strtonum_differential::Rng;

    /// IEEE 754's corners of both formats, and its ties, each with its bits.
    #[test]
    fn the_formats_corners_and_ties_round_to_their_bits() {
        for (number, bits) in [
            ("0x1p0", 1f64.to_bits()),
            ("-0x0p0", (-0f64).to_bits()),
            ("0x1p-52", f64::EPSILON.to_bits()),
            ("0x1.fffffffffffffp1023", f64::MAX.to_bits()),
            ("0x1p-1022", f64::MIN_POSITIVE.to_bits()),
            ("0x1p-1074", 1),
            ("0x1.00000000000008p0", 0x3ff0_0000_0000_0000),
            ("0x1.00000000000018p0", 0x3ff0_0000_0000_0002),
            ("0x1.000000000000080000000001p0", 0x3ff0_0000_0000_0001),
            ("0x1.fffffffffffff7ffp1023", f64::MAX.to_bits()),
            ("0x1.fffffffffffff8p1023", f64::INFINITY.to_bits()),
            ("-0x1p1024", f64::NEG_INFINITY.to_bits()),
            ("0x1p-1075", 0),
            ("0x1.8p-1074", 2),
            ("0x0.fffffffffffff8p-1022", f64::MIN_POSITIVE.to_bits()),
            // A subnormal that rounding twice gets wrong.
            ("0X1.c63b83507cf448000P-1025", 0x0003_8c77_06a0_f9e9),
            ("0x1p-99999999999999999999", 0),
        ] {
            assert_eq!(round(number, BINARY64), bits, "{number}");
        }
        for (number, bits) in [
            ("0x1p0", 1f32.to_bits()),
            ("0x1.fffffep127", f32::MAX.to_bits()),
            ("0x1.ffffffp127", f32::INFINITY.to_bits()),
            ("0x1p-126", f32::MIN_POSITIVE.to_bits()),
            ("0x1p-149", 1),
            ("0x1p-150", 0),
            ("0x1.000001p0", 0x3f80_0000),
            ("0x1.000003p0", 0x3f80_0002),
        ] {
            assert_eq!(round(number, BINARY32), u64::from(bits), "{number}");
        }
    }

    fn pow2_f64(e: i32) -> f64 {
        f64::from_bits(((e + 1023) as u64) << 52)
    }

    fn pow2_f32(e: i32) -> f32 {
        f32::from_bits(((e + 127) as u32) << 23)
    }

    /// The machine as a second judge, where it rounds exactly once: an integer
    /// cast to a float is rounded to nearest, ties to even, and so is a product,
    /// which is exact while it stays normal. Each number is kept bits, then
    /// `dropped` bits holding a tie, a tie with a bit below it, or anything,
    /// lined up so that the rounding falls between the two.
    #[test]
    fn it_rounds_as_the_machine_does() {
        let mut rng = Rng(0x6a09_e667_f3bc_c908);
        let mut random = |bits: u32| u128::from(rng.next()) & ((1 << bits) - 1);
        for _ in 0..100_000 {
            let dropped = 1 + (random(64) % 52) as u32;
            let half = 1u128 << (dropped - 1);
            let tail = [half, half | 1, random(dropped)][random(64) as usize % 3];
            // Normal: as many kept bits as the format holds, rounded by the cast,
            // then scaled no further than the format reaches.
            let m = (random(52) | 1 << 52) << dropped | tail;
            let e = (random(64) % 1800) as i32 - 900;
            assert_eq!(round(&format!("{m:#x}p{e}"), BINARY64), (m as f64 * pow2_f64(e)).to_bits(), "{m:#x}p{e}");
            let m = (random(23) | 1 << 23) << dropped | tail;
            let e = (random(64) % 100) as i32 - 50;
            assert_eq!(round(&format!("{m:#x}p{e}"), BINARY32), u64::from((m as f32 * pow2_f32(e)).to_bits()), "{m:#x}p{e}");
            // Subnormal: fewer kept bits, so the cast is exact and the product
            // rounds, at the least subnormal.
            let m = random((53 - dropped).min(52)) << dropped | tail;
            let e = -1074 - dropped as i32;
            let machine = m as f64 * pow2_f64(-1000) * pow2_f64(e + 1000);
            assert_eq!(round(&format!("{m:#x}p{e}"), BINARY64), machine.to_bits(), "{m:#x}p{e}");
            let m = random((53 - dropped).min(23)) << dropped | tail;
            let e = -149 - dropped as i32;
            let machine = (m as f64 * pow2_f64(e)) as f32;
            assert_eq!(round(&format!("{m:#x}p{e}"), BINARY32), u64::from(machine.to_bits()), "{m:#x}p{e}");
        }
    }
}
