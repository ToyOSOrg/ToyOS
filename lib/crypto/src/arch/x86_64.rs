//! x86-64 instruction paths, admitted by CPUID and, for AVX state, XCR0.
//! UEFI takes AES, CLMUL and SHA, whose assembly blocks preserve every vector
//! register they use, and the general-register 2^255 - 19 kernels. Ordinary
//! hosts use Rust intrinsics and the ABI.
//! Unaligned loads admit byte-aligned keys and buffers.

#[cfg(target_os = "uefi")]
use core::arch::asm;
use core::arch::x86_64::*;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(not(target_os = "uefi"))]
use super::Powers;

#[cfg(not(target_os = "uefi"))]
mod aesni;
#[cfg(not(target_os = "uefi"))]
mod chacha;
mod f25519;
#[cfg(not(target_os = "uefi"))]
mod montgomery;
#[cfg(not(target_os = "uefi"))]
mod p256;
#[cfg(not(target_os = "uefi"))]
mod poly1305;
#[cfg(not(target_os = "uefi"))]
mod sha;
#[cfg(not(target_os = "uefi"))]
mod vaes512;
#[cfg(any(target_os = "uefi", test))]
mod sha_saving;

/// What CPUID and XCR0 admit, read once per process. The module contract
/// makes features uniform across the CPUs a process runs on.
#[derive(Clone, Copy)]
pub(super) struct Features(u32);

impl Features {
    const READ: u32 = 1 << 31;
    const AES: u32 = 1 << 0;
    const CLMUL: u32 = 1 << 1;
    const SSSE3: u32 = 1 << 2;
    const SSE41: u32 = 1 << 3;
    const AVX2: u32 = 1 << 4;
    const BMI2: u32 = 1 << 5;
    const SHA: u32 = 1 << 6;
    const VAES: u32 = 1 << 7;
    const VPCLMUL: u32 = 1 << 8;
    const AVX512VL: u32 = 1 << 9;
    const AVX512BW: u32 = 1 << 10;
    const ADX: u32 = 1 << 11;

    fn has(self, bits: u32) -> bool {
        self.0 & bits == bits
    }
    /// AES-NI and PCLMULQDQ, with SSSE3's byte shuffle.
    #[cfg(not(target_os = "uefi"))]
    pub(super) fn aes_clmul(self) -> bool {
        self.has(Self::AES | Self::CLMUL | Self::SSSE3)
    }
    pub(super) fn aes(self) -> bool {
        self.has(Self::AES)
    }
    #[cfg(any(test, target_os = "uefi"))]
    pub(super) fn clmul(self) -> bool {
        self.has(Self::CLMUL)
    }
    #[cfg(not(target_os = "uefi"))]
    pub(super) fn avx2(self) -> bool {
        self.has(Self::AVX2)
    }
    #[cfg(not(target_os = "uefi"))]
    pub(super) fn bmi2(self) -> bool {
        self.has(Self::BMI2)
    }
    /// MULX and ADCX/ADOX.
    pub(super) fn bmi2_adx(self) -> bool {
        self.has(Self::BMI2 | Self::ADX)
    }
    /// SHA-NI with the SSSE3 and SSE4.1 shuffles its message schedule takes.
    pub(super) fn sha(self) -> bool {
        self.has(Self::SHA | Self::SSSE3 | Self::SSE41)
    }
    /// AVX2 and AVX-512F and VL under OS-enabled ZMM state.
    #[cfg(not(target_os = "uefi"))]
    pub(super) fn avx512vl(self) -> bool {
        self.has(Self::AVX2 | Self::AVX512VL)
    }
    /// 512-bit VAES and VPCLMULQDQ, with AVX-512F, BW and VL under
    /// OS-enabled ZMM state. GCM takes no 256-bit VAES path: no bench has
    /// VAES without AVX-512 (the track issue's exit for it).
    #[cfg(not(target_os = "uefi"))]
    pub(super) fn vaes512(self) -> bool {
        self.has(
            Self::AES | Self::CLMUL | Self::SSSE3 | Self::AVX2 | Self::VAES | Self::VPCLMUL | Self::AVX512VL | Self::AVX512BW,
        )
    }

