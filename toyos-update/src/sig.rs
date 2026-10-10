//! The signature over anything the owner signs: Ed25519, over the message
//! OpenSSH's SSHSIG format builds (`PROTOCOL.sshsig`), in the namespace of
//! what is signed.
//!
//! ```text
//! signed data  "SSHSIG" | string namespace | string "" | string "sha512"
//!              | string SHA-512(bytes)
//! ```
//!
//! where `string` is a big-endian `u32` length and the bytes. **Why SSHSIG
//! and not the bytes raw**: it is exactly what `ssh-keygen -Y sign -n
//! <namespace>` signs, so a signature an implementation this tree did not
//! write made verifies here, and an owner may hold the key in any agent or
//! token OpenSSH can sign with. **The namespace is the [`Signed`] thing's own,
//! never an argument**: it is what stops a signature the owner's key made for
//! anything else — a git commit, a package repository's targets, an image —
//! from verifying as this.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest as _, Sha512};

use crate::image::{HEADER_BYTES, SIGNATURE_BYTES};

/// The SSHSIG namespace every image is signed in.
pub const NAMESPACE: &str = "toyos-image";

const PREAMBLE: &[u8; 6] = b"SSHSIG";
const HASH: &str = "sha512";

/// The longest namespace anything here is signed in: `toyos-timestamp`.
const MAX_NAMESPACE: usize = 15;

/// What a signature is over: bytes, and the namespace they are signed in.
pub trait Signed {
    fn namespace(&self) -> &'static str;
    fn bytes(&self) -> &[u8];
}

/// An image's header, in [`NAMESPACE`].
impl Signed for [u8; HEADER_BYTES] {
    fn namespace(&self) -> &'static str {
        NAMESPACE
    }
    fn bytes(&self) -> &[u8] {
        self
    }
}

/// The bytes Ed25519 signs for one [`Signed`]: the preamble, four strings and
/// a SHA-512, held without allocating.
pub struct Message {
    bytes: [u8; 6 + (4 + MAX_NAMESPACE) + 4 + (4 + HASH.len()) + (4 + 64)],
    len: usize,
}

impl Message {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The bytes Ed25519 signs for `signed`.
pub fn message<S: Signed + ?Sized>(signed: &S) -> Message {
    let namespace = signed.namespace().as_bytes();
    assert!(namespace.len() <= MAX_NAMESPACE, "a namespace this crate names is at most {MAX_NAMESPACE} bytes");
    let mut out = Message { bytes: [0; 6 + (4 + MAX_NAMESPACE) + 4 + (4 + HASH.len()) + (4 + 64)], len: 0 };
    let mut put = |bytes: &[u8]| {
        out.bytes[out.len..out.len + bytes.len()].copy_from_slice(bytes);
        out.len += bytes.len();
    };
    put(PREAMBLE);
    for field in [namespace, b"", HASH.as_bytes()] {
        put(&(field.len() as u32).to_be_bytes());
        put(field);
    }
    put(&64u32.to_be_bytes());
    put(&Sha512::digest(signed.bytes()));
    out
}

/// Why a signature does not vouch for what it is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The key is not a point on the curve.
    Key,
    /// The signature is not the key's over these bytes in this namespace.
    Signature,
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Key => write!(f, "the key is not an Ed25519 public key"),
            Self::Signature => write!(f, "the signature is not this key's over these bytes"),
        }
    }
}

/// Whether `signature` is `key`'s over `signed`, in its namespace.
///
/// `verify_strict`: a signature with a non-canonical `S`, or a key of small
/// order, is refused rather than accepted by one implementation and not
/// another.
pub fn verify<S: Signed + ?Sized>(key: &[u8; 32], signed: &S, signature: &[u8; SIGNATURE_BYTES]) -> Result<(), Refused> {
    let key = VerifyingKey::from_bytes(key).map_err(|_| Refused::Key)?;
    key.verify_strict(message(signed).as_bytes(), &Signature::from_bytes(signature))
        .map_err(|_| Refused::Signature)
}

