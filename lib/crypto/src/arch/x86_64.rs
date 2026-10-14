//! Intel SDM AES/CLMUL fallback uses legacy SSE, gated by CPUID.
//! Hosted GCM additionally admits eight-block VAES/VPCLMUL batches.
//! Hosted ChaCha additionally admits AVX2 through CPUID and OS-enabled XCR0 state.
//! UEFI preserves every used register inside each assembly block. Ordinary
//! hosts use Rust intrinsics and the ABI. Unaligned loads admit byte-aligned keys.

#[cfg(target_os = "uefi")]
use core::arch::asm;
use core::arch::x86_64::*;

#[cfg(not(target_os = "uefi"))]
mod avx2;
#[cfg(not(target_os = "uefi"))]
mod vaes;

pub(super) fn chacha_xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    #[cfg(not(target_os = "uefi"))]
    {
        avx2::xor(key, counter, nonce, data)
    }
    #[cfg(target_os = "uefi")]
    {
        let _ = (key, counter, nonce, data);
        0
    }
}

#[allow(unused_unsafe, reason = "CPUID is unsafe on the supported Rust 1.89")]
fn capabilities() -> u32 {
    // CPUID leaf 1 exists on every x86-64 CPU.
    unsafe { __cpuid(1).ecx }
}
pub(super) fn aes_available() -> bool {
    !cfg!(feature = "portable") && capabilities() & (1 << 25) != 0
}
pub(super) fn clmul_available() -> bool {
    !cfg!(feature = "portable") && capabilities() & (1 << 1) != 0
}

#[cfg(target_os = "uefi")]
pub(super) fn ghash(a: u128, b: u128) -> u128 {
    let a = crate::secret::Secret(a.reverse_bits().to_le_bytes());
    let b = crate::secret::Secret(b.reverse_bits().to_le_bytes());
    let mut products = crate::secret::Secret([0; 48]);
    let mut saved = crate::secret::Secret([0; 80]);
    // CPUID admitted CLMUL. All five used registers are restored; each memory
    // operand refers to a distinct live array of the required size.
    unsafe {
        asm!(
            "movdqu [{saved}], xmm0","movdqu [{saved}+16], xmm1","movdqu [{saved}+32], xmm2","movdqu [{saved}+48], xmm3","movdqu [{saved}+64], xmm4",
            "movdqu xmm0, [{a}]","movdqu xmm1, [{b}]",
            "movdqa xmm2, xmm0","pclmulqdq xmm2, xmm1, 0x00","movdqu [{out}], xmm2",
            "movdqa xmm3, xmm0","pclmulqdq xmm3, xmm1, 0x11","movdqu [{out}+16], xmm3",
            "pshufd xmm4, xmm0, 0x4e","pxor xmm0, xmm4","pshufd xmm4, xmm1, 0x4e","pxor xmm1, xmm4",
            "pclmulqdq xmm0, xmm1, 0x00","pxor xmm0, xmm2","pxor xmm0, xmm3","movdqu [{out}+32], xmm0",
            "movdqu xmm0, [{saved}]","movdqu xmm1, [{saved}+16]","movdqu xmm2, [{saved}+32]","movdqu xmm3, [{saved}+48]","movdqu xmm4, [{saved}+64]",
            saved=in(reg) saved.0.as_mut_ptr(),a=in(reg) a.0.as_ptr(),b=in(reg) b.0.as_ptr(),out=in(reg) products.0.as_mut_ptr(),options(nostack),
        )
    }
    let low = u128::from_le_bytes(products.0[..16].try_into().unwrap());
    let high = u128::from_le_bytes(products.0[16..32].try_into().unwrap());
    let middle = u128::from_le_bytes(products.0[32..].try_into().unwrap());
    let low = low ^ (middle << 64);
    let high = high ^ (middle >> 64);
    let carry = (high >> 127) ^ (high >> 126) ^ (high >> 121);
    (low ^ high
        ^ (high << 1)
        ^ (high << 2)
        ^ (high << 7)
        ^ carry
        ^ (carry << 1)
        ^ (carry << 2)
        ^ (carry << 7))
        .reverse_bits()
}

#[cfg(target_os = "uefi")]
pub(super) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) {
    let mut saved = crate::secret::Secret([0u8; 32]);
    // CPUID admitted AES. The schedule contains every round, `block` and
    // `saved` are exclusive, and both XMM registers are restored before return.
    unsafe {
        asm!(
            "movdqu [{saved}], xmm0","movdqu [{saved}+16], xmm1",
            "movdqu xmm0, [{block}]","movdqu xmm1, [{keys}]","pxor xmm0, xmm1",
            "add {keys}, 16","2:","movdqu xmm1, [{keys}]","aesenc xmm0, xmm1",
            "add {keys}, 16","dec {left}","jnz 2b",
            "movdqu xmm1, [{keys}]","aesenclast xmm0, xmm1","movdqu [{block}], xmm0",
            "movdqu xmm0, [{saved}]","movdqu xmm1, [{saved}+16]",
            saved=in(reg) saved.0.as_mut_ptr(),block=in(reg) block.as_mut_ptr(),
            keys=inout(reg) keys.as_ptr()=>_,left=inout(reg) rounds-1=>_,options(nostack),
        );
    }
}

