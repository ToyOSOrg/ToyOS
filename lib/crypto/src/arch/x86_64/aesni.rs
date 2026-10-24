//! AES-NI and PCLMULQDQ on 128-bit registers.
//!
//! Key expansion follows FIPS 197 §5.2 with AESKEYGENASSIST (Intel SDM Vol.
//! 2A; Gueron, "Intel Advanced Encryption Standard (AES) New Instructions
//! Set", rev. 3.01, 2012).
//!
//! GHASH works on blocks loaded byte-reversed, so a register's bit i is the
//! coefficient of x^(127-i): each field element A appears as its reciprocal
//! A* = x^127 A(1/x). Then A*·B* = x^127 (AB mod g)* modulo g* = x^128 +
//! x^127 + x^126 + x^121 + 1, so with the key held as H' = H*·x mod g*, the
//! product (AH)* is A*·H'·x^-128 mod g*: one carry-less product and a
//! Montgomery reduction by x^128, two more products by x^63 + x^62 + x^57
//! (Gueron and Kounavis, "Efficient implementation of the Galois Counter Mode
//! using a carry-less multiplier and a fast reduction algorithm", IPL 110,
//! 2010). Eight blocks share one reduction with H^8..H^1 (aggregated
//! reduction, ibid.), and CTR interleaves GHASH over its ciphertext (Gopal
//! et al., "Fast Cryptographic Computation on Intel Architecture Processors
//! Via Function Stitching", Intel, 2010).
//!
//! Every branch and address depends only on lengths and round counts.
//!
//! Hot loops index fixed-size arrays rather than iterate: in the shipped
//! profile, `map`, `zip` and slice iterators carry debug preconditions that
//! keep them out of line, and a call from vector code spills every live
//! register around it.

#![allow(clippy::needless_range_loop, reason = "see the module header")]

use core::arch::x86_64::*;

use super::{load, store};
use crate::arch::Powers;
use crate::block::padded;

/// x^63 + x^62 + x^57: g* - x^128 - 1, divided by x^64.
pub(super) const REDUCE: i64 = 0xc200_0000_0000_0000_u64 as i64;
/// x^128 mod g*.
const X128: u128 = 0xc200_0000_0000_0000_0000_0000_0000_0001;

#[inline]
#[target_feature(enable = "sse2")]
fn next_key(key: __m128i, assist: __m128i) -> __m128i {
    // w[i] = w[i-4] ^ w[i-1] chains four words into a prefix XOR.
    let key = _mm_xor_si128(key, _mm_slli_si128::<4>(key));
    let key = _mm_xor_si128(key, _mm_slli_si128::<8>(key));
    _mm_xor_si128(key, assist)
}

/// # Safety
/// AES is admitted and `key` holds 16 or 32 bytes.
#[target_feature(enable = "aes,sse2")]
pub(super) unsafe fn expand(key: &[u8], keys: &mut [u8; 240]) {
    let (out, _) = keys.as_chunks_mut::<16>();
    let (halves, _) = key.as_chunks::<16>();
    let mut a = load(&halves[0]);
    store(&mut out[0], a);
    if key.len() == 16 {
        macro_rules! round {
            ($i:literal, $rcon:literal) => {
                a = next_key(a, _mm_shuffle_epi32::<0xff>(_mm_aeskeygenassist_si128::<$rcon>(a)));
                store(&mut out[$i], a);
            };
        }
        round!(1, 0x01);
        round!(2, 0x02);
        round!(3, 0x04);
        round!(4, 0x08);
        round!(5, 0x10);
        round!(6, 0x20);
        round!(7, 0x40);
        round!(8, 0x80);
        round!(9, 0x1b);
        round!(10, 0x36);
    } else {
        let mut b = load(&halves[1]);
        store(&mut out[1], b);
        // FIPS 197: word 8i takes SubWord(RotWord) and Rcon, word 8i+4
        // SubWord alone; AESKEYGENASSIST's dwords 3 and 2 are those.
        macro_rules! round {
            ($i:literal, $rcon:literal) => {
                a = next_key(a, _mm_shuffle_epi32::<0xff>(_mm_aeskeygenassist_si128::<$rcon>(b)));
                store(&mut out[$i], a);
                if $i < 14 {
                    b = next_key(b, _mm_shuffle_epi32::<0xaa>(_mm_aeskeygenassist_si128::<0>(a)));
                    store(&mut out[$i + 1], b);
                }
            };
        }
        round!(2, 0x01);
        round!(4, 0x02);
        round!(6, 0x04);
        round!(8, 0x08);
        round!(10, 0x10);
        round!(12, 0x20);
        round!(14, 0x40);
    }
}

