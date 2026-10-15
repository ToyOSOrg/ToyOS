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
//! reduction, ibid.), and sealing interleaves the hash of one batch with the
//! encryption of the next (Gopal et al., "Fast Cryptographic Computation on
//! Intel Architecture Processors Via Function Stitching", Intel, 2010).
//!
//! Every branch and address depends only on lengths and round counts.

use core::arch::x86_64::*;

use super::{load, store};

/// x^63 + x^62 + x^57: g* - x^128 - 1, divided by x^64.
const REDUCE: i64 = 0xc200_0000_0000_0000_u64 as i64;
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
fn round_keys(keys: &[u8; 240]) -> [__m128i; 15] {
    let (slots, _) = keys.as_chunks::<16>();
    core::array::from_fn(|i| load(&slots[i]))
}

#[inline]
#[target_feature(enable = "aes,sse2")]
fn rounds<const R: usize>(k: &[__m128i; 15], block: __m128i) -> __m128i {
    let mut v = block;
    for key in &k[1..R] {
        v = _mm_aesenc_si128(v, *key);
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
pub(super) fn counter_base(nonce: &[u8; 12], key0: __m128i) -> __m128i {
    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(nonce);
    _mm_xor_si128(load(&iv), key0)
}

#[inline]
#[target_feature(enable = "sse2")]
fn counter(base: __m128i, counter: u32) -> __m128i {
    _mm_xor_si128(base, _mm_set_epi32(counter.to_be() as i32, 0, 0, 0))
}

/// Eight counter blocks through every round.
#[inline]
#[target_feature(enable = "aes,sse2")]
fn encrypt8<const R: usize>(k: &[__m128i; 15], base: __m128i, first: u32) -> [__m128i; 8] {
    let mut s: [__m128i; 8] = core::array::from_fn(|j| counter(base, first.wrapping_add(j as u32)));
    for key in &k[1..R] {
        for block in &mut s {
            *block = _mm_aesenc_si128(*block, *key);
        }
    }
    s.map(|block| _mm_aesenclast_si128(block, k[R]))
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
    for batch in batches {
        let (blocks, _) = batch.as_chunks_mut::<16>();
        for (block, stream) in blocks.iter_mut().zip(encrypt8::<R>(&k, base, next)) {
            xor_into(block, stream);
        }
        next = next.wrapping_add(8);
    }
    let (blocks, tail) = rest.as_chunks_mut::<16>();
    for block in blocks {
        xor_into(block, rounds::<R>(&k, counter(base, next)));
        next = next.wrapping_add(1);
    }
    if !tail.is_empty() {
        let mut last = crate::secret::Secret([0u8; 16]);
        last.0[..tail.len()].copy_from_slice(tail);
        xor_into(&mut last.0, rounds::<R>(&k, counter(base, next)));
        tail.copy_from_slice(&last.0[..tail.len()]);
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
pub(super) struct Product {
    pub(super) low: __m128i,
    pub(super) high: __m128i,
    pub(super) middle: __m128i,
}

impl Product {
    #[inline]
    #[target_feature(enable = "pclmulqdq,sse2")]
    pub(super) fn new(x: __m128i, h: __m128i) -> Self {
        Self {
            low: _mm_clmulepi64_si128::<0x00>(x, h),
            high: _mm_clmulepi64_si128::<0x11>(x, h),
            middle: _mm_clmulepi64_si128::<0x00>(folded(x), folded(h)),
        }
    }

    #[inline]
    #[target_feature(enable = "pclmulqdq,sse2")]
    pub(super) fn add(self, x: __m128i, h: __m128i) -> Self {
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
    pub(super) fn reduce(self) -> __m128i {
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
pub(super) fn folded(h: __m128i) -> __m128i {
    _mm_xor_si128(h, _mm_shuffle_epi32::<0x4e>(h))
}

#[inline(always)]
pub(super) fn power(p: u128) -> __m128i {
    // Both are 16 plain bytes; the low half of `p` is the low lane.
    unsafe { core::mem::transmute(p) }
}

/// `powers[i]` = (H^(i+1))' for the hash key `h` in GCM's byte order.
///
/// # Safety
/// CLMUL and SSSE3 are admitted.
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn powers(h: &[u8; 16], powers: &mut [u128; 8]) {
    let h = u128::from_be_bytes(*h);
    // H*·x: a shift, reduced by x^128 where the top bit carries out.
    let p1 = power((h << 1) ^ (0u128.wrapping_sub(h >> 127) & X128));
    let p2 = multiply(p1, p1);
    let p3 = multiply(p2, p1);
    let p4 = multiply(p2, p2);
    let p5 = multiply(p4, p1);
    let p6 = multiply(p3, p3);
    let p7 = multiply(p6, p1);
    let p8 = multiply(p4, p4);
    for (out, p) in powers.iter_mut().zip([p1, p2, p3, p4, p5, p6, p7, p8]) {
        // Both are 16 plain bytes.
        *out = unsafe { core::mem::transmute::<__m128i, u128>(p) };
    }
}

/// `y` with up to eight more blocks folded in, by one reduction: block j of
/// n meets H^(n-j).
#[inline]
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
fn absorb(y: __m128i, powers: &[u128; 8], blocks: &[[u8; 16]]) -> __m128i {
    let Some((first, rest)) = blocks.split_first() else { return y };
    let n = blocks.len();
    let mut sum = Product::new(_mm_xor_si128(reverse(load(first)), y), power(powers[n - 1]));
    for (block, p) in rest.iter().zip(powers[..n - 1].iter().rev()) {
        sum = sum.add(reverse(load(block)), power(*p));
    }
    sum.reduce()
}

/// # Safety
/// CLMUL and SSSE3 are admitted, and `powers` came from [`powers`].
#[target_feature(enable = "pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn hash(powers: &[u128; 8], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    let mut y = reverse(load(state));
    for batch in blocks.chunks(8) {
        y = absorb(y, powers, batch);
    }
    store(state, reverse(y));
}

#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
fn seal_rounds<const R: usize>(
    keys: &[u8; 240],
    powers: &[u128; 8],
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    let (batches, _) = data.as_chunks_mut::<128>();
    let n = batches.len();
    if n == 0 {
        return 0;
    }
    let k = round_keys(keys);
    let h: [__m128i; 8] = powers.map(power);
    let folds = h.map(|h| folded(h));
    let base = counter_base(nonce, k[0]);
    let mut y = reverse(load(state));
    let (blocks, _) = batches[0].as_chunks_mut::<16>();
    for (block, stream) in blocks.iter_mut().zip(encrypt8::<R>(&k, base, 2)) {
        xor_into(block, stream);
    }
    for i in 1..n {
        let (done, next) = batches.split_at_mut(i);
        let (previous, _) = done[i - 1].as_chunks::<16>();
        let x: [__m128i; 8] = core::array::from_fn(|j| reverse(load(&previous[j])));
        let first = (i as u32).wrapping_mul(8).wrapping_add(2);
        let mut s: [__m128i; 8] = core::array::from_fn(|j| counter(base, first.wrapping_add(j as u32)));
        let x0 = _mm_xor_si128(x[0], y);
        let mut sum = Product {
            low: _mm_clmulepi64_si128::<0x00>(x0, h[7]),
            high: _mm_clmulepi64_si128::<0x11>(x0, h[7]),
            middle: _mm_clmulepi64_si128::<0x00>(folded(x0), folds[7]),
        };
        // Round r also multiplies block r of the previous batch.
        for (r, key) in k[1..R].iter().enumerate() {
            for block in &mut s {
                *block = _mm_aesenc_si128(*block, *key);
            }
            if (1..8).contains(&r) {
                sum = Product {
                    low: _mm_xor_si128(sum.low, _mm_clmulepi64_si128::<0x00>(x[r], h[7 - r])),
                    high: _mm_xor_si128(sum.high, _mm_clmulepi64_si128::<0x11>(x[r], h[7 - r])),
                    middle: _mm_xor_si128(sum.middle, _mm_clmulepi64_si128::<0x00>(folded(x[r]), folds[7 - r])),
                };
            }
        }
        y = sum.reduce();
        let (blocks, _) = next[0].as_chunks_mut::<16>();
        for (block, s) in blocks.iter_mut().zip(s) {
            xor_into(block, _mm_aesenclast_si128(s, k[R]));
        }
    }
    y = absorb(y, powers, batches[n - 1].as_chunks::<16>().0);
    store(state, reverse(y));
    128 * n
}

/// Encrypts whole 128-byte batches from counter 2 on and hashes their
/// ciphertext, returning the bytes done.
///
/// # Safety
/// AES, CLMUL and SSSE3 are admitted, `rounds` is 10 or 14, `powers` came
/// from [`powers`], and GCM bounded `data`'s length.
#[target_feature(enable = "aes,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn seal(
    keys: &[u8; 240],
    rounds: usize,
    powers: &[u128; 8],
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    if rounds == 10 {
        seal_rounds::<10>(keys, powers, nonce, state, data)
    } else {
        seal_rounds::<14>(keys, powers, nonce, state, data)
    }
}
