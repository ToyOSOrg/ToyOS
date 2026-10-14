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

#[inline]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
fn schedule(w0: __m128i, w1: __m128i, w2: __m128i, w3: __m128i) -> __m128i {
    // W[t..t+4] from the four groups before it, oldest first.
    let partial = _mm_add_epi32(_mm_sha256msg1_epu32(w0, w1), _mm_alignr_epi8::<4>(w3, w2));
    _mm_sha256msg2_epu32(partial, w3)
}

/// # Safety
/// SHA, SSSE3 and SSE4.1 are admitted.
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub(super) unsafe fn sha256(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    // Every access is inside `state`, a block or `K256`.
    unsafe {
        let flip = _mm_set_epi8(12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3);
        let abcd = _mm_loadu_si128(state.as_ptr().cast());
        let efgh = _mm_loadu_si128(state.as_ptr().add(4).cast());
        let badc = _mm_shuffle_epi32::<0xb1>(abcd);
        let hgfe = _mm_shuffle_epi32::<0x1b>(efgh);
        let mut abef = _mm_alignr_epi8::<8>(badc, hgfe);
        let mut cdgh = _mm_blend_epi16::<0xf0>(hgfe, badc);
        let k = |i: usize| _mm_loadu_si128(K256.as_ptr().add(4 * i).cast());
        for block in blocks {
            let (saved_abef, saved_cdgh) = (abef, cdgh);
            let p = block.as_ptr();
            let mut w = [0; 4].map(|_| _mm_setzero_si128());
            for i in 0..16 {
                let group = if i < 4 {
                    _mm_shuffle_epi8(_mm_loadu_si128(p.add(16 * i).cast()), flip)
                } else {
                    schedule(w[i & 3], w[(i + 1) & 3], w[(i + 2) & 3], w[(i + 3) & 3])
                };
                w[i & 3] = group;
                let sum = _mm_add_epi32(group, k(i));
                let previous = abef;
                abef = _mm_sha256rnds2_epu32(cdgh, abef, sum);
                cdgh = previous;
                let previous = abef;
                abef = _mm_sha256rnds2_epu32(cdgh, abef, _mm_shuffle_epi32::<0x0e>(sum));
                cdgh = previous;
            }
            abef = _mm_add_epi32(abef, saved_abef);
            cdgh = _mm_add_epi32(cdgh, saved_cdgh);
        }
        let abef = _mm_shuffle_epi32::<0x1b>(abef);
        let ghcd = _mm_shuffle_epi32::<0xb1>(cdgh);
        _mm_storeu_si128(state.as_mut_ptr().cast(), _mm_blend_epi16::<0xf0>(abef, ghcd));
        _mm_storeu_si128(state.as_mut_ptr().add(4).cast(), _mm_alignr_epi8::<8>(ghcd, abef));
    }
}

/// # Safety
/// BMI2 is admitted.
#[target_feature(enable = "bmi2")]
pub(super) unsafe fn sha512(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
    crate::hash::sha512_rounds(state, blocks)
}
