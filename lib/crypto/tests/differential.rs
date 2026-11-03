//! The crate against ring and RustCrypto, dev-dependencies alone: length
//! and boundary sweeps, then [`CASES`] seeded cases per primitive, each
//! numbered so a red names it. A primitive with no instruction path is the
//! same code in every form, so it runs in the `portable` form alone.

mod support;

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
        0, 1, 15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65, 127, 128, 129, 191, 192, 193, 208,
        223, 224, 225, 255, 256, 257, 383, 384, 385, 447, 448, 449, 511, 512, 513, 575, 576, 577,
        703, 704, 705, 767, 768, 769, 959, 960, 961, 1023, 1024, 1025, 1087, 1088, 1089, 1151,
        1152, 1153, 1279, 1280, 1281, 1471, 1472, 1473, 1535, 1536, 1537, 1792, 1793, 4096, 4111,
        16384, 16400,
    ] {
        for a in [0, 1, 15, 16, 17, 31, 32, 33] {
            let k = bytes::<32>(n + a);
            let nonce = bytes::<12>(a);
            let aad = bytes::<33>(n);
            let aad = &aad[..a];
            let input = bytes::<16400>(a);
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
                    assert_eq!(ciphertext, vec![0; n], "a refusal zeroes the record");
                }
                opener.open(&nonce, aad, &mut ours, &tag).unwrap();
                assert_eq!(ours, input);
            }
        }
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

/// RFC 8017 §8.2.2 step 1: a signature is exactly the modulus's length. A
/// signature whose first byte is zero, without that byte or with another
/// before it, is the same integer, and refused.
#[test]
fn rsa_signature_is_the_modulus_length() {
    use ring::signature::{self, RsaKeyPair, RsaPublicKeyComponents};
    let private = RsaKeyPair::from_pkcs8(include_bytes!("keys/rsa-2048.pk8")).unwrap();
    let public: RsaPublicKeyComponents<Vec<u8>> = private.public().into();
    let ours = toyos_crypto::rsa::PublicKey::from_components(&public.n, &public.e).unwrap();
    let random = ring::rand::SystemRandom::new();
    // PKCS #1 v1.5 is deterministic: about one message in 256 signs to a
    // leading zero.
    let (message, sig) = (0u32..)
        .map(|i| {
            let mut sig = vec![0; private.public().modulus_len()];
            private.sign(&signature::RSA_PKCS1_SHA256, &random, &i.to_be_bytes(), &mut sig).unwrap();
            (i.to_be_bytes(), sig)
        })
        .find(|(_, sig)| sig[0] == 0)
        .unwrap();
    let verify = |sig: &[u8]| ours.verify_pkcs1_v1_5(toyos_crypto::rsa::Hash::Sha256, &message, sig);
    verify(&sig).unwrap();
    for changed in [sig[1..].to_vec(), [&[0][..], &sig].concat()] {
        assert_eq!(verify(&changed), Err(toyos_crypto::Error::Length), "{} bytes", changed.len());
        assert!(public.verify(&signature::RSA_PKCS1_2048_8192_SHA256, &message, &changed).is_err());
    }
}

