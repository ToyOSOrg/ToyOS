//! Eight-block GCM batches using 256-bit VAES and VPCLMULQDQ.
//! Each 128-bit lane remains an independent AES block / polynomial product.
//! Only hosted targets with OS-enabled XMM/YMM state enter these functions.
use core::arch::x86_64::*;
use core::sync::atomic::{AtomicU8, Ordering};

const AES_CLMUL: u32 = (1 << 25) | (1 << 1);
const VAES_VPCLMUL: u32 = (1 << 9) | (1 << 10);

fn features_enabled(leaf1: u32, leaf7: u32) -> bool {
    leaf1 & AES_CLMUL == AES_CLMUL && leaf7 & VAES_VPCLMUL == VAES_VPCLMUL
}

#[allow(unused_unsafe, reason = "CPUID is unsafe on the supported Rust 1.89")]
pub(super) fn available() -> bool {
    // Reuse the guarded XGETBV admission and process-lifetime OS-state contract.
    if !super::avx2::available() {
        return false;
    }
    static ADMITTED: AtomicU8 = AtomicU8::new(0);
    match ADMITTED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            // AVX2 admission established that CPUID leaf 7 exists.
            let yes = features_enabled(super::capabilities(), unsafe { __cpuid_count(7, 0).ecx });
            ADMITTED.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn counter_pair(base: __m256i, first: u32) -> __m256i {
    _mm256_xor_si256(
        base,
        _mm256_set_epi32(
            first.wrapping_add(1).to_be() as i32,
            0,
            0,
            0,
            first.to_be() as i32,
            0,
            0,
            0,
        ),
    )
}

#[target_feature(enable = "avx2,vaes,aes")]
#[inline(never)]
pub(super) unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], data: &mut [u8]) {
    // Admission is checked by the caller; GCM checked len <= 2^36-32.
    // Complete batches therefore end at or before counter u32::MAX.
    unsafe {
        let mut iv = [0; 16];
        iv[..12].copy_from_slice(nonce);
        let base = _mm256_broadcastsi128_si256(_mm_loadu_si128(iv.as_ptr().cast()));
        let initial = _mm256_broadcastsi128_si256(_mm_loadu_si128(keys.as_ptr().cast()));
        let (batches, tail) = data.as_chunks_mut::<128>();
        for (i, batch) in batches.iter_mut().enumerate() {
            let first = i as u32 * 8 + 2;
            let mut a = _mm256_xor_si256(counter_pair(base, first), initial);
            let mut b = _mm256_xor_si256(counter_pair(base, first + 2), initial);
            let mut c = _mm256_xor_si256(counter_pair(base, first + 4), initial);
            let mut d = _mm256_xor_si256(counter_pair(base, first + 6), initial);
            for round in 1..rounds {
                let key = _mm256_broadcastsi128_si256(_mm_loadu_si128(
                    keys.as_ptr().add(round * 16).cast(),
                ));
                a = _mm256_aesenc_epi128(a, key);
                b = _mm256_aesenc_epi128(b, key);
                c = _mm256_aesenc_epi128(c, key);
                d = _mm256_aesenc_epi128(d, key);
            }
            let key =
                _mm256_broadcastsi128_si256(_mm_loadu_si128(keys.as_ptr().add(rounds * 16).cast()));
            a = _mm256_aesenclast_epi128(a, key);
            b = _mm256_aesenclast_epi128(b, key);
            c = _mm256_aesenclast_epi128(c, key);
            d = _mm256_aesenclast_epi128(d, key);
            // Four disjoint unaligned 32-byte accesses inside a full 128-byte batch.
            for (i, stream) in [a, b, c, d].into_iter().enumerate() {
                let p = batch.as_mut_ptr().add(i * 32);
                _mm256_storeu_si256(
                    p.cast(),
                    _mm256_xor_si256(stream, _mm256_loadu_si256(p.cast())),
                );
            }
        }
        let mut stream = crate::secret::Secret([0; 16]);
        for (i, chunk) in tail.chunks_mut(16).enumerate() {
            stream.0[..12].copy_from_slice(nonce);
            stream.0[12..].copy_from_slice(&((batches.len() * 8 + i) as u32 + 2).to_be_bytes());
            super::encrypt_native(keys, rounds, &mut stream.0);
            for (byte, &mask) in chunk.iter_mut().zip(&stream.0) {
                *byte ^= mask;
            }
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn reflect(x: __m256i) -> __m256i {
    // PSHUFB indexes a fixed register, never secret-indexed memory.
    let table = _mm256_setr_epi8(
        0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15, 0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5,
        13, 3, 11, 7, 15,
    );
    let mask = _mm256_set1_epi8(15);
    let low = _mm256_shuffle_epi8(table, _mm256_and_si256(x, mask));
    let high = _mm256_shuffle_epi8(table, _mm256_and_si256(_mm256_srli_epi16::<4>(x), mask));
    _mm256_or_si256(_mm256_slli_epi16::<4>(low), high)
}

#[inline]
#[target_feature(enable = "avx2,vpclmulqdq")]
fn product(a: __m256i, b: __m256i) -> (__m256i, __m256i) {
    let low = _mm256_clmulepi64_epi128::<0>(a, b);
    let high = _mm256_clmulepi64_epi128::<0x11>(a, b);
    let ax = _mm256_xor_si256(a, _mm256_shuffle_epi32::<0x4e>(a));
    let bx = _mm256_xor_si256(b, _mm256_shuffle_epi32::<0x4e>(b));
    let middle = _mm256_xor_si256(
        _mm256_clmulepi64_epi128::<0>(ax, bx),
        _mm256_xor_si256(low, high),
    );
    (
        _mm256_xor_si256(low, _mm256_slli_si256::<8>(middle)),
        _mm256_xor_si256(high, _mm256_srli_si256::<8>(middle)),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
fn pair(low: __m128i, high: __m128i) -> __m256i {
    _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(low), high)
}

#[target_feature(enable = "avx2,vpclmulqdq,pclmulqdq")]
#[inline(never)]
pub(super) unsafe fn hash_blocks(h: &[u8; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    // Admission is checked by the caller. Each input pair is inside a full batch.
    unsafe {
        let h1 = super::reflect_bytes(_mm_loadu_si128(h.as_ptr().cast()));
        let mut y = super::reflect_bytes(_mm_loadu_si128(state.as_ptr().cast()));
        let (batches, tail) = blocks.as_chunks::<8>();
        if !batches.is_empty() {
            let h2 = super::multiply_vectors(h1, h1);
            let h3 = super::multiply_vectors(h2, h1);
            let h4 = super::multiply_vectors(h2, h2);
            let h5 = super::multiply_vectors(h4, h1);
            let h6 = super::multiply_vectors(h3, h3);
            let h7 = super::multiply_vectors(h4, h3);
            let h8 = super::multiply_vectors(h4, h4);
            let p0 = pair(h8, h7);
            let p1 = pair(h6, h5);
            let p2 = pair(h4, h3);
            let p3 = pair(h2, h1);
            for batch in batches {
                let p = batch.as_ptr().cast::<u8>();
                let a = _mm256_xor_si256(
                    reflect(_mm256_loadu_si256(p.cast())),
                    pair(y, _mm_setzero_si128()),
                );
                let b = reflect(_mm256_loadu_si256(p.add(32).cast()));
                let c = reflect(_mm256_loadu_si256(p.add(64).cast()));
                let d = reflect(_mm256_loadu_si256(p.add(96).cast()));
                // (y+x0)H^8 + x1H^7 + ... + x7H; reduce once after summation.
                let (l0, u0) = product(a, p0);
                let (l1, u1) = product(b, p1);
                let (l2, u2) = product(c, p2);
                let (l3, u3) = product(d, p3);
                let low = _mm256_xor_si256(_mm256_xor_si256(l0, l1), _mm256_xor_si256(l2, l3));
                let high = _mm256_xor_si256(_mm256_xor_si256(u0, u1), _mm256_xor_si256(u2, u3));
                y = super::reduce_vectors(
                    _mm_xor_si128(
                        _mm256_castsi256_si128(low),
                        _mm256_extracti128_si256::<1>(low),
                    ),
                    _mm_xor_si128(
                        _mm256_castsi256_si128(high),
                        _mm256_extracti128_si256::<1>(high),
                    ),
                );
            }
        }
        for block in tail {
            let x = super::reflect_bytes(_mm_loadu_si128(block.as_ptr().cast()));
            y = super::multiply_vectors(_mm_xor_si128(y, x), h1);
        }
        _mm_storeu_si128(state.as_mut_ptr().cast(), super::reflect_bytes(y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_requires_both_vector_and_legacy_feature_masks() {
        assert!(features_enabled(AES_CLMUL, VAES_VPCLMUL));
        for bit in [1, 25] {
            assert!(!features_enabled(AES_CLMUL & !(1 << bit), VAES_VPCLMUL));
        }
        for bit in [9, 10] {
            assert!(!features_enabled(AES_CLMUL, VAES_VPCLMUL & !(1 << bit)));
        }
    }
    #[test]
    fn admission_matches_host_detection_and_portable_override() {
        assert_eq!(
            available(),
            !cfg!(feature = "portable")
                && std::is_x86_feature_detected!("avx2")
                && std::is_x86_feature_detected!("aes")
                && std::is_x86_feature_detected!("pclmulqdq")
                && std::is_x86_feature_detected!("vaes")
                && std::is_x86_feature_detected!("vpclmulqdq")
        );
        std::println!("VAES/VPCLMUL admission: {}", available());
    }
    #[test]
    fn ctr_matches_legacy_at_unaligned_batch_and_tail_boundaries() {
        if !available() {
            return;
        }
        // Arbitrary schedules exercise both round counts without sharing key expansion.
        let keys = core::array::from_fn(|i| (i * 73 + 19) as u8);
        let nonce = [0xff; 12];
        for rounds in [10, 14] {
            for n in [
                0usize, 1, 15, 16, 17, 63, 64, 65, 127, 128, 129, 255, 256, 257, 383, 384, 385,
                1024, 16384,
            ] {
                let mut a = std::vec![0x53;n+2];
                let mut b = a.clone();
                unsafe {
                    ctr(&keys, rounds, &nonce, &mut a[1..n + 1]);
                    super::super::ctr(&keys, rounds, &nonce, &mut b[1..n + 1]);
                }
                assert_eq!(a, b, "CTR len={n}, rounds={rounds}");
            }
        }
    }
    #[test]
    fn counter_pair_serializes_nonce_and_high_counters_in_lane_order() {
        if !available() {
            return;
        }
        let nonce = [0xffu8; 12];
        // Helper serialization only: the final message counters use the scalar tail.
        let mut pair_bytes = [0u8; 32];
        unsafe {
            let mut iv = [0xffu8; 16];
            iv[12..].fill(0);
            let base = _mm256_broadcastsi128_si256(_mm_loadu_si128(iv.as_ptr().cast()));
            _mm256_storeu_si256(
                pair_bytes.as_mut_ptr().cast(),
                counter_pair(base, u32::MAX - 1),
            );
        }
        assert_eq!(&pair_bytes[..12], &nonce);
        assert_eq!(&pair_bytes[16..28], &nonce);
        assert_eq!(&pair_bytes[12..16], &(u32::MAX - 1).to_be_bytes());
        assert_eq!(&pair_bytes[28..], &u32::MAX.to_be_bytes());
    }
    #[test]
    fn ghash_batch_lanes_match_legacy_for_every_basis_bit() {
        if !available() {
            return;
        }
        let h = core::array::from_fn(|i| (i * 29 + 7) as u8);
        // Every bit in every batch lane must have the correct power and reflection.
        for lane in 0..8 {
            for bit in 0..128 {
                let mut basis = [[0; 16]; 8];
                basis[lane][bit / 8] = 1 << (bit % 8);
                let mut a = [0; 16];
                let mut b = a;
                unsafe {
                    hash_blocks(&h, &mut a, &basis);
                    super::super::hash_blocks(&h, &mut b, &basis);
                }
                assert_eq!(a, b, "lane={lane}, bit={bit}");
            }
        }
    }
    #[test]
    fn ghash_matches_legacy_for_initial_state_and_partial_batches() {
        if !available() {
            return;
        }
        let h = core::array::from_fn(|i| (i * 29 + 7) as u8);
        let blocks: [[u8; 16]; 33] =
            core::array::from_fn(|i| core::array::from_fn(|j| (i * 71 + j * 43) as u8));
        for n in 0..=33 {
            let mut a = [0x91; 16];
            let mut b = a;
            unsafe {
                hash_blocks(&h, &mut a, &blocks[..n]);
                super::super::hash_blocks(&h, &mut b, &blocks[..n]);
            }
            assert_eq!(a, b, "GHASH blocks={n}");
        }
    }
}
