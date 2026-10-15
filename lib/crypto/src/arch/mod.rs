//! Instruction dispatch is public. Each function here reports whether an
//! instruction path ran; when none did, the caller computes the same result
//! portably. UEFI assembly saves and restores every used XMM register;
//! ordinary hosts use the target's ABI. Hosted AVX paths additionally require
//! CPUID support and OS-enabled extended state in XCR0.
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

#[cfg(target_arch = "x86_64")]
mod x86_64;

/// Hosted x86-64 alone: the paths below with ordinary register ABI.
macro_rules! hosted {
    ($hosted:expr, $otherwise:expr) => {{
        #[cfg(all(target_arch = "x86_64", not(target_os = "uefi")))]
        {
            $hosted
        }
        #[cfg(not(all(target_arch = "x86_64", not(target_os = "uefi"))))]
        {
            $otherwise
        }
    }};
}

/// GHASH key powers, (H^(i+1))' at index i in the instruction paths' form.
pub(crate) type Powers = [u128; 16];

/// FIPS 197 key expansion of a 16- or 32-byte key.
pub(crate) fn aes_expand(key: &[u8], keys: &mut [u8; 240]) -> bool {
    hosted!(x86_64::aes_expand(key, keys), {
        let _ = (key, keys);
        false
    })
}

pub(crate) fn aes_encrypt(keys: &[u8; 240], rounds: usize, block: &mut [u8; 16]) -> bool {
    #[cfg(target_arch = "x86_64")]
    return x86_64::aes_encrypt(keys, rounds, block);
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (keys, rounds, block);
        false
    }
}

/// GCM's multiplication in its own bit order, where only a single-product
/// instruction path exists (UEFI).
pub(crate) fn ghash_multiply(a: u128, b: u128) -> Option<u128> {
    #[cfg(all(target_arch = "x86_64", target_os = "uefi"))]
    return x86_64::ghash_multiply(a, b);
    #[cfg(not(all(target_arch = "x86_64", target_os = "uefi")))]
    {
        let _ = (a, b);
        None
    }
}

/// The hash key's powers in the form [`ghash_blocks`] and [`gcm_seal`] take.
pub(crate) fn ghash_powers(h: &[u8; 16], powers: &mut Powers) -> bool {
    hosted!(x86_64::ghash_powers(h, powers), {
        let _ = (h, powers);
        false
    })
}

/// GHASH over whole blocks; `powers` came from [`ghash_powers`].
pub(crate) fn ghash_blocks(powers: &Powers, state: &mut [u8; 16], blocks: &[[u8; 16]]) -> bool {
    hosted!(x86_64::ghash_blocks(powers, state, blocks), {
        let _ = (powers, state, blocks);
        false
    })
}

/// GCM's counter mode from the 32-bit `counter` on. The caller bounded the
/// block count so that no counter wraps.
pub(crate) fn gcm_ctr(keys: &[u8; 240], rounds: usize, nonce: &[u8; 12], counter: u32, data: &mut [u8]) -> bool {
    hosted!(x86_64::gcm_ctr(keys, rounds, nonce, counter, data), {
        let _ = (keys, rounds, nonce, counter, data);
        false
    })
}

/// Encrypts from counter 2 and hashes a prefix of whole batches, returning
/// its length; `powers` came from [`ghash_powers`].
pub(crate) fn gcm_seal(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
) -> usize {
    hosted!(x86_64::gcm_seal(keys, rounds, powers, nonce, state, data), {
        let _ = (keys, rounds, powers, nonce, state, data);
        0
    })
}

/// A short message's GCM in one batch, where the instruction path admits it:
/// without `stream` it encrypts `data` and returns the tag; with it, it
/// returns the expected tag and writes the keystream to `stream`. `powers`
/// came from [`ghash_powers`].
pub(crate) fn gcm_short(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    stream: Option<&mut [[u8; 16]; 7]>,
) -> Option<[u8; 16]> {
    hosted!(x86_64::gcm_short(keys, rounds, powers, nonce, aad, data, stream), {
        let _ = (keys, rounds, powers, nonce, aad, data, stream);
        None
    })
}

/// XORs a prefix of whole 64-byte blocks with ChaCha20 and returns its
/// length. The caller has checked counter bounds. IETF nonce word zero
/// occupies counter bits 32..63; bounded input cannot carry into it. The
/// remaining nonce words have the same layout as original ChaCha.
pub(crate) fn chacha_xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    hosted!(x86_64::chacha_xor(key, counter, nonce, data), {
        let _ = (key, counter, nonce, data);
        0
    })
}

pub(crate) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    hosted!(x86_64::sha256_blocks(state, blocks), {
        let _ = (state, blocks);
        false
    })
}

pub(crate) fn sha512_blocks(state: &mut [u64; 8], blocks: &[[u8; 128]]) -> bool {
    hosted!(x86_64::sha512_blocks(state, blocks), {
        let _ = (state, blocks);
        false
    })
}


/// Poly1305 over a prefix of whole blocks, each with 2^128 added; returns its
/// length in blocks. `r` is r0, r1 and 5·r1/4; `h` is in radix 2^64.
pub(crate) fn poly1305_blocks(r: &[u64; 3], h: &mut [u64; 3], blocks: &[[u8; 16]]) -> usize {
    hosted!(x86_64::poly1305_blocks(r, h, blocks), {
        let _ = (r, h, blocks);
        0
    })
}

/// The keystream of four independent ChaCha20 states, each 16 words.
pub(crate) fn chacha_blocks(states: &[[u32; 16]; 4], out: &mut [u8; 256]) -> bool {
    hosted!(x86_64::chacha_blocks(states, out), {
        let _ = (states, out);
        false
    })
}

/// [`chacha_xor`] over a prefix of whole batches, absorbing the ciphertext
/// into the Poly1305 accumulator `h` under `r` as it goes; returns its length.
pub(crate) fn chacha_seal(
    key: &[u8; 32],
    counter: u64,
    nonce: &[u8; 8],
    data: &mut [u8],
    r: &[u64; 3],
    h: &mut [u64; 3],
) -> usize {
    hosted!(x86_64::chacha_seal(key, counter, nonce, data, r, h), {
        let _ = (key, counter, nonce, data, r, h);
        0
    })
}
