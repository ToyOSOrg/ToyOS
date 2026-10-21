//! GCM on 512-bit VAES and VPCLMULQDQ under AVX-512 (Intel SDM Vol. 2B):
//! each register holds four blocks or four carry-less products, in `aesni`'s
//! GHASH form, and sixteen blocks share one reduction with H^16..H^1. Only
//! hosted targets with OS-enabled ZMM state enter these functions. Hot loops
//! index fixed-size arrays, as `aesni`'s header explains.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

use core::arch::x86_64::*;

use super::aesni::{Product, counter_base, power, reverse};
use super::{load, store};
use crate::arch::Powers;

#[inline]
#[target_feature(enable = "avx512f")]
fn load4(bytes: &[u8; 64]) -> __m512i {
    // Both are 64 plain bytes, every pattern valid.
    unsafe { core::mem::transmute(*bytes) }
}

#[inline]
#[target_feature(enable = "avx512f")]
fn store4(bytes: &mut [u8; 64], v: __m512i) {
    // Both are 64 plain bytes, every pattern valid.
    *bytes = unsafe { core::mem::transmute::<__m512i, [u8; 64]>(v) };
}

/// Each 128-bit lane's bytes reversed.
#[inline]
#[target_feature(enable = "avx512f,avx512bw")]
fn reverse_lanes(x: __m512i) -> __m512i {
    let lane: [u8; 16] = [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    _mm512_shuffle_epi8(x, _mm512_broadcast_i32x4(load(&lane)))
}

/// Four lanes from four 128-bit values, the first lowest.
#[inline]
#[target_feature(enable = "avx512f")]
fn quad(a: __m128i, b: __m128i, c: __m128i, d: __m128i) -> __m512i {
    let low = _mm512_inserti32x4::<1>(_mm512_castsi128_si512(a), b);
    let high = _mm512_inserti32x4::<2>(low, c);
    _mm512_inserti32x4::<3>(high, d)
}

/// The four lanes XORed into one.
#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn fold(x: __m512i) -> __m128i {
    let half = _mm256_xor_si256(_mm512_castsi512_si256(x), _mm512_extracti64x4_epi64::<1>(x));
    _mm_xor_si128(_mm256_castsi256_si128(half), _mm256_extracti128_si256::<1>(half))
}

#[derive(Clone, Copy)]
struct Product4 {
    low: __m512i,
    high: __m512i,
    middle: __m512i,
}

impl Product4 {
    #[inline]
    #[target_feature(enable = "avx512f,vpclmulqdq")]
    fn new(x: __m512i, h: __m512i, folded: __m512i) -> Self {
        Self {
            low: _mm512_clmulepi64_epi128::<0x00>(x, h),
            high: _mm512_clmulepi64_epi128::<0x11>(x, h),
            middle: _mm512_clmulepi64_epi128::<0x00>(_mm512_xor_si512(x, _mm512_shuffle_epi32::<0x4e>(x)), folded),
        }
    }

    #[inline]
    #[target_feature(enable = "avx512f,vpclmulqdq")]
    fn add(self, x: __m512i, h: __m512i, folded: __m512i) -> Self {
        let p = Self::new(x, h, folded);
        Self {
            low: _mm512_xor_si512(self.low, p.low),
            high: _mm512_xor_si512(self.high, p.high),
            middle: _mm512_xor_si512(self.middle, p.middle),
        }
    }

    /// All lanes' sums, reduced once.
    #[inline]
    #[target_feature(enable = "avx512f,avx512vl,pclmulqdq")]
    fn reduce(self) -> __m128i {
        Product { low: fold(self.low), high: fold(self.high), middle: fold(self.middle) }.reduce()
    }
}

/// H^16..H^1 in lane quads, block 4j + l against H^(16-4j-l), and their folds.
struct Quads {
    h: [__m512i; 4],
    folds: [__m512i; 4],
}

impl Quads {
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn new(powers: &Powers) -> Self {
        let mut h = [_mm512_setzero_si512(); 4];
        let mut folds = [_mm512_setzero_si512(); 4];
        for j in 0..4 {
            let top = 15 - 4 * j;
            h[j] = quad(power(powers[top]), power(powers[top - 1]), power(powers[top - 2]), power(powers[top - 3]));
            folds[j] = _mm512_xor_si512(h[j], _mm512_shuffle_epi32::<0x4e>(h[j]));
        }
        Self { h, folds }
    }

    /// Sixteen byte-reversed blocks after `y`.
    #[inline]
    #[target_feature(enable = "avx512f,avx512vl,vpclmulqdq,pclmulqdq")]
    fn absorb(&self, y: __m128i, x: [__m512i; 4]) -> __m128i {
        let first = _mm512_xor_si512(x[0], _mm512_zextsi128_si512(y));
        let mut sum = Product4::new(first, self.h[0], self.folds[0]);
        for j in 1..4 {
            sum = sum.add(x[j], self.h[j], self.folds[j]);
        }
        sum.reduce()
    }
}

#[inline]
#[target_feature(enable = "avx512f,avx512bw")]
fn load16(batch: &[u8; 256]) -> [__m512i; 4] {
    let (quads, _) = batch.as_chunks::<64>();
    let mut x = [_mm512_setzero_si512(); 4];
    for j in 0..4 {
        x[j] = reverse_lanes(load4(&quads[j]));
    }
    x
}

#[inline]
#[target_feature(enable = "avx512f")]
fn round_keys(keys: &[u8; 240]) -> [__m512i; 15] {
    let (slots, _) = keys.as_chunks::<16>();
    let mut k = [_mm512_setzero_si512(); 15];
    for i in 0..15 {
        k[i] = _mm512_broadcast_i32x4(load(&slots[i]));
    }
    k
}

