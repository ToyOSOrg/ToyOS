//! RFC 8439 ChaCha20 on AVX2. With AVX-512VL's 32 registers and VPROLD,
//! eight blocks run with each register holding one state word of all eight
//! (Goll and Gueron, "Vectorization on ChaCha Stream Cipher", ITNG 2014). On
//! AVX2 alone, and for shorter runs, blocks run in pairs with each 128-bit
//! lane holding one block's row; with AVX-512F, four blocks run in one
//! register set with a block per lane. Rows are built from the key bytes in
//! registers, never from a state in memory. Rounds, shuffles and addresses
//! depend only on public lengths and counters. Only hosted targets with
//! OS-enabled YMM state enter here. Hot loops index fixed-size arrays, as
//! `aesni`'s header explains.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

use core::arch::x86_64::*;

use super::load;
use crate::poly1305::absorb;

const SIGMA: [u32; 4] = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574];

/// Key word `i`.
#[inline(always)]
fn word(key: &[u8; 32], i: usize) -> u32 {
    u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]])
}

/// Row 1 or 2 of a state: the key's first or second 16 bytes.
#[inline(always)]
fn key_row(key: &[u8; 32], row: usize) -> __m128i {
    let (halves, _) = key.as_chunks::<16>();
    load(&halves[row - 1])
}

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
        use super::{$r16, $r12, $r8, $r7, SIGMA, key_row, word};

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
        fn eight(key: &[u8; 32], nonce: u64, counter: u64, out: &mut [u8; 512]) {
            let mut lanes = [[0u32; 8]; 2];
            for j in 0..8 {
                let c = counter.wrapping_add(j as u64);
                (lanes[0][j], lanes[1][j]) = (c as u32, (c >> 32) as u32);
            }
            let mut start = [_mm256_setzero_si256(); 16];
            for i in 0..4 {
                start[i] = _mm256_set1_epi32(SIGMA[i] as i32);
            }
            for i in 0..8 {
                start[4 + i] = _mm256_set1_epi32(word(key, i) as i32);
            }
            start[14] = _mm256_set1_epi32(nonce as i32);
            start[15] = _mm256_set1_epi32((nonce >> 32) as i32);
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

        /// The first `n` of the pairs' `2P` blocks of keystream in `halves`'
        /// first `2n`, XORed in or written over them.
        #[inline]
        #[target_feature(enable = $features)]
        fn emit<const P: usize>(rows: [[__m256i; 4]; P], n: usize, halves: &mut [[u8; 32]], xor: bool) {
            for p in 0..P {
                let rows = rows[p];
                let blocks = [
                    _mm256_permute2x128_si256::<0x20>(rows[0], rows[1]),
                    _mm256_permute2x128_si256::<0x20>(rows[2], rows[3]),
                    _mm256_permute2x128_si256::<0x31>(rows[0], rows[1]),
                    _mm256_permute2x128_si256::<0x31>(rows[2], rows[3]),
                ];
                // Halves 4p and 4p + 1 are block 2p.
                for i in 0..4.min((2 * n).saturating_sub(4 * p)) {
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
        fn counter_rows<const P: usize>(key: &[u8; 32], nonce: u64, counter: u64) -> [[__m256i; 4]; P] {
            // Four plain words make 16 plain bytes.
            let a = _mm256_broadcastsi128_si256(unsafe { core::mem::transmute::<[u32; 4], __m128i>(SIGMA) });
            let (b, c) = (_mm256_broadcastsi128_si256(key_row(key, 1)), _mm256_broadcastsi128_si256(key_row(key, 2)));
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
        pub(super) fn pairs_of<const P: usize>(key: &[u8; 32], nonce: u64, counter: u64, out: &mut [u8], beside: impl FnMut(usize)) {
            let (halves, _) = out.as_chunks_mut::<32>();
            emit(pairs::<P>(counter_rows::<P>(key, nonce, counter), beside), 2 * P, halves, true);
        }

        /// The rows of the pair of lanes `2p` and `2p + 1`, one per register.
        #[inline]
        #[target_feature(enable = $features)]
        fn lane_rows<const P: usize>(keys: [&[u8; 32]; 4], counters: [u64; 4], nonce: u64) -> [[__m256i; 4]; P] {
            // Four plain words make 16 plain bytes.
            let a = _mm256_broadcastsi128_si256(unsafe { core::mem::transmute::<[u32; 4], __m128i>(SIGMA) });
            let mut rows = [[a; 4]; P];
            for p in 0..P {
                let (low, high) = (keys[2 * p], keys[2 * p + 1]);
                for i in 1..3 {
                    rows[p][i] = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(key_row(low, i)), key_row(high, i));
                }
                rows[p][3] =
                    _mm256_set_epi64x(nonce as i64, counters[2 * p + 1] as i64, nonce as i64, counters[2 * p] as i64);
            }
            rows
        }

        /// XORs all of `data` and returns its length.
        ///
        /// # Safety
        /// AVX2 under YMM state is admitted, and the caller bounded `counter` plus
        /// the block count.
        #[target_feature(enable = $features)]
        pub(in crate::arch) unsafe fn xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
            let nonce = u64::from_le_bytes(*nonce);
            let mut counter = counter;
            let done = if $six {
                let (sixes, _) = data.as_chunks_mut::<384>();
                for i in 0..sixes.len() {
                    pairs_of::<3>(key, nonce, counter, &mut sixes[i], |_| {});
                    counter = counter.wrapping_add(6);
                }
                384 * sixes.len()
            } else {
                let (eights, _) = data.as_chunks_mut::<512>();
                for i in 0..eights.len() {
                    eight(key, nonce, counter, &mut eights[i]);
                    counter = counter.wrapping_add(8);
                }
                512 * eights.len()
            };
            let (fours, rest) = data[done..].as_chunks_mut::<256>();
            for i in 0..fours.len() {
                four(key, nonce, counter, &mut fours[i], true);
                counter = counter.wrapping_add(4);
            }
            // The last blocks, up to 255 bytes, from one keystream batch.
            if !rest.is_empty() {
                let mut stream = crate::secret::Secret([0u8; 256]);
                if rest.len() <= 128 {
                    let (halves, _) = stream.0.as_chunks_mut::<32>();
                    emit(pairs::<1>(counter_rows::<1>(key, nonce, counter), |_| {}), 2, halves, false);
                } else {
                    four(key, nonce, counter, &mut stream.0, false);
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

    /// Four blocks of one key from `counter` on, XORed into `out` or written
    /// over it.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn four(key: &[u8; 32], nonce: u64, counter: u64, out: &mut [u8; 256], xor: bool) {
        let (halves, _) = out.as_chunks_mut::<32>();
        emit(pairs::<2>(counter_rows::<2>(key, nonce, counter), |_| {}), 4, halves, xor);
    }

    /// The keystream of the first `n` of four lanes, `n` 2 to 4, into
    /// `out`'s first `64n` bytes: lane i is block `counters[i]` of
    /// `keys[i]` under `nonce`. One pair where two blocks suffice.
    ///
    /// # Safety
    /// AVX2 under YMM state is admitted.
    #[target_feature(enable = "avx2")]
    pub(in crate::arch) unsafe fn blocks(keys: [&[u8; 32]; 4], counters: [u64; 4], nonce: &[u8; 8], n: usize, out: &mut [u8; 256]) {
        let nonce = u64::from_le_bytes(*nonce);
        let (halves, _) = out.as_chunks_mut::<32>();
        if n <= 2 {
            emit(pairs::<1>(lane_rows::<1>(keys, counters, nonce), |_| {}), n, halves, false);
        } else {
            emit(pairs::<2>(lane_rows::<2>(keys, counters, nonce), |_| {}), n, halves, false);
        }
    }
}

pub(super) mod avx512 {
    chacha256!("avx2,avx512f,avx512vl", false, rol16, rol12, rol8, rol7);

    use super::super::{load, store};

    /// A ZMM register per row of four blocks, block i in 128-bit lane i:
    /// one pair's instructions and latency for twice its blocks.
    type Quad = [__m512i; 4];

    #[inline]
    #[target_feature(enable = "avx512f")]
    fn quarter4(a: &mut __m512i, b: &mut __m512i, c: &mut __m512i, d: &mut __m512i) {
        *a = _mm512_add_epi32(*a, *b);
        *d = _mm512_rol_epi32::<16>(_mm512_xor_si512(*d, *a));
        *c = _mm512_add_epi32(*c, *d);
        *b = _mm512_rol_epi32::<12>(_mm512_xor_si512(*b, *c));
        *a = _mm512_add_epi32(*a, *b);
        *d = _mm512_rol_epi32::<8>(_mm512_xor_si512(*d, *a));
        *c = _mm512_add_epi32(*c, *d);
        *b = _mm512_rol_epi32::<7>(_mm512_xor_si512(*b, *c));
    }

    /// The rows after the rounds and the feed-forward.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn quad(start: Quad) -> Quad {
        let [mut a, mut b, mut c, mut d] = start;
        for _ in 0..10 {
            quarter4(&mut a, &mut b, &mut c, &mut d);
            b = _mm512_shuffle_epi32::<0x39>(b);
            c = _mm512_shuffle_epi32::<0x4e>(c);
            d = _mm512_shuffle_epi32::<0x93>(d);
            quarter4(&mut a, &mut b, &mut c, &mut d);
            b = _mm512_shuffle_epi32::<0x93>(b);
            c = _mm512_shuffle_epi32::<0x4e>(c);
            d = _mm512_shuffle_epi32::<0x39>(d);
        }
        [
            _mm512_add_epi32(a, start[0]),
            _mm512_add_epi32(b, start[1]),
            _mm512_add_epi32(c, start[2]),
            _mm512_add_epi32(d, start[3]),
        ]
    }

    #[inline]
    #[target_feature(enable = "avx512f")]
    fn sigma4() -> __m512i {
        // Four plain words make 16 plain bytes.
        _mm512_broadcast_i32x4(unsafe { core::mem::transmute::<[u32; 4], __m128i>(SIGMA) })
    }

    #[inline]
    #[target_feature(enable = "avx512f")]
    fn counters4(nonce: u64, c: [u64; 4]) -> __m512i {
        let n = nonce as i64;
        _mm512_set_epi64(n, c[3] as i64, n, c[2] as i64, n, c[1] as i64, n, c[0] as i64)
    }

    /// Lane i is block `counters[i]` of `keys[i]`.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn lane_quad(keys: [&[u8; 32]; 4], counters: [u64; 4], nonce: u64) -> Quad {
        let row = |i: usize| {
            let r = _mm512_castsi128_si512(key_row(keys[0], i));
            let r = _mm512_inserti32x4::<1>(r, key_row(keys[1], i));
            let r = _mm512_inserti32x4::<2>(r, key_row(keys[2], i));
            _mm512_inserti32x4::<3>(r, key_row(keys[3], i))
        };
        [sigma4(), row(1), row(2), counters4(nonce, counters)]
    }

    /// Blocks `counter` to `counter + 3` of one key.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn counter_quad(key: &[u8; 32], nonce: u64, counter: u64) -> Quad {
        let c = |i: u64| counter.wrapping_add(i);
        [
            sigma4(),
            _mm512_broadcast_i32x4(key_row(key, 1)),
            _mm512_broadcast_i32x4(key_row(key, 2)),
            counters4(nonce, [c(0), c(1), c(2), c(3)]),
        ]
    }

    /// The first `n` blocks into `out`'s first `64n` bytes, XORed in or
    /// written over them. Caller data is accessed 16 bytes at a time, as
    /// [`load_data`]'s alignment note explains.
    #[inline]
    #[target_feature(enable = "avx512f")]
    fn quad_emit(rows: Quad, n: usize, out: &mut [u8; 256], xor: bool) {
        let [a, b, c, d] = rows;
        // Lanes a0 a1 b0 b1, c0 c1 d0 d1, a2 a3 b2 b3 and c2 c3 d2 d3; then
        // block i is lane i of a, b, c and d.
        let (ab01, cd01) = (_mm512_shuffle_i32x4::<0x44>(a, b), _mm512_shuffle_i32x4::<0x44>(c, d));
        let (ab23, cd23) = (_mm512_shuffle_i32x4::<0xee>(a, b), _mm512_shuffle_i32x4::<0xee>(c, d));
        let blocks = [
            _mm512_shuffle_i32x4::<0x88>(ab01, cd01),
            _mm512_shuffle_i32x4::<0xdd>(ab01, cd01),
            _mm512_shuffle_i32x4::<0x88>(ab23, cd23),
            _mm512_shuffle_i32x4::<0xdd>(ab23, cd23),
        ];
        let (rows, _) = out.as_chunks_mut::<16>();
        for i in 0..n {
            let v = blocks[i];
            let lanes = [
                _mm512_castsi512_si128(v),
                _mm512_extracti32x4_epi32::<1>(v),
                _mm512_extracti32x4_epi32::<2>(v),
                _mm512_extracti32x4_epi32::<3>(v),
            ];
            for j in 0..4 {
                let row = &mut rows[4 * i + j];
                let v = if xor { _mm_xor_si128(lanes[j], load(row)) } else { lanes[j] };
                store(row, v);
            }
        }
    }

    /// Four blocks of one key from `counter` on, XORed into `out` or written
    /// over it.
    #[inline]
    #[target_feature(enable = "avx2,avx512f,avx512vl")]
    fn four(key: &[u8; 32], nonce: u64, counter: u64, out: &mut [u8; 256], xor: bool) {
        quad_emit(quad(counter_quad(key, nonce, counter)), 4, out, xor);
    }

    /// As [`super::avx2::blocks`], with four blocks in one register set.
    ///
    /// # Safety
    /// AVX-512VL under ZMM state is admitted.
    #[target_feature(enable = "avx2,avx512f,avx512vl")]
    pub(in crate::arch) unsafe fn blocks(keys: [&[u8; 32]; 4], counters: [u64; 4], nonce: &[u8; 8], n: usize, out: &mut [u8; 256]) {
        let nonce = u64::from_le_bytes(*nonce);
        if n <= 2 {
            let (halves, _) = out.as_chunks_mut::<32>();
            emit(pairs::<1>(lane_rows::<1>(keys, counters, nonce), |_| {}), n, halves, false);
        } else {
            quad_emit(quad(lane_quad(keys, counters, nonce)), n, out, false);
        }
    }
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
    let nonce = u64::from_le_bytes(*nonce);
    let (batches, _) = data.as_chunks_mut::<384>();
    let n = batches.len();
    if n == 0 {
        return 0;
    }
    let mut acc = *h;
    avx2::pairs_of::<3>(key, nonce, counter, &mut batches[0], |_| {});
    for i in 1..n {
        let (done, next) = batches.split_at_mut(i);
        let (blocks, _) = done[i - 1].as_chunks::<16>();
        avx2::pairs_of::<3>(key, nonce, counter.wrapping_add(6 * i as u64), &mut next[0], |round| {
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
