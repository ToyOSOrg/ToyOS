//! GCM on 256-bit VAES and VPCLMULQDQ (Intel SDM Vol. 2B): each 128-bit lane
//! is one AES block or one carry-less product, in `aesni`'s GHASH form. Only
//! hosted targets with OS-enabled YMM state enter these functions.

use core::arch::x86_64::*;

use super::aesni::{Product, load_powers, reverse};

#[inline]
#[target_feature(enable = "avx2")]
fn reverse_lanes(x: __m256i) -> __m256i {
    _mm256_shuffle_epi8(
        x,
        _mm256_setr_epi8(
            15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1,
            0,
        ),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
fn pair(low: __m128i, high: __m128i) -> __m256i {
    _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(low), high)
}

/// Counter blocks `first` and `first + 1`, round key 0 folded into `base`.
#[inline]
#[target_feature(enable = "avx2")]
fn counters(base: __m256i, first: u32) -> __m256i {
    _mm256_xor_si256(
        base,
        _mm256_set_epi32(first.wrapping_add(1).to_be() as i32, 0, 0, 0, first.to_be() as i32, 0, 0, 0),
    )
}

#[derive(Clone, Copy)]
struct Product2 {
    low: __m256i,
    high: __m256i,
    middle: __m256i,
}

impl Product2 {
    #[inline]
    #[target_feature(enable = "avx2,vpclmulqdq")]
    fn new(x: __m256i, h: __m256i, folded: __m256i) -> Self {
        Self {
            low: _mm256_clmulepi64_epi128::<0x00>(x, h),
            high: _mm256_clmulepi64_epi128::<0x11>(x, h),
            middle: _mm256_clmulepi64_epi128::<0x00>(_mm256_xor_si256(x, _mm256_shuffle_epi32::<0x4e>(x)), folded),
        }
    }

    #[inline]
    #[target_feature(enable = "avx2,vpclmulqdq")]
    fn add(self, x: __m256i, h: __m256i, folded: __m256i) -> Self {
        let p = Self::new(x, h, folded);
        Self {
            low: _mm256_xor_si256(self.low, p.low),
            high: _mm256_xor_si256(self.high, p.high),
            middle: _mm256_xor_si256(self.middle, p.middle),
        }
    }

    /// Both lanes' sums, reduced once.
    #[inline]
    #[target_feature(enable = "avx2,pclmulqdq")]
    fn reduce(self) -> __m128i {
        let fold = |x: __m256i| _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256::<1>(x));
        Product { low: fold(self.low), high: fold(self.high), middle: fold(self.middle) }.reduce()
    }
}

/// H^8..H^1 in lane pairs, block 2j against H^(8-2j), and their folds.
struct Powers {
    h: [__m256i; 4],
    folds: [__m256i; 4],
}

impl Powers {
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn load(powers: &[u128; 8]) -> Self {
        // `load_powers` reads the eight 16-byte powers.
        let p = unsafe { load_powers(powers) };
        let h = [pair(p[7], p[6]), pair(p[5], p[4]), pair(p[3], p[2]), pair(p[1], p[0])];
        Self { h, folds: h.map(|h| _mm256_xor_si256(h, _mm256_shuffle_epi32::<0x4e>(h))) }
    }

    /// Eight byte-reversed blocks after `y`.
    #[inline]
    #[target_feature(enable = "avx2,vpclmulqdq,pclmulqdq")]
    fn absorb(&self, y: __m128i, x: &[__m256i; 4]) -> __m128i {
        let first = _mm256_xor_si256(x[0], _mm256_zextsi128_si256(y));
        let mut sum = Product2::new(first, self.h[0], self.folds[0]);
        for ((x, h), folded) in x[1..].iter().zip(&self.h[1..]).zip(&self.folds[1..]) {
            sum = sum.add(*x, *h, *folded);
        }
        sum.reduce()
    }
}

/// # Safety
/// `p` addresses 128 readable bytes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn load8(p: *const u8) -> [__m256i; 4] {
    // The caller provides the 128 bytes.
    core::array::from_fn(|j| reverse_lanes(unsafe { _mm256_loadu_si256(p.add(32 * j).cast()) }))
}

