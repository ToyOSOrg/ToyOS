//! SHA-256 on the SHA extensions, and SHA-512's portable rounds compiled for
//! BMI2's non-destructive rotate.
//!
//! SHA256RNDS2 computes two rounds over the working variables held as ABEF
//! and CDGH (A in the high dword); SHA256MSG1 and SHA256MSG2 extend the
//! message schedule four words at a time (Intel SDM Vol. 2B; Gulley et al.,
//! "Intel SHA Extensions", Intel, 2013). No address or branch depends on the
//! message.

use core::arch::x86_64::*;

use crate::hash::K256;

use super::load;

#[inline]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
fn schedule(w0: __m128i, w1: __m128i, w2: __m128i, w3: __m128i) -> __m128i {
    // W[t..t+4] from the four groups before it, oldest first.
    let partial = _mm_add_epi32(_mm_sha256msg1_epu32(w0, w1), _mm_alignr_epi8::<4>(w3, w2));
    _mm_sha256msg2_epu32(partial, w3)
}

#[inline]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
fn k(i: usize) -> __m128i {
    // Four plain words make 16 plain bytes.
    unsafe { core::mem::transmute::<[u32; 4], __m128i>(K256.as_chunks::<4>().0[i]) }
}

/// # Safety
/// SHA, SSSE3 and SSE4.1 are admitted.
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub(super) unsafe fn sha256(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    let flip = _mm_set_epi8(12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3);
    // Eight plain words make two vectors of 16 plain bytes.
    let [abcd, efgh] = unsafe { core::mem::transmute::<[u32; 8], [__m128i; 2]>(*state) };
    let badc = _mm_shuffle_epi32::<0xb1>(abcd);
    let hgfe = _mm_shuffle_epi32::<0x1b>(efgh);
    let mut abef = _mm_alignr_epi8::<8>(badc, hgfe);
    let mut cdgh = _mm_blend_epi16::<0xf0>(hgfe, badc);
    for block in blocks {
        let (saved_abef, saved_cdgh) = (abef, cdgh);
        let (groups, _) = block.as_chunks::<16>();
        // Four rounds with message group `w`: two SHA256RNDS2, each of which
        // turns ABEF into the next and leaves the old as CDGH.
        macro_rules! rounds4 {
            ($w:expr, $i:expr) => {
                let sum = _mm_add_epi32($w, k($i));
                let previous = abef;
                abef = _mm_sha256rnds2_epu32(cdgh, abef, sum);
                cdgh = previous;
                let previous = abef;
                abef = _mm_sha256rnds2_epu32(cdgh, abef, _mm_shuffle_epi32::<0x0e>(sum));
                cdgh = previous;
            };
        }
        let mut w0 = _mm_shuffle_epi8(load(&groups[0]), flip);
        let mut w1 = _mm_shuffle_epi8(load(&groups[1]), flip);
        let mut w2 = _mm_shuffle_epi8(load(&groups[2]), flip);
        let mut w3 = _mm_shuffle_epi8(load(&groups[3]), flip);
        rounds4!(w0, 0);
        rounds4!(w1, 1);
        rounds4!(w2, 2);
        rounds4!(w3, 3);
        for i in 1..4 {
            w0 = schedule(w0, w1, w2, w3);
            rounds4!(w0, 4 * i);
            w1 = schedule(w1, w2, w3, w0);
            rounds4!(w1, 4 * i + 1);
            w2 = schedule(w2, w3, w0, w1);
            rounds4!(w2, 4 * i + 2);
            w3 = schedule(w3, w0, w1, w2);
            rounds4!(w3, 4 * i + 3);
        }
        abef = _mm_add_epi32(abef, saved_abef);
        cdgh = _mm_add_epi32(cdgh, saved_cdgh);
    }
    let abef = _mm_shuffle_epi32::<0x1b>(abef);
    let ghcd = _mm_shuffle_epi32::<0xb1>(cdgh);
    let halves = [_mm_blend_epi16::<0xf0>(abef, ghcd), _mm_alignr_epi8::<8>(ghcd, abef)];
    // Two vectors of 16 plain bytes make eight plain words.
    *state = unsafe { core::mem::transmute::<[__m128i; 2], [u32; 8]>(halves) };
}

#[inline]
#[target_feature(enable = "avx2")]
fn ror_avx2<const R: i32, const L: i32>(x: __m128i) -> __m128i {
    _mm_or_si128(_mm_srli_epi64::<R>(x), _mm_slli_epi64::<L>(x))
}

#[inline]
#[target_feature(enable = "avx2")]
fn small0_avx2(x: __m128i) -> __m128i {
    _mm_xor_si128(_mm_xor_si128(ror_avx2::<1, 63>(x), ror_avx2::<8, 56>(x)), _mm_srli_epi64::<7>(x))
}