/// Counters `first` to `first + 3`, big-endian in each lane's last word,
/// round key 0 folded into `base`.
#[inline]
#[target_feature(enable = "avx512f")]
fn counters(base: __m512i, first: u32) -> __m512i {
    let mut words = [0u32; 16];
    for l in 0..4 {
        words[4 * l + 3] = first.wrapping_add(l as u32).to_be();
    }
    // Sixteen plain words are 64 plain bytes.
    _mm512_xor_si512(base, unsafe { core::mem::transmute::<[u32; 16], __m512i>(words) })
}

/// `4n` counter blocks from `first` through every round.
#[inline]
#[target_feature(enable = "avx512f,vaes")]
fn encrypt<const R: usize, const N: usize>(k: &[__m512i; 15], base: __m512i, first: u32) -> [__m512i; N] {
    let mut s = [_mm512_setzero_si512(); N];
    for j in 0..N {
        s[j] = counters(base, first.wrapping_add(4 * j as u32));
    }
    for r in 1..R {
        for j in 0..N {
            s[j] = _mm512_aesenc_epi128(s[j], k[r]);
        }
    }
    for j in 0..N {
        s[j] = _mm512_aesenclast_epi128(s[j], k[R]);
    }
    s
}

#[inline]
#[target_feature(enable = "avx512f")]
fn xor_into<const N: usize>(bytes: &mut [[u8; 64]], s: &[__m512i; N]) {
    for j in 0..N {
        let stream = _mm512_xor_si512(s[j], load4(&bytes[j]));
        store4(&mut bytes[j], stream);
    }
}

#[inline]
#[target_feature(enable = "avx512f")]
fn base(keys: &[u8; 240], nonce: &[u8; 12]) -> __m512i {
    _mm512_broadcast_i32x4(counter_base(nonce, load(&keys.as_chunks::<16>().0[0])))
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
fn hash_body(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    let q = Quads::new(powers);
    let mut y = reverse(load(state));
    let (batches, rest) = blocks.as_chunks::<16>();
    for i in 0..batches.len() {
        y = q.absorb(y, load16(batches[i].as_flattened().try_into().unwrap()));
    }
    store(state, reverse(y));
    if !rest.is_empty() {
        // CLMUL and SSSE3 are admitted with VPCLMULQDQ.
        unsafe { super::aesni::hash(powers, state, rest) }
    }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
fn ctr_body<const R: usize>(keys: &[u8; 240], nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    let k = round_keys(keys);
    let base = base(keys, nonce);
    let (batches, rest) = data.as_chunks_mut::<512>();
    let mut next = first;
    for i in 0..batches.len() {
        let (quads, _) = batches[i].as_chunks_mut::<64>();
        xor_into(quads, &encrypt::<R, 8>(&k, base, next));
        next = next.wrapping_add(32);
    }
    // AES is admitted with VAES; the caller's bound covers the rest.
    unsafe { super::aesni::ctr(keys, R, nonce, next, rest) }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
fn seal_body<const R: usize>(
    keys: &[u8; 240],
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    let (batches, _) = data.as_chunks_mut::<256>();
    let n = batches.len();
    if n == 0 {
        return 0;
    }
    let k = round_keys(keys);
    let q = Quads::new(powers);
    let base = base(keys, nonce);
    let mut y = reverse(load(state));
    let (quads, _) = batches[0].as_chunks_mut::<64>();
    xor_into(quads, &encrypt::<R, 4>(&k, base, 2));
    for i in 1..n {
        let (done, next) = batches.split_at_mut(i);
        let x = load16(&done[i - 1]);
        let first = (i as u32).wrapping_mul(16).wrapping_add(2);
        let mut s = [_mm512_setzero_si512(); 4];
        for j in 0..4 {
            s[j] = counters(base, first.wrapping_add(4 * j as u32));
        }
        // The previous batch's four products ride along rounds 1 to 4.
        let mut sum = Product4::new(_mm512_xor_si512(x[0], _mm512_zextsi128_si512(y)), q.h[0], q.folds[0]);
        for r in 0..R - 1 {
            for j in 0..4 {
                s[j] = _mm512_aesenc_epi128(s[j], k[r + 1]);
            }
            if (1..4).contains(&r) {
                sum = sum.add(x[r], q.h[r], q.folds[r]);
            }
        }
        y = sum.reduce();
        for j in 0..4 {
            s[j] = _mm512_aesenclast_epi128(s[j], k[R]);
        }
        let (quads, _) = next[0].as_chunks_mut::<64>();
        xor_into(quads, &s);
    }
    y = q.absorb(y, load16(&batches[n - 1]));
    store(state, reverse(y));
    256 * n
}

/// # Safety
/// The features are admitted and `powers` came from `aesni::powers`.
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn hash(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    hash_body(powers, state, blocks)
}

/// # Safety
/// The features are admitted, `rounds` is 10 or 14, and no counter from
/// `first` on over `data` wraps.
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    if rounds == 10 {
        ctr_body::<10>(keys, nonce, first, data)
    } else {
        ctr_body::<14>(keys, nonce, first, data)
    }
}

/// Encrypts whole 256-byte batches from counter 2 on and hashes their
/// ciphertext, returning the bytes done.
///
/// # Safety
/// The features are admitted, `rounds` is 10 or 14, `powers` came from
/// `aesni::powers`, and GCM bounded `data`'s length.
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn seal(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    if rounds == 10 {
        seal_body::<10>(keys, powers, nonce, state, data)
    } else {
        seal_body::<14>(keys, powers, nonce, state, data)
    }
}