#[inline]
#[target_feature(enable = "sse2")]
fn round_keys(keys: &[u8; 240]) -> [__m128i; 15] {
    let (slots, _) = keys.as_chunks::<16>();
    let mut k = [_mm_setzero_si128(); 15];
    for i in 0..15 {
        k[i] = load(&slots[i]);
    }
    k
}

#[inline]
#[target_feature(enable = "aes,sse2")]
fn rounds<const R: usize>(k: &[__m128i; 15], block: __m128i) -> __m128i {
    let mut v = block;
    for r in 1..R {
        v = _mm_aesenc_si128(v, k[r]);
    }
    _mm_aesenclast_si128(v, k[R])
}

/// # Safety
/// AES is admitted and `rounds` is 10 or 14.
#[target_feature(enable = "aes,sse2")]
pub(super) unsafe fn encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) {
    let k = round_keys(keys);
    let v = _mm_xor_si128(load(block), k[0]);
    let v = if rounds == 10 { self::rounds::<10>(&k, v) } else { self::rounds::<14>(&k, v) };
    store(block, v);
}

/// The nonce's counter block with round key 0 folded in.
#[inline]
#[target_feature(enable = "sse2")]
fn counter_base(nonce: &[u8; 12], key0: __m128i) -> __m128i {
    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(nonce);
    _mm_xor_si128(load(&iv), key0)
}

#[inline]
#[target_feature(enable = "sse2")]
fn counter(base: __m128i, counter: u32) -> __m128i {
    _mm_xor_si128(base, _mm_set_epi32(counter.to_be() as i32, 0, 0, 0))
}

/// `N` counter blocks from `first` through every round.
#[inline]
#[target_feature(enable = "aes,sse2")]
fn encrypt_n<const R: usize, const N: usize>(k: &[__m128i; 15], base: __m128i, first: u32) -> [__m128i; N] {
    let mut s = [_mm_setzero_si128(); N];
    for j in 0..N {
        s[j] = counter(base, first.wrapping_add(j as u32));
    }
    for r in 1..R {
        for j in 0..N {
            s[j] = _mm_aesenc_si128(s[j], k[r]);
        }
    }
    for j in 0..N {
        s[j] = _mm_aesenclast_si128(s[j], k[R]);
    }
    s
}

/// `N` counter blocks from `first` through every round, into `s[..N]`,
/// each round key read from the schedule where it is used: a local copy of
/// the AES-256 schedule does not fit the registers beside fifteen blocks,
/// and its spill is a 240-byte library copy.
#[inline]
#[target_feature(enable = "aes,sse2")]
fn encrypt_into<const R: usize, const N: usize>(keys: &[[u8; 16]; 15], base: __m128i, first: u32, s: &mut [__m128i; 15]) {
    for j in 0..N {
        s[j] = counter(base, first.wrapping_add(j as u32));
    }
    for r in 1..R {
        let key = load(&keys[r]);
        for j in 0..N {
            s[j] = _mm_aesenc_si128(s[j], key);
        }
    }
    let last = load(&keys[R]);
    for j in 0..N {
        s[j] = _mm_aesenclast_si128(s[j], last);
    }
}

#[inline]
#[target_feature(enable = "aes,sse2")]
fn encrypt8<const R: usize>(k: &[__m128i; 15], base: __m128i, first: u32) -> [__m128i; 8] {
    encrypt_n::<R, 8>(k, base, first)
}

