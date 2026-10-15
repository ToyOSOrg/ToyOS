//! Poly1305 on AVX2, four blocks at a time (Goll and Gueron, "Vectorization
//! of Poly1305 Message Authentication Code", ITNG 2015). Each 64-bit lane
//! holds one 26-bit limb of one of four interleaved accumulators: lane l
//! takes every fourth block and multiplies by r^4, and the last four blocks
//! meet r^4, r^3, r^2 and r, so the lanes' sum is the sequential result.
//! Limbs stay below 2^27 and every product sum below 2^60, so carries are
//! taken once per multiplication. No address or branch depends on the data
//! or the key.

use core::arch::x86_64::*;

use super::load2;
use crate::poly1305::{low, multiply};

const LIMB: u64 = (1 << 26) - 1;

/// Five 26-bit limbs of h0 + h1·2^64 + h2·2^128, h2 below 8.
fn limbs([h0, h1, h2]: [u64; 3]) -> [u64; 5] {
    [h0 & LIMB, h0 >> 26 & LIMB, (h0 >> 52 | h1 << 12) & LIMB, h1 >> 14 & LIMB, h1 >> 40 | h2 << 24]
}

/// A value's limbs in each lane, and 5 times limbs 1 to 4 (2^130 ≡ 5).
struct Multiplier {
    r: [__m256i; 5],
    s: [__m256i; 4],
}

impl Multiplier {
    #[inline]
    #[target_feature(enable = "avx2")]
    fn new(lanes: [[u64; 5]; 4]) -> Self {
        let r: [__m256i; 5] = core::array::from_fn(|k| {
            _mm256_set_epi64x(lanes[3][k] as i64, lanes[2][k] as i64, lanes[1][k] as i64, lanes[0][k] as i64)
        });
        let s = core::array::from_fn(|k| _mm256_add_epi64(r[k + 1], _mm256_slli_epi64::<2>(r[k + 1])));
        Self { r, s }
    }

    /// h·r per lane, carried once.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn multiply(&self, h: [__m256i; 5]) -> [__m256i; 5] {
        let [r0, r1, r2, r3, r4] = self.r;
        let [s1, s2, s3, s4] = self.s;
        let m = |a, b| _mm256_mul_epu32(a, b);
        let sum = |terms: [__m256i; 5]| terms.into_iter().reduce(|a, b| _mm256_add_epi64(a, b)).unwrap();
        let [h0, h1, h2, h3, h4] = h;
        let d0 = sum([m(h0, r0), m(h1, s4), m(h2, s3), m(h3, s2), m(h4, s1)]);
        let d1 = sum([m(h0, r1), m(h1, r0), m(h2, s4), m(h3, s3), m(h4, s2)]);
        let d2 = sum([m(h0, r2), m(h1, r1), m(h2, r0), m(h3, s4), m(h4, s3)]);
        let d3 = sum([m(h0, r3), m(h1, r2), m(h2, r1), m(h3, r0), m(h4, s4)]);
        let d4 = sum([m(h0, r4), m(h1, r3), m(h2, r2), m(h3, r1), m(h4, r0)]);
        carry([d0, d1, d2, d3, d4])
    }
}

/// One carry pass, two chains at a time; limbs leave below 2^26 but limb 1
/// and limb 4, which stay below 2^27.
#[inline]
#[target_feature(enable = "avx2")]
fn carry([mut d0, mut d1, mut d2, mut d3, mut d4]: [__m256i; 5]) -> [__m256i; 5] {
    let mask = _mm256_set1_epi64x(LIMB as i64);
    let step = |from: &mut __m256i, to: &mut __m256i| {
        *to = _mm256_add_epi64(*to, _mm256_srli_epi64::<26>(*from));
        *from = _mm256_and_si256(*from, mask);
    };
    step(&mut d0, &mut d1);
    step(&mut d3, &mut d4);
    step(&mut d1, &mut d2);
    let over = _mm256_srli_epi64::<26>(d4);
    d4 = _mm256_and_si256(d4, mask);
    d0 = _mm256_add_epi64(d0, _mm256_add_epi64(over, _mm256_slli_epi64::<2>(over)));
    step(&mut d2, &mut d3);
    step(&mut d0, &mut d1);
    step(&mut d3, &mut d4);
    [d0, d1, d2, d3, d4]
}

