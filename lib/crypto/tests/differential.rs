//! Oracles are dev-dependencies only; these tests never enter a shipped binary.
use aes_gcm::aead::{AeadInPlace, KeyInit};
use toyos_crypto::{
    aead::{OpeningKey, SealingKey},
    hash::{Sha256, Sha384, Sha512},
};
fn bytes<const N: usize>(seed: usize) -> [u8; N] {
    core::array::from_fn(|i| ((i * 73 + seed * 29 + i * i) % 256) as u8)
}
#[test]
fn sha2_streaming_boundaries() {
    use sha2::Digest;
    for n in [
        0, 1, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 255, 256, 257, 1024,
    ] {
        let input = bytes::<1024>(n);
        let input = &input[..n];
        assert_eq!(
            Sha256::digest(input).unwrap().as_slice(),
            sha2::Sha256::digest(input).as_slice()
        );
        assert_eq!(
            Sha384::digest(input).unwrap().as_slice(),
            sha2::Sha384::digest(input).as_slice()
        );
        assert_eq!(
            Sha512::digest(input).unwrap().as_slice(),
            sha2::Sha512::digest(input).as_slice()
        );
        for chunk_size in [1, 7, 63, 64, 65, 127, 128] {
            let mut h = Sha512::new();
            for chunk in input.chunks(chunk_size) {
                h.update(chunk).unwrap()
            }
            assert_eq!(h.finalize(), Sha512::digest(input).unwrap());
        }
    }
}
#[test]
fn aead_all_algorithms_and_partial_blocks() {
    for n in [
        0, 1, 15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65, 127, 128, 129, 255, 256, 257, 1024,
        4096, 16384,
    ] {
        for a in [0, 1, 15, 16, 17, 31, 32, 33] {
            let k = bytes::<32>(n + a);
            let nonce = bytes::<12>(a);
            let aad = bytes::<33>(n);
            let aad = &aad[..a];
            let input = bytes::<16384>(a);
            let input = &input[..n];
            for alg in 0..3 {
                let mut ours = input.to_vec();
                let mut reference = input.to_vec();
                let (mut key, opener) = match alg {
                    0 => (
                        SealingKey::aes128_gcm(k[..16].try_into().unwrap(), nonce),
                        OpeningKey::aes128_gcm(k[..16].try_into().unwrap()),
                    ),
                    1 => (SealingKey::aes256_gcm(k, nonce), OpeningKey::aes256_gcm(k)),
                    _ => (
                        SealingKey::chacha20_poly1305(k, nonce),
                        OpeningKey::chacha20_poly1305(k),
                    ),
                };
                let (_, tag) = key.seal(aad, &mut ours).unwrap();
                let expected = match alg {
                    0 => aes_gcm::Aes128Gcm::new_from_slice(&k[..16])
                        .unwrap()
                        .encrypt_in_place_detached((&nonce).into(), aad, &mut reference)
                        .unwrap(),
                    1 => aes_gcm::Aes256Gcm::new_from_slice(&k)
                        .unwrap()
                        .encrypt_in_place_detached((&nonce).into(), aad, &mut reference)
                        .unwrap(),
                    _ => chacha20poly1305::ChaCha20Poly1305::new_from_slice(&k)
                        .unwrap()
                        .encrypt_in_place_detached((&nonce).into(), aad, &mut reference)
                        .unwrap(),
                };
                assert_eq!(ours, reference);
                assert_eq!(tag.as_slice(), expected.as_slice());
                for i in 0..16 {
                    let mut bad = tag;
                    bad[i] ^= 1;
                    let mut ciphertext = ours.clone();
                    assert!(opener.open(&nonce, aad, &mut ciphertext, &bad).is_err());
                    assert_eq!(ciphertext, ours);
                }
                opener.open(&nonce, aad, &mut ours, &tag).unwrap();
                assert_eq!(ours, input);
            }
        }
    }
}
#[test]
fn x25519_reference_keys_and_shared_secrets() {
    use toyos_crypto::x25519::SecretKey;
    for i in 0..24 {
        let a = bytes::<32>(i);
        let b = bytes::<32>(i + 7);
        let ar = x25519_dalek::StaticSecret::from(a);
        let br = x25519_dalek::StaticSecret::from(b);
        let bp = SecretKey::from_bytes(b).public_key();
        assert_eq!(
            SecretKey::from_bytes(a).public_key().0,
            x25519_dalek::PublicKey::from(&ar).to_bytes()
        );
        assert_eq!(
            *SecretKey::from_bytes(a).agree(&bp).unwrap().as_bytes(),
            ar.diffie_hellman(&x25519_dalek::PublicKey::from(&br))
                .to_bytes()
        );
    }
}
#[test]
fn ed25519_reference_verification() {
    use ed25519_dalek::Signer;
    for i in 0..16 {
        let seed = bytes::<32>(i);
        let message = bytes::<257>(i);
        let message = &message[..i * 16];
        let reference = ed25519_dalek::SigningKey::from_bytes(&seed);
        let signature = reference.sign(message).to_bytes();
        let key =
            toyos_crypto::ed25519::PublicKey::from_bytes(reference.verifying_key().to_bytes())
                .unwrap();
        key.verify(message, &signature).unwrap();
        #[cfg(feature = "sign")]
        {
            let ours = toyos_crypto::ed25519::SigningKey::from_seed(seed);
            assert_eq!(ours.public_key().as_bytes(), key.as_bytes());
            assert_eq!(ours.sign(message).unwrap(), signature);
        }
    }
}
#[test]
fn nist_curves_reference_ecdh_and_ecdsa() {
    use p256::{ecdsa::signature::Signer, elliptic_curve::sec1::ToEncodedPoint};
    for i in 1..=4 {
        let a = bytes::<32>(i);
        let b = bytes::<32>(i + 7);
        let ar = p256::SecretKey::from_slice(&a).unwrap();
        let br = p256::SecretKey::from_slice(&b).unwrap();
        let ours = toyos_crypto::ecc::p256::SecretKey::from_bytes(a).unwrap();
        assert_eq!(
            ours.public_key().as_sec1().as_slice(),
            ar.public_key().to_encoded_point(false).as_bytes()
        );
        let peer = toyos_crypto::ecc::p256::PublicKey::from_sec1(
            br.public_key().to_encoded_point(false).as_bytes(),
        )
        .unwrap();
        assert_eq!(
            ours.agree(&peer).unwrap().as_bytes().as_slice(),
            p256::ecdh::diffie_hellman(ar.to_nonzero_scalar(), br.public_key().as_affine())
                .raw_secret_bytes()
                .as_slice()
        );
        let signer = p256::ecdsa::SigningKey::from(ar);
        let signature: p256::ecdsa::Signature = signer.sign(b"ToyOS");
        let public = toyos_crypto::ecc::p256::PublicKey::from_sec1(
            signer.verifying_key().to_encoded_point(false).as_bytes(),
        )
        .unwrap();
        public
            .verify(b"ToyOS", signature.to_der().as_bytes())
            .unwrap();
        assert!(
            public
                .verify(b"changed", signature.to_der().as_bytes())
                .is_err()
        );
        let a = bytes::<48>(i);
        let b = bytes::<48>(i + 7);
        let ar = p384::SecretKey::from_slice(&a).unwrap();
        let br = p384::SecretKey::from_slice(&b).unwrap();
        let ours = toyos_crypto::ecc::p384::SecretKey::from_bytes(a).unwrap();
        assert_eq!(
            ours.public_key().as_sec1().as_slice(),
            ar.public_key().to_encoded_point(false).as_bytes()
        );
        let peer = toyos_crypto::ecc::p384::PublicKey::from_sec1(
            br.public_key().to_encoded_point(false).as_bytes(),
        )
        .unwrap();
        assert_eq!(
            ours.agree(&peer).unwrap().as_bytes().as_slice(),
            p384::ecdh::diffie_hellman(ar.to_nonzero_scalar(), br.public_key().as_affine())
                .raw_secret_bytes()
                .as_slice()
        );
        let signer = p384::ecdsa::SigningKey::from(ar);
        let signature: p384::ecdsa::Signature = signer.sign(b"ToyOS");
        let public = toyos_crypto::ecc::p384::PublicKey::from_sec1(
            signer.verifying_key().to_encoded_point(false).as_bytes(),
        )
        .unwrap();
        public
            .verify(b"ToyOS", signature.to_der().as_bytes())
            .unwrap();
        assert!(
            public
                .verify(b"changed", signature.to_der().as_bytes())
                .is_err()
        );
    }
}
#[test]
fn nist_curve_boundary_scalars() {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    macro_rules! check {
        ($curve:ident, $bytes:literal, $order:literal) => {{
            let order: [u8; $bytes] = core::array::from_fn(|i| {
                u8::from_str_radix(&$order[2 * i..2 * i + 2], 16).unwrap()
            });
            assert!(toyos_crypto::ecc::$curve::SecretKey::from_bytes([0; $bytes]).is_err());
            assert!(toyos_crypto::ecc::$curve::SecretKey::from_bytes(order).is_err());
            let mut one = [0; $bytes];
            one[$bytes - 1] = 1;
            let mut two = one;
            two[$bytes - 1] = 2;
            let mut last = order;
            last[$bytes - 1] -= 1;
            for scalar in [one, two, last] {
                let reference = $curve::SecretKey::from_slice(&scalar).unwrap();
                let ours = toyos_crypto::ecc::$curve::SecretKey::from_bytes(scalar).unwrap();
                assert_eq!(
                    ours.public_key().as_sec1().as_slice(),
                    reference.public_key().to_encoded_point(false).as_bytes()
                );
                for peer_scalar in [one, two, last] {
                    let peer_reference = $curve::SecretKey::from_slice(&peer_scalar).unwrap();
                    let peer = toyos_crypto::ecc::$curve::PublicKey::from_sec1(
                        peer_reference
                            .public_key()
                            .to_encoded_point(false)
                            .as_bytes(),
                    )
                    .unwrap();
                    let ours = toyos_crypto::ecc::$curve::SecretKey::from_bytes(scalar).unwrap();
                    assert_eq!(
                        ours.agree(&peer).unwrap().as_bytes().as_slice(),
                        $curve::ecdh::diffie_hellman(
                            reference.to_nonzero_scalar(),
                            peer_reference.public_key().as_affine()
                        )
                        .raw_secret_bytes()
                        .as_slice()
                    );
                }
            }
        }};
    }
    check!(
        p256,
        32,
        "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"
    );
    check!(
        p384,
        48,
        "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973"
    );
}

