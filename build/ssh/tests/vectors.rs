//! **X25519 against RFC 7748**, through the `ring` calls the transport makes:
//! an ephemeral key generated from the randomness it is handed, its public
//! key, and the agreement with the client's. And the one input RFC 7748 §6.1
//! and RFC 8731 §3 say to refuse, a client key that makes a zero secret.

mod common;

use common::client::{string, Client};
use common::driver::{host_key, Keys};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::SystemRandom;
use toyos_ssh::{Refusal, Server};

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
        #[allow(deprecated)]
        let rng = ring::test::rand::FixedSliceRandom { bytes: private };
        let key = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
        assert_eq!(key.compute_public_key().unwrap().as_ref(), &public[..]);
        let secret = agree_ephemeral(key, &UnparsedPublicKey::new(&X25519, peer), |k| k.to_vec()).unwrap();
        assert_eq!(secret, shared);
    }
}

/// A client X25519 key of small order makes the zero secret, and the
/// exchange is refused by name; so is one that is not 32 bytes.
#[test]
fn a_client_key_that_makes_a_zero_secret_is_refused() {
    // Zero, one, and a point of order eight (RFC 7748 §6.1's "MUST check").
    let order_eight = hex("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800");
    for point in [vec![0; 32], [&[1u8][..], &[0; 31]].concat(), order_eight] {
        let mut c = Client::new(Server::new(host_key(), Keys::new(vec![]), SystemRandom::new())).unwrap();
        c.start_exchange("curve25519-sha256,kex-strict-c-v00@openssh.com").unwrap();
        let mut init = vec![30];
        string(&mut init, &point);
        assert_eq!(c.send(&init), Err(Refusal::KeyAgreement));
    }
    let mut c = Client::new(Server::new(host_key(), Keys::new(vec![]), SystemRandom::new())).unwrap();
    c.start_exchange("curve25519-sha256,kex-strict-c-v00@openssh.com").unwrap();
    let mut init = vec![30];
    string(&mut init, &[9; 31]);
    assert_eq!(c.send(&init), Err(Refusal::Truncated("Q_C")));
}
