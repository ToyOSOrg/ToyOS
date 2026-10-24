//! RFC 8439 ChaCha20 on AVX2. Eight blocks run with each register holding
//! one state word of all eight (Goll and Gueron, "Vectorization on ChaCha
//! Stream Cipher", ITNG 2014), with AVX-512VL's VPROLD or AVX2's shifts. On
//! AVX2 the sixteen registers spill a few words a round, but this layout
//! needs no diagonal shuffles, which Zen 2 runs on two pipes, one of them its
//! only shift pipe. Shorter runs go in pairs, each 128-bit lane holding one
//! block's row; with AVX-512F, four blocks run in one register set with a
//! block per lane. Rows are built from the key bytes in registers, never from
//! a state in memory. Rounds, shuffles and addresses depend only on public
//! lengths and counters. Only hosted targets with OS-enabled YMM state enter
//! here. Hot loops index fixed-size arrays, as `aesni`'s header explains.

#![allow(clippy::needless_range_loop, reason = "see `aesni`'s header")]

use core::arch::x86_64::*;

use super::load;

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

/// `mask` through an assembly block LLVM cannot see into, which emits
/// nothing. A shuffle by a known 16-bit rotation mask is otherwise lowered
/// as VPSHUFLW and VPSHUFHW, two shuffle uops for VPSHUFB's one.
#[inline]
#[target_feature(enable = "avx2")]
fn opaque(mut mask: __m256i) -> __m256i {
    // SAFETY: the template reads and writes only the operand's register.
    unsafe { core::arch::asm!("/* {0} */", inout(ymm_reg) mask, options(pure, nomem, nostack, preserves_flags)) };
    mask
}