    /// Intel SDM Vol. 2A, CPUID: leaf 1 ECX bits 1 (PCLMULQDQ), 9 (SSSE3),
    /// 19 (SSE4.1), 25 (AES), 27 (OSXSAVE) and 28 (AVX); leaf 7 sub-leaf 0
    /// EBX bits 5 (AVX2), 8 (BMI2), 16 (AVX512F), 19 (ADX), 29 (SHA), 30 (AVX512BW)
    /// and 31 (AVX512VL), ECX bits 9 (VAES) and 10 (VPCLMULQDQ). Vol. 1
    /// §13.3: AVX needs XCR0's SSE and AVX state (bits 1 and 2) enabled by
    /// the OS, AVX-512 its opmask, ZMM_Hi256 and Hi16_ZMM state (bits 5 to 7)
    /// too.
    #[allow(unused_unsafe, reason = "CPUID is unsafe on the supported Rust 1.89")]
    fn read() -> Self {
        // SAFETY: leaf 0 and leaf 1 exist on every x86-64 CPU.
        let (leaf1, max) = unsafe { (__cpuid(1).ecx, __cpuid(0).eax) };
        let (leaf7b, leaf7c) = if max >= 7 {
            // SAFETY: leaf 0 says leaf 7 exists.
            let leaf7 = unsafe { __cpuid_count(7, 0) };
            (leaf7.ebx, leaf7.ecx)
        } else {
            (0, 0)
        };
        let xcr0 = if leaf1 & (3 << 27) == 3 << 27 {
            // SAFETY: OSXSAVE says XGETBV is enabled.
            unsafe { _xgetbv(0) }
        } else {
            0
        };
        Self::decode(leaf1, leaf7b, leaf7c, xcr0)
    }

    fn decode(leaf1: u32, leaf7b: u32, leaf7c: u32, xcr0: u64) -> Self {
        let ymm = xcr0 & 0x6 == 0x6;
        let zmm = xcr0 & 0xe6 == 0xe6;
        let bit = |word: u32, n: u32, feature: u32| if word >> n & 1 == 1 { feature } else { 0 };
        let mut bits = Self::READ
            | bit(leaf1, 25, Self::AES)
            | bit(leaf1, 1, Self::CLMUL)
            | bit(leaf1, 9, Self::SSSE3)
            | bit(leaf1, 19, Self::SSE41)
            | bit(leaf7b, 8, Self::BMI2)
            | bit(leaf7b, 19, Self::ADX)
            | bit(leaf7b, 29, Self::SHA);
        if ymm {
            bits |= bit(leaf7b, 5, Self::AVX2) | bit(leaf7c, 9, Self::VAES) | bit(leaf7c, 10, Self::VPCLMUL);
        }
        if zmm && leaf7b >> 16 & 1 == 1 {
            bits |= bit(leaf7b, 31, Self::AVX512VL) | bit(leaf7b, 30, Self::AVX512BW);
        }
        Self(bits)
    }
}

/// The cache publishes only this public word, not any other memory.
static FEATURES: AtomicU32 = AtomicU32::new(0);

/// Inline: every instruction path asks, several times per message.
#[inline(always)]
pub(super) fn features() -> Features {
    if cfg!(feature = "portable") {
        return Features(Features::READ);
    }
    let cached = FEATURES.load(Ordering::Relaxed);
    if cached & Features::READ != 0 {
        return Features(cached);
    }
    first_features()
}

#[cold]
#[inline(never)]
fn first_features() -> Features {
    let read = Features::read();
    FEATURES.store(read.0, Ordering::Relaxed);
    read
}

/// RSA's Montgomery multiplication on BMI2 and ADX, when the CPU has them:
/// out and the returned top word are the unreduced result, below 2m.
#[cfg(not(target_os = "uefi"))]
pub(super) fn rsa_mul(a: &[u64], b: &[u64], m: &[u64], n0: u64, n: usize, out: &mut [u64]) -> Option<u64> {
    if !features().bmi2_adx() {
        return None;
    }
    assert!(n.is_multiple_of(4) && n <= 64 && a.len() >= n && b.len() >= n && m.len() >= n && out.len() >= n);
    // CPUID admitted BMI2 and ADX, and the lengths are checked above.
    Some(unsafe { montgomery::mul(a, b, m, n0, n, out) })
}

