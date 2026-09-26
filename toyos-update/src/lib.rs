//! A signed image, the slots it is installed into, and every decision the
//! loader and `/system/bin/update` make about one. Pure: no firmware, no
//! device, no allocation.
//!
//! **The contract.** An [`image`] is a header naming a monotonic version and
//! the SHA-256 of each of its sections — the kernel, its boot parameter and
//! ROOT — followed by an Ed25519 signature over that header, and then the
//! sections. The signature covers the hashes and never the bytes, so the
//! bytes may arrive by any path and in any order: an `ssh … update < image`
//! today, a pull of content-addressed chunks tomorrow, the same signature over
//! the same hashes either way. [`sig`] is the one verifier, and it verifies
//! exactly what OpenSSH's `ssh-keygen -Y sign` signs, so the owner's key is an
//! ordinary Ed25519 key and a second implementation can sign or check an image.
//!
//! A machine carries two [`slots`], each a FAT partition holding the kernel,
//! its boot parameter and the signed header, and a ROOT partition holding
//! ROOT; the slot table names both slots and marks one. The loader boots the
//! marked slot, and falls back to the other where the marked one is refused or
//! died on its last boot ([`record`]); the updater writes only the slot the
//! machine is not running, and moves the mark last.
//!
//! **What anti-rollback is here** ([`policy`]): the loader refuses an image
//! whose version is below the highest version a boot has proven, and keeps
//! that floor in a firmware variable no running kernel can write; the updater
//! refuses an image older than what the machine runs. Neither can defend a
//! machine whose firmware variables anyone with the machine in hand can reset.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod image;
pub mod policy;
pub mod record;
pub mod sig;
pub mod slots;

/// A SHA-256 digest.
pub type Digest = [u8; 32];

/// The SHA-256 of `bytes`: the one definition the Mac, the loader and the
/// updater share.
pub fn sha256(bytes: &[u8]) -> Digest {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// A digest in lowercase hex, into `out`.
pub fn hex<'a>(digest: &[u8], out: &'a mut [u8]) -> &'a str {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let n = digest.len().min(out.len() / 2);
    for (i, byte) in digest[..n].iter().enumerate() {
        out[2 * i] = DIGITS[(byte >> 4) as usize];
        out[2 * i + 1] = DIGITS[(byte & 0xF) as usize];
    }
    core::str::from_utf8(&out[..2 * n]).expect("hex digits are ASCII")
}