#[inline]
#[target_feature(enable = "avx2")]
fn rotate16(x: __m256i) -> __m256i {
    let mask = _mm256_setr_epi8(
        2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13,
    );
    _mm256_shuffle_epi8(x, opaque(mask))
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
    ($features:literal, $r16:ident, $r12:ident, $r8:ident, $r7:ident) => {
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

        /// Eight blocks from `counter` on, a register per state word of all
        /// eight, with `beside` run once in each double round. Returns the
        /// words after the rounds and the feed-forward.
        #[inline]
        #[target_feature(enable = $features)]
        fn eight(key: &[u8; 32], nonce: u64, counter: u64, mut beside: impl FnMut(usize)) -> [__m256i; 16] {
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
            for round in 0..10 {
                beside(round);
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
            v
        }

        /// [`eight`]'s words as eight blocks of keystream XORed into `out` or
        /// written over it.
        #[inline]
        #[target_feature(enable = $features)]
        fn emit_eight(v: [__m256i; 16], out: &mut [u8; 512], xor: bool) {
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
                let blocks = [
                    _mm256_permute2x128_si256::<0x20>(w0, w1),
                    _mm256_permute2x128_si256::<0x20>(w2, w3),
                    _mm256_permute2x128_si256::<0x31>(w0, w1),
                    _mm256_permute2x128_si256::<0x31>(w2, w3),
                ];
                let at = [2 * q, 2 * q + 1, 2 * q + 8, 2 * q + 9];
                for i in 0..4 {
                    if xor {
                        xor_into(&mut halves[at[i]], blocks[i]);
                    } else {
                        store_data(&mut halves[at[i]], blocks[i]);
                    }
                }
            }
        }

        /// `2P` blocks, two per register: pair p holds blocks 2p and 2p + 1, each
        /// register one row of both, with `beside` run once in each double
        /// round. Returns each pair's rows after the rounds and the
        /// feed-forward.
        #[inline]
        #[target_feature(enable = $features)]
        fn pairs<const P: usize>(start: [[__m256i; 4]; P], mut beside: impl FnMut(usize)) -> [[__m256i; 4]; P] {
            let mut v = start;
            for round in 0..10 {
                beside(round);
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
            let (eights, _) = data.as_chunks_mut::<512>();
            for i in 0..eights.len() {
                emit_eight(eight(key, nonce, counter, |_| {}), &mut eights[i], true);
                counter = counter.wrapping_add(8);
            }
            let done = 512 * eights.len();
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
    chacha256!("avx2", rotate16, rotate12, rotate8, rotate7);

    use core::ops::Range;

    use crate::block::{padded, terminated, xor as xor_bytes};
    use crate::poly1305::{Poly1305, absorb};
    use crate::secret::Secret;

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

    /// Absorbs `blocks[3 round..3 round + 3]`, those there are, into `acc`.
    #[inline(always)]
    fn absorb_round(acc: &mut [u64; 3], blocks: &[[u8; 16]], r: [u64; 3], round: usize) {
        for j in 3 * round..(3 * round + 3).min(blocks.len()) {
            *acc = absorb(*acc, &blocks[j], r, true);
        }
    }

    /// Absorbs `blocks[30..]`, the blocks past three in each double round.
    #[inline(always)]
    fn absorb_after(acc: &mut [u64; 3], blocks: &[[u8; 16]], r: [u64; 3]) {
        for j in 30..blocks.len() {
            *acc = absorb(*acc, &blocks[j], r, true);
        }
    }

    /// Eight blocks from `counter` XORed into `data[at..at + 512]`, while the
    /// blocks of `data[hashed]` are absorbed into `acc` under `r`: scalar
    /// multiplication on the integer units beside the vector rounds (Gopal et
    /// al., "Fast Cryptographic Computation on Intel Architecture Processors
    /// Via Function Stitching", Intel, 2010), three in each double round and
    /// any past thirty after them, then `data[partial]` as the MAC's [`last`]
    /// block, all before the keystream lands.
    #[inline]
    #[target_feature(enable = "avx2,bmi2")]
    fn batch(
        key: &[u8; 32],
        nonce: u64,
        counter: u64,
        data: &mut [u8],
        at: usize,
        (hashed, partial): (Range<usize>, Range<usize>),
        (acc, r, pad): (&mut [u64; 3], [u64; 3], bool),
    ) {
        let v = {
            let (blocks, _) = data[hashed].as_chunks::<16>();
            let v = eight(key, nonce, counter, |round| absorb_round(acc, blocks, r, round));
            absorb_after(acc, blocks, r);
            if !partial.is_empty() {
                *acc = last(*acc, &data[partial], r, pad);
            }
            v
        };
        emit_eight(v, (&mut data[at..at + 512]).try_into().unwrap(), true);
    }

    /// Keystream from block `counter` over `stream`, enough for `n` bytes, at
    /// most 512, in the fewest pairs or eight blocks, absorbing `blocks` as
    /// [`batch`] does.
    #[inline]
    #[target_feature(enable = "avx2,bmi2")]
    fn keystream(
        key: &[u8; 32],
        nonce: u64,
        counter: u64,
        n: usize,
        blocks: &[[u8; 16]],
        (acc, r): (&mut [u64; 3], [u64; 3]),
        stream: &mut [u8; 512],
    ) {
        if n > 256 {
            let v = eight(key, nonce, counter, |round| absorb_round(acc, blocks, r, round));
            emit_eight(v, stream, false);
        } else {
            let (halves, _) = stream.as_chunks_mut::<32>();
            if n <= 128 {
                let rows = pairs::<1>(counter_rows::<1>(key, nonce, counter), |round| absorb_round(acc, blocks, r, round));
                emit(rows, 2, halves, false);
            } else {
                let rows = pairs::<2>(counter_rows::<2>(key, nonce, counter), |round| absorb_round(acc, blocks, r, round));
                emit(rows, 4, halves, false);
            }
        }
        absorb_after(acc, blocks, r);
    }

    /// The MAC's last block, partial: RFC 8439 pads it with zeros; OpenSSH's
    /// ends in a one byte, as Poly1305's own message end does.
    #[inline(always)]
    fn last(acc: [u64; 3], bytes: &[u8], r: [u64; 3], pad: bool) -> [u64; 3] {
        if pad {
            absorb(acc, &padded(bytes), r, true)
        } else {
            absorb(acc, &terminated(&padded(bytes), bytes.len(), 1), r, false)
        }
    }

    /// ChaCha20 and its Poly1305 MAC in one pass over `data`: block `counter`
    /// is the Poly1305 key and `data` is XORed with the blocks after it. The
    /// MAC runs over `prefix` and the ciphertext, each zero-padded to a block
    /// where `pad` asks it, as RFC 8439 §2.8 does, or run together, as
    /// OpenSSH's does. Returns the MAC ready for anything after the
    /// ciphertext.
    ///
    /// The key comes from the first batch, which carries the first seven
    /// blocks of keystream; an open that runs on past it carries one, since
    /// its MAC waits for the key. Sealing absorbs each 512-byte batch's
    /// ciphertext beside the next batch's rounds; opening absorbs a batch, to
    /// the end of the MAC block it ends in, beside its own rounds before
    /// they overwrite it.
    ///
    /// # Safety
    /// AVX2 and BMI2 under YMM state are admitted, and the caller bounded
    /// `counter` plus the block count.
    #[target_feature(enable = "avx2,bmi2")]
    pub(in crate::arch) unsafe fn chacha20_poly1305(
        key: &[u8; 32],
        counter: u64,
        nonce: &[u8; 8],
        prefix: &[u8],
        data: &mut [u8],
        seal: bool,
        pad: bool,
    ) -> Poly1305 {
        let nonce = u64::from_le_bytes(*nonce);
        let len = data.len();
        // The MAC's blocks over `data` begin `skew` bytes in, after the bytes
        // that complete an unpadded prefix's last block.
        let skew = if pad { 0 } else { prefix.len().wrapping_neg() % 16 };
        // The end of the MAC block holding byte `end - 1`, or of `data`.
        let reach = |end: usize| (skew + end.saturating_sub(skew).next_multiple_of(16)).min(len);
        let rest = |mac: &mut Poly1305, bytes: &[u8]| if pad { mac.update_padded(bytes) } else { mac.update(bytes) };
        let mut stream = Secret([0u8; 512]);
        let first = if seal || len <= 448 { len.min(448) } else { 64 };
        keystream(key, nonce, counter, 64 + first, &[], (&mut [0; 3], [0; 3]), &mut stream.0);
        let mut mac = Poly1305::new(stream.0[..32].try_into().unwrap());
        rest(&mut mac, prefix);
        // The bytes of `data` absorbed.
        let mut hashed = if seal { 0 } else { reach(first) };
        rest(&mut mac, &data[..hashed]);
        xor_bytes(&mut data[..first], &stream.0[64..]);
        if seal {
            hashed = skew.min(len);
            rest(&mut mac, &data[..hashed]);
        }
        let mut done = first;
        if done < len {
            let Some((r, h)) = mac.parts() else { unreachable!("the MAC is at a block boundary") };
            let (r, mut acc) = (*r, *h);
            let mut counter = counter.wrapping_add(1 + first as u64 / 64);
            while done < len {
                let end = len.min(done + 512);
                let upto = if seal { done } else { reach(end) };
                let whole = hashed + (upto - hashed) / 16 * 16;
                // An open's final partial block, where this batch reaches it.
                let partial = if seal { whole..whole } else { whole..upto };
                if end - done == 512 {
                    batch(key, nonce, counter, data, done, (hashed..whole, partial), (&mut acc, r, pad));
                } else {
                    let blocks = data[hashed..whole].as_chunks::<16>().0;
                    keystream(key, nonce, counter, end - done, blocks, (&mut acc, r), &mut stream.0);
                    if !partial.is_empty() {
                        acc = last(acc, &data[partial], r, pad);
                    }
                    xor_bytes(&mut data[done..end], &stream.0);
                }
                hashed = if seal { whole } else { upto };
                done = end;
                counter = counter.wrapping_add(8);
            }
            let Some((_, h)) = mac.parts() else { unreachable!("the MAC is at a block boundary") };
            *h = acc;
        }
        rest(&mut mac, &data[hashed..]);
        mac
    }
}

pub(super) mod avx512 {
    chacha256!("avx2,avx512f,avx512vl", rol16, rol12, rol8, rol7);

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

#[cfg(test)]
mod tests {
    use std::{format, vec::Vec};

    use crate::chacha::legacy_block;

    /// Whether this host can test the AVX2 stream; a host without AVX2 says so
    /// rather than passing.
    fn avx2() -> bool {
        assert!(
            super::super::features().avx2() || cfg!(feature = "portable"),
            "this host's CPU has no AVX2, so nothing here can test the AVX2 ChaCha20 paths"
        );
        !cfg!(feature = "portable")
    }

    /// Whether this host can test the one-pass kernel, which also takes BMI2;
    /// a host without either says so rather than passing.
    fn avx2_bmi2() -> bool {
        let features = super::super::features();
        assert!(
            (features.avx2() && features.bmi2()) || cfg!(feature = "portable"),
            "this host's CPU lacks AVX2 or BMI2, so nothing here can test the one-pass ChaCha20-Poly1305 kernel"
        );
        !cfg!(feature = "portable")
    }

    /// `data` XORed with blocks `counter` on, one portable block at a time.
    fn portable_stream(key: &[u8; 32], nonce: &[u8; 8], counter: u64, data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        for (i, chunk) in out.chunks_mut(64).enumerate() {
            let block = legacy_block(key, nonce, counter.wrapping_add(i as u64));
            for (byte, mask) in chunk.iter_mut().zip(block.0) {
                *byte ^= mask;
            }
        }
        out
    }

    fn message(len: usize, seed: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 167 + seed * 31 + 13) as u8).collect()
    }

    /// The eight-block stream from every counter carry a batch can straddle,
    /// through every batch tail, against one portable block at a time. The
    /// dispatched stream takes AVX-512 where the host has it, so this drives
    /// the AVX2 kernel itself.
    #[test]
    fn the_avx2_stream_matches_one_block_at_a_time() {
        if !avx2() {
            return;
        }
        let key: [u8; 32] = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let nonce = [0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe];
        for len in (0..=1600).chain([2047, 2048, 2049, 4095, 4096, 4097]) {
            let data = message(len, len);
            for counter in [0, u64::from(u32::MAX) - 3, u64::MAX - len.div_ceil(64) as u64] {
                let mut ours = data.clone();
                // CPUID and XCR0 admitted AVX2 above; no counter passes 2^64.
                let done = unsafe { super::avx2::xor(&key, counter, &nonce, &mut ours) };
                assert_eq!(done, len);
                assert_eq!(ours, portable_stream(&key, &nonce, counter, &data), "{len} bytes from counter {counter}");
            }
        }
    }

    /// The one-pass kernel against both MACs composed from portable blocks
    /// and `poly1305::tag`: RFC 8439 §2.8's, with AAD that fills no block,
    /// one and a part, and OpenSSH's over a four-byte header and the payload
    /// run together. Every length through the first batch, two whole batches
    /// and each tail, then longer runs; sealing and opening.
    #[test]
    fn the_avx2_one_pass_matches_both_macs_composed() {
        if !avx2_bmi2() {
            return;
        }
        let key: [u8; 32] = core::array::from_fn(|i| (i * 89 + 1) as u8);
        let nonce = [0xa5, 0x5a, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        // An RFC 8439 nonce's first word above the 32-bit block counter.
        let counter = 0x0807_0605_0000_0000;
        for len in (1..=1600).chain([1983, 1984, 1985, 2047, 2048, 2049, 4096, 16383, 16384, 16385]) {
            for pad in [true, false] {
                let prefix = if pad { message([0, 5, 17][len % 3], len) } else { message(4, len) };
                let plain = message(len, len + 1);
                let sealed = portable_stream(&key, &nonce, counter + 1, &plain);
                let mut input = Vec::new();
                for part in [&prefix, &sealed] {
                    input.extend_from_slice(part);
                    if pad {
                        input.resize(input.len().next_multiple_of(16), 0);
                    }
                }
                let lengths = [(prefix.len() as u64).to_le_bytes(), (len as u64).to_le_bytes()].concat();
                if pad {
                    input.extend_from_slice(&lengths);
                }
                let tag = crate::poly1305::tag(legacy_block(&key, &nonce, counter).0[..32].try_into().unwrap(), &input);
                for seal in [true, false] {
                    let mut data = if seal { plain.clone() } else { sealed.clone() };
                    // CPUID and XCR0 admitted AVX2 and BMI2 above; the counter
                    // stays below 2^32 blocks.
                    let mut mac =
                        unsafe { super::avx2::chacha20_poly1305(&key, counter, &nonce, &prefix, &mut data, seal, pad) };
                    if pad {
                        mac.update_padded(&lengths);
                    }
                    let case = format!("{len} bytes, seal {seal}, pad {pad}");
                    assert_eq!(&data, if seal { &sealed } else { &plain }, "{case}");
                    assert_eq!(mac.finish(), tag, "{case}");
                }
            }
        }
    }
}