#[inline]
#[target_feature(enable = "sse2")]
fn xor_into(block: &mut [u8; 16], stream: __m128i) {
    store(block, _mm_xor_si128(stream, load(block)));
}

#[target_feature(enable = "aes,sse2")]
fn ctr_rounds<const R: usize>(keys: &[u8; 240], nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    let k = round_keys(keys);
    let base = counter_base(nonce, k[0]);
    let mut next = first;
    let (batches, rest) = data.as_chunks_mut::<128>();
    for i in 0..batches.len() {
        let (blocks, _) = batches[i].as_chunks_mut::<16>();
        let s = encrypt8::<R>(&k, base, next);
        for j in 0..8 {
            xor_into(&mut blocks[j], s[j]);
        }
        next = next.wrapping_add(8);
    }
    let (blocks, tail) = rest.as_chunks_mut::<16>();
    for i in 0..blocks.len() {
        xor_into(&mut blocks[i], rounds::<R>(&k, counter(base, next)));
        next = next.wrapping_add(1);
    }
    if !tail.is_empty() {
        xor_bytes(tail, rounds::<R>(&k, counter(base, next)));
    }
}

/// # Safety
/// AES is admitted, `rounds` is 10 or 14, and no counter from `first` on
/// over `data` wraps.
#[target_feature(enable = "aes,sse2")]
pub(super) unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    if rounds == 10 {
        ctr_rounds::<10>(keys, nonce, first, data)
    } else {
        ctr_rounds::<14>(keys, nonce, first, data)
    }
}

#[inline]
#[target_feature(enable = "ssse3")]
pub(super) fn reverse(x: __m128i) -> __m128i {
    _mm_shuffle_epi8(x, _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))
}

/// A carry-less product's three Karatsuba terms, summed over several pairs
/// before one reduction.
#[derive(Clone, Copy)]
struct Product {
    low: __m128i,
    high: __m128i,
    middle: __m128i,
}

impl Product {
    #[inline]
    #[target_feature(enable = "pclmulqdq,sse2")]
    fn new(x: __m128i, h: __m128i) -> Self {
        Self {
            low: _mm_clmulepi64_si128::<0x00>(x, h),
            high: _mm_clmulepi64_si128::<0x11>(x, h),
            middle: _mm_clmulepi64_si128::<0x00>(folded(x), folded(h)),
        }
    }

    #[inline]
    #[target_feature(enable = "pclmulqdq,sse2")]
    fn add(self, x: __m128i, h: __m128i) -> Self {
        let p = Self::new(x, h);
        Self {
            low: _mm_xor_si128(self.low, p.low),
            high: _mm_xor_si128(self.high, p.high),
            middle: _mm_xor_si128(self.middle, p.middle),
        }
    }

    /// The sum times x^-128 mod g*.
    #[inline]
    #[target_feature(enable = "pclmulqdq,sse2")]
    fn reduce(self) -> __m128i {
        let middle = _mm_xor_si128(self.middle, _mm_xor_si128(self.low, self.high));
        let low = _mm_xor_si128(self.low, _mm_slli_si128::<8>(middle));
        let high = _mm_xor_si128(self.high, _mm_srli_si128::<8>(middle));
        montgomery(low, high)
    }
}

/// `(high·x^128 + low)·x^-128 mod g*`: adding m·g* clears the low 128 bits,
/// 64 at a time, and leaves a sum below x^128.
#[inline]
#[target_feature(enable = "pclmulqdq,sse2")]
fn montgomery(low: __m128i, high: __m128i) -> __m128i {
    let c = _mm_set_epi64x(0, REDUCE);
    let x = _mm_xor_si128(_mm_shuffle_epi32::<0x4e>(low), _mm_clmulepi64_si128::<0x00>(low, c));
    let y = _mm_xor_si128(_mm_shuffle_epi32::<0x4e>(x), _mm_clmulepi64_si128::<0x00>(x, c));
    _mm_xor_si128(high, y)
}

#[inline]
#[target_feature(enable = "pclmulqdq,sse2")]
fn multiply(a: __m128i, h: __m128i) -> __m128i {
    Product::new(a, h).reduce()
}