/// Four blocks' limbs, the lanes in order b0, b2, b1, b3, each with 2^128.
#[inline]
#[target_feature(enable = "avx2")]
fn message(quad: &[[u8; 16]; 4]) -> [__m256i; 5] {
    let (pairs, _) = quad.as_flattened().as_chunks::<32>();
    let (a, b) = (load2(&pairs[0]), load2(&pairs[1]));
    let low = _mm256_unpacklo_epi64(a, b);
    let high = _mm256_unpackhi_epi64(a, b);
    let mask = _mm256_set1_epi64x(LIMB as i64);
    [
        _mm256_and_si256(low, mask),
        _mm256_and_si256(_mm256_srli_epi64::<26>(low), mask),
        _mm256_and_si256(_mm256_or_si256(_mm256_srli_epi64::<52>(low), _mm256_slli_epi64::<12>(high)), mask),
        _mm256_and_si256(_mm256_srli_epi64::<14>(high), mask),
        _mm256_or_si256(_mm256_srli_epi64::<40>(high), _mm256_set1_epi64x(1 << 24)),
    ]
}

/// Absorbs the longest prefix of whole four-block groups, each block with
/// 2^128 added, into `h` and returns its length in blocks.
///
/// # Safety
/// AVX2 under YMM state is admitted.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn blocks(r: &[u64; 3], h: &mut [u64; 3], blocks: &[[u8; 16]]) -> usize {
    let (quads, _) = blocks.as_chunks::<4>();
    let Some((last, quads)) = quads.split_last() else { return 0 };
    let r1 = [r[0], r[1], 0];
    let r2 = multiply(r1, *r);
    let r3 = multiply(r2, *r);
    let r4 = multiply(r3, *r);
    let every = Multiplier::new([limbs(r4); 4]);
    // Lanes b0, b2, b1, b3 meet r^4, r^2, r^3 and r.
    let closing = Multiplier::new([limbs(r4), limbs(r2), limbs(r3), limbs(r1)]);
    let start = limbs(*h);
    let mut acc: [__m256i; 5] = core::array::from_fn(|k| _mm256_set_epi64x(0, 0, 0, start[k] as i64));
    let add = |acc: [__m256i; 5], m: [__m256i; 5]| -> [__m256i; 5] {
        core::array::from_fn(|k| _mm256_add_epi64(acc[k], m[k]))
    };
    for quad in quads {
        acc = every.multiply(add(acc, message(quad)));
    }
    acc = closing.multiply(add(acc, message(last)));
    // Limb sums over the four lanes, each below 2^29.
    let sums: [u64; 5] = acc.map(|limb| {
        // Four plain words make 32 plain bytes.
        let lanes = unsafe { core::mem::transmute::<__m256i, [u64; 4]>(limb) };
        lanes.into_iter().fold(0, u64::wrapping_add)
    });
    *h = radix64(sums);
    4 * (quads.len() + 1)
}

/// Σ limb_k·2^26k in radix 2^64 with h2 below 8, folding bits from 2^130 up
/// by 2^130 ≡ 5.
fn radix64([l0, l1, l2, l3, l4]: [u64; 5]) -> [u64; 3] {
    let t = u128::from(l0) + (u128::from(l1) << 26) + (u128::from(l2) << 52) + (u128::from(l3) << 78);
    let (sum, carry) = t.overflowing_add(u128::from(l4) << 104);
    let top = (l4 >> 24) + u64::from(carry);
    let five = (top >> 2) * 5;
    let sum0 = u128::from(low(sum)) + u128::from(five);
    let sum1 = (sum >> 64) + (sum0 >> 64);
    [low(sum0), low(sum1), (top & 3) + (sum1 >> 64) as u64]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_round_trip_and_folds_above_2_130() {
        for h in [[0, 0, 0], [u64::MAX, u64::MAX, 3], [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 2]] {
            assert_eq!(radix64(limbs(h)), h);
        }
        // 2^130 + 7 ≡ 12.
        assert_eq!(radix64([7, 0, 0, 0, 1 << 26]), [12, 0, 0]);
    }
}