#[test]
fn openssh_construction_matches_independent_primitives() {
    use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
    use poly1305::universal_hash::KeyInit;
    for n in [
        0, 1, 11, 12, 13, 15, 16, 17, 63, 64, 65, 128, 129, 255, 256, 257, 383, 384, 385, 447, 448,
        449, 511, 512, 513, 575, 576, 577, 588, 589, 767, 768, 769, 1024, 1087, 1088, 1089, 1100,
        1101, 4096,
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


/// Seeded cases per primitive.
const CASES: usize = 10_000;

/// A message length: mostly short, sometimes several blocks, now and then
/// past the vector paths' batch widths.
fn length(rng: &mut support::Rng) -> usize {
    match rng.below(10) {
        0..=5 => rng.below(129),
        6..=8 => rng.below(1025),
        _ => rng.below(4097),
    }
}

/// `data` cut at random points, as a streaming caller would hand it in.
fn pieces<'a>(rng: &mut support::Rng, mut data: &'a [u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let n = (rng.below(200) + 1).min(data.len());
        out.push(&data[..n]);
        data = &data[n..];
    }
    out
}

#[test]
fn seeded_sha2_against_ring() {
    use ring::digest;
    support::seeded(CASES, 0x5a2, |case, rng| {
        let n = length(rng);
        let message = rng.exactly(n);
        macro_rules! check {
            ($ours:ident, $ring:ident) => {
                let mut streamed = $ours::new();
                for piece in pieces(rng, &message) {
                    streamed.update(piece).unwrap();
                }
                let want = digest::digest(&digest::$ring, &message);
                assert_eq!(streamed.finalize().as_slice(), want.as_ref(), "case {case}");
                assert_eq!($ours::digest(&message).unwrap().as_slice(), want.as_ref(), "case {case}");
            };
        }
        check!(Sha256, SHA256);
        check!(Sha384, SHA384);
        check!(Sha512, SHA512);
    });
}

#[test]
fn seeded_hmac_and_hkdf_against_ring() {
    use ring::{hkdf, hmac};
    use toyos_crypto::kdf::{Hkdf, Hmac};
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    support::seeded(CASES, 0x4ac, |case, rng| {
        let key = rng.bytes(200);
        let n = length(rng);
        let message = rng.exactly(n);
        let (salt, info) = (rng.bytes(80), rng.bytes(80));
        macro_rules! check {
            ($ours:ident, $hmac:ident, $hkdf:ident, $len:literal) => {
                let want = hmac::sign(&hmac::Key::new(hmac::$hmac, &key), &message);
                let mut mac = Hmac::<$ours>::new(&key).unwrap();
                for piece in pieces(rng, &message) {
                    mac.update(piece).unwrap();
                }
                let mut tag = [0; $len];
                mac.finalize(&mut tag).unwrap();
                assert_eq!(tag.as_slice(), want.as_ref(), "case {case}");
                let verify = |tag: &[u8]| {
                    let mut mac = Hmac::<$ours>::new(&key).unwrap();
                    mac.update(&message).unwrap();
                    mac.verify(tag)
                };
                assert_eq!(verify(&tag), Ok(()), "case {case}");
                tag[rng.below($len)] ^= 1 << rng.below(8);
                assert!(verify(&tag).is_err(), "case {case}: a changed tag verified");

                let n = rng.below(255 * $len) + 1;
                let mut want = vec![0; n];
                hkdf::Salt::new(hkdf::$hkdf, &salt).extract(&message).expand(&[&info], Len(n)).unwrap().fill(&mut want).unwrap();
                let mut out = vec![0; n];
                Hkdf::<$ours>::extract(&salt, &message).unwrap().expand(&[&info], &mut out).unwrap();
                assert_eq!(out, want, "case {case}: {n} bytes");
            };
        }
        check!(Sha256, HMAC_SHA256, HKDF_SHA256, 32);
        check!(Sha384, HMAC_SHA384, HKDF_SHA384, 48);
        check!(Sha512, HMAC_SHA512, HKDF_SHA512, 64);
    });
}

#[test]
fn seeded_aead_against_ring_and_rustcrypto() {
    use ring::aead;
    support::seeded(CASES, 0xae4d, |case, rng| {
        let key: [u8; 32] = rng.array();
        let nonce: [u8; 12] = rng.array();
        let n = rng.below(3) * 40;
        let aad = rng.exactly(n);
        let n = length(rng);
        let message = rng.exactly(n);
        let (algorithm, ring_algorithm, ours, opener): (_, _, SealingKey, OpeningKey) = match case % 3 {
            0 => (0, &aead::AES_128_GCM, SealingKey::aes128_gcm(key[..16].try_into().unwrap(), nonce), OpeningKey::aes128_gcm(key[..16].try_into().unwrap())),
            1 => (1, &aead::AES_256_GCM, SealingKey::aes256_gcm(key, nonce), OpeningKey::aes256_gcm(key)),
            _ => (2, &aead::CHACHA20_POLY1305, SealingKey::chacha20_poly1305(key, nonce), OpeningKey::chacha20_poly1305(key)),
        };
        let mut ours = ours;
        let ring_key = aead::LessSafeKey::new(aead::UnboundKey::new(ring_algorithm, &key[..ring_algorithm.key_len()]).unwrap());
        let mut want = message.clone();
        let want_tag = ring_key.seal_in_place_separate_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(&aad), &mut want).unwrap();
        let mut sealed = message.clone();
        let (_, tag) = ours.seal(&aad, &mut sealed).unwrap();
        assert_eq!((sealed.as_slice(), tag.as_slice()), (want.as_slice(), want_tag.as_ref()), "case {case}");
        let mut rustcrypto = message.clone();
        let rustcrypto_tag = match algorithm {
            0 => aes_gcm::Aes128Gcm::new_from_slice(&key[..16]).unwrap().encrypt_in_place_detached((&nonce).into(), &aad, &mut rustcrypto).unwrap(),
            1 => aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap().encrypt_in_place_detached((&nonce).into(), &aad, &mut rustcrypto).unwrap(),
            _ => chacha20poly1305::ChaCha20Poly1305::new_from_slice(&key).unwrap().encrypt_in_place_detached((&nonce).into(), &aad, &mut rustcrypto).unwrap(),
        };
        assert_eq!((rustcrypto.as_slice(), rustcrypto_tag.as_slice()), (want.as_slice(), want_tag.as_ref()), "case {case}");

        // One change to ciphertext, tag or AAD, which both refuse.
        let (mut ct, mut bad_tag, mut bad_aad) = (sealed.clone(), tag, aad.clone());
        match rng.below(3) {
            0 if !ct.is_empty() => {
                let i = rng.below(ct.len());
                ct[i] ^= 1 << rng.below(8);
            }
            1 if !bad_aad.is_empty() => {
                let i = rng.below(bad_aad.len());
                bad_aad[i] ^= 1 << rng.below(8);
            }
            _ => bad_tag[rng.below(16)] ^= 1 << rng.below(8),
        }
        let changed = ct.clone();
        assert!(opener.open(&nonce, &bad_aad, &mut ct, &bad_tag).is_err(), "case {case}: a changed record opened");
        assert_eq!(ct, vec![0; changed.len()], "case {case}: a refusal left more than zeros");
        let mut ring_ct = [changed.as_slice(), &bad_tag].concat();
        assert!(ring_key.open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(&bad_aad), &mut ring_ct).is_err(), "case {case}");
        opener.open(&nonce, &aad, &mut sealed, &tag).unwrap();
        assert_eq!(sealed, message, "case {case}");
    });
}

