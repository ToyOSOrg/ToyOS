//! GCM on 256-bit VAES and VPCLMULQDQ (Intel SDM Vol. 2B): each 128-bit lane
//! is one AES block or one carry-less product, in `aesni`'s GHASH form. Only
//! hosted targets with OS-enabled YMM state enter these functions. Hot loops
//! index fixed-size arrays, as `aesni`'s header says why. Each
//! operation is built twice: for AVX2, and for AVX-512VL, whose 32 vector
//! registers hold a batch's state without spilling it.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

/// The operations, every function compiled for `$features`.
macro_rules! gcm256 {
    ($features:literal) => {
        use core::arch::x86_64::*;

        use super::super::aesni::{Product, counter_base, power, reverse};
        use super::super::{load, load_data, store, store_data};

        #[inline]
        #[target_feature(enable = $features)]
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
        #[target_feature(enable = $features)]
        fn pair(low: __m128i, high: __m128i) -> __m256i {
            _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(low), high)
        }

        /// Counter blocks `first` and `first + 1`, round key 0 folded into `base`.
        #[inline]
        #[target_feature(enable = $features)]
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
            #[target_feature(enable = $features)]
            fn new(x: __m256i, h: __m256i, folded: __m256i) -> Self {
                Self {
                    low: _mm256_clmulepi64_epi128::<0x00>(x, h),
                    high: _mm256_clmulepi64_epi128::<0x11>(x, h),
                    middle: _mm256_clmulepi64_epi128::<0x00>(_mm256_xor_si256(x, _mm256_shuffle_epi32::<0x4e>(x)), folded),
                }
            }

            #[inline]
            #[target_feature(enable = $features)]
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
            #[target_feature(enable = $features)]
            fn reduce(self) -> __m128i {
                let fold = |x: __m256i| _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256::<1>(x));
                Product { low: fold(self.low), high: fold(self.high), middle: fold(self.middle) }.reduce()
            }
        }

        /// H^8..H^1 in lane pairs, block 2j against H^(8-2j), and their folds.
        struct PowerPairs {
            h: [__m256i; 4],
            folds: [__m256i; 4],
        }

        impl PowerPairs {
            #[inline]
            #[target_feature(enable = $features)]
            fn new(powers: &[u128; 16]) -> Self {
                let mut h = [_mm256_setzero_si256(); 4];
                let mut folds = [_mm256_setzero_si256(); 4];
                for j in 0..4 {
                    h[j] = pair(power(powers[7 - 2 * j]), power(powers[6 - 2 * j]));
                    folds[j] = _mm256_xor_si256(h[j], _mm256_shuffle_epi32::<0x4e>(h[j]));
                }
                Self { h, folds }
            }

            /// Eight byte-reversed blocks after `y`.
            #[inline]
            #[target_feature(enable = $features)]
            fn absorb(&self, y: __m128i, x: [__m256i; 4]) -> __m128i {
                let first = _mm256_xor_si256(x[0], _mm256_zextsi128_si256(y));
                let mut sum = Product2::new(first, self.h[0], self.folds[0]);
                for j in 1..4 {
                    sum = sum.add(x[j], self.h[j], self.folds[j]);
                }
                sum.reduce()
            }
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn load8(batch: &[u8; 128]) -> [__m256i; 4] {
            let (pairs, _) = batch.as_chunks::<32>();
            let mut x = [_mm256_setzero_si256(); 4];
            for j in 0..4 {
                x[j] = reverse_lanes(load_data(&pairs[j]));
            }
            x
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn hash_body(powers: &[u128; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
            let p = PowerPairs::new(powers);
            let mut y = reverse(load(state));
            let (batches, rest) = blocks.as_chunks::<8>();
            for i in 0..batches.len() {
                y = p.absorb(y, load8(batches[i].as_flattened().try_into().unwrap()));
            }
            store(state, reverse(y));
            if !rest.is_empty() {
                // CLMUL and SSSE3 are admitted with VPCLMULQDQ.
                unsafe { super::super::aesni::hash(powers, state, rest) }
            }
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn round_keys(keys: &[u8; 240]) -> [__m256i; 15] {
            let (slots, _) = keys.as_chunks::<16>();
            let mut k = [_mm256_setzero_si256(); 15];
            for i in 0..15 {
                k[i] = _mm256_broadcastsi128_si256(load(&slots[i]));
            }
            k
        }

        /// Sixteen counter blocks from `first` through every round.
        #[inline]
        #[target_feature(enable = $features)]
        fn encrypt16<const R: usize>(k: &[__m256i; 15], base: __m256i, first: u32) -> [__m256i; 8] {
            let mut s = [_mm256_setzero_si256(); 8];
            for j in 0..8 {
                s[j] = counters(base, first.wrapping_add(2 * j as u32));
            }
            for r in 1..R {
                for j in 0..8 {
                    s[j] = _mm256_aesenc_epi128(s[j], k[r]);
                }
            }
            for j in 0..8 {
                s[j] = _mm256_aesenclast_epi128(s[j], k[R]);
            }
            s
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn xor_into(batch: &mut [u8; 256], s: [__m256i; 8]) {
            let (pairs, _) = batch.as_chunks_mut::<32>();
            for j in 0..8 {
                let stream = _mm256_xor_si256(s[j], load_data(&pairs[j]));
                store_data(&mut pairs[j], stream);
            }
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn ctr_body<const R: usize>(keys: &[u8; 240], nonce: &[u8; 12], first: u32, data: &mut [u8]) {
            let k = round_keys(keys);
            let base = _mm256_broadcastsi128_si256(counter_base(nonce, load(&keys.as_chunks::<16>().0[0])));
            let (batches, rest) = data.as_chunks_mut::<256>();
            let mut next = first;
            for i in 0..batches.len() {
                xor_into(&mut batches[i], encrypt16::<R>(&k, base, next));
                next = next.wrapping_add(16);
            }
            // AES is admitted with VAES; the caller's bound covers the rest.
            unsafe { super::super::aesni::ctr(keys, R, nonce, next, rest) }
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn seal_body<const R: usize>(
            keys: &[u8; 240],
            powers: &[u128; 16],
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
            let h = PowerPairs::new(powers);
            let base = _mm256_broadcastsi128_si256(counter_base(nonce, load(&keys.as_chunks::<16>().0[0])));
            let mut y = reverse(load(state));
            xor_into(&mut batches[0], encrypt16::<R>(&k, base, 2));
            for i in 1..n {
                let (done, next) = batches.split_at_mut(i);
                let (halves, _) = done[i - 1].as_chunks::<128>();
                let (low, high) = (load8(&halves[0]), load8(&halves[1]));
                let first = (i as u32).wrapping_mul(16).wrapping_add(2);
                let mut s = [_mm256_setzero_si256(); 8];
                for j in 0..8 {
                    s[j] = counters(base, first.wrapping_add(2 * j as u32));
                }
                // The previous batch's two halves ride along rounds 1-4 and 5-8.
                let mut sum = Product2::new(_mm256_xor_si256(low[0], _mm256_zextsi128_si256(y)), h.h[0], h.folds[0]);
                for r in 0..R - 1 {
                    for j in 0..8 {
                        s[j] = _mm256_aesenc_epi128(s[j], k[r + 1]);
                    }
                    match r {
                        1..=3 => sum = sum.add(low[r], h.h[r], h.folds[r]),
                        4 => {
                            y = sum.reduce();
                            sum = Product2::new(_mm256_xor_si256(high[0], _mm256_zextsi128_si256(y)), h.h[0], h.folds[0]);
                        }
                        5..=7 => sum = sum.add(high[r - 4], h.h[r - 4], h.folds[r - 4]),
                        _ => {}
                    }
                }
                y = sum.reduce();
                for j in 0..8 {
                    s[j] = _mm256_aesenclast_epi128(s[j], k[R]);
                }
                xor_into(&mut next[0], s);
            }
            let (halves, _) = batches[n - 1].as_chunks::<128>();
            y = h.absorb(y, load8(&halves[0]));
            y = h.absorb(y, load8(&halves[1]));
            store(state, reverse(y));
            256 * n
        }

        /// # Safety
        /// The features are admitted and `powers` came from `aesni::powers`.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn hash(powers: &[u128; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
            hash_body(powers, state, blocks)
        }

        /// # Safety
        /// The features are admitted, `rounds` is 10 or 14, and no counter
        /// from `first` on over `data` wraps.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
            if rounds == 10 {
                ctr_body::<10>(keys, nonce, first, data)
            } else {
                ctr_body::<14>(keys, nonce, first, data)
            }
        }

        /// Encrypts whole 256-byte batches from counter 2 on and hashes
        /// their ciphertext, returning the bytes done.
        ///
        /// # Safety
        /// The features are admitted, `rounds` is 10 or 14, `powers` came
        /// from `aesni::powers`, and GCM bounded `data`'s length.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn seal(
            keys: &[u8; 240],
            rounds: usize,
            powers: &[u128; 16],
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
    };
}

pub(super) mod avx2 {
    gcm256!("avx2,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3");
}

pub(super) mod avx512 {
    gcm256!("avx2,avx512f,avx512vl,vaes,aes,vpclmulqdq,pclmulqdq,sse2,ssse3");
}