#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "aes,sse2")]
unsafe fn encrypt_native(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) {
    // Every unaligned access stays within a schedule or block array.
    unsafe {
        let mut v = _mm_xor_si128(
            _mm_loadu_si128(block.as_ptr().cast()),
            _mm_loadu_si128(keys.as_ptr().cast()),
        );
        for i in 1..rounds {
            v = _mm_aesenc_si128(v, _mm_loadu_si128(keys.as_ptr().add(i * 16).cast()))
        }
        v = _mm_aesenclast_si128(v, _mm_loadu_si128(keys.as_ptr().add(rounds * 16).cast()));
        _mm_storeu_si128(block.as_mut_ptr().cast(), v)
    }
}
#[cfg(not(target_os = "uefi"))]
pub(super) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) {
    // CPUID admitted AES; the ordinary ABI owns SIMD register preservation.
    unsafe { encrypt_native(keys, rounds, block) }
}
#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "pclmulqdq,sse2")]
unsafe fn product_native(a: u128, b: u128) -> (u128, u128) {
    let a = _mm_set_epi64x((a >> 64) as i64, a as i64);
    let b = _mm_set_epi64x((b >> 64) as i64, b as i64);
    let lo = _mm_clmulepi64_si128::<0>(a, b);
    let hi = _mm_clmulepi64_si128::<0x11>(a, b);
    let ax = _mm_xor_si128(a, _mm_shuffle_epi32::<0x4e>(a));
    let bx = _mm_xor_si128(b, _mm_shuffle_epi32::<0x4e>(b));
    let middle = _mm_xor_si128(_mm_xor_si128(_mm_clmulepi64_si128::<0>(ax, bx), lo), hi);
    let lo = u128::from(_mm_cvtsi128_si64(lo) as u64)
        | (u128::from(_mm_cvtsi128_si64(_mm_unpackhi_epi64(lo, lo)) as u64) << 64);
    let hi = u128::from(_mm_cvtsi128_si64(hi) as u64)
        | (u128::from(_mm_cvtsi128_si64(_mm_unpackhi_epi64(hi, hi)) as u64) << 64);
    let mid = u128::from(_mm_cvtsi128_si64(middle) as u64)
        | (u128::from(_mm_cvtsi128_si64(_mm_unpackhi_epi64(middle, middle)) as u64) << 64);
    (lo ^ (mid << 64), hi ^ (mid >> 64))
}
#[cfg(not(target_os = "uefi"))]
pub(super) fn ghash(a: u128, b: u128) -> u128 {
    // CPUID admitted CLMUL; reflection maps GCM's bit order to polynomial order.
    let (low, high) = unsafe { product_native(a.reverse_bits(), b.reverse_bits()) };
    let carry = (high >> 127) ^ (high >> 126) ^ (high >> 121);
    (low ^ high
        ^ (high << 1)
        ^ (high << 2)
        ^ (high << 7)
        ^ carry
        ^ (carry << 1)
        ^ (carry << 2)
        ^ (carry << 7))
        .reverse_bits()
}

#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "aes,sse2")]
#[inline(never)]
unsafe fn subword(word: &mut [u8; 4]) {
    // Identical columns make ShiftRows the identity; the zero key isolates SubBytes.
    let columns = _mm_set1_epi32(u32::from_le_bytes(*word) as i32);
    let out = _mm_aesenclast_si128(columns, _mm_setzero_si128());
    *word = (_mm_cvtsi128_si32(out) as u32).to_le_bytes();
}
pub(super) fn aes_subword(word: &mut [u8; 4]) -> bool {
    #[cfg(not(target_os = "uefi"))]
    {
        // Dispatch admitted AES and the ordinary ABI owns SIMD state.
        unsafe { subword(word) }
        true
    }
    #[cfg(target_os = "uefi")]
    {
        let _ = word;
        false
    }
}

