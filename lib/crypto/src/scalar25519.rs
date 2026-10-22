//! Scalars modulo the Ed25519 prime subgroup order L, in four 64-bit words.
//! Reduction of a 512-bit integer is Barrett's (Menezes, van Oorschot and
//! Vanstone, Handbook of Applied Cryptography, algorithm 14.42, b = 2^64,
//! k = 4): a fixed schedule of products and one masked subtraction.
//!
//! [`S`] is a secret scalar, erased on drop. [`PublicScalar`] is a public one,
//! for verification's variable-time multiplication; neither converts into the
//! other.
#![allow(
    clippy::needless_range_loop,
    reason = "carry chains index parallel fixed-size arrays, as `montgomery` does"
)]
use crate::{ct, secret::wipe64};

const L: [u64; 4] = [0x5812631a5cf5d3ed, 0x14def9dea2f79cd6, 0, 0x1000000000000000];
/// floor(2^512 / L).
const MU: [u64; 5] = [
    0xed9ce5a30a2c131b,
    0x2106215d086329a7,
    0xffffffffffffffeb,
    0xffffffffffffffff,
    0x000000000000000f,
];

/// A secret scalar below L.
#[cfg(any(test, feature = "sign"))]
pub(crate) struct S([u64; 4]);

#[cfg(any(test, feature = "sign"))]
impl Drop for S {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}

/// A public scalar below L.
#[derive(Clone, Copy)]
pub(crate) struct PublicScalar([u64; 4]);

#[inline(always)]
fn mac(t: u64, a: u64, b: u64, c: u64) -> (u64, u64) {
    let v = u128::from(a)
        .wrapping_mul(u128::from(b))
        .wrapping_add(u128::from(t))
        .wrapping_add(u128::from(c));
    (v as u64, (v >> 64) as u64)
}

/// The full product of an M-word and an N-word integer, in M + N words.
#[inline(always)]
fn product<const M: usize, const N: usize, const MN: usize>(a: &[u64; M], b: &[u64; N]) -> [u64; MN] {
    let mut t = [0u64; MN];
    for i in 0..M {
        let mut c = 0;
        for j in 0..N {
            (t[i + j], c) = mac(t[i + j], a[i], b[j], c);
        }
        t[i + N] = c;
    }
    t
}

/// a - b modulo 2^320.
fn sub5(a: &[u64; 5], b: &[u64; 5]) -> [u64; 5] {
    let mut r = [0u64; 5];
    let mut borrow = false;
    for i in 0..5 {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(u64::from(borrow));
        r[i] = d;
        borrow = b1 | b2;
    }
    r
}

/// r - L under a mask when r >= L, for r < 2L.
fn subtract_l(r: &mut [u64; 5]) {
    let mut d = sub5(r, &[L[0], L[1], L[2], L[3], 0]);
    // r < 2L < 2^254, so the difference's top bit is set exactly when r < L.
    let keep = ct::mask(d[4] >> 63);
    for i in 0..5 {
        r[i] = (r[i] & keep) | (d[i] & !keep);
    }
    wipe64(&mut d);
}

/// x mod L for a 512-bit x.
fn barrett(x: &[u64; 8]) -> [u64; 4] {
    // q3 = floor(floor(x / b^(k-1)) mu / b^(k+1)) is floor(x / L) or one
    // less: for x < 2^512 the estimate falls short of x / L by under
    // (2^512 / L - mu) + 2^-60 < 1/4 (HAC note 14.44 bounds it by 2 in general).
    let q1 = [x[3], x[4], x[5], x[6], x[7]];
    let mut q2: [u64; 10] = product(&q1, &MU);
    let q3 = [q2[5], q2[6], q2[7], q2[8], q2[9]];
    let mut q3l: [u64; 9] = product(&q3, &L);
    // r = x - q3 L modulo b^(k+1), which is below 2L.
    let mut r = sub5(&[x[0], x[1], x[2], x[3], x[4]], &[q3l[0], q3l[1], q3l[2], q3l[3], q3l[4]]);
    subtract_l(&mut r);
    let s = [r[0], r[1], r[2], r[3]];
    wipe64(&mut q2);
    wipe64(&mut q3l);
    wipe64(&mut r);
    s
}

fn words<const N: usize, const W: usize>(bytes: &[u8; N]) -> [u64; W] {
    core::array::from_fn(|i| u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap()))
}

#[cfg(any(test, feature = "sign"))]
fn bytes(words: &[u64; 4]) -> [u8; 32] {
    let mut out = [0; 32];
    for i in 0..4 {
        out[8 * i..8 * i + 8].copy_from_slice(&words[i].to_le_bytes())
    }
    out
}

#[cfg(any(test, feature = "sign"))]
impl S {
    /// A 64-byte little-endian integer, reduced.
    pub(crate) fn reduce(input: &[u8; 64]) -> Self {
        let mut x: [u64; 8] = words(input);
        let s = Self(barrett(&x));
        wipe64(&mut x);
        s
    }