/// P-256 field multiplication by `p256::mul` where BMI2 and ADX are
/// admitted.
#[cfg(not(target_os = "uefi"))]
#[inline]
pub(super) fn p256_mul(a: &[u64; 4], b: &[u64; 4]) -> Option<[u64; 4]> {
    if !features().bmi2_adx() {
        return None;
    }
    // CPUID admitted BMI2 and ADX.
    Some(unsafe { p256::mul(a, b) })
}

/// P-256 field squaring by `p256::square`, as [`p256_mul`].
#[cfg(not(target_os = "uefi"))]
#[inline]
pub(super) fn p256_square(a: &[u64; 4]) -> Option<[u64; 4]> {
    if !features().bmi2_adx() {
        return None;
    }
    // CPUID admitted BMI2 and ADX.
    Some(unsafe { p256::square(a) })
}

/// 2^255 - 19 multiplication by `f25519::mul` where BMI2 and ADX are
/// admitted.
#[inline]
pub(super) fn f25519_mul(a: &[u64; 4], b: &[u64; 4]) -> Option<[u64; 4]> {
    if !features().bmi2_adx() {
        return None;
    }
    // SAFETY: CPUID admitted BMI2 and ADX.
    Some(unsafe { f25519::mul(a, b) })
}

/// 2^255 - 19 squaring by `f25519::square`, as [`f25519_mul`].
#[inline]
pub(super) fn f25519_square(a: &[u64; 4]) -> Option<[u64; 4]> {
    if !features().bmi2_adx() {
        return None;
    }
    // SAFETY: CPUID admitted BMI2 and ADX.
    Some(unsafe { f25519::square(a) })
}

/// A vector from 16 bytes by value. With no pointer there is no copy
/// precondition, which debug assertions would check per access in hot loops.
#[cfg(not(target_os = "uefi"))]
#[inline(always)]
fn load(bytes: &[u8; 16]) -> __m128i {
    // Both are 16 plain bytes, every pattern valid.
    unsafe { core::mem::transmute(*bytes) }
}

#[cfg(not(target_os = "uefi"))]
#[inline(always)]
fn store(bytes: &mut [u8; 16], v: __m128i) {
    // Both are 16 plain bytes, every pattern valid.
    *bytes = unsafe { core::mem::transmute::<__m128i, [u8; 16]>(v) };
}

/// 32 bytes of caller data as two 16-byte halves. The allocator aligns
/// buffers to 16 bytes, not 32, and a 32-byte access to a 16-aligned address
/// can straddle a cache line, where two 16-byte accesses never do.
#[cfg(not(target_os = "uefi"))]
#[inline]
#[target_feature(enable = "avx2")]
fn load_data(bytes: &[u8; 32]) -> __m256i {
    let (halves, _) = bytes.as_chunks::<16>();
    _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(load(&halves[0])), load(&halves[1]))
}

#[cfg(not(target_os = "uefi"))]
#[inline]
#[target_feature(enable = "avx2")]
fn store_data(bytes: &mut [u8; 32], v: __m256i) {
    let (halves, _) = bytes.as_chunks_mut::<16>();
    store(&mut halves[0], _mm256_castsi256_si128(v));
    store(&mut halves[1], _mm256_extracti128_si256::<1>(v));
}

#[cfg(target_os = "uefi")]
pub(super) fn ghash_multiply(a: u128, b: u128) -> Option<u128> {
    if !features().clmul() {
        return None;
    }
    let a = crate::secret::Secret(a.reverse_bits().to_le_bytes());
    let b = crate::secret::Secret(b.reverse_bits().to_le_bytes());
    let mut products = crate::secret::Secret([0u8; 48]);
    let mut saved = crate::secret::Secret([0u8; 80]);
    // SAFETY: CPUID admitted CLMUL. All five used registers are restored; each memory
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
    Some(
        (low ^ high
            ^ (high << 1)
            ^ (high << 2)
            ^ (high << 7)
            ^ carry
            ^ (carry << 1)
            ^ (carry << 2)
            ^ (carry << 7))
            .reverse_bits(),
    )
}