#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "aes,sse2")]
#[inline(never)]
unsafe fn ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], data: &mut [u8]) {
    // GCM checked len <= 2^36-32; four counters per batch cannot exceed u32::MAX.
    unsafe {
        let mut iv = [0; 16];
        iv[..12].copy_from_slice(nonce);
        let base = _mm_loadu_si128(iv.as_ptr().cast());
        let first = _mm_loadu_si128(keys.as_ptr().cast());
        let (batches, tail) = data.as_chunks_mut::<64>();
        for (i, batch) in batches.iter_mut().enumerate() {
            let counter = i as u32 * 4 + 2;
            let mut a = _mm_xor_si128(base, _mm_set_epi32(counter.to_be() as i32, 0, 0, 0));
            let mut b = _mm_xor_si128(base, _mm_set_epi32((counter + 1).to_be() as i32, 0, 0, 0));
            let mut c = _mm_xor_si128(base, _mm_set_epi32((counter + 2).to_be() as i32, 0, 0, 0));
            let mut d = _mm_xor_si128(base, _mm_set_epi32((counter + 3).to_be() as i32, 0, 0, 0));
            a = _mm_xor_si128(a, first);
            b = _mm_xor_si128(b, first);
            c = _mm_xor_si128(c, first);
            d = _mm_xor_si128(d, first);
            for round in 1..rounds {
                let key = _mm_loadu_si128(keys.as_ptr().add(16 * round).cast());
                a = _mm_aesenc_si128(a, key);
                b = _mm_aesenc_si128(b, key);
                c = _mm_aesenc_si128(c, key);
                d = _mm_aesenc_si128(d, key);
            }
            let last = _mm_loadu_si128(keys.as_ptr().add(16 * rounds).cast());
            a = _mm_aesenclast_si128(a, last);
            b = _mm_aesenclast_si128(b, last);
            c = _mm_aesenclast_si128(c, last);
            d = _mm_aesenclast_si128(d, last);
            // All four unaligned load/stores are contained in this complete batch.
            let out = batch.as_mut_ptr();
            _mm_storeu_si128(out.cast(), _mm_xor_si128(a, _mm_loadu_si128(out.cast())));
            _mm_storeu_si128(
                out.add(16).cast(),
                _mm_xor_si128(b, _mm_loadu_si128(out.add(16).cast())),
            );
            _mm_storeu_si128(
                out.add(32).cast(),
                _mm_xor_si128(c, _mm_loadu_si128(out.add(32).cast())),
            );
            _mm_storeu_si128(
                out.add(48).cast(),
                _mm_xor_si128(d, _mm_loadu_si128(out.add(48).cast())),
            );
        }
        let mut stream = crate::secret::Secret([0; 16]);
        for (i, chunk) in tail.chunks_mut(16).enumerate() {
            stream.0[..12].copy_from_slice(nonce);
            stream.0[12..].copy_from_slice(&((4 * batches.len() + i) as u32 + 2).to_be_bytes());
            encrypt_native(keys, rounds, &mut stream.0);
            for (byte, &mask) in chunk.iter_mut().zip(&stream.0) {
                *byte ^= mask;
            }
        }
    }
}
pub(super) fn aes_ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], data: &mut [u8]) -> bool {
    #[cfg(not(target_os = "uefi"))]
    {
        // Extended-state admission is additional to the caller's AES/counter checks.
        if data.len() >= 128 && vaes::available() {
            unsafe { vaes::ctr(keys, rounds, nonce, data) }
            return true;
        }
        // Dispatch admitted AES; GCM validated the counter bound before mutation.
        unsafe { ctr(keys, rounds, nonce, data) }
        true
    }
    #[cfg(target_os = "uefi")]
    {
        let _ = (keys, rounds, nonce, data);
        false
    }
}

