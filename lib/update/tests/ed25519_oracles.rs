//! The image signature beside RustCrypto's `verify_strict` and ring: the same
//! signature bytes over SSHSIG's message from every seed, and the same verdict
//! on every changed signature and message. Where the verifiers part is
//! `toyos-crypto`'s `tests/ed25519_verdicts.rs`.

use ring::signature::{self, KeyPair};
use toyos_update::image::{HEADER_BYTES, SIGNATURE_BYTES};
use toyos_update::sig;

const CASES: usize = 2_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        core::array::from_fn(|_| self.next() as u8)
    }
}

fn dalek_strict(public: &[u8; 32], message: &[u8], signature: &[u8; SIGNATURE_BYTES]) -> bool {
    ed25519_dalek::VerifyingKey::from_bytes(public)
        .is_ok_and(|key| key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature)).is_ok())
}

fn ring(public: &[u8; 32], message: &[u8], signature: &[u8; SIGNATURE_BYTES]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ED25519, public).verify(message, signature).is_ok()
}

#[test]
fn signatures_and_verdicts_match_dalek_and_ring() {
    let mut rng = Rng(0x5eed_ed25);
    for case in 0..CASES {
        let seed: [u8; 32] = rng.bytes();
        let header: [u8; HEADER_BYTES] = rng.bytes();
        let message = sig::message(&header);
        let public = sig::public_of(&seed);
        let dalek = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(dalek.verifying_key().to_bytes(), public, "case {case}: the public key");
        let ours = sig::sign(&seed, &header);
        use ed25519_dalek::Signer as _;
        assert_eq!(dalek.sign(message.as_bytes()).to_bytes(), ours, "case {case}: the signature");
        let pair = signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        assert_eq!(pair.public_key().as_ref(), public, "case {case}: ring's public key");

        let (mut signature, mut signed) = (ours, header);
        match case % 4 {
            0 => {}
            1 => signature[rng.below(SIGNATURE_BYTES)] ^= 1 << rng.below(8),
            2 => signed[rng.below(HEADER_BYTES)] ^= 1 << rng.below(8),
            _ => {
                // S + L: the same scalar, not canonically encoded.
                let mut carry = 0u16;
                const L: [u8; 32] = [
                    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
                ];
                for (byte, l) in signature[32..].iter_mut().zip(L) {
                    let sum = u16::from(*byte) + u16::from(l) + carry;
                    *byte = sum as u8;
                    carry = sum >> 8;
                }
            }
        }
        let verdict = sig::verify(&public, &signed, &signature).is_ok();
        let message = sig::message(&signed);
        assert_eq!(verdict, dalek_strict(&public, message.as_bytes(), &signature), "case {case}: verify_strict");
        assert_eq!(verdict, ring(&public, message.as_bytes(), &signature), "case {case}: ring");
        assert_eq!(verdict, case % 4 == 0, "case {case}");
    }
}