#[test]
fn seeded_openssh_against_rustcrypto() {
    use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
    use poly1305::universal_hash::KeyInit;
    support::seeded(CASES, 0x55a, |case, rng| {
        let key: [u8; 64] = rng.array();
        let seq = rng.next() as u32;
        let n = length(rng);
        let message = rng.exactly(n);
        let nonce = u64::from(seq).to_be_bytes();
        let mut header = (message.len() as u32).to_be_bytes();
        let mut want_header = header;
        chacha20::ChaCha20Legacy::new(key[32..].into(), (&nonce).into()).apply_keystream(&mut want_header);
        let mut payload = chacha20::ChaCha20Legacy::new(key[..32].into(), (&nonce).into());
        let mut poly_key = [0; 32];
        payload.apply_keystream(&mut poly_key);
        payload.seek(64);
        let mut want = message.clone();
        payload.apply_keystream(&mut want);
        let want_tag = poly1305::Poly1305::new((&poly_key).into()).compute_unpadded(&[want_header.as_slice(), &want].concat());
        let mut sealed = message.clone();
        let (_, tag) = toyos_crypto::ssh::SealingKey::new(key, seq).seal(&mut header, &mut sealed).unwrap();
        assert_eq!((header, sealed.as_slice(), tag.as_slice()), (want_header, want.as_slice(), want_tag.as_slice()), "case {case}");
        let opener = toyos_crypto::ssh::OpeningKey::new(key);
        assert_eq!(opener.packet_length(seq, header) as usize, message.len(), "case {case}");
        let mut bad = tag;
        bad[rng.below(16)] ^= 1 << rng.below(8);
        let (mut h, mut p) = (header, sealed.clone());
        assert!(opener.open(seq, &mut h, &mut p, &bad).is_err(), "case {case}");
        assert_eq!((h, p), (header, vec![0; n]), "case {case}: a refusal left more than the header and zeros");
        opener.open(seq, &mut header, &mut sealed, &tag).unwrap();
        assert_eq!(sealed, message, "case {case}");
    });
}

