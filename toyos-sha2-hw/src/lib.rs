//! SHA-256 on the CPU's own instructions where it has them: x86-64's SHA
//! extensions, chosen by CPUID as each hash begins. Where the CPU has none, the
//! blocks are `toyos-sha2`'s scalar compression. Both compute FIPS 180-4
//! §6.2.2, so which one ran decides how fast a digest came, never what it is.
//!
//! **AArch64 is scalar.** ToyOS has no AArch64 metal, so no gain from its SHA2
//! instructions could be measured.

#![cfg_attr(not(test), no_std)]

use toyos_sha2::Sha256;

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(all(test, target_arch = "x86_64"))]
mod tests;

/// A SHA-256 hash on this CPU's fastest compression.
pub fn sha256() -> Sha256 {
    #[cfg(target_arch = "x86_64")]
    if let Some(compress) = x86_64::compress() {
        return Sha256::with(compress);
    }
    Sha256::new()
}

/// The SHA-256 digest of `bytes`, on this CPU's fastest compression.
pub fn sha256_digest(bytes: impl AsRef<[u8]>) -> [u8; 32] {
    let mut hash = sha256();
    hash.update(bytes);
    hash.finalize()
}
