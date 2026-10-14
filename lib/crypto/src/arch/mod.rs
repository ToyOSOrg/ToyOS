//! Instruction dispatch is public. UEFI assembly saves and restores every used
//! XMM register; ordinary hosts use the target's ABI. Hosted AVX paths additionally
//! require CPUID support and OS-enabled extended state in XCR0.
//! Callers must arrange OS permission to execute SIMD and homogeneous features
//! on every CPU to which an imported key can migrate. Bare-metal x86 and
//! soft-float ARM exclude SIMD implementations: interrupted task state is not
//! owned by the kernel, and vector intrinsics violate the soft-float ABI.
//! Those targets are outside this crate's consumer scope and fail compilation.

#[cfg(any(
    all(target_arch = "x86_64", target_os = "none"),
    all(target_arch = "aarch64", target_abi = "softfloat")
))]
compile_error!(
    "toyos-crypto supports userland and the UEFI loader; bare-metal kernel targets are unsupported"
);

#[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
mod x86_64;

pub(crate) fn aes_available() -> bool {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::aes_available()
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        false
    }
}

/// Processes only complete batches after the caller has checked counter bounds.
/// IETF nonce word zero occupies counter bits 32..63; bounded input cannot carry
/// into it. The remaining nonce words have the same layout as original ChaCha.
pub(crate) fn chacha_xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::chacha_xor(key, counter, nonce, data)
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        let _ = (key, counter, nonce, data);
        0
    }
}

pub(crate) fn clmul_available() -> bool {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::clmul_available()
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        false
    }
}
pub(crate) fn ghash(a: u128, b: u128) -> u128 {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::ghash(a, b)
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        let _ = (a, b);
        unreachable!("CLMUL dispatch")
    }
}

pub(crate) fn ghash_blocks(h: &[u8; 16], state: &mut [u8; 16], blocks: &[[u8; 16]]) {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    x86_64::ghash_blocks(h, state, blocks);
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    for block in blocks {
        *state = ghash(
            u128::from_be_bytes(*state) ^ u128::from_be_bytes(*block),
            u128::from_be_bytes(*h),
        )
        .to_be_bytes();
    }
}
pub(crate) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::aes_encrypt(keys, rounds, block);
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        let _ = (keys, rounds, block);
        unreachable!("instruction dispatch");
    }
}

pub(crate) fn aes_ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], data: &mut [u8]) -> bool {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::aes_ctr(keys, rounds, nonce, data)
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        let _ = (keys, rounds, nonce, data);
        false
    }
}

pub(crate) fn aes_subword(word: &mut [u8; 4]) -> bool {
    #[cfg(all(target_arch = "x86_64", not(target_os = "none")))]
    {
        x86_64::aes_subword(word)
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_os = "none"))))]
    {
        let _ = word;
        false
    }
}