/// Random scalars against random u-coordinates, the high bit and values from
/// p up included: RFC 7748 takes them all.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_x25519_against_dalek() {
    use toyos_crypto::x25519::{PublicKey, SecretKey};
    support::seeded(CASES, 0x255, |case, rng| {
        let (scalar, u): ([u8; 32], [u8; 32]) = (rng.array(), rng.array());
        let want = x25519_dalek::x25519(scalar, u);
        let agreed = SecretKey::from_bytes(scalar).agree(&PublicKey(u));
        if want == [0; 32] {
            assert!(agreed.is_err(), "case {case}");
        } else {
            assert_eq!(agreed.unwrap().as_bytes(), &want, "case {case}");
        }
        assert_eq!(SecretKey::from_bytes(scalar).public_key().0, x25519_dalek::x25519(scalar, x25519_dalek::X25519_BASEPOINT_BYTES), "case {case}");
    });
}

/// Ring's and dalek's signatures, verified and changed: a flipped bit, S
/// raised by the order, another message. On these RFC 8032 keys ours agrees
/// with `verify_strict` and with ring, and a random encoding decodes where
/// dalek's does: y of p or more, where they part, is too rare to draw.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_ed25519_against_ring_and_dalek() {
    use ring::signature::{self, KeyPair};
    let order = num_bigint::BigUint::parse_bytes(b"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed", 16).unwrap();
    support::seeded(CASES, 0xed, |case, rng| {
        let seed: [u8; 32] = rng.array();
        let n = length(rng) / 4;
        let message = rng.exactly(n);
        let pair = signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let public: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
        let mut sig: [u8; 64] = pair.sign(&message).as_ref().try_into().unwrap();
        let dalek = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(dalek.verifying_key().to_bytes(), public, "case {case}");
        #[cfg(feature = "sign")]
        {
            let ours = toyos_crypto::ed25519::SigningKey::from_seed(seed);
            assert_eq!(ours.public_key().as_bytes(), &public, "case {case}");
            assert_eq!(ours.sign(&message).unwrap(), sig, "case {case}");
        }
        let mut message = message;
        match case % 4 {
            0 => {}
            1 => sig[rng.below(64)] ^= 1 << rng.below(8),
            2 => {
                let s = num_bigint::BigUint::from_bytes_le(&sig[32..]) + &order;
                let bytes = s.to_bytes_le();
                if bytes.len() <= 32 {
                    sig[32..32 + bytes.len()].copy_from_slice(&bytes);
                }
            }
            _ => message.push(rng.next() as u8),
        }
        let key = toyos_crypto::ed25519::PublicKey::from_bytes(public).unwrap();
        let ours = key.verify(&message, &sig).is_ok();
        let strict = ed25519_dalek::VerifyingKey::from_bytes(&public).unwrap().verify_strict(&message, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok();
        let ring = signature::UnparsedPublicKey::new(&signature::ED25519, public).verify(&message, &sig).is_ok();
        assert_eq!(ours, strict, "case {case}, change {}: dalek's verify_strict", case % 4);
        assert_eq!(ours, ring, "case {case}, change {}: ring", case % 4);
        assert_eq!(ours, case % 4 == 0, "case {case}");

        let encoding: [u8; 32] = rng.array();
        let decoded = toyos_crypto::ed25519::PublicKey::from_bytes(encoding).is_ok();
        let dalek = ed25519_dalek::VerifyingKey::from_bytes(&encoding).is_ok_and(|k| !k.is_weak());
        assert_eq!(decoded, dalek, "case {case}: decoding {encoding:02x?}");
    });
}