#[inline]
#[target_feature(enable = "avx2")]
fn small1_avx2(x: __m128i) -> __m128i {
    _mm_xor_si128(_mm_xor_si128(ror_avx2::<19, 45>(x), ror_avx2::<61, 3>(x)), _mm_srli_epi64::<6>(x))
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn small0_avx512(x: __m128i) -> __m128i {
    // 0x96: the three-way XOR.
    _mm_ternarylogic_epi64::<0x96>(_mm_ror_epi64::<1>(x), _mm_ror_epi64::<8>(x), _mm_srli_epi64::<7>(x))
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn small1_avx512(x: __m128i) -> __m128i {
    _mm_ternarylogic_epi64::<0x96>(_mm_ror_epi64::<19>(x), _mm_ror_epi64::<61>(x), _mm_srli_epi64::<6>(x))
}

/// SHA-512 with the message schedule on vector registers, two words each
/// (σ1 reads W[t-2], so a pair is the widest independent step), sixteen
/// words ahead of the scalar rounds, which take W[t] + K[t] whole (Guilford
/// et al., "Fast SHA512 Implementations on Intel Architecture Processors",
/// Intel, 2012). Built for AVX2 and BMI2, and for AVX-512VL's rotates.
macro_rules! sha512 {
    ($name:ident, $features:literal, $small0:ident, $small1:ident) => {
        /// # Safety
        /// The features are admitted.
        #[target_feature(enable = $features)]
        pub(super) unsafe fn $name(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
            use crate::hash::{K512, big0, big1, round};
            let flip = _mm_set_epi8(8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7);
            let (k, _) = K512.as_chunks::<2>();
            // Two plain words make 16 plain bytes, and back.
            let pair = |x: [u64; 2]| unsafe { core::mem::transmute::<[u64; 2], __m128i>(x) };
            let words = |x: __m128i| unsafe { core::mem::transmute::<__m128i, [u64; 2]>(x) };
            const ZERO: [u64; 16] = [0; 16];
            let mut wk = crate::secret::Secret([0u64; 16]);
            for block in blocks {
                let (groups, _) = block.as_chunks::<16>();
                let mut w: [__m128i; 8] = core::array::from_fn(|j| _mm_shuffle_epi8(load(&groups[j]), flip));
                for (j, w) in w.iter().enumerate() {
                    let [x, y] = words(_mm_add_epi64(*w, pair(k[j])));
                    (wk.0[2 * j], wk.0[2 * j + 1]) = (x, y);
                }
                let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
                let mut bc = b ^ c;
                for pass in 0..5 {
                    let mut current = wk.0;
                    if pass < 4 {
                        // W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16]
                        // for the next sixteen t, in place over a window of
                        // eight pairs.
                        for j in 0..8 {
                            let w15 = _mm_alignr_epi8::<8>(w[(j + 1) & 7], w[j]);
                            let w7 = _mm_alignr_epi8::<8>(w[(j + 5) & 7], w[(j + 4) & 7]);
                            let next = _mm_add_epi64(
                                _mm_add_epi64(w[j], $small0(w15)),
                                _mm_add_epi64(w7, $small1(w[(j + 7) & 7])),
                            );
                            w[j] = next;
                            let [x, y] = words(_mm_add_epi64(next, pair(k[8 * (pass + 1) + j])));
                            (wk.0[2 * j], wk.0[2 * j + 1]) = (x, y);
                        }
                    }
                    let sum = |x: u64| x;
                    round!(current, ZERO, bc, 0, false, [a, b, c, d, e, f, g, h], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 1, false, [h, a, b, c, d, e, f, g], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 2, false, [g, h, a, b, c, d, e, f], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 3, false, [f, g, h, a, b, c, d, e], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 4, false, [e, f, g, h, a, b, c, d], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 5, false, [d, e, f, g, h, a, b, c], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 6, false, [c, d, e, f, g, h, a, b], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 7, false, [b, c, d, e, f, g, h, a], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 8, false, [a, b, c, d, e, f, g, h], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 9, false, [h, a, b, c, d, e, f, g], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 10, false, [g, h, a, b, c, d, e, f], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 11, false, [f, g, h, a, b, c, d, e], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 12, false, [e, f, g, h, a, b, c, d], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 13, false, [d, e, f, g, h, a, b, c], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 14, false, [c, d, e, f, g, h, a, b], big0, big1, sum, sum);
                    round!(current, ZERO, bc, 15, false, [b, c, d, e, f, g, h, a], big0, big1, sum, sum);
                }
                for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
                    *s = s.wrapping_add(v);
                }
            }
        }
    };
}

sha512!(sha512_avx2, "avx2,bmi2,ssse3", small0_avx2, small1_avx2);
sha512!(sha512_avx512, "avx2,bmi2,ssse3,avx512f,avx512vl", small0_avx512, small1_avx512);
