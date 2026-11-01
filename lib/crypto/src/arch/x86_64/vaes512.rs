//! GCM on 512-bit VAES and VPCLMULQDQ under AVX-512 (Intel SDM Vol. 2B),
//! four blocks to a register in `aesni`'s GHASH form. Data runs in steps of
//! sixteen blocks, each step's CTR beside GHASH over sixteen blocks of
//! ciphertext, and three steps share one reduction with H^48..H^1. A product
//! is four carry-less multiplies, low, high and the two cross terms, merged
//! by three-input XORs. Only hosted targets with OS-enabled ZMM state enter
//! these functions. Hot loops index fixed-size arrays, as `aesni`'s header
//! explains.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

use core::arch::x86_64::*;

use super::aesni::{self, REDUCE, reverse};
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

/// Each 128-bit lane's bytes reversed, for GHASH.
#[inline]
#[target_feature(enable = "avx512f,avx512bw")]
fn reflect(x: [__m512i; 4]) -> [__m512i; 4] {
    let lane: [u8; 16] = [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    let pattern = _mm512_broadcast_i32x4(load(&lane));
    let mut out = x;
    for j in 0..4 {
        out[j] = _mm512_shuffle_epi8(x[j], pattern);
    }
    out
}

#[inline]
#[target_feature(enable = "avx512f")]
fn xor3(a: __m512i, b: __m512i, c: __m512i) -> __m512i {
    _mm512_ternarylogic_epi64::<0x96>(a, b, c)
}

/// The four lanes XORed into one.
#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn fold(x: __m512i) -> __m128i {
    let half = _mm256_xor_si256(_mm512_castsi512_si256(x), _mm512_extracti64x4_epi64::<1>(x));
    _mm_xor_si128(_mm256_castsi256_si128(half), _mm256_extracti128_si256::<1>(half))
}

/// `powers[at..at + 4]`, one to a lane, for `at` a multiple of four.
#[inline]
#[target_feature(enable = "avx512f")]
fn powers4(powers: &Powers, at: usize) -> __m512i {
    // Four plain words are 64 plain bytes; the first is the lowest lane.
    unsafe { core::mem::transmute::<[u128; 4], __m512i>(powers.as_chunks::<4>().0[at / 4]) }
}

/// Carry-less products summed lane by lane, unreduced: `low` and `high` the
/// halves' products, `middle` the cross terms.
#[derive(Clone, Copy)]
struct Sums {
    low: __m512i,
    high: __m512i,
    middle: __m512i,
}

impl Sums {
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn zero() -> Self {
        Self { low: _mm512_setzero_si512(), high: _mm512_setzero_si512(), middle: _mm512_setzero_si512() }
    }

    #[inline]
    #[target_feature(enable = "avx512f,vpclmulqdq")]
    fn product(x: __m512i, h: __m512i) -> Self {
        Self {
            low: _mm512_clmulepi64_epi128::<0x00>(x, h),
            high: _mm512_clmulepi64_epi128::<0x11>(x, h),
            middle: _mm512_xor_si512(_mm512_clmulepi64_epi128::<0x01>(x, h), _mm512_clmulepi64_epi128::<0x10>(x, h)),
        }
    }

    /// Adds x·h and y·g.
    #[inline]
    #[target_feature(enable = "avx512f,vpclmulqdq")]
    fn add(&mut self, x: __m512i, h: __m512i, y: __m512i, g: __m512i) {
        self.low = xor3(self.low, _mm512_clmulepi64_epi128::<0x00>(x, h), _mm512_clmulepi64_epi128::<0x00>(y, g));
        self.high = xor3(self.high, _mm512_clmulepi64_epi128::<0x11>(x, h), _mm512_clmulepi64_epi128::<0x11>(y, g));
        self.middle =
            xor3(self.middle, _mm512_clmulepi64_epi128::<0x01>(x, h), _mm512_clmulepi64_epi128::<0x10>(x, h));
        self.middle =
            xor3(self.middle, _mm512_clmulepi64_epi128::<0x01>(y, g), _mm512_clmulepi64_epi128::<0x10>(y, g));
    }

    /// Each lane's sum times x^-128 mod g*: `aesni`'s Montgomery reduction,
    /// four lanes at once.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,vpclmulqdq")]
    fn reduce_lanes(self) -> __m512i {
        let low = _mm512_xor_si512(self.low, _mm512_bslli_epi128::<8>(self.middle));
        let high = _mm512_xor_si512(self.high, _mm512_bsrli_epi128::<8>(self.middle));
        let c = _mm512_set1_epi64(REDUCE);
        let x = _mm512_xor_si512(_mm512_shuffle_epi32::<0x4e>(low), _mm512_clmulepi64_epi128::<0x00>(low, c));
        xor3(high, _mm512_shuffle_epi32::<0x4e>(x), _mm512_clmulepi64_epi128::<0x00>(x, c))
    }
}