/// One case of key derivation and agreement beside RustCrypto's, and
/// verification of its deterministic signature, changed or not, beside
/// ring's verifier.
macro_rules! nist_case {
    ($curve:ident, $bytes:literal, $ring:ident, $case:expr, $rng:expr) => {{
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        use p256::ecdsa::signature::Signer as _;
        use ring::signature;
        let (case, rng): (usize, &mut support::Rng) = ($case, $rng);
        let (a, b) = loop {
            let (a, b): ([u8; $bytes], [u8; $bytes]) = (rng.array(), rng.array());
            if let (Ok(a), Ok(b)) = ($curve::SecretKey::from_slice(&a), $curve::SecretKey::from_slice(&b)) {
                break (a, b);
            }
        };
        let ours = toyos_crypto::ecc::$curve::SecretKey::from_bytes(a.to_bytes().into()).unwrap();
        let a_public = a.public_key().to_encoded_point(false);
        assert_eq!(ours.public_key().as_sec1().as_slice(), a_public.as_bytes(), "case {case}");
        let b_public = b.public_key().to_encoded_point(false);
        let peer = toyos_crypto::ecc::$curve::PublicKey::from_sec1(b_public.as_bytes()).unwrap();
        let want = $curve::ecdh::diffie_hellman(a.to_nonzero_scalar(), b.public_key().as_affine());
        assert_eq!(ours.agree(&peer).unwrap().as_bytes().as_slice(), want.raw_secret_bytes().as_slice(), "case {case}");

        let mut message = rng.bytes(64);
        let signed: $curve::ecdsa::Signature = $curve::ecdsa::SigningKey::from(&a).sign(&message);
        let mut der = signed.to_der().as_bytes().to_vec();
        match case % 4 {
            0 => {}
            1 => {
                let i = rng.below(der.len());
                der[i] ^= 1 << rng.below(8);
            }
            2 => der = signed.normalize_s().unwrap_or(signed).to_der().as_bytes().to_vec(),
            _ => message.push(0),
        }
        let key = toyos_crypto::ecc::$curve::PublicKey::from_sec1(a_public.as_bytes()).unwrap();
        let ours = key.verify(&message, &der).is_ok();
        let ring = signature::UnparsedPublicKey::new(&signature::$ring, a_public.as_bytes()).verify(&message, &der).is_ok();
        assert_eq!(ours, ring, "case {case}, change {}: {der:02x?}", case % 4);
        if case % 4 == 0 || case % 4 == 2 {
            assert!(ours, "case {case}");
        }
    }};
}

#[test]
fn seeded_p256_against_rustcrypto_and_ring() {
    support::seeded(CASES, 0x9256, |case, rng| nist_case!(p256, 32, ECDSA_P256_SHA256_ASN1, case, rng));
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_p384_against_rustcrypto_and_ring() {
    support::seeded(CASES, 0x9384, |case, rng| nist_case!(p384, 48, ECDSA_P384_SHA384_ASN1, case, rng));
}

/// Signatures ring makes with the committed keys, changed or not, verified
/// by ours and by ring. Signing is the slow side, so each case changes one
/// of a pool's signatures.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_rsa_against_ring() {
    use ring::signature::{self, RsaKeyPair, RsaPublicKeyComponents};
    use toyos_crypto::rsa::Hash;
    const POOL: usize = 16;
    let random = ring::rand::SystemRandom::new();
    let mut rng = support::Rng(0x45a);
    let schemes: [(Hash, bool, &'static dyn signature::RsaEncoding, &'static signature::RsaParameters); 6] = [
        (Hash::Sha256, false, &signature::RSA_PKCS1_SHA256, &signature::RSA_PKCS1_2048_8192_SHA256),
        (Hash::Sha384, false, &signature::RSA_PKCS1_SHA384, &signature::RSA_PKCS1_2048_8192_SHA384),
        (Hash::Sha512, false, &signature::RSA_PKCS1_SHA512, &signature::RSA_PKCS1_2048_8192_SHA512),
        (Hash::Sha256, true, &signature::RSA_PSS_SHA256, &signature::RSA_PSS_2048_8192_SHA256),
        (Hash::Sha384, true, &signature::RSA_PSS_SHA384, &signature::RSA_PSS_2048_8192_SHA384),
        (Hash::Sha512, true, &signature::RSA_PSS_SHA512, &signature::RSA_PSS_2048_8192_SHA512),
    ];
    let keys = [&include_bytes!("keys/rsa-2048.pk8")[..], include_bytes!("keys/rsa-4096.pk8")].map(|der| {
        let private = RsaKeyPair::from_pkcs8(der).unwrap();
        let public: RsaPublicKeyComponents<Vec<u8>> = private.public().into();
        let ours = toyos_crypto::rsa::PublicKey::from_components(&public.n, &public.e).unwrap();
        let pool: Vec<Vec<(Vec<u8>, Vec<u8>)>> = schemes
            .iter()
            .map(|(_, _, encoding, _)| {
                (0..POOL)
                    .map(|_| {
                        let message = rng.bytes(100);
                        let mut sig = vec![0; private.public().modulus_len()];
                        private.sign(*encoding, &random, &message, &mut sig).unwrap();
                        (message, sig)
                    })
                    .collect()
            })
            .collect();
        (public, ours, pool)
    });
    support::seeded(CASES, 0x45a1, |case, rng| {
        let (public, ours, pool) = &keys[case % 2];
        let scheme = rng.below(schemes.len());
        let (hash, pss, _, parameters) = schemes[scheme];
        let (message, sig) = &pool[scheme][rng.below(POOL)];
        let (mut message, mut sig) = (message.clone(), sig.clone());
        let change = rng.below(6);
        match change {
            0 => {}
            1 => {
                let i = rng.below(sig.len());
                sig[i] ^= 1 << rng.below(8);
            }
            2 => message.push(1),
            3 => sig = public.n.clone(),
            4 => {
                sig.pop();
            }
            _ => rng.fill(&mut sig[1..]),
        }
        let verdict = if pss { ours.verify_pss(hash, &message, &sig) } else { ours.verify_pkcs1_v1_5(hash, &message, &sig) };
        let ring = public.verify(parameters, &message, &sig).is_ok();
        assert_eq!(verdict.is_ok(), ring, "case {case}, scheme {scheme}, change {change}");
        assert_eq!(ring, change == 0, "case {case}");
    });
}

