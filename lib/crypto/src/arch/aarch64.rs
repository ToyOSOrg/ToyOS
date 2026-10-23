//! SHA-256 and SHA-512 on the Armv8 Cryptographic Extension, where the target
//! guarantees it at compile time (`sha2`: FEAT_SHA256; `sha3`: FEAT_SHA512, as
//! Rust names them). No AArch64 target ToyOS runs on detects them at run time
//! yet, so a target that does not promise them takes the portable rounds.
//!
//! The instructions' semantics are the Arm Architecture Reference Manual's
//! (DDI 0487), SHA256H, SHA256H2, SHA256SU0, SHA256SU1, SHA512H, SHA512H2,
//! SHA512SU0 and SHA512SU1. No address or branch depends on the message.

#![allow(clippy::needless_range_loop, reason = "the rounds index fixed-size arrays")]

#[cfg(any(target_feature = "sha2", target_feature = "sha3"))]
use core::arch::aarch64::*;

#[cfg(target_feature = "sha2")]
pub(super) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    if cfg!(feature = "portable") {
        return false;
    }
    // SAFETY: the target guarantees FEAT_SHA256.
    unsafe { sha256(state, blocks) }
    true
}

#[cfg(target_feature = "sha2")]
#[target_feature(enable = "neon,sha2")]
fn sha256(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    let mut k = [vdupq_n_u32(0); 16];
    for i in 0..16 {
        // SAFETY: four plain words.
        k[i] = unsafe { vld1q_u32(crate::hash::K256[4 * i..].as_ptr()) };
    }
    // SAFETY: `state` holds eight plain words.
    let (mut abcd, mut efgh) = unsafe { (vld1q_u32(state.as_ptr()), vld1q_u32(state[4..].as_ptr())) };
    for block in blocks {
        let (saved_abcd, saved_efgh) = (abcd, efgh);
        let mut w = [vdupq_n_u32(0); 4];
        for i in 0..4 {
            // SAFETY: the block's 64 bytes, read 16 at a time: its sixteen
            // big-endian words, four to a vector.
            w[i] = unsafe { vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block[16 * i..].as_ptr()))) };
        }
        for i in 0..16 {
            if i >= 4 {
                // W[4i..4i+4] from the four groups before it, oldest first.
                w[i % 4] = vsha256su1q_u32(vsha256su0q_u32(w[i % 4], w[(i + 1) % 4]), w[(i + 2) % 4], w[(i + 3) % 4]);
            }
            let wk = vaddq_u32(w[i % 4], k[i]);
            let previous = abcd;
            abcd = vsha256hq_u32(abcd, efgh, wk);
            efgh = vsha256h2q_u32(efgh, previous, wk);
        }
        abcd = vaddq_u32(abcd, saved_abcd);
        efgh = vaddq_u32(efgh, saved_efgh);
    }
    // SAFETY: `state` has room for eight words.
    unsafe {
        vst1q_u32(state.as_mut_ptr(), abcd);
        vst1q_u32(state[4..].as_mut_ptr(), efgh);
    }
}

#[cfg(not(target_feature = "sha2"))]
pub(super) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    let _ = (state, blocks);
    false
}

/// Two rounds to a SHA512H and SHA512H2 pair. The working variables are held
/// in pairs, the lower lane first: `ab` is (A, B), `cd` (C, D), `ef` (E, F)
/// and `gh` (G, H). SHA512H takes (H + K[t] + W[t]) in its high lane and
/// (G + K[t+1] + W[t+1]) in its low one, with (D, E) and (F, G), and answers
/// T1 of both rounds; SHA512H2 turns those, with (A, B) and C, into the two
/// new A.
#[cfg(target_feature = "sha3")]
pub(super) fn sha512_blocks(state: &mut [u64; 8], blocks: &[[u8; 128]]) -> bool {
    if cfg!(feature = "portable") {
        return false;
    }
    // SAFETY: the target guarantees FEAT_SHA512.
    unsafe { sha512(state, blocks) }
    true
}

#[cfg(target_feature = "sha3")]
#[target_feature(enable = "neon,sha3")]
fn sha512(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
    let mut s = [vdupq_n_u64(0); 4];
    for i in 0..4 {
        // SAFETY: `state` holds eight plain words.
        s[i] = unsafe { vld1q_u64(state[2 * i..].as_ptr()) };
    }
    for block in blocks {
        let saved = s;
        let [mut ab, mut cd, mut ef, mut gh] = s;
        let mut w = [vdupq_n_u64(0); 8];
        for i in 0..8 {
            // SAFETY: the block's 128 bytes, read 16 at a time: its sixteen
            // big-endian words, two to a vector.
            w[i] = unsafe { vreinterpretq_u64_u8(vrev64q_u8(vld1q_u8(block[16 * i..].as_ptr()))) };
        }
        for i in 0..40 {
            if i >= 8 {
                // W[2i] and W[2i+1]: the pair eight before, σ0 of the words
                // after it, σ1 of the pair just before and W[2i-7..2i-5].
                let sigma0 = vsha512su0q_u64(w[i % 8], w[(i + 1) % 8]);
                w[i % 8] = vsha512su1q_u64(sigma0, w[(i + 7) % 8], vextq_u64::<1>(w[(i + 4) % 8], w[(i + 5) % 8]));
            }
            // SAFETY: two plain words of K.
            let kw = vaddq_u64(w[i % 8], unsafe { vld1q_u64(crate::hash::K512[2 * i..].as_ptr()) });
            let t1 = vsha512hq_u64(
                vaddq_u64(gh, vextq_u64::<1>(kw, kw)),
                vextq_u64::<1>(ef, gh),
                vextq_u64::<1>(cd, ef),
            );
            let a = vsha512h2q_u64(t1, cd, ab);
            (ab, cd, ef, gh) = (a, ab, vaddq_u64(cd, t1), ef);
        }
        s = [ab, cd, ef, gh];
        for i in 0..4 {
            s[i] = vaddq_u64(s[i], saved[i]);
        }
    }
    for i in 0..4 {
        // SAFETY: `state` has room for eight words.
        unsafe { vst1q_u64(state[2 * i..].as_mut_ptr(), s[i]) }
    }
}

#[cfg(not(target_feature = "sha3"))]
pub(super) fn sha512_blocks(state: &mut [u64; 8], blocks: &[[u8; 128]]) -> bool {
    let _ = (state, blocks);
    false
}
