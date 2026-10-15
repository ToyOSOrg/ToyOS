//! RFC 8439 ChaCha20 on AVX2. Eight blocks run with each register holding one
//! state word of all eight (Goll and Gueron, "Vectorization on ChaCha Stream
//! Cipher", ITNG 2014); four run with each 128-bit lane holding one block's
//! row. Rounds, shuffles and addresses depend only on public lengths and
//! counters. Only hosted targets with OS-enabled YMM state enter here.

use core::arch::x86_64::*;

use super::{load2, store2};

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
#[target_feature(enable = "avx2")]
fn quarter(a: &mut __m256i, b: &mut __m256i, c: &mut __m256i, d: &mut __m256i) {
    *a = _mm256_add_epi32(*a, *b);
    *d = rotate16(_mm256_xor_si256(*d, *a));
    *c = _mm256_add_epi32(*c, *d);
    *b = rotate::<12, 20>(_mm256_xor_si256(*b, *c));
    *a = _mm256_add_epi32(*a, *b);
    *d = rotate8(_mm256_xor_si256(*d, *a));
    *c = _mm256_add_epi32(*c, *d);
    *b = rotate::<7, 25>(_mm256_xor_si256(*b, *c));
}

#[inline]
#[target_feature(enable = "avx2")]
fn xor_into(bytes: &mut [u8; 32], stream: __m256i) {
    store2(bytes, _mm256_xor_si256(stream, load2(bytes)));
}

/// Eight blocks from `counter` on, XORed into `out`.
#[inline]
#[target_feature(enable = "avx2")]
fn eight(initial: &[u32; 16], counter: u64, out: &mut [u8; 512]) {
    let lanes: [[u32; 8]; 2] = [
        core::array::from_fn(|j| counter.wrapping_add(j as u64) as u32),
        core::array::from_fn(|j| (counter.wrapping_add(j as u64) >> 32) as u32),
    ];
    let start: [__m256i; 16] = core::array::from_fn(|i| match i {
        // Eight plain words make 32 plain bytes.
        12 | 13 => unsafe { core::mem::transmute::<[u32; 8], __m256i>(lanes[i - 12]) },
        _ => _mm256_set1_epi32(initial[i] as i32),
    });
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
    for (word, initial) in v.iter_mut().zip(start) {
        *word = _mm256_add_epi32(*word, initial);
    }
    // Transpose each group of four words: lane 0 of `t[g][q]` is words
    // 4g..4g+4 of block q, lane 1 of block q + 4.
    let t: [[__m256i; 4]; 4] = core::array::from_fn(|g| {
        let (a, b, c, d) = (v[4 * g], v[4 * g + 1], v[4 * g + 2], v[4 * g + 3]);
        let ab_low = _mm256_unpacklo_epi32(a, b);
        let ab_high = _mm256_unpackhi_epi32(a, b);
        let cd_low = _mm256_unpacklo_epi32(c, d);
        let cd_high = _mm256_unpackhi_epi32(c, d);
        [
            _mm256_unpacklo_epi64(ab_low, cd_low),
            _mm256_unpackhi_epi64(ab_low, cd_low),
            _mm256_unpacklo_epi64(ab_high, cd_high),
            _mm256_unpackhi_epi64(ab_high, cd_high),
        ]
    });
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

/// Four blocks from `counter` on, XORed into `out`, two per register.
#[inline]
#[target_feature(enable = "avx2")]
fn four(initial: &[u32; 16], counter: u64, out: &mut [u8; 256]) {
    let (rows, _) = initial.as_chunks::<4>();
    // Four plain words make 16 plain bytes.
    let row = |i: usize| _mm256_broadcastsi128_si256(unsafe { core::mem::transmute::<[u32; 4], __m128i>(rows[i]) });
    let (a, b, c) = (row(0), row(1), row(2));
    let nonce = (initial[15] as u64) << 32 | initial[14] as u64;
    let counters = |c: u64| _mm256_set_epi64x(nonce as i64, c.wrapping_add(1) as i64, nonce as i64, c as i64);
    let (d0, d1) = (counters(counter), counters(counter.wrapping_add(2)));
    let (mut a0, mut b0, mut c0, mut e0) = (a, b, c, d0);
    let (mut a1, mut b1, mut c1, mut e1) = (a, b, c, d1);
    for _ in 0..10 {
        quarter(&mut a0, &mut b0, &mut c0, &mut e0);
        quarter(&mut a1, &mut b1, &mut c1, &mut e1);
        b0 = _mm256_shuffle_epi32::<0x39>(b0);
        b1 = _mm256_shuffle_epi32::<0x39>(b1);
        c0 = _mm256_shuffle_epi32::<0x4e>(c0);
        c1 = _mm256_shuffle_epi32::<0x4e>(c1);
        e0 = _mm256_shuffle_epi32::<0x93>(e0);
        e1 = _mm256_shuffle_epi32::<0x93>(e1);
        quarter(&mut a0, &mut b0, &mut c0, &mut e0);
        quarter(&mut a1, &mut b1, &mut c1, &mut e1);
        b0 = _mm256_shuffle_epi32::<0x93>(b0);
        b1 = _mm256_shuffle_epi32::<0x93>(b1);
        c0 = _mm256_shuffle_epi32::<0x4e>(c0);
        c1 = _mm256_shuffle_epi32::<0x4e>(c1);
        e0 = _mm256_shuffle_epi32::<0x39>(e0);
        e1 = _mm256_shuffle_epi32::<0x39>(e1);
    }
    let rows = [
        [_mm256_add_epi32(a0, a), _mm256_add_epi32(b0, b), _mm256_add_epi32(c0, c), _mm256_add_epi32(e0, d0)],
        [_mm256_add_epi32(a1, a), _mm256_add_epi32(b1, b), _mm256_add_epi32(c1, c), _mm256_add_epi32(e1, d1)],
    ];
    // Pair p is blocks 2p and 2p + 1, halves 4p to 4p + 3.
    let (halves, _) = out.as_chunks_mut::<32>();
    for (quarter, rows) in halves.as_chunks_mut::<4>().0.iter_mut().zip(rows) {
        xor_into(&mut quarter[0], _mm256_permute2x128_si256::<0x20>(rows[0], rows[1]));
        xor_into(&mut quarter[1], _mm256_permute2x128_si256::<0x20>(rows[2], rows[3]));
        xor_into(&mut quarter[2], _mm256_permute2x128_si256::<0x31>(rows[0], rows[1]));
        xor_into(&mut quarter[3], _mm256_permute2x128_si256::<0x31>(rows[2], rows[3]));
    }
}

/// XORs the longest prefix of whole 256-byte batches and returns its length.
///
/// # Safety
/// AVX2 under YMM state is admitted, and the caller bounded `counter` plus
/// the block count.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    let initial = initial(key, nonce);
    let (eights, rest) = data.as_chunks_mut::<512>();
    let mut counter = counter;
    for batch in eights.iter_mut() {
        eight(&initial.0, counter, batch);
        counter = counter.wrapping_add(8);
    }
    let (fours, _) = rest.as_chunks_mut::<256>();
    for batch in fours.iter_mut() {
        four(&initial.0, counter, batch);
        counter = counter.wrapping_add(4);
    }
    512 * eights.len() + 256 * fours.len()
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