/// A value's halves XORed, the Karatsuba middle operand.
#[inline]
#[target_feature(enable = "sse2")]
fn folded(h: __m128i) -> __m128i {
    _mm_xor_si128(h, _mm_shuffle_epi32::<0x4e>(h))
}

#[inline(always)]
fn power(p: u128) -> __m128i {
    // Both are 16 plain bytes; the low half of `p` is the low lane.
    unsafe { core::mem::transmute(p) }
}

/// (H^n)'.
#[inline(always)]
fn key_power(powers: &Powers, n: usize) -> __m128i {
    power(powers[48 - n])
}

/// `p[i]` = (H^(i+1))' for the hash key `h` in GCM's byte order.
#[inline]
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
pub(super) fn powers16(h: &[u8; 16]) -> [__m128i; 16] {
    let h = u128::from_be_bytes(*h);
    // H*·x: a shift, reduced by x^128 where the top bit carries out.
    let mut p = [_mm_setzero_si128(); 16];
    p[0] = power((h << 1) ^ (0u128.wrapping_sub(h >> 127) & X128));
    // H^(i+1) from the largest powers already known: a tree of depth five.
    for i in 1..16usize {
        let half = i.div_ceil(2);
        p[i] = multiply(p[half - 1], p[i - half]);
    }
    p
}

/// H^16..H^1 for the hash key `h` in GCM's byte order.
///
/// # Safety
/// CLMUL and SSSE3 are admitted.
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn powers(h: &[u8; 16], powers: &mut Powers) {
    let p = powers16(h);
    for i in 0..16 {
        // Both are 16 plain bytes.
        powers[47 - i] = unsafe { core::mem::transmute::<__m128i, u128>(p[i]) };
    }
}

/// `y` with up to eight more blocks folded in, by one reduction: block j of
/// n meets H^(n-j).
#[inline]
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
fn absorb(y: __m128i, powers: &Powers, blocks: &[[u8; 16]]) -> __m128i {
    let n = blocks.len();
    if n == 0 {
        return y;
    }
    let mut sum = Product::new(_mm_xor_si128(reverse(load(&blocks[0])), y), key_power(powers, n));
    for j in 1..n {
        sum = sum.add(reverse(load(&blocks[j])), key_power(powers, n - j));
    }
    sum.reduce()
}

/// # Safety
/// CLMUL and SSSE3 are admitted, and `powers` came from [`powers`].
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn hash(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    let mut y = reverse(load(state));
    let (batches, rest) = blocks.as_chunks::<8>();
    if !batches.is_empty() {
        // H^8..H^1 and their Karatsuba folds, once per call.
        let mut h = [_mm_setzero_si128(); 8];
        let mut folds = [_mm_setzero_si128(); 8];
        for j in 0..8 {
            h[j] = key_power(powers, 8 - j);
            folds[j] = folded(h[j]);
        }
        for i in 0..batches.len() {
            let batch = &batches[i];
            let x0 = _mm_xor_si128(reverse(load(&batch[0])), y);
            let mut sum = Product {
                low: _mm_clmulepi64_si128::<0x00>(x0, h[0]),
                high: _mm_clmulepi64_si128::<0x11>(x0, h[0]),
                middle: _mm_clmulepi64_si128::<0x00>(folded(x0), folds[0]),
            };
            for j in 1..8 {
                let x = reverse(load(&batch[j]));
                sum = Product {
                    low: _mm_xor_si128(sum.low, _mm_clmulepi64_si128::<0x00>(x, h[j])),
                    high: _mm_xor_si128(sum.high, _mm_clmulepi64_si128::<0x11>(x, h[j])),
                    middle: _mm_xor_si128(sum.middle, _mm_clmulepi64_si128::<0x00>(folded(x), folds[j])),
                };
            }
            y = sum.reduce();
        }
    }
    if !rest.is_empty() {
        y = absorb(y, powers, rest);
    }
    store(state, reverse(y));
}