#[cfg(target_os = "uefi")]
pub(super) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) -> bool {
    if !features().aes() {
        return false;
    }
    let mut saved = crate::secret::Secret([0u8; 32]);
    // SAFETY: CPUID admitted AES. The schedule contains every round, `block` and
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
    true
}

#[cfg(target_os = "uefi")]
pub(super) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    if !features().sha() {
        return false;
    }
    // SAFETY: CPUID admitted SHA and SSSE3.
    unsafe { sha_saving::sha256(state, blocks) }
    true
}

#[cfg(not(target_os = "uefi"))]
pub(super) use hosted::*;

#[cfg(not(target_os = "uefi"))]
mod hosted {
    //! Each function checks its own admission and reports whether it ran.
    use super::{Powers, aesni, chacha, features, poly1305, sha, vaes512};
    use crate::poly1305::Poly1305;

    pub(in crate::arch) fn aes_expand(key: &[u8], keys: &mut [u8; 240]) -> bool {
        if !features().aes() {
            return false;
        }
        // AES is admitted; the key is 16 or 32 bytes.
        unsafe { aesni::expand(key, keys) }
        true
    }

    pub(in crate::arch) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) -> bool {
        if !features().aes() {
            return false;
        }
        // AES is admitted and the schedule holds `rounds + 1` keys.
        unsafe { aesni::encrypt(keys, rounds, block) }
        true
    }

    pub(in crate::arch) fn ghash_powers(h: &[u8; 16], powers: &mut Powers) -> bool {
        let features = features();
        if features.vaes512() {
            // 512-bit VPCLMULQDQ under ZMM state is admitted.
            unsafe { vaes512::powers(h, powers) }
        } else if features.aes_clmul() {
            // CLMUL and SSSE3 are admitted.
            unsafe { aesni::powers(h, powers) }
        } else {
            return false;
        }
        true
    }

    /// `powers` came from [`ghash_powers`].
    pub(in crate::arch) fn ghash_blocks(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) -> bool {
        let features = features();
        if blocks.len() >= 16 && features.vaes512() {
            // 512-bit VPCLMULQDQ under ZMM state is admitted.
            unsafe { vaes512::hash(powers, state, blocks) }
        } else if features.aes_clmul() {
            // CLMUL and SSSE3 are admitted.
            unsafe { aesni::hash(powers, state, blocks) }
        } else {
            return false;
        }
        true
    }

    /// CTR from the 32-bit `counter` on. GCM bounded the block count so that
    /// no counter wraps.
    pub(in crate::arch) fn gcm_ctr(
        keys: &[u8; 240],
        rounds: usize,
        nonce: &[u8; 12],
        counter: u32,
        data: &mut [u8],
    ) -> bool {
        if !features().aes_clmul() {
            return false;
        }
        // AES and SSSE3 are admitted.
        unsafe { aesni::ctr(keys, rounds, nonce, counter, data) }
        true
    }

    /// CTR and GHASH over whole batches from counter 2 on, in one pass;
    /// returns the bytes it did. `powers` came from [`ghash_powers`].
    pub(in crate::arch) fn gcm_crypt(
        keys: &[u8; 240],
        rounds: usize,
        powers: &Powers,
        nonce: &[u8; 12],
        state: &mut [u8; 16],
        data: &mut [u8],
        open: bool,
    ) -> usize {
        let features = features();
        if data.len() >= 256 && features.vaes512() {
            // 512-bit VAES and VPCLMULQDQ under ZMM state are admitted.
            unsafe { vaes512::crypt(keys, rounds, powers, nonce, state, data, open) }
        } else if features.aes_clmul() {
            // AES, CLMUL and SSSE3 are admitted.
            unsafe { aesni::crypt(keys, rounds, powers, nonce, state, data, open) }
        } else {
            0
        }
    }

    /// `aesni::short` where its bounds hold: AAD up to 16 bytes, data up to 224.
    pub(in crate::arch) fn gcm_short(
        keys: &[u8; 240],
        rounds: usize,
        powers: &Powers,
        nonce: &[u8; 12],
        aad: &[u8],
        data: &mut [u8],
        stream: Option<&mut [[u8; 16]; 14]>,
    ) -> Option<[u8; 16]> {
        if aad.len() > 16 || data.len() > 224 || !features().aes_clmul() {
            return None;
        }
        // AES, CLMUL and SSSE3 are admitted.
        Some(unsafe { aesni::short(keys, rounds, powers, nonce, aad, data, stream) })
    }

    pub(in crate::arch) fn chacha20_poly1305(
        key: &[u8; 32],
        counter: u64,
        nonce: &[u8; 8],
        prefix: &[u8],
        data: &mut [u8],
        seal: bool,
        pad: bool,
    ) -> Option<Poly1305> {
        // Block 0 alone is quicker in scalar code. With AVX-512VL's rounds the
        // vector Poly1305 after them is faster than the stitch, as the T14
        // measures it.
        if data.is_empty() || !features().avx2() || !features().bmi2() || features().avx512vl() {
            return None;
        }
        // AVX2 and BMI2 under YMM state are admitted; the caller bounded the
        // counter.
        Some(unsafe { chacha::avx2::chacha20_poly1305(key, counter, nonce, prefix, data, seal, pad) })
    }

    pub(in crate::arch) fn chacha_blocks(
        keys: [&[u8; 32]; 4],
        counters: [u64; 4],
        nonce: &[u8; 8],
        n: usize,
        out: &mut [u8; 256],
    ) -> bool {
        if !features().avx2() {
            return false;
        }
        if features().avx512vl() {
            // AVX-512VL under ZMM state is admitted.
            unsafe { chacha::avx512::blocks(keys, counters, nonce, n, out) }
        } else {
            // AVX2 under YMM state is admitted.
            unsafe { chacha::avx2::blocks(keys, counters, nonce, n, out) }
        }
        true
    }

    /// Returns the bytes it did, whole blocks only; the caller bounded the counter.
    pub(in crate::arch) fn chacha_xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
        if !features().avx2() {
            return 0;
        }
        if features().avx512vl() {
            // AVX-512VL under ZMM state is admitted.
            return unsafe { chacha::avx512::xor(key, counter, nonce, data) };
        }
        // AVX2 under YMM state is admitted.
        unsafe { chacha::avx2::xor(key, counter, nonce, data) }
    }

    /// Whole eight-block groups of at least sixteen blocks; returns the
    /// blocks it absorbed.
    pub(in crate::arch) fn poly1305_blocks(r: &[u64; 3], h: &mut [u64; 3], blocks: &[[u8; 16]]) -> usize {
        if blocks.len() < 16 || !features().avx2() {
            return 0;
        }
        // AVX2 under YMM state is admitted.
        unsafe { poly1305::blocks(r, h, blocks) }
    }

    pub(in crate::arch) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
        if !features().sha() {
            return false;
        }
        // SHA, SSSE3 and SSE4.1 are admitted.
        unsafe { sha::sha256(state, blocks) }
        true
    }

    pub(in crate::arch) fn sha512_blocks(state: &mut [u64; 8], blocks: &[[u8; 128]]) -> bool {
        let features = features();
        if features.avx512vl() && features.bmi2() {
            // AVX2, BMI2 and AVX-512VL under ZMM state are admitted.
            unsafe { sha::sha512_avx512(state, blocks) }
        } else if features.bmi2() {
            // BMI2 is admitted.
            unsafe { sha::sha512_bmi2(state, blocks) }
        } else {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_names_each_cpuid_bit() {
        let all = Features::decode(!0, !0, !0, 0xe7);
        assert!(all.aes_clmul() && all.sha() && all.bmi2() && all.avx2() && all.avx512vl() && all.vaes512());
        assert!(!Features::decode(!0, !0, !0, 0x7).vaes512(), "ZMM state is the OS's to enable");
        assert!(!Features::decode(!0, !0, !0, 0x7).avx512vl());
        assert!(!Features::decode(!0, !(1 << 16), !0, 0xe7).vaes512(), "VL and BW need F");
        for bit in [30, 31] {
            assert!(!Features::decode(!0, !(1 << bit), !0, 0xe7).vaes512(), "leaf 7 EBX bit {bit}");
        }
        for bit in [9, 10] {
            assert!(!Features::decode(!0, !0, !(1 << bit), 0xe7).vaes512(), "leaf 7 ECX bit {bit}");
        }
        assert!(!Features::decode(!0, !0, !0, 0x3).avx2(), "YMM state is the OS's to enable");
        for (bit, feature) in [(1, Features::CLMUL), (9, Features::SSSE3), (19, Features::SSE41), (25, Features::AES)] {
            assert_eq!(Features::decode(!(1 << bit), !0, !0, 0xe7).0 & feature, 0, "leaf 1 bit {bit}");
        }
        for (bit, feature) in [(5, Features::AVX2), (8, Features::BMI2), (19, Features::ADX), (29, Features::SHA)] {
            assert_eq!(Features::decode(!0, !(1 << bit), !0, 0xe7).0 & feature, 0, "leaf 7 EBX bit {bit}");
        }
        for (bit, feature) in [(9, Features::VAES), (10, Features::VPCLMUL)] {
            assert_eq!(Features::decode(!0, !0, !(1 << bit), 0xe7).0 & feature, 0, "leaf 7 ECX bit {bit}");
        }
    }

    /// The loader's block, which only UEFI dispatches to, against the portable
    /// rounds. A host without the instructions cannot test it, and says so
    /// rather than passing.
    #[test]
    fn the_register_saving_sha256_block_matches_the_portable_rounds() {
        assert!(
            features().sha() || cfg!(feature = "portable"),
            "this host's CPU has no SHA extensions, so nothing here can test the register-saving block"
        );
        if !features().sha() {
            return;
        }
        let bytes: std::vec::Vec<u8> = (0..64 * 9).map(|i: usize| (i * 151 + i / 7) as u8).collect();
        for n in 0..=9 {
            let (blocks, _) = bytes[..64 * n].as_chunks::<64>();
            let (mut saving, mut portable) = ([0x89ab_cdefu32; 8], [0x89ab_cdefu32; 8]);
            // CPUID admitted SHA and SSSE3.
            unsafe { sha_saving::sha256(&mut saving, blocks) };
            crate::hash::sha256_rounds(&mut portable, blocks);
            assert_eq!(saving, portable, "{n} blocks");
        }
    }

    #[test]
    fn features_match_host_detection_and_portable_override() {
        let f = features();
        let on = !cfg!(feature = "portable");
        assert_eq!(f.aes(), on && std::is_x86_feature_detected!("aes"));
        assert_eq!(f.clmul(), on && std::is_x86_feature_detected!("pclmulqdq"));
        assert_eq!(f.avx2(), on && std::is_x86_feature_detected!("avx2"));
        assert_eq!(f.bmi2(), on && std::is_x86_feature_detected!("bmi2"));
        assert_eq!(
            f.bmi2_adx(),
            on && std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("adx")
        );
        assert_eq!(
            f.sha(),
            on && std::is_x86_feature_detected!("sha")
                && std::is_x86_feature_detected!("ssse3")
                && std::is_x86_feature_detected!("sse4.1")
        );
        assert_eq!(
            f.avx512vl(),
            f.avx2() && std::is_x86_feature_detected!("avx512f") && std::is_x86_feature_detected!("avx512vl")
        );
        assert_eq!(
            f.vaes512(),
            f.aes_clmul()
                && f.avx512vl()
                && std::is_x86_feature_detected!("avx512bw")
                && std::is_x86_feature_detected!("vaes")
                && std::is_x86_feature_detected!("vpclmulqdq")
        );
        std::println!(
            "admitted: aes+clmul {} avx2 {} bmi2 {} sha {} avx512vl {} vaes512 {}",
            f.aes_clmul(),
            f.avx2(),
            f.bmi2(),
            f.sha(),
            f.avx512vl(),
            f.vaes512()
        );
    }
}
