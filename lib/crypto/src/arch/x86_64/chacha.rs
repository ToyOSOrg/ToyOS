//! RFC 8439 ChaCha20 on AVX2. With AVX-512VL's 32 registers and VPROLD,
//! eight blocks run with each register holding one state word of all eight
//! (Goll and Gueron, "Vectorization on ChaCha Stream Cipher", ITNG 2014). On
//! AVX2 alone, and for shorter runs, blocks run in pairs with each 128-bit
//! lane holding one block's row. Rounds, shuffles and addresses depend only
//! on public lengths and counters. Only hosted targets with OS-enabled YMM
//! state enter here. Hot loops index fixed-size arrays, as `aesni`'s header
//! explains.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

use core::arch::x86_64::*;

use crate::poly1305::absorb;

const SIGMA: [u32; 4] = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574];

#[inline]
#[target_feature(enable = "avx2")]
fn rotate16(x: __m256i) -> __m256i {
    _mm256_shuffle_epi8(
        x,
        _mm256_setr_epi8(
            2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13,
        ),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
fn rotate8(x: __m256i) -> __m256i {
    _mm256_shuffle_epi8(
        x,
        _mm256_setr_epi8(
            3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14, 3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14,
        ),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
fn rotate<const L: i32, const R: i32>(x: __m256i) -> __m256i {
    _mm256_or_si256(_mm256_slli_epi32::<L>(x), _mm256_srli_epi32::<R>(x))
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn rol16(x: __m256i) -> __m256i {
    _mm256_rol_epi32::<16>(x)
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn rol12(x: __m256i) -> __m256i {
    _mm256_rol_epi32::<12>(x)
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn rol8(x: __m256i) -> __m256i {
    _mm256_rol_epi32::<8>(x)
}

#[inline]
#[target_feature(enable = "avx512f,avx512vl")]
fn rol7(x: __m256i) -> __m256i {
    _mm256_rol_epi32::<7>(x)
}

#[inline]
#[target_feature(enable = "avx2")]
fn rotate12(x: __m256i) -> __m256i {
    rotate::<12, 20>(x)
}

#[inline]
#[target_feature(enable = "avx2")]
fn rotate7(x: __m256i) -> __m256i {
    rotate::<7, 25>(x)
}

/// The vector rounds and their callers, every function compiled for
/// `$features` with the rotations given.
macro_rules! chacha256 {
    ($features:literal, $six:literal, $r16:ident, $r12:ident, $r8:ident, $r7:ident) => {
        use core::arch::x86_64::*;

        use super::super::{load_data, store_data};
        use super::{$r16, $r12, $r8, $r7};

        #[inline]
        #[target_feature(enable = $features)]
        fn quarter(a: &mut __m256i, b: &mut __m256i, c: &mut __m256i, d: &mut __m256i) {
            *a = _mm256_add_epi32(*a, *b);
            *d = $r16(_mm256_xor_si256(*d, *a));
            *c = _mm256_add_epi32(*c, *d);
            *b = $r12(_mm256_xor_si256(*b, *c));
            *a = _mm256_add_epi32(*a, *b);
            *d = $r8(_mm256_xor_si256(*d, *a));
            *c = _mm256_add_epi32(*c, *d);
            *b = $r7(_mm256_xor_si256(*b, *c));
        }

        #[inline]
        #[target_feature(enable = $features)]
        fn xor_into(bytes: &mut [u8; 32], stream: __m256i) {
            store_data(bytes, _mm256_xor_si256(stream, load_data(bytes)));
        }

        /// Eight blocks from `counter` on, XORed into `out`.
        #[inline]
        #[target_feature(enable = $features)]
        fn eight(initial: &[u32; 16], counter: u64, out: &mut [u8; 512]) {
            let mut lanes = [[0u32; 8]; 2];
            for j in 0..8 {
                let c = counter.wrapping_add(j as u64);
                (lanes[0][j], lanes[1][j]) = (c as u32, (c >> 32) as u32);
            }
            let mut start = [_mm256_setzero_si256(); 16];
            for i in 0..16 {
                start[i] = _mm256_set1_epi32(initial[i] as i32);
            }
            // Eight plain words make 32 plain bytes.
            start[12] = unsafe { core::mem::transmute::<[u32; 8], __m256i>(lanes[0]) };
            start[13] = unsafe { core::mem::transmute::<[u32; 8], __m256i>(lanes[1]) };
            let mut v = start;
            macro_rules! q {
                ($a:literal, $b:literal, $c:literal, $d:literal) => {{
                    let (mut a, mut b, mut c, mut d) = (v[$a], v[$b], v[$c], v[$d]);
                    quarter(&mut a, &mut b, &mut c, &mut d);
                    (v[$a], v[$b], v[$c], v[$d]) = (a, b, c, d);
                }};
            }
            for _ in 0..10 {
                q!(0, 4, 8, 12);
                q!(1, 5, 9, 13);
                q!(2, 6, 10, 14);
                q!(3, 7, 11, 15);
                q!(0, 5, 10, 15);
                q!(1, 6, 11, 12);
                q!(2, 7, 8, 13);
                q!(3, 4, 9, 14);
            }
            for i in 0..16 {
                v[i] = _mm256_add_epi32(v[i], start[i]);
            }
            // Transpose each group of four words: lane 0 of `t[g][q]` is words
            // 4g..4g+4 of block q, lane 1 of block q + 4.
            let mut t = [[_mm256_setzero_si256(); 4]; 4];
            for g in 0..4 {
                let (a, b, c, d) = (v[4 * g], v[4 * g + 1], v[4 * g + 2], v[4 * g + 3]);
                let ab_low = _mm256_unpacklo_epi32(a, b);
                let ab_high = _mm256_unpackhi_epi32(a, b);
                let cd_low = _mm256_unpacklo_epi32(c, d);
                let cd_high = _mm256_unpackhi_epi32(c, d);
                t[g] = [
                    _mm256_unpacklo_epi64(ab_low, cd_low),
                    _mm256_unpackhi_epi64(ab_low, cd_low),
                    _mm256_unpacklo_epi64(ab_high, cd_high),
                    _mm256_unpackhi_epi64(ab_high, cd_high),
                ];
            }
            // Block q is halves 2q and 2q + 1, block q + 4 halves 2q + 8 and 2q + 9.
            let (halves, _) = out.as_chunks_mut::<32>();
            for q in 0..4 {
                let [w0, w1, w2, w3] = [t[0][q], t[1][q], t[2][q], t[3][q]];
                xor_into(&mut halves[2 * q], _mm256_permute2x128_si256::<0x20>(w0, w1));
                xor_into(&mut halves[2 * q + 1], _mm256_permute2x128_si256::<0x20>(w2, w3));
                xor_into(&mut halves[2 * q + 8], _mm256_permute2x128_si256::<0x31>(w0, w1));
                xor_into(&mut halves[2 * q + 9], _mm256_permute2x128_si256::<0x31>(w2, w3));
            }
        }

        /// `2P` blocks, two per register: pair p holds blocks 2p and 2p + 1, each
        /// register one row of both, with `beside` run once in each of the first
        /// eight double rounds. Returns each pair's rows after the rounds and the
        /// feed-forward.
        #[inline]
        #[target_feature(enable = $features)]
        fn pairs<const P: usize>(start: [[__m256i; 4]; P], mut beside: impl FnMut(usize)) -> [[__m256i; 4]; P] {
            let mut v = start;
            for round in 0..10 {
                if round < 8 {
                    beside(round);
                }
                for p in 0..P {
                    let [mut a, mut b, mut c, mut d] = v[p];
                    quarter(&mut a, &mut b, &mut c, &mut d);
                    b = _mm256_shuffle_epi32::<0x39>(b);
                    c = _mm256_shuffle_epi32::<0x4e>(c);
                    d = _mm256_shuffle_epi32::<0x93>(d);
                    v[p] = [a, b, c, d];
                }
                for p in 0..P {
                    let [mut a, mut b, mut c, mut d] = v[p];
                    quarter(&mut a, &mut b, &mut c, &mut d);
                    b = _mm256_shuffle_epi32::<0x93>(b);
                    c = _mm256_shuffle_epi32::<0x4e>(c);
                    d = _mm256_shuffle_epi32::<0x39>(d);
                    v[p] = [a, b, c, d];
                }
            }
            for p in 0..P {
                for i in 0..4 {
                    v[p][i] = _mm256_add_epi32(v[p][i], start[p][i]);
                }
            }
            v
        }

        /// Pair rows as `128P` bytes of keystream in `halves`' first `4P`,
        /// XORed in or written over them.
        #[inline]
        #[target_feature(enable = $features)]
        fn emit<const P: usize>(rows: [[__m256i; 4]; P], halves: &mut [[u8; 32]], xor: bool) {
            for p in 0..P {
                let rows = rows[p];
                let blocks = [
                    _mm256_permute2x128_si256::<0x20>(rows[0], rows[1]),
                    _mm256_permute2x128_si256::<0x20>(rows[2], rows[3]),
                    _mm256_permute2x128_si256::<0x31>(rows[0], rows[1]),
                    _mm256_permute2x128_si256::<0x31>(rows[2], rows[3]),
                ];
                for i in 0..4 {
                    if xor {
                        xor_into(&mut halves[4 * p + i], blocks[i]);
                    } else {
                        store_data(&mut halves[4 * p + i], blocks[i]);
                    }
                }
            }
        }

        /// The rows of blocks `counter` to `counter + 2P - 1` of one key.
        #[inline]
        #[target_feature(enable = $features)]
        fn counter_rows<const P: usize>(initial: &[u32; 16], counter: u64) -> [[__m256i; 4]; P] {
            let (r, _) = initial.as_chunks::<4>();
            // Four plain words make 16 plain bytes.
            let row = |i: usize| _mm256_broadcastsi128_si256(unsafe { core::mem::transmute::<[u32; 4], __m128i>(r[i]) });
            let (a, b, c) = (row(0), row(1), row(2));
            let nonce = (initial[15] as u64) << 32 | initial[14] as u64;
            let mut rows = [[a, b, c, a]; P];
            for p in 0..P {
                let first = counter.wrapping_add(2 * p as u64);
                rows[p][3] = _mm256_set_epi64x(nonce as i64, first.wrapping_add(1) as i64, nonce as i64, first as i64);
            }
            rows
        }

        /// `2P` blocks of one key from `counter` on, XORed into `out`'s first
        /// `128P` bytes, with `beside` as [`pairs`] runs it.
        #[inline]
        #[target_feature(enable = $features)]
        pub(super) fn pairs_of<const P: usize>(initial: &[u32; 16], counter: u64, out: &mut [u8], beside: impl FnMut(usize)) {
            let (halves, _) = out.as_chunks_mut::<32>();
            emit(pairs::<P>(counter_rows::<P>(initial, counter), beside), halves, true);
        }

        /// The rows of states `2p` and `2p + 1`, one per register.
        #[inline]
        #[target_feature(enable = $features)]
        fn rows<const P: usize>(states: &[[u32; 16]]) -> [[__m256i; 4]; P] {
            let mut rows = [[_mm256_setzero_si256(); 4]; P];
            for p in 0..P {
                let (low, _) = states[2 * p].as_chunks::<4>();
                let (high, _) = states[2 * p + 1].as_chunks::<4>();
                for i in 0..4 {
                    // Four plain words make 16 plain bytes.
                    let (l, h) = unsafe {
                        (core::mem::transmute::<[u32; 4], __m128i>(low[i]), core::mem::transmute::<[u32; 4], __m128i>(high[i]))
                    };
                    rows[p][i] = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(l), h);
                }
            }
            rows
        }

        /// The keystream of the first `n` of four independent states, `n`
        /// 2 to 4, into `out`'s first `64n` bytes: one pair where two blocks
        /// suffice.
        ///
        /// # Safety
        /// AVX2 under YMM state is admitted.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn blocks(states: &[[u32; 16]; 4], n: usize, out: &mut [u8; 256]) {
            let (halves, _) = out.as_chunks_mut::<32>();
            if n <= 2 {
                emit(pairs::<1>(rows::<1>(states), |_| {}), halves, false);
            } else {
                emit(pairs::<2>(rows::<2>(states), |_| {}), halves, false);
            }
        }

        /// XORs all of `data` and returns its length.
        ///
        /// # Safety
        /// AVX2 under YMM state is admitted, and the caller bounded `counter` plus
        /// the block count.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
            let initial = super::initial(key, nonce);
            let mut counter = counter;
            let done = if $six {
                let (sixes, _) = data.as_chunks_mut::<384>();
                for i in 0..sixes.len() {
                    pairs_of::<3>(&initial.0, counter, &mut sixes[i], |_| {});
                    counter = counter.wrapping_add(6);
                }
                384 * sixes.len()
            } else {
                let (eights, _) = data.as_chunks_mut::<512>();
                for i in 0..eights.len() {
                    eight(&initial.0, counter, &mut eights[i]);
                    counter = counter.wrapping_add(8);
                }
                512 * eights.len()
            };
            let (fours, rest) = data[done..].as_chunks_mut::<256>();
            for i in 0..fours.len() {
                pairs_of::<2>(&initial.0, counter, &mut fours[i], |_| {});
                counter = counter.wrapping_add(4);
            }
            // The last blocks, up to 255 bytes, from one keystream batch.
            if !rest.is_empty() {
                let mut stream = crate::secret::Secret([0u8; 256]);
                if rest.len() <= 128 {
                    pairs_of::<1>(&initial.0, counter, &mut stream.0, |_| {});
                } else {
                    pairs_of::<2>(&initial.0, counter, &mut stream.0, |_| {});
                }
                crate::block::xor(rest, &stream.0);
            }
            data.len()
        }
    };
}

pub(super) mod avx2 {
    // Sixteen YMM registers hold three pairs' rows and the rotation masks;
    // the word-per-register layout's eight blocks spill.
    chacha256!("avx2", true, rotate16, rotate12, rotate8, rotate7);
}

pub(super) mod avx512 {
    chacha256!("avx2,avx512f,avx512vl", false, rol16, rol12, rol8, rol7);
}

/// The AVX2 `xor` over whole 384-byte batches, absorbing each batch's
/// ciphertext into the Poly1305 accumulator `h` under `r` in the next
/// batch's double rounds: scalar multiplication on the integer units beside
/// the vector rounds (Gopal et al., "Fast Cryptographic Computation on Intel
/// Architecture Processors Via Function Stitching", Intel, 2010). Returns the
/// bytes done.
///
/// Where AVX-512VL admits its own rounds, the vector Poly1305 after them
/// is faster than this.
///
/// # Safety
/// AVX2 under YMM state is admitted, and the caller bounded `counter` plus
/// the block count.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn seal(
    key: &[u8; 32],
    counter: u64,
    nonce: &[u8; 8],
    data: &mut [u8],
    r: &[u64; 3],
    h: &mut [u64; 3],
) -> usize {
    let initial = initial(key, nonce);
    let (batches, _) = data.as_chunks_mut::<384>();
    let n = batches.len();
    if n == 0 {
        return 0;
    }
    let mut acc = *h;
    avx2::pairs_of::<3>(&initial.0, counter, &mut batches[0], |_| {});
    for i in 1..n {
        let (done, next) = batches.split_at_mut(i);
        let (blocks, _) = done[i - 1].as_chunks::<16>();
        avx2::pairs_of::<3>(&initial.0, counter.wrapping_add(6 * i as u64), &mut next[0], |round| {
            for j in 3 * round..3 * round + 3 {
                acc = absorb(acc, &blocks[j], *r, true);
            }
        });
    }
    let (blocks, _) = batches[n - 1].as_chunks::<16>();
    for j in 0..24 {
        acc = absorb(acc, &blocks[j], *r, true);
    }
    *h = acc;
    384 * n
}

fn initial(key: &[u8; 32], nonce: &[u8; 8]) -> crate::secret::Secret<16, u32> {
    let mut initial = [0u32; 16];
    initial[..4].copy_from_slice(&SIGMA);
    for (word, bytes) in initial[4..12].iter_mut().zip(key.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    for (word, bytes) in initial[14..].iter_mut().zip(nonce.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    crate::secret::Secret(initial)
}
