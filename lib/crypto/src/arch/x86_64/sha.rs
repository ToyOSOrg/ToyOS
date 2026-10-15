//! SHA-256 on the SHA extensions, and SHA-512 with BMI2's non-destructive
//! rotate and its message schedule on vector registers.
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

/// SHA-512 with the message schedule on vector registers, two words each
/// (σ1 reads W[t-2], so a pair is the widest independent step), sixteen
/// words ahead of the scalar rounds, which take W[t] + K[t] whole (Guilford
/// et al., "Fast SHA512 Implementations on Intel Architecture Processors",
/// Intel, 2012). The sums alternate between two buffers, so a round reads
/// a word stored a whole pass of rounds earlier: a 64-bit load from half of
/// a 128-bit store just made is not forwarded on every CPU. The first
/// pass's sums are formed in scalar registers.
macro_rules! sha512 {
    ($name:ident, $features:literal, $small0:ident, $small1:ident) => {
        /// # Safety
        /// The features are admitted.
        #[target_feature(enable = $features)]
        pub(super) unsafe fn $name(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
            use crate::hash::{K512, big0, big1};
            let flip = _mm_set_epi8(8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7);
            let (k, _) = K512.as_chunks::<2>();
            // Two plain words make 16 plain bytes, and back.
            let pair = |x: [u64; 2]| unsafe { core::mem::transmute::<[u64; 2], __m128i>(x) };
            let words = |x: __m128i| unsafe { core::mem::transmute::<__m128i, [u64; 2]>(x) };
            let mut wk = crate::secret::Secret([[0u64; 16]; 2]);
            for n in 0..blocks.len() {
                let (groups, _) = blocks[n].as_chunks::<16>();
                let (scalar, _) = blocks[n].as_chunks::<8>();
                let mut w = [_mm_setzero_si128(); 8];
                for j in 0..8 {
                    w[j] = _mm_shuffle_epi8(load(&groups[j]), flip);
                }
                for i in 0..16 {
                    wk.0[0][i] = u64::from_be_bytes(scalar[i]).wrapping_add(K512[i]);
                }
                let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
                let mut bc = b ^ c;
                for pass in 0..5 {
                    if pass < 4 {
                        // W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16]
                        // for the next sixteen t, in place over a window of
                        // eight pairs, into the other buffer.
                        let (next, _) = wk.0[(pass + 1) & 1].as_chunks_mut::<2>();
                        for j in 0..8 {
                            let w15 = _mm_alignr_epi8::<8>(w[(j + 1) & 7], w[j]);
                            let w7 = _mm_alignr_epi8::<8>(w[(j + 5) & 7], w[(j + 4) & 7]);
                            w[j] = _mm_add_epi64(
                                _mm_add_epi64(w[j], $small0(w15)),
                                _mm_add_epi64(w7, $small1(w[(j + 7) & 7])),
                            );
                            next[j] = words(_mm_add_epi64(w[j], pair(k[8 * (pass + 1) + j])));
                        }
                    }
                    let current = &wk.0[pass & 1];
                    macro_rules! round {
                        ($i:literal, [$a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident]) => {
                            let t1 = $h
                                .wrapping_add(big1($e))
                                .wrapping_add($g ^ ($e & ($f ^ $g)))
                                .wrapping_add(current[$i]);
                            // Maj(a, b, c) = ((a ^ b) & (b ^ c)) ^ b, and this
                            // round's a ^ b is the next round's b ^ c.
                            let ab = $a ^ $b;
                            let t2 = big0($a).wrapping_add((ab & bc) ^ $b);
                            bc = ab;
                            $d = $d.wrapping_add(t1);
                            $h = t1.wrapping_add(t2);
                        };
                    }
                    round!(0, [a, b, c, d, e, f, g, h]);
                    round!(1, [h, a, b, c, d, e, f, g]);
                    round!(2, [g, h, a, b, c, d, e, f]);
                    round!(3, [f, g, h, a, b, c, d, e]);
                    round!(4, [e, f, g, h, a, b, c, d]);
                    round!(5, [d, e, f, g, h, a, b, c]);
                    round!(6, [c, d, e, f, g, h, a, b]);
                    round!(7, [b, c, d, e, f, g, h, a]);
                    round!(8, [a, b, c, d, e, f, g, h]);
                    round!(9, [h, a, b, c, d, e, f, g]);
                    round!(10, [g, h, a, b, c, d, e, f]);
                    round!(11, [f, g, h, a, b, c, d, e]);
                    round!(12, [e, f, g, h, a, b, c, d]);
                    round!(13, [d, e, f, g, h, a, b, c]);
                    round!(14, [c, d, e, f, g, h, a, b]);
                    round!(15, [b, c, d, e, f, g, h, a]);
                }
                let v = [a, b, c, d, e, f, g, h];
                for i in 0..8 {
                    state[i] = state[i].wrapping_add(v[i]);
                }
            }
        }
    };
}

sha512!(sha512_avx2, "avx2,bmi2,ssse3", small0_avx2, small1_avx2);
sha512!(sha512_avx512, "avx2,bmi2,ssse3,avx512f,avx512vl", small0_avx512, small1_avx512);