    /// A 32-byte little-endian integer, reduced.
    #[cfg(feature = "sign")]
    pub(crate) fn reduce32(input: &[u8; 32]) -> Self {
        let mut x = [0u64; 8];
        x[..4].copy_from_slice(&words::<32, 4>(input));
        let s = Self(barrett(&x));
        wipe64(&mut x);
        s
    }

    pub(crate) fn bytes(&self) -> [u8; 32] {
        bytes(&self.0)
    }

    /// self * b + c mod L.
    #[cfg(feature = "sign")]
    pub(crate) fn mul_add(&self, b: &Self, c: &Self) -> Self {
        let mut t: [u64; 8] = product(&self.0, &b.0);
        // t < L^2 < 2^506, so adding c < L stays below 2^512.
        let mut carry = false;
        for i in 0..8 {
            let add = if i < 4 { c.0[i] } else { 0 };
            let (s, c1) = t[i].overflowing_add(add);
            let (s, c2) = s.overflowing_add(u64::from(carry));
            t[i] = s;
            carry = c1 | c2;
        }
        let s = Self(barrett(&t));
        wipe64(&mut t);
        s
    }
}

impl PublicScalar {
    /// A public 64-byte little-endian integer, reduced.
    pub(crate) fn reduce(input: &[u8; 64]) -> Self {
        Self(barrett(&words(input)))
    }

    /// A 32-byte encoding below L, or `None`.
    pub(crate) fn canonical(input: &[u8; 32]) -> Option<Self> {
        let s: [u64; 4] = words(input);
        let mut borrow = false;
        for i in 0..4 {
            let (d, b1) = s[i].overflowing_sub(L[i]);
            let (_, b2) = d.overflowing_sub(u64::from(borrow));
            borrow = b1 | b2;
        }
        borrow.then_some(Self(s))
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> [u8; 32] {
        bytes(&self.0)
    }

    /// The width-w non-adjacent form, w in 2..=8: odd digits below 2^(w-1) in
    /// magnitude, any two nonzero ones at least w positions apart (Hankerson,
    /// Menezes and Vanstone, Guide to Elliptic Curve Cryptography, algorithm
    /// 3.35). Variable time: the scalar is public.
    pub(crate) fn naf(&self, w: u32) -> [i8; 256] {
        debug_assert!((2..=8).contains(&w));
        let mut k = self.0;
        let mut digits = [0i8; 256];
        let mut i = 0;
        while k != [0; 4] {
            if k[0] & 1 == 1 {
                let window = (k[0] & ((1 << w) - 1)) as i32;
                let d = if window >= 1 << (w - 1) { window - (1 << w) } else { window };
                digits[i] = d as i8;
                // k - d, where |d| < 2^7 and k < 2^253 + 2^7: no word overflows.
                if d >= 0 {
                    k[0] -= d as u64;
                } else {
                    let (s, mut carry) = k[0].overflowing_add((-d) as u64);
                    k[0] = s;
                    for word in &mut k[1..] {
                        if !carry {
                            break;
                        }
                        (*word, carry) = word.overflowing_add(1);
                    }
                }
            }
            k = [k[0] >> 1 | k[1] << 63, k[1] >> 1 | k[2] << 63, k[2] >> 1 | k[3] << 63, k[3] >> 1];
            i += 1;
        }
        digits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::{BigInt, BigUint};

    /// mu = floor(2^512 / L), and 2^512 / L - mu < 1/4, the bound by which
    /// one subtraction of L suffices.
    #[test]
    fn barretts_constant_is_floor_of_2_512_over_l() {
        let l = BigUint::from_bytes_le(&bytes(&L));
        let mu = BigUint::from_bytes_le(&MU.iter().flat_map(|w| w.to_le_bytes()).collect::<std::vec::Vec<_>>());
        let two_512 = BigUint::from(1u8) << 512;
        assert_eq!(mu, &two_512 / &l);
        assert!((two_512 - &mu * &l) * 4u8 < l);
    }

    /// Each width's digits are odd, bounded, w apart, and sum to the scalar.
    #[test]
    fn naf_digits_recompose_the_scalar() {
        let mut seed = 0x2545f4914f6cdd1du64;
        for case in 0..200 {
            let mut wide = [0u8; 64];
            for byte in &mut wide {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *byte = seed as u8;
            }
            if case == 0 {
                wide = [0xff; 64];
            }
            let s = PublicScalar::reduce(&wide);
            for w in 2..=8u32 {
                let digits = s.naf(w);
                let mut sum = BigInt::from(0);
                let mut last: Option<usize> = None;
                for (i, &d) in digits.iter().enumerate() {
                    if d != 0 {
                        assert!(d % 2 != 0 && i32::from(d).abs() < 1 << (w - 1), "digit {d} at width {w}");
                        if let Some(j) = last {
                            assert!(i - j >= w as usize, "digits {j} and {i} at width {w}");
                        }
                        last = Some(i);
                    }
                    sum += BigInt::from(d) << i;
                }
                assert_eq!(sum, BigInt::from(BigUint::from_bytes_le(&s.bytes())));
            }
        }
    }
}
