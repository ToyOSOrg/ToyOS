//! Four RFC 8439 blocks per batch, with two blocks in each pair of 128-bit lanes.
//! CPUID and XCR0 admit AVX2 only on ordinary hosts; UEFI never enters this module.
//! Rounds, shuffles and memory offsets depend only on public lengths and counters.

use core::arch::x86_64::*;
use core::sync::atomic::{AtomicU8, Ordering};

const XSAVE_AVX: u32 = (1 << 26) | (1 << 27) | (1 << 28);

fn state_enabled(ecx: u32, xcr0: u64) -> bool {
    ecx & XSAVE_AVX == XSAVE_AVX && xcr0 & 6 == 6
}

pub(super) fn available() -> bool {
    if cfg!(feature = "portable") {
        return false;
    }
    // Feature and OS-state permission remain uniform for this process lifetime.
    // The cache publishes only this public boolean, not any other memory.
    static ADMITTED: AtomicU8 = AtomicU8::new(0);
    match ADMITTED.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let yes = detect();
    ADMITTED.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
    yes
}

#[allow(unused_unsafe, reason = "CPUID is unsafe on the supported Rust 1.89")]
fn detect() -> bool {
    let ecx = super::capabilities();
    if ecx & XSAVE_AVX != XSAVE_AVX {
        return false;
    }
    // OSXSAVE admits XGETBV; both XMM and YMM state must be enabled before AVX.
    unsafe {
        state_enabled(ecx, _xgetbv(0))
            && __cpuid(0).eax >= 7
            && __cpuid_count(7, 0).ebx & (1 << 5) != 0
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn quarter(a: &mut __m256i, b: &mut __m256i, c: &mut __m256i, d: &mut __m256i) {
    let rot16 = _mm256_setr_epi8(
        2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9,
        14, 15, 12, 13,
    );
    let rot8 = _mm256_setr_epi8(
        3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14, 3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10,
        15, 12, 13, 14,
    );
    *a = _mm256_add_epi32(*a, *b);
    *d = _mm256_shuffle_epi8(_mm256_xor_si256(*d, *a), rot16);
    *c = _mm256_add_epi32(*c, *d);
    *b = _mm256_xor_si256(*b, *c);
    *b = _mm256_or_si256(_mm256_slli_epi32::<12>(*b), _mm256_srli_epi32::<20>(*b));
    *a = _mm256_add_epi32(*a, *b);
    *d = _mm256_shuffle_epi8(_mm256_xor_si256(*d, *a), rot8);
    *c = _mm256_add_epi32(*c, *d);
    *b = _mm256_xor_si256(*b, *c);
    *b = _mm256_or_si256(_mm256_slli_epi32::<7>(*b), _mm256_srli_epi32::<25>(*b));
}

#[inline]
#[target_feature(enable = "avx2")]
fn counters(counter: u64, nonce: i64) -> __m256i {
    _mm256_set_epi64x(nonce, counter.wrapping_add(1) as i64, nonce, counter as i64)
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn store_pair(out: *mut u8, rows: [__m256i; 4]) {
    // The caller provides 128 exclusive bytes; each half-lane is one row.
    unsafe {
        for (i, row) in rows.into_iter().enumerate() {
            let low = out.add(i * 16);
            let high = low.add(64);
            _mm_storeu_si128(
                low.cast(),
                _mm_xor_si128(_mm_loadu_si128(low.cast()), _mm256_castsi256_si128(row)),
            );
            _mm_storeu_si128(
                high.cast(),
                _mm_xor_si128(
                    _mm_loadu_si128(high.cast()),
                    _mm256_extracti128_si256::<1>(row),
                ),
            );
        }
    }
}

#[target_feature(enable = "avx2")]
#[inline(never)]
unsafe fn batches(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    // The caller bounded counter + block count before any mutation. Every load
    // is within the key; store_pair receives halves of a complete 256-byte batch.
    unsafe {
        let initial_a = _mm256_broadcastsi128_si256(_mm_setr_epi32(
            0x61707865, 0x3320646e, 0x79622d32, 0x6b206574,
        ));
        let initial_b = _mm256_broadcastsi128_si256(_mm_loadu_si128(key.as_ptr().cast()));
        let initial_c = _mm256_broadcastsi128_si256(_mm_loadu_si128(key.as_ptr().add(16).cast()));
        let nonce = i64::from_le_bytes(*nonce);
        let (batches, _) = data.as_chunks_mut::<256>();
        for (i, batch) in batches.iter_mut().enumerate() {
            let first = counter + i as u64 * 4;
            let initial_d0 = counters(first, nonce);
            let initial_d1 = counters(first + 2, nonce);
            let (mut a0, mut b0, mut c0, mut d0) = (initial_a, initial_b, initial_c, initial_d0);
            let (mut a1, mut b1, mut c1, mut d1) = (initial_a, initial_b, initial_c, initial_d1);
            for _ in 0..10 {
                quarter(&mut a0, &mut b0, &mut c0, &mut d0);
                quarter(&mut a1, &mut b1, &mut c1, &mut d1);
                b0 = _mm256_shuffle_epi32::<0x39>(b0);
                b1 = _mm256_shuffle_epi32::<0x39>(b1);
                c0 = _mm256_shuffle_epi32::<0x4e>(c0);
                c1 = _mm256_shuffle_epi32::<0x4e>(c1);
                d0 = _mm256_shuffle_epi32::<0x93>(d0);
                d1 = _mm256_shuffle_epi32::<0x93>(d1);
                quarter(&mut a0, &mut b0, &mut c0, &mut d0);
                quarter(&mut a1, &mut b1, &mut c1, &mut d1);
                b0 = _mm256_shuffle_epi32::<0x93>(b0);
                b1 = _mm256_shuffle_epi32::<0x93>(b1);
                c0 = _mm256_shuffle_epi32::<0x4e>(c0);
                c1 = _mm256_shuffle_epi32::<0x4e>(c1);
                d0 = _mm256_shuffle_epi32::<0x39>(d0);
                d1 = _mm256_shuffle_epi32::<0x39>(d1);
            }
            store_pair(
                batch.as_mut_ptr(),
                [
                    _mm256_add_epi32(a0, initial_a),
                    _mm256_add_epi32(b0, initial_b),
                    _mm256_add_epi32(c0, initial_c),
                    _mm256_add_epi32(d0, initial_d0),
                ],
            );
            store_pair(
                batch.as_mut_ptr().add(128),
                [
                    _mm256_add_epi32(a1, initial_a),
                    _mm256_add_epi32(b1, initial_b),
                    _mm256_add_epi32(c1, initial_c),
                    _mm256_add_epi32(d1, initial_d1),
                ],
            );
        }
        batches.len() * 256
    }
}

pub(super) fn xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    if data.len() < 256 || !available() {
        return 0;
    }
    // CPUID and OS-enabled XMM/YMM state admit AVX2; the caller checked counters.
    unsafe { batches(key, counter, nonce, data) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avx_requires_cpu_support_and_both_os_state_components() {
        assert!(state_enabled(XSAVE_AVX, 6));
        for bit in [26, 27, 28] {
            assert!(!state_enabled(XSAVE_AVX & !(1 << bit), 6));
        }
        for xcr0 in [0, 2, 4] {
            assert!(!state_enabled(XSAVE_AVX, xcr0));
        }
    }

    #[test]
    fn admission_matches_host_detection_and_portable_override() {
        assert_eq!(
            available(),
            !cfg!(feature = "portable") && std::is_x86_feature_detected!("avx2")
        );
    }
}