/// Eight blocks byte-reflected for GHASH.
#[inline]
#[target_feature(enable = "sse2,ssse3")]
fn reflect8(batch: &[u8; 128]) -> [__m128i; 8] {
    let (blocks, _) = batch.as_chunks::<16>();
    let mut x = [_mm_setzero_si128(); 8];
    for j in 0..8 {
        x[j] = reverse(load(&blocks[j]));
    }
    x
}

#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
fn crypt_rounds<const R: usize, const OPEN: bool>(
    keys: &[u8; 240],
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    let (batches, _) = data.as_chunks_mut::<128>();
    let n = batches.len();
    if n == 0 {
        return 0;
    }
    let (slots, _) = keys.as_chunks::<16>();
    let k: &[[u8; 16]; 15] = slots.try_into().unwrap();
    // H^8..H^1, block j of a batch meeting H^(8-j).
    let mut h = [_mm_setzero_si128(); 8];
    let mut folds = [_mm_setzero_si128(); 8];
    for j in 0..8 {
        h[j] = key_power(powers, 8 - j);
        folds[j] = folded(h[j]);
    }
    let base = counter_base(nonce, load(&k[0]));
    let mut y = reverse(load(state));
    // Opening hashes each batch beside its own rounds; sealing hashes each
    // batch's ciphertext beside the next batch's.
    let from = if OPEN {
        0
    } else {
        let (blocks, _) = batches[0].as_chunks_mut::<16>();
        let mut s = [_mm_setzero_si128(); 15];
        encrypt_into::<R, 8>(k, base, 2, &mut s);
        for j in 0..8 {
            xor_into(&mut blocks[j], s[j]);
        }
        1
    };
    for i in from..n {
        let x = reflect8(&batches[if OPEN { i } else { i - 1 }]);
        let first = (i as u32).wrapping_mul(8).wrapping_add(2);
        let mut s = [_mm_setzero_si128(); 8];
        for j in 0..8 {
            s[j] = counter(base, first.wrapping_add(j as u32));
        }
        let x0 = _mm_xor_si128(x[0], y);
        let mut sum = Product {
            low: _mm_clmulepi64_si128::<0x00>(x0, h[0]),
            high: _mm_clmulepi64_si128::<0x11>(x0, h[0]),
            middle: _mm_clmulepi64_si128::<0x00>(folded(x0), folds[0]),
        };
        // Round r + 1 also multiplies block r.
        for r in 0..R - 1 {
            let key = load(&k[r + 1]);
            for j in 0..8 {
                s[j] = _mm_aesenc_si128(s[j], key);
            }
            if (1..8).contains(&r) {
                sum = Product {
                    low: _mm_xor_si128(sum.low, _mm_clmulepi64_si128::<0x00>(x[r], h[r])),
                    high: _mm_xor_si128(sum.high, _mm_clmulepi64_si128::<0x11>(x[r], h[r])),
                    middle: _mm_xor_si128(sum.middle, _mm_clmulepi64_si128::<0x00>(folded(x[r]), folds[r])),
                };
            }
        }
        y = sum.reduce();
        let (blocks, _) = batches[i].as_chunks_mut::<16>();
        let last = load(&k[R]);
        for j in 0..8 {
            xor_into(&mut blocks[j], _mm_aesenclast_si128(s[j], last));
        }
    }
    if !OPEN {
        y = absorb(y, powers, batches[n - 1].as_chunks::<16>().0);
    }
    store(state, reverse(y));
    128 * n
}

/// CTR from counter 2 over whole 128-byte batches, each hashed in the same
/// pass: the ciphertext it writes when sealing, the ciphertext it reads when
/// `open`. Returns the bytes done.
///
/// # Safety
/// AES, CLMUL and SSSE3 are admitted, `rounds` is 10 or 14, `powers` came
/// from [`powers`], and GCM bounded `data`'s length.
#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn crypt(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
    open: bool,
) -> usize {
    match (rounds, open) {
        (10, false) => crypt_rounds::<10, false>(keys, powers, nonce, state, data),
        (10, true) => crypt_rounds::<10, true>(keys, powers, nonce, state, data),
        (_, false) => crypt_rounds::<14, false>(keys, powers, nonce, state, data),
        (_, true) => crypt_rounds::<14, true>(keys, powers, nonce, state, data),
    }
}