/// Keys from seeds, deterministic encapsulation, and decapsulation of the
/// ciphertext and of a changed one, whose implicit rejection must agree.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_mlkem768_against_rustcrypto() {
    use ml_kem::{Decapsulate, KeyExport};
    use support::Given;
    support::seeded(CASES, 0x768, |case, rng| {
        let seed: [u8; 64] = rng.array();
        let message: [u8; 32] = rng.array();
        let oracle = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(seed.into());
        let oracle_public = oracle.encapsulation_key();
        let ours = toyos_crypto::mlkem::DecapsulationKey::generate(&mut Given::new(seed)).unwrap();
        assert_eq!(ours.public_key().as_bytes().as_slice(), oracle_public.to_bytes().as_slice(), "case {case}");
        let (mut ct, shared) = ours.public_key().encapsulate(&mut Given::new(message)).unwrap();
        let (want_ct, want_shared) = oracle_public.encapsulate_deterministic(&message.into());
        assert_eq!((ct.as_slice(), shared.as_bytes().as_slice()), (want_ct.as_slice(), want_shared.as_slice()), "case {case}");
        if case % 2 == 1 {
            ct[rng.below(ct.len())] ^= 1 << rng.below(8);
        }
        let decapsulated = ours.decapsulate(&ct).unwrap();
        assert_eq!(decapsulated.as_bytes().as_slice(), oracle.decapsulate(&ct.into()).as_slice(), "case {case}");
        assert_eq!(decapsulated.as_bytes() == shared.as_bytes(), case % 2 == 0, "case {case}");
    });
}

#[cfg(feature = "wpa2")]
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn seeded_wpa2_against_ring() {
    use ring::{hmac, pbkdf2};
    use toyos_crypto::wpa2;
    support::seeded(CASES, 0x802, |case, rng| {
        let key = rng.bytes(100);
        let n = length(rng);
        let message = rng.exactly(n);
        let want = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &key), &message);
        let parts = pieces(rng, &message);
        assert_eq!(wpa2::hmac_sha1(&key, &parts).unwrap().as_slice(), want.as_ref(), "case {case}");

        // IEEE 802.11-2020 §12.7.1.2: HMAC-SHA1(K, A || 0 || B || i), i from 0.
        let (label, data) = (rng.bytes(30), rng.bytes(100));
        let mut out = vec![0; rng.below(129)];
        wpa2::prf(&key, &label, &data, &mut out).unwrap();
        let mut want = Vec::new();
        for i in 0..out.len().div_ceil(20) {
            let block = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &key), &[label.as_slice(), &[0], &data, &[i as u8]].concat());
            want.extend_from_slice(block.as_ref());
        }
        assert_eq!(out, want[..out.len()], "case {case}");
    });
    // Each PMK is 8,192 HMAC-SHA1 compressions on both sides.
    support::seeded(PMK_CASES, 0x4096, |case, rng| {
        let passphrase = rng.bytes(55).into_iter().chain([b'x'; 8]).collect::<Vec<_>>();
        let ssid = [rng.bytes(31), vec![b's']].concat();
        let mut want = [0; 32];
        pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA1, 4096.try_into().unwrap(), &ssid, &passphrase, &mut want);
        assert_eq!(wpa2::derive_pmk(&passphrase, &ssid).unwrap().as_bytes(), &want, "PMK case {case}");
    });
}