/// # Safety
/// VPCLMULQDQ and PCLMULQDQ under YMM state are admitted and `powers` came
/// from `aesni::powers`.
#[target_feature(enable = "avx2,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn hash(powers: &[u128; 8], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    // Each batch is eight whole blocks; the rest are `aesni`'s.
    unsafe {
        let powers_ = Powers::load(powers);
        let mut y = reverse(_mm_loadu_si128(state.as_ptr().cast()));
        let (batches, rest) = blocks.as_chunks::<8>();
        for batch in batches {
            y = powers_.absorb(y, &load8(batch.as_ptr().cast()));
        }
        _mm_storeu_si128(state.as_mut_ptr().cast(), reverse(y));
        if !rest.is_empty() {
            super::aesni::hash(powers, state, rest);
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn round_keys(keys: &[u8; 240]) -> [__m256i; 15] {
    // Each load is one of the schedule's fifteen 16-byte slots.
    core::array::from_fn(|i| unsafe { _mm256_broadcastsi128_si256(_mm_loadu_si128(keys.as_ptr().add(16 * i).cast())) })
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn base(keys: &[u8; 240], nonce: &[u8; 12]) -> __m256i {
    let mut iv = [0u8; 16];
    iv[..12].copy_from_slice(nonce);
    // `iv` and the schedule's first slot are whole 16-byte arrays.
    unsafe {
        _mm256_broadcastsi128_si256(_mm_xor_si128(
            _mm_loadu_si128(iv.as_ptr().cast()),
            _mm_loadu_si128(keys.as_ptr().cast()),
        ))
    }
}

/// Sixteen counter blocks from `first` through every round.
#[inline]
#[target_feature(enable = "avx2,vaes")]
fn encrypt16<const R: usize>(k: &[__m256i; 15], base: __m256i, first: u32) -> [__m256i; 8] {
    let mut s: [__m256i; 8] = core::array::from_fn(|j| counters(base, first.wrapping_add(2 * j as u32)));
    for key in &k[1..R] {
        for block in &mut s {
            *block = _mm256_aesenc_epi128(*block, *key);
        }
    }
    s.map(|block| _mm256_aesenclast_epi128(block, k[R]))
}

/// # Safety
/// `p` addresses 256 exclusive bytes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn xor_store16(p: *mut u8, s: [__m256i; 8]) {
    for (j, stream) in s.into_iter().enumerate() {
        // The caller provides the 256 bytes.
        unsafe {
            let at = p.add(32 * j).cast();
            _mm256_storeu_si256(at, _mm256_xor_si256(stream, _mm256_loadu_si256(at)));
        }
    }
}

#[target_feature(enable = "avx2,vaes,aes,sse2,ssse3")]
unsafe fn ctr_rounds<const R: usize>(keys: &[u8; 240], nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    // Whole 256-byte batches here, the rest in `aesni`.
    unsafe {
        let k = round_keys(keys);
        let base = base(keys, nonce);
        let (batches, rest) = data.as_chunks_mut::<256>();
        let mut next = first;
        for batch in batches.iter_mut() {
            xor_store16(batch.as_mut_ptr(), encrypt16::<R>(&k, base, next));
            next = next.wrapping_add(16);
        }
        super::aesni::ctr(keys, R, nonce, next, rest);
    }
}

/// # Safety
/// VAES and AES under YMM state are admitted, `rounds` is 10 or 14, and no
/// counter from `first` on over `data` wraps.
#[target_feature(enable = "avx2,vaes,aes,sse2,ssse3")]
pub(super) unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    // The caller's admission and bounds carry over.
    unsafe {
        if rounds == 10 {
            ctr_rounds::<10>(keys, nonce, first, data)
        } else {
            ctr_rounds::<14>(keys, nonce, first, data)
        }
    }
}

#[target_feature(enable = "avx2,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
unsafe fn seal_rounds<const R: usize>(
    keys: &[u8; 240],
    powers: &[u128; 8],
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    let n = data.len() / 256;
    if n == 0 {
        return 0;
    }
    // Batch i is bytes 256i..256i+256 of `data`, i < n; each batch is
    // written once and read back only after.
    unsafe {
        let k = round_keys(keys);
        let h = Powers::load(powers);
        let base = base(keys, nonce);
        let mut y = reverse(_mm_loadu_si128(state.as_ptr().cast()));
        let p = data.as_mut_ptr();
        xor_store16(p, encrypt16::<R>(&k, base, 2));
        for i in 1..n {
            let previous = p.add(256 * (i - 1));
            let low = load8(previous);
            let high = load8(previous.add(128));
            let mut s: [__m256i; 8] =
                core::array::from_fn(|j| counters(base, (i as u32).wrapping_mul(16).wrapping_add(2 + 2 * j as u32)));
            // The two halves of the previous batch ride along rounds 1-4 and 5-8.
            let mut sum = Product2::new(
                _mm256_xor_si256(low[0], _mm256_zextsi128_si256(y)),
                h.h[0],
                h.folds[0],
            );
            for (r, key) in k[1..R].iter().enumerate() {
                for block in &mut s {
                    *block = _mm256_aesenc_epi128(*block, *key);
                }
                match r {
                    1..=3 => sum = sum.add(low[r], h.h[r], h.folds[r]),
                    4 => {
                        y = sum.reduce();
                        sum = Product2::new(
                            _mm256_xor_si256(high[0], _mm256_zextsi128_si256(y)),
                            h.h[0],
                            h.folds[0],
                        );
                    }
                    5..=7 => sum = sum.add(high[r - 4], h.h[r - 4], h.folds[r - 4]),
                    _ => {}
                }
            }
            y = sum.reduce();
            xor_store16(p.add(256 * i), s.map(|block| _mm256_aesenclast_epi128(block, k[R])));
        }
        let last = p.add(256 * (n - 1));
        y = h.absorb(y, &load8(last));
        y = h.absorb(y, &load8(last.add(128)));
        _mm_storeu_si128(state.as_mut_ptr().cast(), reverse(y));
    }
    256 * n
}

/// Encrypts whole 256-byte batches from counter 2 on and hashes their
/// ciphertext, returning the bytes done.
///
/// # Safety
/// VAES, AES, VPCLMULQDQ and PCLMULQDQ under YMM state are admitted,
/// `rounds` is 10 or 14, `powers` came from `aesni::powers`, and GCM bounded
/// `data`'s length.
#[target_feature(enable = "avx2,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3")]
pub(super) unsafe fn seal(
    keys: &[u8; 240],
    rounds: usize,
    powers: &[u128; 8],
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    // The caller's admission and bounds carry over.
    unsafe {
        if rounds == 10 {
            seal_rounds::<10>(keys, powers, nonce, state, data)
        } else {
            seal_rounds::<14>(keys, powers, nonce, state, data)
        }
    }
}