#[test]
fn openssh_construction_matches_independent_primitives() {
    use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
    use poly1305::universal_hash::KeyInit;
    for n in [
        0, 1, 15, 16, 17, 63, 64, 65, 128, 255, 256, 257, 511, 512, 513, 1024, 4096,
    ] {
        let key = bytes::<64>(n);
        let mut ours = bytes::<4096>(n)[..n].to_vec();
        let mut reference = ours.clone();
        let mut header = (n as u32).to_be_bytes();
        let mut rh = header;
        let seq = 7;
        let nonce = (seq as u64).to_be_bytes();
        let mut h = chacha20::ChaCha20Legacy::new(key[32..].into(), (&nonce).into());
        h.apply_keystream(&mut rh);
        let mut c = chacha20::ChaCha20Legacy::new(key[..32].into(), (&nonce).into());
        let mut poly_key = [0; 32];
        c.apply_keystream(&mut poly_key);
        c.seek(64);
        c.apply_keystream(&mut reference);
        let mut authenticated = rh.to_vec();
        authenticated.extend(&reference);
        let tag = poly1305::Poly1305::new((&poly_key).into()).compute_unpadded(&authenticated);
        let mut sealer = toyos_crypto::ssh::SealingKey::new(key, seq);
        let (s, ours_tag) = sealer.seal(&mut header, &mut ours).unwrap();
        assert_eq!(s, seq);
        assert_eq!(header, rh);
        assert_eq!(ours, reference);
        assert_eq!(ours_tag.as_slice(), tag.as_slice());
        let opener = toyos_crypto::ssh::OpeningKey::new(key);
        assert_eq!(opener.packet_length(seq, header), n as u32);
        opener.open(seq, &mut header, &mut ours, &ours_tag).unwrap();
        assert_eq!(ours, bytes::<4096>(n)[..n]);
    }
}
#[cfg(feature = "wpa2")]
#[test]
fn wpa2_pmk_and_rfc3394_unwrap() {
    fn hex<const N: usize>(s: &str) -> [u8; N] {
        core::array::from_fn(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
    }
    assert_eq!(
        toyos_crypto::wpa2::derive_pmk(b"password", b"IEEE")
            .unwrap()
            .as_bytes(),
        &hex::<32>("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
    );
    let key = toyos_crypto::wpa2::KeyEncryptionKey::new(hex("000102030405060708090a0b0c0d0e0f"));
    let ciphertext = hex::<24>("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    let mut out = [0; 16];
    key.unwrap(&ciphertext, &mut out).unwrap();
    assert_eq!(out, hex("00112233445566778899aabbccddeeff"));
    let mut bad = ciphertext;
    bad[0] ^= 1;
    assert!(key.unwrap(&bad, &mut out).is_err());
    assert_eq!(out, [0; 16]);
}

/// PKCS #1 v1.5 and PSS signatures ring makes with the committed test keys,
/// over each hash, verified; and refused for another message or a short PSS
/// signature. PSS's salt is ring's system randomness, which a verdict does
/// not depend on.
#[test]
fn rsa_reference_pkcs1_and_pss() {
    use ring::signature::{self, RsaKeyPair, RsaPublicKeyComponents};
    let random = ring::rand::SystemRandom::new();
    for der in [
        &include_bytes!("keys/rsa-2048.pk8")[..],
        include_bytes!("keys/rsa-4096.pk8"),
    ] {
        let private = RsaKeyPair::from_pkcs8(der).unwrap();
        let public: RsaPublicKeyComponents<Vec<u8>> = private.public().into();
        let ours = toyos_crypto::rsa::PublicKey::from_components(&public.n, &public.e).unwrap();
        let sign = |scheme: &'static dyn signature::RsaEncoding| {
            let mut signature = vec![0; private.public().modulus_len()];
            private.sign(scheme, &random, b"ToyOS", &mut signature).unwrap();
            signature
        };
        macro_rules! check {
            ($hash:ident, $pkcs1:ident, $pss:ident) => {
                let signature = sign(&signature::$pkcs1);
                ours.verify_pkcs1_v1_5(toyos_crypto::rsa::Hash::$hash, b"ToyOS", &signature)
                    .unwrap();
                assert!(
                    ours.verify_pkcs1_v1_5(toyos_crypto::rsa::Hash::$hash, b"changed", &signature)
                        .is_err()
                );
                let signature = sign(&signature::$pss);
                ours.verify_pss(toyos_crypto::rsa::Hash::$hash, b"ToyOS", &signature)
                    .unwrap();
                assert!(
                    ours.verify_pss(toyos_crypto::rsa::Hash::$hash, b"changed", &signature)
                        .is_err()
                );
                assert!(
                    ours.verify_pss(
                        toyos_crypto::rsa::Hash::$hash,
                        b"ToyOS",
                        &signature[..signature.len() - 1]
                    )
                    .is_err()
                );
            };
        }
        check!(Sha256, RSA_PKCS1_SHA256, RSA_PSS_SHA256);
        check!(Sha384, RSA_PKCS1_SHA384, RSA_PSS_SHA384);
        check!(Sha512, RSA_PKCS1_SHA512, RSA_PSS_SHA512);
    }
}

struct InputRandom {
    bytes: Vec<u8>,
    position: usize,
}
impl toyos_crypto::Random for InputRandom {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        let end = self.position + destination.len();
        let source = self
            .bytes
            .get(self.position..end)
            .ok_or(toyos_crypto::Error::Entropy)?;
        destination.copy_from_slice(source);
        self.position = end;
        Ok(())
    }
}
#[test]
fn mlkem_reference_keygen_encapsulation_and_implicit_rejection() {
    use ml_kem::{Decapsulate, KeyExport};
    for i in 0..8 {
        let d = bytes::<32>(i);
        let z = bytes::<32>(i + 7);
        let message = bytes::<32>(i + 13);
        let mut seed = d.to_vec();
        seed.extend(z);
        let oracle_private = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(
            seed.as_slice().try_into().unwrap(),
        );
        let oracle_public = oracle_private.encapsulation_key();
        let ours = toyos_crypto::mlkem::DecapsulationKey::generate(&mut InputRandom {
            bytes: seed.clone(),
            position: 0,
        })
        .unwrap();
        assert_eq!(
            ours.public_key().as_bytes().as_slice(),
            oracle_public.to_bytes().as_slice()
        );
        let (ct, shared) = ours
            .public_key()
            .encapsulate(&mut InputRandom {
                bytes: message.to_vec(),
                position: 0,
            })
            .unwrap();
        let (reference_ct, reference_shared) =
            oracle_public.encapsulate_deterministic(&message.into());
        assert_eq!(ct.as_slice(), reference_ct.as_slice());
        assert_eq!(shared.as_bytes().as_slice(), reference_shared.as_slice());
        assert_eq!(ours.decapsulate(&ct).unwrap().as_bytes(), shared.as_bytes());
        let mut bad = ct;
        bad[i] ^= 1;
        let ours = toyos_crypto::mlkem::DecapsulationKey::generate(&mut InputRandom {
            bytes: seed,
            position: 0,
        })
        .unwrap();
        let rejected = ours.decapsulate(&bad).unwrap();
        let reference = oracle_private.decapsulate(&bad.into());
        assert_eq!(rejected.as_bytes().as_slice(), reference.as_slice());
        assert_ne!(rejected.as_bytes(), shared.as_bytes());
    }
}