/// PMK derivations, each 4,096 iterations of the HMAC above.
#[cfg(feature = "wpa2")]
const PMK_CASES: usize = 200;

#[test]
fn seeded_pbkdf2_against_ring() {
    use ring::pbkdf2;
    use toyos_crypto::kdf::pbkdf2;
    support::seeded(CASES / 10, 0x8018, |case, rng| {
        let (password, salt) = (rng.bytes(200), rng.bytes(80));
        let iterations = (rng.below(40) + 1) as u32;
        let n = rng.below(300) + 1;
        macro_rules! check {
            ($ours:ident, $ring:ident) => {
                let mut want = vec![0; n];
                pbkdf2::derive(pbkdf2::$ring, iterations.try_into().unwrap(), &salt, &password, &mut want);
                let mut out = vec![0; n];
                pbkdf2::<$ours>(&password, &salt, iterations.try_into().unwrap(), &mut out).unwrap();
                assert_eq!(out, want, "case {case}: {iterations} iterations, {n} bytes");
            };
        }
        check!(Sha256, PBKDF2_HMAC_SHA256);
        check!(Sha384, PBKDF2_HMAC_SHA384);
        check!(Sha512, PBKDF2_HMAC_SHA512);
    });
}

const ANY: toyos_crypto::scrypt::Limits = toyos_crypto::scrypt::Limits { memory: usize::MAX, work: u64::MAX };

/// scrypt's, and so Salsa20/8's, BlockMix's and ROMix's, against RustCrypto's.
fn scrypt_case(passphrase: &[u8], salt: &[u8], log_n: u8, r: u32, p: u32, n: usize) -> Result<(), String> {
    let params = toyos_crypto::scrypt::Params::new(log_n, r, p, ANY).unwrap();
    let mut workspace = vec![0; params.workspace_len()];
    let mut out = vec![0; n];
    toyos_crypto::scrypt::derive(passphrase, salt, params, &mut workspace, &mut out).unwrap();
    let mut want = vec![0; n];
    scrypt::scrypt(passphrase, salt, &scrypt::Params::new(log_n, r, p).unwrap(), &mut want).unwrap();
    if out == want { Ok(()) } else { Err(format!("N = 2^{log_n}, r = {r}, p = {p}, {n} bytes")) }
}

#[test]
fn seeded_scrypt_against_rustcrypto() {
    support::seeded(CASES / 10, 0x7914, |case, rng| {
        let (passphrase, salt) = (rng.bytes(100), rng.bytes(40));
        let log_n = (rng.below(8) + 1) as u8;
        let (r, p) = ((rng.below(8) + 1) as u32, (rng.below(4) + 1) as u32);
        let n = rng.below(200) + 1;
        if let Err(e) = scrypt_case(&passphrase, &salt, log_n, r, p, n) {
            panic!("case {case}: {e}");
        }
    });
}

/// Upstream bcachefs's default cost, N = 2^14, r = 8 and p = 16, and the
/// smallest, N = 2, r = 1 and p = 1, over a 64-character hex passphrase and
/// the format's salt.
#[test]
fn scrypt_at_the_bcachefs_costs() {
    let passphrase = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    scrypt_case(passphrase, b"bcache\0", 14, 8, 16, 32).unwrap();
    scrypt_case(passphrase, b"bcache\0", 1, 1, 1, 32).unwrap();
}
