//! Instruction dispatch is public. Each dispatch function here reports whether
//! an instruction path ran; when none did, the caller computes the same result
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

#[cfg(target_arch = "aarch64")]
mod aarch64;

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

/// `value`, through an assembly block LLVM cannot see into. The block is a
/// comment: it emits no instruction and keeps the word in its register.
#[cfg(not(miri))]
#[inline(always)]
pub(crate) fn value_barrier(mut value: u64) -> u64 {
    // SAFETY: the template reads and writes only the operand's register.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) value, options(pure, nomem, nostack, preserves_flags))
    };
    value
}

/// Miri runs no assembly; a volatile read of the local stands in.
#[cfg(miri)]
#[inline(always)]
pub(crate) fn value_barrier(value: u64) -> u64 {
    // The local word remains live for this volatile read.
    unsafe { core::ptr::read_volatile(&value) }
}

/// For a, b < m and n a multiple of four up to 64, by an instruction path:
/// out and the returned top word are a b / 2^(64n) mod m plus at most one
/// m, below 2m and unreduced; `None` where the CPU has no such path. RSA's
/// data is public.
pub(crate) fn rsa_mul(a: &[u64], b: &[u64], m: &[u64], n0: u64, n: usize, out: &mut [u64]) -> Option<u64> {
    hosted!(x86_64::rsa_mul(a, b, m, n0, n, out), {
        let _ = (a, b, m, n0, n, out);
        None
    })
}

/// a b / 2^256 mod P-256's p, for a, b < p, by an instruction path; `None`
/// where the CPU has no such path. The time does not depend on a or b.
#[inline]
pub(crate) fn p256_mul(a: &[u64; 4], b: &[u64; 4]) -> Option<[u64; 4]> {
    hosted!(x86_64::p256_mul(a, b), {
        let _ = (a, b);
        None
    })
}

/// a^2 / 2^256 mod P-256's p, for a < p, as [`p256_mul`].
#[inline]
pub(crate) fn p256_square(a: &[u64; 4]) -> Option<[u64; 4]> {
    hosted!(x86_64::p256_square(a), {
        let _ = a;
        None
    })
}

/// a b mod 2^255 - 19 in `field25519`'s four-word form, by an instruction
/// path; `None` where the CPU has no such path. The time does not depend on
/// a or b.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn f25519_mul(a: &[u64; 4], b: &[u64; 4]) -> Option<[u64; 4]> {
    x86_64::f25519_mul(a, b)
}

/// a^2 mod 2^255 - 19, as [`f25519_mul`].
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn f25519_square(a: &[u64; 4]) -> Option<[u64; 4]> {
    x86_64::f25519_square(a)
}

/// GHASH key powers in the instruction paths' form, highest first:
/// `powers[48 - n]` is (H^n)'. The 512-bit path fills all 48 and the AES-NI
/// path H^16..H^1.
pub(crate) type Powers = [u128; 48];

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

/// The hash key's powers in the form [`ghash_blocks`] and [`gcm_crypt`] take.
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

/// CTR from counter 2 over a prefix of whole batches, which it hashes in
/// the same pass, returning the prefix's length: the ciphertext it writes
/// when sealing, the ciphertext it reads when `open`. `powers` came from
/// [`ghash_powers`].
pub(crate) fn gcm_crypt(
    keys: &[u8; 240],
    rounds: usize,
    powers: &Powers,
    nonce: &[u8; 12],
    state: &mut [u8; 16],
    data: &mut [u8],
    open: bool,
) -> usize {
    hosted!(x86_64::gcm_crypt(keys, rounds, powers, nonce, state, data, open), {
        let _ = (keys, rounds, powers, nonce, state, data, open);
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
    stream: Option<&mut [[u8; 16]; 14]>,
) -> Option<[u8; 16]> {
    hosted!(x86_64::gcm_short(keys, rounds, powers, nonce, aad, data, stream), {
        let _ = (keys, rounds, powers, nonce, aad, data, stream);
        None
    })
}

/// XORs a prefix of `data` with ChaCha20 and returns its length: all of it
/// where an instruction path runs. The caller has checked counter bounds.
/// IETF nonce word zero occupies counter bits 32..63; bounded input cannot
/// carry into it. The remaining nonce words have the same layout as original
/// ChaCha.
pub(crate) fn chacha_xor(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) -> usize {
    hosted!(x86_64::chacha_xor(key, counter, nonce, data), {
        let _ = (key, counter, nonce, data);
        0
    })
}

pub(crate) fn sha256_blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) -> bool {
    #[cfg(target_arch = "x86_64")]
    return x86_64::sha256_blocks(state, blocks);
    #[cfg(target_arch = "aarch64")]
    return aarch64::sha256_blocks(state, blocks);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (state, blocks);
        false
    }
}

pub(crate) fn sha512_blocks(state: &mut [u64; 8], blocks: &[[u8; 128]]) -> bool {
    #[cfg(target_arch = "aarch64")]
    return aarch64::sha512_blocks(state, blocks);
    #[cfg(not(target_arch = "aarch64"))]
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

/// The ChaCha20 keystream of the first `n` of four lanes, `n` 2 to 4: lane
/// i is block `counters[i]` of `keys[i]` under `nonce`, in the original
/// construction's layout.
pub(crate) fn chacha_blocks(
    keys: [&[u8; 32]; 4],
    counters: [u64; 4],
    nonce: &[u8; 8],
    n: usize,
    out: &mut [u8; 256],
) -> bool {
    hosted!(x86_64::chacha_blocks(keys, counters, nonce, n, out), {
        let _ = (keys, counters, nonce, n, out);
        false
    })
}

/// ChaCha20 and its Poly1305 MAC in one pass over `data`, sealing or opening
/// it: block `counter` is the Poly1305 key, and `data` is XORed with the
/// blocks after it. The MAC runs over `prefix` and the ciphertext, each
/// zero-padded to a block where `pad` asks it (RFC 8439 §2.8), or run
/// together (OpenSSH). Returns the MAC, ready for anything after the
/// ciphertext, where an instruction path runs. The caller has checked counter
/// bounds, as for [`chacha_xor`].
pub(crate) fn chacha20_poly1305(
    key: &[u8; 32],
    counter: u64,
    nonce: &[u8; 8],
    prefix: &[u8],
    data: &mut [u8],
    seal: bool,
    pad: bool,
) -> Option<crate::poly1305::Poly1305> {
    hosted!(x86_64::chacha20_poly1305(key, counter, nonce, prefix, data, seal, pad), {
        let _ = (key, counter, nonce, prefix, data, seal, pad);
        None
    })
}