/// GHASH over steps of sixteen byte-reflected blocks. Three steps share a
/// reduction, and the first reduction takes the steps a count leaves over,
/// so that a group of m steps meets H^16m..H^1.
struct Ghash<'a> {
    powers: &'a Powers,
    sums: Sums,
    /// The last reduction, which the next step's first block carries.
    carry: __m512i,
    /// Steps left in the group.
    left: usize,
}

impl<'a> Ghash<'a> {
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn new(powers: &'a Powers, y: __m128i, steps: usize) -> Self {
        let left = match steps % 3 {
            0 => 3,
            m => m,
        };
        Self { powers, sums: Sums::zero(), carry: _mm512_zextsi128_si512(y), left }
    }

    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vl,vpclmulqdq")]
    fn absorb(&mut self, x: [__m512i; 4]) {
        // The step meets H^16left..H^(16left - 15).
        let at = 48 - 16 * self.left;
        let first = _mm512_xor_si512(x[0], self.carry);
        self.sums.add(first, powers4(self.powers, at), x[1], powers4(self.powers, at + 4));
        self.sums.add(x[2], powers4(self.powers, at + 8), x[3], powers4(self.powers, at + 12));
        self.carry = _mm512_setzero_si512();
        self.left -= 1;
        if self.left == 0 {
            self.carry = _mm512_zextsi128_si512(fold(self.sums.reduce_lanes()));
            self.sums = Sums::zero();
            self.left = 3;
        }
    }

    /// The hash, once the step count [`Ghash::new`] took has been absorbed.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn finish(self) -> __m128i {
        debug_assert_eq!(self.left, 3, "a group is unreduced");
        _mm512_castsi512_si128(self.carry)
    }
}

/// GCM's counter blocks, four to a register: the nonce, then the counter
/// big-endian. Sixteen increments are vector adds to the low byte where it
/// does not carry; elsewhere the blocks are built from the word. The counter
/// is public, and so is the branch.
struct Counter {
    nonce: __m512i,
    /// Lanes `next` to `next + 3`.
    quad: __m512i,
    next: u32,
}

impl Counter {
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw")]
    fn new(nonce: &[u8; 12], first: u32) -> Self {
        let mut block = [0u8; 16];
        block[..12].copy_from_slice(nonce);
        let nonce = _mm512_broadcast_i32x4(load(&block));
        Self { nonce, quad: Self::quad(nonce, first), next: first }
    }

    /// Lanes `n` to `n + 3`, from the word.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw")]
    fn quad(nonce: __m512i, n: u32) -> __m512i {
        let words = _mm512_add_epi32(
            _mm512_set1_epi32(n as i32),
            _mm512_set_epi32(3, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0),
        );
        // Each lane's word 3, byte-swapped into the block's last four bytes.
        let swap: [u8; 16] = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 15, 14, 13, 12];
        _mm512_or_si512(nonce, _mm512_shuffle_epi8(words, _mm512_broadcast_i32x4(load(&swap))))
    }

    /// `k` added to each lane's big-endian low byte.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn plus(quad: __m512i, k: i32) -> __m512i {
        _mm512_add_epi32(quad, _mm512_set_epi32(k << 24, 0, 0, 0, k << 24, 0, 0, 0, k << 24, 0, 0, 0, k << 24, 0, 0, 0))
    }

    /// The next sixteen blocks.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw")]
    fn next16(&mut self) -> [__m512i; 4] {
        let (n, q) = (self.next, self.quad);
        self.next = n.wrapping_add(16);
        // Up to n + 19, the next quad's last lane, the low byte does not carry.
        if n & 0xff <= 0xff - 19 {
            self.quad = Self::plus(q, 16);
            [q, Self::plus(q, 4), Self::plus(q, 8), Self::plus(q, 12)]
        } else {
            self.quad = Self::quad(self.nonce, self.next);
            let nonce = self.nonce;
            [q, Self::quad(nonce, n.wrapping_add(4)), Self::quad(nonce, n.wrapping_add(8)), Self::quad(nonce, n.wrapping_add(12))]
        }
    }
}