/// The public key of the seed `seed`.
pub fn public_of(seed: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// `seed`'s signature over `signed`: the host's half, and no binary on the
/// machine is built with it.
#[cfg(any(test, feature = "sign"))]
pub fn sign<S: Signed + ?Sized>(seed: &[u8; 32], signed: &S) -> [u8; SIGNATURE_BYTES] {
    use ed25519_dalek::Signer as _;
    ed25519_dalek::SigningKey::from_bytes(seed).sign(message(signed).as_bytes()).to_bytes()
}

/// A 32-byte key from 64 hex digits, at compile time: how the loader and the
/// updater take the key the build embeds. Anything else fails the build.
pub const fn key_from_hex(text: &str) -> [u8; 32] {
    let bytes = text.as_bytes();
    assert!(bytes.len() == 64, "the image key is 64 hex digits");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (digit(bytes[2 * i]) << 4) | digit(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

const fn digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("the image key is lowercase hex"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8032 §7.1, TEST 1: the empty message.
    const RFC_1: ([u8; 32], [u8; 32], &[u8], [u8; 64]) = (
        hex32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
        hex32("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
        b"",
        hex64(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
    );

    /// RFC 8032 §7.1, TEST 2: one byte.
    const RFC_2: ([u8; 32], [u8; 32], &[u8], [u8; 64]) = (
        hex32("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb"),
        hex32("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"),
        &[0x72],
        hex64(
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
    );

    /// RFC 8032 §7.1, TEST 3: two bytes.
    const RFC_3: ([u8; 32], [u8; 32], &[u8], [u8; 64]) = (
        hex32("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7"),
        hex32("fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025"),
        &[0xaf, 0x82],
        hex64(
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
    );

    const fn hex32(text: &str) -> [u8; 32] {
        key_from_hex(text)
    }

    const fn hex64(text: &str) -> [u8; 64] {
        let b = text.as_bytes();
        assert!(b.len() == 128);
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 64 {
            out[i] = (digit(b[2 * i]) << 4) | digit(b[2 * i + 1]);
            i += 1;
        }
        out
    }

    /// **The external oracle for the primitive**: the RFC's own keys, messages
    /// and signatures. The key is derived from the seed, the signature checks
    /// under the verifier this crate calls, and a flipped bit anywhere in the
    /// signature or the message is refused.
    #[test]
    fn rfc_8032_vectors_verify_and_a_flipped_bit_does_not() {
        for (seed, public, msg, sig) in [RFC_1, RFC_2, RFC_3] {
            assert_eq!(public_of(&seed), public, "the public key derived from the RFC's seed");
            let key = VerifyingKey::from_bytes(&public).expect("the RFC's key");
            key.verify_strict(msg, &Signature::from_bytes(&sig)).expect("the RFC's signature");
            for bit in [0, 255, 256, 511] {
                let mut bent = sig;
                bent[bit / 8] ^= 1 << (bit % 8);
                assert!(key.verify_strict(msg, &Signature::from_bytes(&bent)).is_err(), "bit {bit}");
            }
            let mut longer = msg.to_vec();
            longer.push(0);
            assert!(key.verify_strict(&longer, &Signature::from_bytes(&sig)).is_err());
        }
    }

    /// The signed data is PROTOCOL.sshsig's layout, byte for byte.
    #[test]
    fn the_message_is_sshsigs_signed_data() {
        let header = [0xA5u8; HEADER_BYTES];
        let m = message(&header);
        let m = m.as_bytes();
        assert_eq!(m.len(), 6 + (4 + 11) + 4 + (4 + 6) + (4 + 64));
        assert_eq!(&m[..6], b"SSHSIG");
        assert_eq!(&m[6..10], &11u32.to_be_bytes());
        assert_eq!(&m[10..21], b"toyos-image");
        assert_eq!(&m[21..25], &0u32.to_be_bytes());
        assert_eq!(&m[25..29], &6u32.to_be_bytes());
        assert_eq!(&m[29..35], b"sha512");
        assert_eq!(&m[35..39], &64u32.to_be_bytes());
        assert_eq!(&m[39..], &Sha512::digest(header)[..]);
    }
}