#[inline]
fn xor_bytes(data: &mut [u8], stream: __m128i) {
    let mut bytes = crate::secret::Secret([0u8; 16]);
    store(&mut bytes.0, stream);
    for i in 0..data.len() {
        data[i] ^= bytes.0[i];
    }
}

#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
fn short_rounds<const R: usize>(
    keys: &[u8; 240],
    powers: &Powers,
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    stream: Option<&mut [[u8; 16]; 14]>,
) -> [u8; 16] {
    let (slots, _) = keys.as_chunks::<16>();
    let k: &[[u8; 16]; 15] = slots.try_into().unwrap();
    let base = counter_base(nonce, load(&k[0]));
    // E(J0) and the data's keystream, in as small a batch as covers them.
    let mut s = [_mm_setzero_si128(); 15];
    // Each arm encrypts in place: moving a returned array into `s` is a
    // library copy.
    match data.len() {
        0 => encrypt_into::<R, 1>(k, base, 1, &mut s),
        1..=16 => encrypt_into::<R, 2>(k, base, 1, &mut s),
        17..=48 => encrypt_into::<R, 4>(k, base, 1, &mut s),
        49..=64 => encrypt_into::<R, 5>(k, base, 1, &mut s),
        65..=112 => encrypt_into::<R, 8>(k, base, 1, &mut s),
        _ => encrypt_into::<R, 15>(k, base, 1, &mut s),
    }
    let (blocks, tail) = data.as_chunks_mut::<16>();
    let whole = blocks.len();
    match stream {
        None => {
            for i in 0..whole {
                xor_into(&mut blocks[i], s[1 + i]);
            }
            if !tail.is_empty() {
                xor_bytes(tail, s[1 + whole]);
            }
        }
        Some(stream) => {
            for i in 0..whole + usize::from(!tail.is_empty()) {
                store(&mut stream[i], s[1 + i]);
            }
        }
    }
    // The padded AAD, the padded ciphertext and the lengths: at most sixteen,
    // one per key power.
    let mut x = [_mm_setzero_si128(); 16];
    let mut n = 0;
    if !aad.is_empty() {
        x[n] = reverse(match aad.as_chunks::<16>() {
            ([block], _) => load(block),
            _ => load(&padded(aad)),
        });
        n += 1;
    }
    for i in 0..whole {
        x[n] = reverse(load(&blocks[i]));
        n += 1;
    }
    if !tail.is_empty() {
        x[n] = reverse(load(&padded(tail)));
        n += 1;
    }
    let lengths = ((aad.len() as u128 * 8) << 64) | (data.len() as u128 * 8);
    x[n] = power(lengths);
    n += 1;
    let mut sum = Product::new(x[0], key_power(powers, n));
    for j in 1..n {
        sum = sum.add(x[j], key_power(powers, n - j));
    }
    let mut tag = [0u8; 16];
    store(&mut tag, _mm_xor_si128(reverse(sum.reduce()), s[0]));
    tag
}

/// GCM over at most 16 bytes of AAD and 224 of data in one batch: counters 1
/// to 15 give E(J0) and the keystream, and one reduction folds the padded AAD,
/// the padded ciphertext and the lengths. Without `stream` it seals,
/// encrypting `data` first; with it, it opens, leaving `data` and writing the
/// keystream to `stream`. Returns the tag.
///
/// # Safety
/// AES, CLMUL and SSSE3 are admitted, `rounds` is 10 or 14, and `powers`
/// came from [`powers`].
#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn short(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    stream: Option<&mut [[u8; 16]; 14]>,
) -> [u8; 16] {
    if rounds == 10 {
        short_rounds::<10>(keys, powers, nonce, aad, data, stream)
    } else {
        short_rounds::<14>(keys, powers, nonce, aad, data, stream)
    }
}