/// Round key `r` in every lane, read where it is used: the AES-256 schedule
/// does not fit beside a step's registers.
#[inline]
#[target_feature(enable = "avx512f")]
fn key(keys: &[[u8; 16]; 15], r: usize) -> __m512i {
    _mm512_broadcast_i32x4(load(&keys[r]))
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
fn crypt_body<const R: usize, const OPEN: bool>(
    keys: &[u8; 240],
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    let (steps, _) = data.as_chunks_mut::<256>();
    let n = steps.len();
    let (slots, _) = keys.as_chunks::<16>();
    let k: &[[u8; 16]; 15] = slots.try_into().unwrap();
    let mut counter = Counter::new(nonce, 2);
    let mut ghash = Ghash::new(powers, reverse(load(state)), n);
    // Sealing hashes each step's ciphertext beside the next step's rounds.
    let mut sealed = [_mm512_setzero_si512(); 4];
    for t in 0..n {
        // Opaque each step, so that the round keys are loads where they are
        // used rather than registers, which the step's blocks need.
        let k = core::hint::black_box(k);
        let (quads, _) = steps[t].as_chunks_mut::<64>();
        let mut input = [_mm512_setzero_si512(); 4];
        for j in 0..4 {
            input[j] = load4(&quads[j]);
        }
        let mut s = counter.next16();
        let first = key(k, 0);
        for j in 0..4 {
            s[j] = _mm512_xor_si512(s[j], first);
        }
        // The rounds and the products are independent until the store.
        if OPEN {
            ghash.absorb(reflect(input));
        } else if t > 0 {
            ghash.absorb(sealed);
        }
        for r in 1..R {
            let key = key(k, r);
            for j in 0..4 {
                s[j] = _mm512_aesenc_epi128(s[j], key);
            }
        }
        let last = key(k, R);
        for j in 0..4 {
            s[j] = _mm512_xor_si512(_mm512_aesenclast_epi128(s[j], last), input[j]);
            store4(&mut quads[j], s[j]);
        }
        if !OPEN {
            sealed = reflect(s);
        }
    }
    if !OPEN && n > 0 {
        ghash.absorb(sealed);
    }
    store(state, reverse(ghash.finish()));
    256 * n
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
fn hash_body(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    let (steps, rest) = blocks.as_chunks::<16>();
    let mut ghash = Ghash::new(powers, reverse(load(state)), steps.len());
    for t in 0..steps.len() {
        let (quads, _) = steps[t].as_flattened().as_chunks::<64>();
        let mut x = [_mm512_setzero_si512(); 4];
        for j in 0..4 {
            x[j] = load4(&quads[j]);
        }
        ghash.absorb(reflect(x));
    }
    store(state, reverse(ghash.finish()));
    if !rest.is_empty() {
        // CLMUL and SSSE3 are admitted with VPCLMULQDQ.
        unsafe { aesni::hash(powers, state, rest) }
    }
}

/// H^48..H^1 for the hash key `h` in GCM's byte order.
///
/// # Safety
/// The features are admitted.
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn powers(h: &[u8; 16], powers: &mut Powers) {
    let p = aesni::powers16(h);
    // H^16..H^1 four to a register, highest first, as the table holds them.
    let mut q = [_mm512_setzero_si512(); 12];
    for g in 0..4 {
        let top = 15 - 4 * g;
        let low = _mm512_inserti32x4::<1>(_mm512_castsi128_si512(p[top]), p[top - 1]);
        q[8 + g] = _mm512_inserti32x4::<3>(_mm512_inserti32x4::<2>(low, p[top - 2]), p[top - 3]);
    }
    // H^32..H^17 from H^16..H^1, then H^48..H^33 from those.
    let h16 = _mm512_broadcast_i32x4(p[15]);
    for i in 0..8 {
        let g = 7 - i;
        q[g] = Sums::product(q[g + 4], h16).reduce_lanes();
    }
    for g in 0..12 {
        // Both are 64 plain bytes.
        powers.as_chunks_mut::<4>().0[g] = unsafe { core::mem::transmute::<__m512i, [u128; 4]>(q[g]) };
    }
}

/// # Safety
/// The features are admitted and `powers` came from [`powers`].
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn hash(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    hash_body(powers, state, blocks)
}

/// CTR from counter 2 over whole 256-byte steps, each hashed in the same
/// pass: the ciphertext it writes when sealing, the ciphertext it reads when
/// `open`. Returns the bytes done.
///
/// # Safety
/// The features are admitted, `rounds` is 10 or 14, `powers` came from
/// [`powers`], and GCM bounded `data`'s length.
#[target_feature(enable = "avx512f,avx512bw,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
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
        (10, false) => crypt_body::<10, false>(keys, powers, nonce, state, data),
        (10, true) => crypt_body::<10, true>(keys, powers, nonce, state, data),
        (_, false) => crypt_body::<14, false>(keys, powers, nonce, state, data),
        (_, true) => crypt_body::<14, true>(keys, powers, nonce, state, data),
    }
}
