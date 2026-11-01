//! **X25519 against RFC 7748**, through the calls the transport makes: an
//! ephemeral key generated from the randomness it is handed, its public key,
//! and the agreement with the client's. And the inputs RFC 7748 §6.1, RFC 8731
//! §3 and RFC 10042 §2.1 say to refuse: a client key that makes a zero secret,
//! a key of the wrong length, and an ML-KEM-768 encapsulation key that fails
//! FIPS 203 §7.2's check. NIST's ML-KEM-768 known answers are `toyos-crypto`'s
//! (`lib/crypto/tests/nist.rs`).

mod common;

use common::client::{string, Client, CURVE25519, HYBRID, STRICT};
use common::driver::{host_key, Fixed, Keys};
use toyos_crypto::x25519::{PublicKey, SecretKey};
use toyos_ssh::{Random, Refusal, Server};

/// Randomness that is these bytes.
struct Given<'a>(&'a [u8]);

impl Random for Given<'_> {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        destination.copy_from_slice(self.0);
        Ok(())
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// RFC 7748 §6.1: Alice's and Bob's keys, each side's public key and the
/// secret they share.
#[test]
fn rfc_7748_section_6_1() {
    let alice = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let alice_public = hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    let bob = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let bob_public = hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    let shared = hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    for (private, public, peer) in [(&alice, &alice_public, &bob_public), (&bob, &bob_public, &alice_public)] {
        let key = SecretKey::generate(&mut Given(private)).unwrap();
        assert_eq!(key.public_key().0, public[..]);
        let secret = key.agree(&PublicKey(peer[..].try_into().unwrap())).unwrap();
        assert_eq!(secret.as_bytes()[..], shared);
    }
}

/// The server's answer to a message 30 carrying `value`, under `method`.
fn exchange(method: &str, value: &[u8]) -> Result<(), Refusal> {
    let mut c = Client::new(Server::new(host_key(), Keys::new(vec![]), Fixed(1))).unwrap();
    c.start_exchange(&format!("{method},{STRICT}")).unwrap();
    let mut init = vec![30];
    string(&mut init, value);
    c.send(&init)
}

/// A valid ML-KEM-768 encapsulation key, generated from a fixed seed.
fn kem_key() -> Vec<u8> {
    toyos_crypto::mlkem::DecapsulationKey::generate(&mut Fixed(7)).unwrap().public_key().as_bytes().to_vec()
}

/// Zero, one, and a point of order eight (RFC 7748 §6.1's "MUST check").
fn small_order_points() -> [Vec<u8>; 3] {
    [vec![0; 32], [&[1u8][..], &[0; 31]].concat(), hex("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800")]
}

/// A client X25519 key of small order makes the zero secret, and the
/// exchange is refused by name, under either method; so is one that is not
/// 32 bytes.
#[test]
fn a_client_key_that_makes_a_zero_secret_is_refused() {
    let zero = Err(Refusal::KeyExchange("an X25519 key that makes the zero secret"));
    for point in small_order_points() {
        assert_eq!(exchange(CURVE25519, &point), zero);
        assert_eq!(exchange(HYBRID, &[kem_key(), point].concat()), zero);
    }
    assert_eq!(exchange(CURVE25519, &[9; 31]), Err(Refusal::KeyExchange("a Q_C that is not 32 bytes")));
    assert_eq!(exchange(CURVE25519, &[9; 33]), Err(Refusal::KeyExchange("a Q_C that is not 32 bytes")));
}

/// RFC 10042 §2.1: `C_INIT` is exactly an encapsulation key and an X25519
/// key, and the encapsulation key passes FIPS 203 §7.2's modulus check.
#[test]
fn a_hybrid_init_is_held_to_its_length_and_its_key_check() {
    let x25519 = SecretKey::from_bytes([9; 32]).public_key().0;
    let good = [kem_key(), x25519.to_vec()].concat();
    let length = Err(Refusal::KeyExchange("a C_INIT that is not an ML-KEM-768 key and an X25519 key"));
    for len in [0, 32, 1184, good.len() - 1, good.len() + 1] {
        let init: Vec<u8> = good.iter().copied().cycle().take(len).collect();
        assert_eq!(exchange(HYBRID, &init), length, "{len} bytes");
    }
    // The classical method's 32 bytes are not the hybrid's.
    assert_eq!(exchange(HYBRID, &x25519), length);
    // The first coefficient set to q = 3329, one past the field (FIPS 203
    // §7.2's ByteEncode(ByteDecode(ek)) == ek fails on it).
    let mut bad = good.clone();
    bad[0] = 0x01;
    bad[1] = (bad[1] & 0xf0) | 0x0d;
    assert_eq!(exchange(HYBRID, &bad), Err(Refusal::KeyExchange("an ML-KEM-768 encapsulation key that fails FIPS 203's check")));
    // The good one is taken.
    assert_eq!(exchange(HYBRID, &good), Ok(()));
}