#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "sse2")]
fn reflect_bytes(x: __m128i) -> __m128i {
    // Reflect within each byte: masks discard inter-byte carries of 16-bit shifts.
    let m1 = _mm_set1_epi8(0x55);
    let m2 = _mm_set1_epi8(0x33);
    let m4 = _mm_set1_epi8(0x0f);
    let x = _mm_or_si128(
        _mm_and_si128(_mm_srli_epi16::<1>(x), m1),
        _mm_slli_epi16::<1>(_mm_and_si128(x, m1)),
    );
    let x = _mm_or_si128(
        _mm_and_si128(_mm_srli_epi16::<2>(x), m2),
        _mm_slli_epi16::<2>(_mm_and_si128(x, m2)),
    );
    _mm_or_si128(
        _mm_and_si128(_mm_srli_epi16::<4>(x), m4),
        _mm_slli_epi16::<4>(_mm_and_si128(x, m4)),
    )
}
#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "pclmulqdq,sse2")]
fn product_vectors(a: __m128i, b: __m128i) -> (__m128i, __m128i) {
    let low = _mm_clmulepi64_si128::<0>(a, b);
    let high = _mm_clmulepi64_si128::<0x11>(a, b);
    let ax = _mm_xor_si128(a, _mm_shuffle_epi32::<0x4e>(a));
    let bx = _mm_xor_si128(b, _mm_shuffle_epi32::<0x4e>(b));
    let middle = _mm_xor_si128(_mm_xor_si128(_mm_clmulepi64_si128::<0>(ax, bx), low), high);
    (
        _mm_xor_si128(low, _mm_slli_si128::<8>(middle)),
        _mm_xor_si128(high, _mm_srli_si128::<8>(middle)),
    )
}
#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "sse2")]
fn reduce_vectors(low: __m128i, high: __m128i) -> __m128i {
    let shifted1 = _mm_xor_si128(
        _mm_slli_epi64::<1>(high),
        _mm_slli_si128::<8>(_mm_srli_epi64::<63>(high)),
    );
    let shifted2 = _mm_xor_si128(
        _mm_slli_epi64::<2>(high),
        _mm_slli_si128::<8>(_mm_srli_epi64::<62>(high)),
    );
    let shifted7 = _mm_xor_si128(
        _mm_slli_epi64::<7>(high),
        _mm_slli_si128::<8>(_mm_srli_epi64::<57>(high)),
    );
    let carry = _mm_srli_si128::<8>(_mm_xor_si128(
        _mm_xor_si128(_mm_srli_epi64::<63>(high), _mm_srli_epi64::<62>(high)),
        _mm_srli_epi64::<57>(high),
    ));
    // x^128 = x^7+x^2+x+1; first-fold overflow occupies at most seven low bits.
    let folded = _mm_xor_si128(
        _mm_xor_si128(low, high),
        _mm_xor_si128(shifted1, _mm_xor_si128(shifted2, shifted7)),
    );
    _mm_xor_si128(
        folded,
        _mm_xor_si128(
            carry,
            _mm_xor_si128(
                _mm_slli_epi64::<1>(carry),
                _mm_xor_si128(_mm_slli_epi64::<2>(carry), _mm_slli_epi64::<7>(carry)),
            ),
        ),
    )
}
#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "pclmulqdq,sse2")]
fn multiply_vectors(a: __m128i, b: __m128i) -> __m128i {
    let (low, high) = product_vectors(a, b);
    reduce_vectors(low, high)
}
#[cfg(not(target_os = "uefi"))]
#[target_feature(enable = "pclmulqdq,sse2")]
#[inline(never)]
unsafe fn hash_blocks(h: &[u8; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    // CLMUL is admitted; each unaligned access stays within its fixed array.
    unsafe {
        let h = reflect_bytes(_mm_loadu_si128(h.as_ptr().cast()));
        let mut y = reflect_bytes(_mm_loadu_si128(state.as_ptr().cast()));
        let (batches, tail) = blocks.as_chunks::<4>();
        if !batches.is_empty() {
            let h2 = multiply_vectors(h, h);
            let h3 = multiply_vectors(h2, h);
            let h4 = multiply_vectors(h2, h2);
            for batch in batches {
                let x0 = reflect_bytes(_mm_loadu_si128(batch[0].as_ptr().cast()));
                let x1 = reflect_bytes(_mm_loadu_si128(batch[1].as_ptr().cast()));
                let x2 = reflect_bytes(_mm_loadu_si128(batch[2].as_ptr().cast()));
                let x3 = reflect_bytes(_mm_loadu_si128(batch[3].as_ptr().cast()));
                // Four steps expand to (y+x0)H^4+x1H^3+x2H^2+x3H.
                let (l0, u0) = product_vectors(_mm_xor_si128(y, x0), h4);
                let (l1, u1) = product_vectors(x1, h3);
                let (l2, u2) = product_vectors(x2, h2);
                let (l3, u3) = product_vectors(x3, h);
                y = reduce_vectors(
                    _mm_xor_si128(_mm_xor_si128(l0, l1), _mm_xor_si128(l2, l3)),
                    _mm_xor_si128(_mm_xor_si128(u0, u1), _mm_xor_si128(u2, u3)),
                );
            }
        }
        for block in tail {
            let x = reflect_bytes(_mm_loadu_si128(block.as_ptr().cast()));
            y = multiply_vectors(_mm_xor_si128(y, x), h);
        }
        _mm_storeu_si128(state.as_mut_ptr().cast(), reflect_bytes(y));
    }
}
pub(super) fn ghash_blocks(h: &[u8; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    #[cfg(not(target_os = "uefi"))]
    {
        if blocks.len() >= 8 && vaes::available() {
            // VAES admission also establishes VPCLMUL and OS-owned YMM state.
            unsafe { vaes::hash_blocks(h, state, blocks) }
            return;
        }
        // Dispatch admitted CLMUL and the ordinary ABI preserves SIMD state.
        unsafe { hash_blocks(h, state, blocks) }
    }
    #[cfg(target_os = "uefi")]
    for block in blocks {
        *state = ghash(
            u128::from_be_bytes(*state) ^ u128::from_be_bytes(*block),
            u128::from_be_bytes(*h),
        )
        .to_be_bytes();
    }
}
