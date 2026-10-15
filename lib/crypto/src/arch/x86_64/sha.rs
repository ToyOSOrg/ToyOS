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

/// # Safety
/// BMI2 is admitted.
#[target_feature(enable = "bmi2")]
pub(super) unsafe fn sha512(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
    crate::hash::sha512_rounds(state, blocks)
}
