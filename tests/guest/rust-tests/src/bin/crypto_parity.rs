//! `toyos-crypto` beside ring, on every hot-path operation a consumer calls,
//! sampled and judged by `toyos-parity`'s rule
//! (`issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`, stages
//! 2 and 3). ML-KEM, which ring lacks, is measured beside RustCrypto's `ml-kem`
//! and decides nothing.
//!
//! Ring is the oracle: each row is built only once ours has computed what ring
//! computes from the same inputs, and every open or rejection opens ring's
//! ciphertext. Every backend of a row does the same work from the same
//! inputs, A and B each on keys and buffers of their own. An open restores its
//! ciphertext before each operation, and the copy is in every backend's time.
//! A key agreement draws its ephemeral key from the system, as ring can only;
//! ours draws from the same source.
//!
//! The binary is built twice: in `[profile.toyos]`, as it ships, and as
//! `crypto_parity_unasserted` with debug assertions off, whose column decides
//! nothing.

use std::hint::black_box;
use std::time::Instant;

use ring::{aead, agreement, digest, hkdf, hmac, rand, signature};
use toyos_crypto::hash::{Sha256, Sha384, Sha512};
use toyos_crypto::kdf::{Hkdf, Hmac};
use toyos_parity::{Backend, Build, EndLine, IsaLine, Row, RowLine, SamplesLine, Summary};

#[path = "../arch/features.rs"]
mod features;

#[cfg(target_arch = "x86_64")]
#[path = "../probe_kernels.rs"]
mod kernels;

const KEY: [u8; 64] = [0x42; 64];
const IV: [u8; 12] = [0x24; 12];
/// A TLS 1.3 record header.
const AAD: [u8; 5] = [0x17, 0x03, 0x03, 0x40, 0x11];
const SIZES: [usize; 4] = [0, 64, 1024, 16384];
/// The payload of the packet every SSH implementation must handle (RFC 4253,
/// section 6.1).
const SSH_PACKET: usize = 32768;
const MESSAGE: [u8; 64] = [0x6d; 64];
const SEED: [u8; 32] = [0x5e; 32];

/// One row: ours, A and B, each running `ops` operations of its own.
struct Trio {
    name: String,
    run: [Box<dyn FnMut(u64)>; 3],
}

impl Row for Trio {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&mut self, backend: Backend, ops: u64) {
        (self.run[backend as usize])(ops)
    }
}

/// Hold `value` past the optimiser, which would otherwise drop the work that
/// made it.
fn keep<T>(value: T) {
    drop(black_box(value));
}

fn looped(mut op: impl FnMut() + 'static) -> Box<dyn FnMut(u64)> {
    Box::new(move |ops| (0..ops).for_each(|_| op()))
}

/// A row of `ours` beside two instances `reference` makes.
fn trio<R: FnMut() + 'static>(name: String, ours: impl FnMut() + 'static, reference: impl Fn() -> R) -> Box<dyn Row> {
    Box::new(Trio { name, run: [looped(ours), looped(reference()), looped(reference())] })
}

/// The system's random bytes, from where ring's `SystemRandom` takes them.
struct System;

impl toyos_crypto::Random for System {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        getrandom::getrandom(destination).map_err(|_| toyos_crypto::Error::Entropy)
    }
}

/// The same bytes on every fill, for ML-KEM's deterministic forms.
struct Fixed(&'static [u8]);

impl toyos_crypto::Random for Fixed {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        destination.copy_from_slice(&self.0[..destination.len()]);
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Tls {
    Aes128,
    Aes256,
    ChaCha,
}

impl Tls {
    fn name(self) -> &'static str {
        match self {
            Tls::Aes128 => "aes-128-gcm",
            Tls::Aes256 => "aes-256-gcm",
            Tls::ChaCha => "chacha20-poly1305",
        }
    }

    fn ring(self) -> aead::LessSafeKey {
        let (algorithm, key): (_, &[u8]) = match self {
            Tls::Aes128 => (&aead::AES_128_GCM, &KEY[..16]),
            Tls::Aes256 => (&aead::AES_256_GCM, &KEY[..32]),
            Tls::ChaCha => (&aead::CHACHA20_POLY1305, &KEY[..32]),
        };
        aead::LessSafeKey::new(aead::UnboundKey::new(algorithm, key).expect("an AEAD key"))
    }

    fn sealing(self) -> toyos_crypto::aead::SealingKey {
        use toyos_crypto::aead::SealingKey;
        match self {
            Tls::Aes128 => SealingKey::aes128_gcm(KEY[..16].try_into().unwrap(), IV),
            Tls::Aes256 => SealingKey::aes256_gcm(KEY[..32].try_into().unwrap(), IV),
            Tls::ChaCha => SealingKey::chacha20_poly1305(KEY[..32].try_into().unwrap(), IV),
        }
    }

    fn opening(self) -> toyos_crypto::aead::OpeningKey {
        use toyos_crypto::aead::OpeningKey;
        match self {
            Tls::Aes128 => OpeningKey::aes128_gcm(KEY[..16].try_into().unwrap()),
            Tls::Aes256 => OpeningKey::aes256_gcm(KEY[..32].try_into().unwrap()),
            Tls::ChaCha => OpeningKey::chacha20_poly1305(KEY[..32].try_into().unwrap()),
        }
    }
}

/// The nonce of record `sequence`, as TLS 1.3 forms it and ours does inside
/// its sealing key.
fn nonce(sequence: u64) -> aead::Nonce {
    let mut nonce = IV;
    for (n, s) in nonce[4..].iter_mut().zip(sequence.to_be_bytes()) {
        *n ^= s;
    }
    aead::Nonce::assume_unique_for_key(nonce)
}

/// Ring's record at sequence 0 and its tag, `forged` with one bit flipped.
fn ring_sealed(key: &aead::LessSafeKey, len: usize, forged: bool) -> (Vec<u8>, [u8; 16]) {
    let mut sealed = vec![0x70; len];
    let tag = key.seal_in_place_separate_tag(nonce(0), aead::Aad::from(AAD), &mut sealed).expect("seal");
    let mut tag: [u8; 16] = tag.as_ref().try_into().unwrap();
    tag[0] ^= u8::from(forged);
    (sealed, tag)
}

/// Ours seals at sequence 0 what ring does.
fn seals_as_ring(alg: Tls, len: usize) {
    let mut sealed = vec![0x70; len];
    let (nonce, tag) = alg.sealing().seal(&AAD, &mut sealed).expect("seal");
    assert_eq!(nonce, IV, "{} sequence 0's nonce", alg.name());
    assert_eq!((sealed, tag), ring_sealed(&alg.ring(), len, false), "{} seals {len} B as ring", alg.name());
}

fn tls_rows(alg: Tls, rows: &mut Vec<Box<dyn Row>>) {
    let name = alg.name();
    rows.push(trio(format!("{name}/key"), move || keep(alg.sealing()), move || {
        move || keep(alg.ring())
    }));
    for len in SIZES {
        seals_as_ring(alg, len);
        let mut key = alg.sealing();
        let mut buf = vec![0x70; len];
        rows.push(trio(
            format!("{name}/seal/{len}"),
            move || keep(key.seal(&AAD, &mut buf).expect("seal")),
            move || {
                let (key, mut buf, mut sequence) = (alg.ring(), vec![0x70; len], 0);
                move || {
                    let tag = key.seal_in_place_separate_tag(nonce(sequence), aead::Aad::from(AAD), &mut buf);
                    keep(tag.expect("seal"));
                    sequence += 1;
                }
            },
        ));
        for forged in [false, true] {
            let op = if forged { "reject" } else { "open" };
            let key = alg.opening();
            let (sealed, tag) = ring_sealed(&alg.ring(), len, forged);
            let mut work = sealed.clone();
            rows.push(trio(
                format!("{name}/{op}/{len}"),
                move || {
                    if !forged {
                        work.copy_from_slice(&sealed);
                    }
                    assert_eq!(key.open(&IV, &AAD, &mut work, &tag).is_err(), forged);
                },
                move || {
                    let key = alg.ring();
                    let (sealed, tag) = ring_sealed(&key, len, forged);
                    let mut work = sealed.clone();
                    move || {
                        if !forged {
                            work.copy_from_slice(&sealed);
                        }
                        let opened =
                            key.open_in_place_separate_tag(nonce(0), aead::Aad::from(AAD), tag.into(), &mut work, 0..);
                        assert_eq!(opened.is_err(), forged);
                    }
                },
            ));
        }
    }
}

/// `chacha20-poly1305@openssh.com`: the packet's length field, then its
/// payload.
fn ssh_rows(rows: &mut Vec<Box<dyn Row>>) {
    use ring::aead::chacha20_poly1305_openssh as ring_ssh;
    use toyos_crypto::ssh;
    const NAME: &str = "chacha20-poly1305-openssh";
    rows.push(trio(format!("{NAME}/key"), || keep(ssh::SealingKey::new(KEY, 0)), || {
        || keep(ring_ssh::SealingKey::new(&KEY))
    }));
    // The packet length a reader decrypts before each packet: one ChaCha20
    // block of the header key.
    let encrypted = [0x12, 0x34, 0x56, 0x78];
    assert_eq!(
        ssh::OpeningKey::new(KEY).packet_length(7, encrypted).to_be_bytes(),
        ring_ssh::OpeningKey::new(&KEY).decrypt_packet_length(7, encrypted),
        "{NAME} decrypts a packet length as ring"
    );
    let key = ssh::OpeningKey::new(KEY);
    rows.push(trio(format!("{NAME}/length"), move || keep(key.packet_length(7, black_box(encrypted))), || {
        let key = ring_ssh::OpeningKey::new(&KEY);
        move || keep(key.decrypt_packet_length(7, black_box(encrypted)))
    }));
    // Ring's packet at sequence 0, its length field first, and its tag,
    // `forged` with one bit flipped.
    let ring_sealed = |len: usize, forged: bool| {
        let mut packet = vec![0x70; 4 + len];
        packet[..4].copy_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
        let mut tag = [0; 16];
        ring_ssh::SealingKey::new(&KEY).seal_in_place(0, &mut packet, &mut tag);
        tag[0] ^= u8::from(forged);
        (packet, tag)
    };
    for len in SIZES.into_iter().chain([SSH_PACKET]) {
        let length = u32::try_from(len).unwrap().to_be_bytes();
        let (mut header, mut payload) = (length, vec![0x70; len]);
        let (sequence, tag) = ssh::SealingKey::new(KEY, 0).seal(&mut header, &mut payload).expect("seal");
        assert_eq!(([&header[..], &payload].concat(), tag), ring_sealed(len, false), "{NAME} seals {len} B as ring");
        assert_eq!(sequence, 0);
        let mut key = ssh::SealingKey::new(KEY, 0);
        let (mut header, mut payload) = (length, vec![0x70; len]);
        rows.push(trio(
            format!("{NAME}/seal/{len}"),
            move || keep(key.seal(&mut header, &mut payload).expect("seal")),
            move || {
                let key = ring_ssh::SealingKey::new(&KEY);
                let (mut packet, mut tag, mut sequence) = (vec![0x70; 4 + len], [0; 16], 0u32);
                packet[..4].copy_from_slice(&length);
                move || {
                    key.seal_in_place(sequence, &mut packet, &mut tag);
                    keep(&tag);
                    sequence = sequence.wrapping_add(1);
                }
            },
        ));
        for forged in [false, true] {
            let op = if forged { "reject" } else { "open" };
            let (packet, tag) = ring_sealed(len, forged);
            let (sealed_header, sealed): ([u8; 4], Vec<u8>) = (packet[..4].try_into().unwrap(), packet[4..].to_vec());
            let key = ssh::OpeningKey::new(KEY);
            let (mut header, mut work) = (sealed_header, sealed.clone());
            rows.push(trio(
                format!("{NAME}/{op}/{len}"),
                move || {
                    if !forged {
                        header = sealed_header;
                        work.copy_from_slice(&sealed);
                    }
                    keep(key.packet_length(0, header));
                    assert_eq!(key.open(0, &mut header, &mut work, &tag).is_err(), forged);
                },
                move || {
                    let (sealed, tag) = ring_sealed(len, forged);
                    let key = ring_ssh::OpeningKey::new(&KEY);
                    let mut work = sealed.clone();
                    move || {
                        if !forged {
                            work.copy_from_slice(&sealed);
                        }
                        keep(key.decrypt_packet_length(0, work[..4].try_into().unwrap()));
                        assert_eq!(key.open_in_place(0, &mut work, &tag).is_err(), forged);
                    }
                },
            ));
        }
    }
}

/// Ours digests, authenticates and derives what ring does from each row's
/// inputs.
fn hashes_as_ring() {
    for len in [64, 16384] {
        let message = vec![0x6d; len];
        assert_eq!(Sha256::digest(&message).unwrap().as_slice(), digest::digest(&digest::SHA256, &message).as_ref());
        assert_eq!(Sha512::digest(&message).unwrap().as_slice(), digest::digest(&digest::SHA512, &message).as_ref());
    }
    let (mut tag256, mut tag384) = ([0; 32], [0; 48]);
    Hmac::<Sha256>::authenticate(&KEY[..32], &MESSAGE, &mut tag256).unwrap();
    Hmac::<Sha384>::authenticate(&KEY[..48], &MESSAGE, &mut tag384).unwrap();
    assert_eq!(&tag256[..], hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &KEY[..32]), &MESSAGE).as_ref());
    assert_eq!(&tag384[..], hmac::sign(&hmac::Key::new(hmac::HMAC_SHA384, &KEY[..48]), &MESSAGE).as_ref());
    let (mut ours256, mut ours384, mut ring256, mut ring384) = ([0; 32], [0; 48], [0; 32], [0; 48]);
    Hkdf::<Sha256>::extract(&IV, &KEY[..32]).unwrap().expand(&[&MESSAGE[..16]], &mut ours256).unwrap();
    Hkdf::<Sha384>::extract(&IV, &KEY[..48]).unwrap().expand(&[&MESSAGE[..16]], &mut ours384).unwrap();
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &IV).extract(&KEY[..32]);
    prk.expand(&[&MESSAGE[..16]], hkdf::HKDF_SHA256).unwrap().fill(&mut ring256).unwrap();
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA384, &IV).extract(&KEY[..48]);
    prk.expand(&[&MESSAGE[..16]], hkdf::HKDF_SHA384).unwrap().fill(&mut ring384).unwrap();
    assert_eq!((ours256, ours384), (ring256, ring384), "hkdf");
}

fn hash_rows(rows: &mut Vec<Box<dyn Row>>) {
    hashes_as_ring();
    for len in [64, 16384] {
        let message = vec![0x6d; len];
        rows.push(trio(format!("sha-256/{len}"), move || keep(Sha256::digest(&message).unwrap()), move || {
            let message = vec![0x6d; len];
            move || keep(digest::digest(&digest::SHA256, &message))
        }));
        let message = vec![0x6d; len];
        rows.push(trio(format!("sha-512/{len}"), move || keep(Sha512::digest(&message).unwrap()), move || {
            let message = vec![0x6d; len];
            move || keep(digest::digest(&digest::SHA512, &message))
        }));
    }
    // A key schedule's HMAC: a fresh key over one short message.
    rows.push(trio(
        "hmac-sha-256/64".into(),
        || {
            let mut tag = [0; 32];
            Hmac::<Sha256>::authenticate(&KEY[..32], &MESSAGE, &mut tag).unwrap();
            keep(tag);
        },
        || || keep(hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &KEY[..32]), &MESSAGE)),
    ));
    rows.push(trio(
        "hmac-sha-384/64".into(),
        || {
            let mut tag = [0; 48];
            Hmac::<Sha384>::authenticate(&KEY[..48], &MESSAGE, &mut tag).unwrap();
            keep(tag);
        },
        || || keep(hmac::sign(&hmac::Key::new(hmac::HMAC_SHA384, &KEY[..48]), &MESSAGE)),
    ));
    // Extract, then expand one secret of the hash's length.
    rows.push(trio(
        "hkdf-sha-256/32".into(),
        || {
            let mut out = [0; 32];
            Hkdf::<Sha256>::extract(&IV, &KEY[..32]).unwrap().expand(&[&MESSAGE[..16]], &mut out).unwrap();
            keep(out);
        },
        || {
            || {
                let mut out = [0; 32];
                let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &IV).extract(&KEY[..32]);
                prk.expand(&[&MESSAGE[..16]], hkdf::HKDF_SHA256).unwrap().fill(&mut out).unwrap();
                keep(out);
            }
        },
    ));
    rows.push(trio(
        "hkdf-sha-384/48".into(),
        || {
            let mut out = [0; 48];
            Hkdf::<Sha384>::extract(&IV, &KEY[..48]).unwrap().expand(&[&MESSAGE[..16]], &mut out).unwrap();
            keep(out);
        },
        || {
            || {
                let mut out = [0; 48];
                let prk = hkdf::Salt::new(hkdf::HKDF_SHA384, &IV).extract(&KEY[..48]);
                prk.expand(&[&MESSAGE[..16]], hkdf::HKDF_SHA384).unwrap().fill(&mut out).unwrap();
                keep(out);
            }
        },
    ));
}

/// A key agreement as TLS and SSH make one: an ephemeral key drawn from the
/// system, its public key, and the secret it shares with the peer's.
fn agreement_rows(rows: &mut Vec<Box<dyn Row>>) {
    use toyos_crypto::{ecc, x25519};
    let peer = |alg| {
        let key = agreement::EphemeralPrivateKey::generate(alg, &rand::SystemRandom::new()).expect("a peer");
        key.compute_public_key().expect("its public key").as_ref().to_vec()
    };
    fn ring(alg: &'static agreement::Algorithm, peer: Vec<u8>) -> impl Fn() -> Box<dyn FnMut()> {
        move || {
            let (rng, peer) = (rand::SystemRandom::new(), peer.clone());
            Box::new(move || {
                let key = agreement::EphemeralPrivateKey::generate(alg, &rng).expect("a key");
                keep(key.compute_public_key().expect("its public key"));
                let peer = agreement::UnparsedPublicKey::new(alg, &peer);
                agreement::agree_ephemeral(key, &peer, |secret| black_box(secret[0])).expect("the agreement");
            })
        }
    }
    // Ours and ring agree on one secret: ours with ring's public key, ring
    // with ours's.
    fn agrees_with_ring(
        name: &str,
        alg: &'static agreement::Algorithm,
        ours: Vec<u8>,
        agree: impl FnOnce(&[u8]) -> Vec<u8>,
    ) {
        let key = agreement::EphemeralPrivateKey::generate(alg, &rand::SystemRandom::new()).expect("a key");
        let ring_public = key.compute_public_key().expect("its public key");
        let ours_secret = agree(ring_public.as_ref());
        let peer = agreement::UnparsedPublicKey::new(alg, &ours);
        let ring_secret = agreement::agree_ephemeral(key, &peer, <[u8]>::to_vec).expect("ring agrees with ours");
        assert_eq!(ours_secret, ring_secret, "{name}: ours and ring agree on one secret");
    }
    let key = x25519::SecretKey::generate(&mut System).expect("a key");
    agrees_with_ring("x25519", &agreement::X25519, key.public_key().0.to_vec(), |peer| {
        let peer = x25519::PublicKey(peer.try_into().unwrap());
        key.agree(&peer).expect("ours agrees with ring").as_bytes().to_vec()
    });
    let key = ecc::p256::SecretKey::generate(&mut System).expect("a key");
    agrees_with_ring("p-256", &agreement::ECDH_P256, key.public_key().as_sec1().to_vec(), |peer| {
        let peer = ecc::p256::PublicKey::from_sec1(peer).expect("ring's public key");
        key.agree(&peer).expect("ours agrees with ring").as_bytes().to_vec()
    });
    let key = ecc::p384::SecretKey::generate(&mut System).expect("a key");
    agrees_with_ring("p-384", &agreement::ECDH_P384, key.public_key().as_sec1().to_vec(), |peer| {
        let peer = ecc::p384::PublicKey::from_sec1(peer).expect("ring's public key");
        key.agree(&peer).expect("ours agrees with ring").as_bytes().to_vec()
    });

    let x = peer(&agreement::X25519);
    let public = x25519::PublicKey(x.as_slice().try_into().unwrap());
    rows.push(trio(
        "x25519/ecdh".into(),
        move || {
            let key = x25519::SecretKey::generate(&mut System).expect("a key");
            keep(key.public_key());
            keep(key.agree(&public).expect("the agreement").as_bytes()[0]);
        },
        ring(&agreement::X25519, x),
    ));
    let p256 = peer(&agreement::ECDH_P256);
    let encoded = p256.clone();
    rows.push(trio(
        "p-256/ecdh".into(),
        move || {
            let key = ecc::p256::SecretKey::generate(&mut System).expect("a key");
            keep(key.public_key());
            let peer = ecc::p256::PublicKey::from_sec1(&encoded).expect("the peer");
            keep(key.agree(&peer).expect("the agreement").as_bytes()[0]);
        },
        ring(&agreement::ECDH_P256, p256),
    ));
    let p384 = peer(&agreement::ECDH_P384);
    let encoded = p384.clone();
    rows.push(trio(
        "p-384/ecdh".into(),
        move || {
            let key = ecc::p384::SecretKey::generate(&mut System).expect("a key");
            keep(key.public_key());
            let peer = ecc::p384::PublicKey::from_sec1(&encoded).expect("the peer");
            keep(key.agree(&peer).expect("the agreement").as_bytes()[0]);
        },
        ring(&agreement::ECDH_P384, p384),
    ));
}

/// Signatures over [`MESSAGE`]; a verification parses its public key, as a
/// peer's arrives.
fn signature_rows(rows: &mut Vec<Box<dyn Row>>) {
    use signature::KeyPair;
    use toyos_crypto::{ecc, ed25519, rsa};
    let rng = rand::SystemRandom::new();

    let key = ed25519::SigningKey::from_seed(SEED);
    let pair = signature::Ed25519KeyPair::from_seed_unchecked(&SEED).expect("the seed");
    assert_eq!(key.public_key().as_bytes(), pair.public_key().as_ref(), "ed25519's public key");
    assert_eq!(&key.sign(&MESSAGE).expect("sign")[..], pair.sign(&MESSAGE).as_ref(), "ed25519 signs as ring");
    rows.push(trio("ed25519/sign/64".into(), move || keep(key.sign(&MESSAGE).expect("sign")), || {
        let pair = signature::Ed25519KeyPair::from_seed_unchecked(&SEED).expect("the seed");
        move || keep(pair.sign(&MESSAGE))
    }));
    let pair = signature::Ed25519KeyPair::from_seed_unchecked(&SEED).expect("the seed");
    let (public, sig) = (pair.public_key().as_ref().to_vec(), pair.sign(&MESSAGE).as_ref().to_vec());
    let ring_verify = |alg: &'static dyn signature::VerificationAlgorithm, public: Vec<u8>, sig: Vec<u8>| {
        move || {
            let (public, sig) = (public.clone(), sig.clone());
            move || signature::UnparsedPublicKey::new(alg, &public).verify(&MESSAGE, &sig).expect("verify")
        }
    };
    let (encoded, signed) = (public.clone().try_into().unwrap(), sig.clone());
    rows.push(trio(
        "ed25519/verify/64".into(),
        move || ed25519::PublicKey::from_bytes(encoded).expect("the key").verify(&MESSAGE, &signed).expect("verify"),
        ring_verify(&signature::ED25519, public, sig),
    ));

    let ecdsa = |alg| {
        let pkcs8 = signature::EcdsaKeyPair::generate_pkcs8(alg, &rng).expect("a key");
        let pair = signature::EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).expect("the key");
        (pair.public_key().as_ref().to_vec(), pair.sign(&rng, &MESSAGE).expect("sign").as_ref().to_vec())
    };
    let (public, sig) = ecdsa(&signature::ECDSA_P256_SHA256_ASN1_SIGNING);
    let (encoded, signed) = (public.clone(), sig.clone());
    rows.push(trio(
        "ecdsa-p-256-sha-256/verify/64".into(),
        move || ecc::p256::PublicKey::from_sec1(&encoded).expect("the key").verify(&MESSAGE, &signed).expect("verify"),
        ring_verify(&signature::ECDSA_P256_SHA256_ASN1, public, sig),
    ));
    let (public, sig) = ecdsa(&signature::ECDSA_P384_SHA384_ASN1_SIGNING);
    let (encoded, signed) = (public.clone(), sig.clone());
    rows.push(trio(
        "ecdsa-p-384-sha-384/verify/64".into(),
        move || ecc::p384::PublicKey::from_sec1(&encoded).expect("the key").verify(&MESSAGE, &signed).expect("verify"),
        ring_verify(&signature::ECDSA_P384_SHA384_ASN1, public, sig),
    ));

    // `toyos-crypto`'s own committed test keys, which its RSA differential
    // signs with.
    let keys: [(usize, &[u8]); 2] = [
        (2048, include_bytes!("../../../../../lib/crypto/tests/keys/rsa-2048.pk8")),
        (4096, include_bytes!("../../../../../lib/crypto/tests/keys/rsa-4096.pk8")),
    ];
    for (bits, pkcs8) in keys {
        let pair = signature::RsaKeyPair::from_pkcs8(pkcs8).expect("the key");
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = pair.public().into();
        let schemes: [(&str, &'static dyn signature::RsaEncoding, &'static signature::RsaParameters, bool); 2] = [
            ("pkcs1", &signature::RSA_PKCS1_SHA256, &signature::RSA_PKCS1_2048_8192_SHA256, false),
            ("pss", &signature::RSA_PSS_SHA256, &signature::RSA_PSS_2048_8192_SHA256, true),
        ];
        for (scheme, padding, verification, pss) in schemes {
            let mut sig = vec![0; pair.public().modulus_len()];
            pair.sign(padding, &rng, &MESSAGE, &mut sig).expect("sign");
            let (n, e, signed) = (public.n.clone(), public.e.clone(), sig.clone());
            let public = (public.n.clone(), public.e.clone());
            rows.push(trio(
                format!("rsa-{bits}-{scheme}-sha-256/verify/64"),
                move || {
                    let key = rsa::PublicKey::from_components(&n, &e).expect("the key");
                    let verified = if pss {
                        key.verify_pss(rsa::Hash::Sha256, &MESSAGE, &signed)
                    } else {
                        key.verify_pkcs1_v1_5(rsa::Hash::Sha256, &MESSAGE, &signed)
                    };
                    verified.expect("verify");
                },
                move || {
                    let ((n, e), sig) = (public.clone(), sig.clone());
                    move || {
                        let key = signature::RsaPublicKeyComponents { n: &n, e: &e };
                        key.verify(verification, &MESSAGE, &sig).expect("verify");
                    }
                },
            ));
        }
    }
}

/// ML-KEM-768 beside RustCrypto's `ml-kem`, on fixed seeds. Ours consumes its
/// decapsulation key, so a decapsulation includes the key's generation on
/// both sides.
fn ml_kem_rows(rows: &mut Vec<Box<dyn Row>>) {
    use ml_kem::{Decapsulate, MlKem768};
    use toyos_crypto::mlkem;
    const SEEDS: [u8; 64] = [0x3d; 64];
    const M: [u8; 32] = [0x4e; 32];
    let theirs = || ml_kem::DecapsulationKey::<MlKem768>::from_seed(SEEDS.into());
    rows.push(trio(
        "ml-kem-768/keygen".into(),
        || keep(mlkem::DecapsulationKey::generate(&mut Fixed(&SEEDS)).expect("a key")),
        move || move || keep(theirs()),
    ));
    let key = mlkem::DecapsulationKey::generate(&mut Fixed(&SEEDS)).expect("a key");
    let public = mlkem::EncapsulationKey::from_bytes(key.public_key().as_bytes()).expect("its public key");
    let (ciphertext, shared) = public.encapsulate(&mut Fixed(&M)).expect("encapsulate");
    // Ours against `ml-kem` on the same seeds, as the crate's differential
    // test holds it.
    {
        use ml_kem::KeyExport;
        let reference = theirs();
        assert_eq!(public.as_bytes().as_slice(), reference.encapsulation_key().to_bytes().as_slice(), "ml-kem's public key");
        let (reference_ciphertext, reference_shared) = reference.encapsulation_key().encapsulate_deterministic(&M.into());
        assert_eq!(ciphertext.as_slice(), reference_ciphertext.as_slice(), "ml-kem's ciphertext");
        assert_eq!(shared.as_bytes().as_slice(), reference_shared.as_slice(), "ml-kem's shared secret");
        assert_eq!(key.decapsulate(&ciphertext).expect("decapsulate").as_bytes(), shared.as_bytes());
    }
    rows.push(trio(
        "ml-kem-768/encapsulate".into(),
        move || keep(public.encapsulate(&mut Fixed(&M)).expect("encapsulate")),
        move || {
            let public = theirs().encapsulation_key().clone();
            move || keep(public.encapsulate_deterministic(&M.into()))
        },
    ));
    rows.push(trio(
        "ml-kem-768/keygen+decapsulate".into(),
        move || {
            let key = mlkem::DecapsulationKey::generate(&mut Fixed(&SEEDS)).expect("a key");
            keep(key.decapsulate(&ciphertext).expect("decapsulate").as_bytes()[0]);
        },
        move || move || keep(theirs().decapsulate(&ciphertext.into())),
    ));
}

/// Ours computes what ring computes at every length up to 1153 B, so that
/// every batch, threshold and tail of every instruction path the bench
/// admits runs against ring: AEAD seal, open and rejection with AAD of 0, 5,
/// 16, 17 and 33 B; the OpenSSH packet construction; SHA-2 and HMAC, the
/// latter with a key longer than a block too. A mismatch ends the job, and
/// with it the boot's rows.
fn agrees_with_ring_at_every_length() {
    use ring::aead::chacha20_poly1305_openssh as ring_ssh;
    use toyos_crypto::ssh;
    const MAX: usize = 1153;
    let source: Vec<u8> = (0..MAX + 64).map(|i| (i * 167 + 13) as u8).collect();
    let message = |len: usize| source[len % 61..][..len].to_vec();
    for alg in [Tls::Aes128, Tls::Aes256, Tls::ChaCha] {
        let ring_key = alg.ring();
        let opener = alg.opening();
        for aad_len in [0usize, 5, 16, 17, 33] {
            let aad = &source[7..7 + aad_len];
            for len in 0..=MAX {
                let plain = message(len);
                let mut ours = plain.clone();
                let (_, tag) = alg.sealing().seal(aad, &mut ours).expect("seal");
                let mut theirs = plain.clone();
                let ring_tag = ring_key
                    .seal_in_place_separate_tag(nonce(0), aead::Aad::from(aad), &mut theirs)
                    .expect("ring seals");
                assert_eq!((&ours, &tag[..]), (&theirs, ring_tag.as_ref()), "{} seals {len} B, AAD {aad_len} B, as ring", alg.name());
                let mut opened = theirs.clone();
                opener.open(&IV, aad, &mut opened, ring_tag.as_ref()).expect("ours opens ring's record");
                assert_eq!(opened, plain, "{} opens {len} B, AAD {aad_len} B", alg.name());
                let mut forged = ring_tag.as_ref().to_vec();
                forged[len % 16] ^= 1;
                let mut refused = theirs.clone();
                assert!(opener.open(&IV, aad, &mut refused, &forged).is_err(), "{} refuses a forged tag at {len} B", alg.name());
                assert_eq!(refused, theirs, "{} leaves a refused record as it was", alg.name());
            }
        }
    }
    let opener = ssh::OpeningKey::new(KEY);
    for len in 0..=MAX {
        let sequence = len as u32 * 7919;
        let plain = message(len);
        let length = u32::try_from(len).unwrap().to_be_bytes();
        let (mut header, mut ours) = (length, plain.clone());
        let (_, tag) = ssh::SealingKey::new(KEY, sequence).seal(&mut header, &mut ours).expect("seal");
        let mut packet = [&length[..], &plain].concat();
        let mut ring_tag = [0; 16];
        ring_ssh::SealingKey::new(&KEY).seal_in_place(sequence, &mut packet, &mut ring_tag);
        assert_eq!(([&header[..], &ours].concat(), tag), (packet.clone(), ring_tag), "OpenSSH seals {len} B as ring");
        let mut header: [u8; 4] = packet[..4].try_into().unwrap();
        let mut opened = packet[4..].to_vec();
        assert_eq!(opener.packet_length(sequence, header), len as u32);
        opener.open(sequence, &mut header, &mut opened, &ring_tag).expect("ours opens ring's packet");
        assert_eq!(opened, plain, "OpenSSH opens {len} B");
    }
    let (short, long) = (&source[..32], &source[..200]);
    for len in 0..=MAX {
        let data = message(len);
        assert_eq!(&Sha256::digest(&data).unwrap()[..], digest::digest(&digest::SHA256, &data).as_ref(), "SHA-256 of {len} B");
        assert_eq!(&Sha384::digest(&data).unwrap()[..], digest::digest(&digest::SHA384, &data).as_ref(), "SHA-384 of {len} B");
        assert_eq!(&Sha512::digest(&data).unwrap()[..], digest::digest(&digest::SHA512, &data).as_ref(), "SHA-512 of {len} B");
        for key in [short, long] {
            let (mut tag256, mut tag384) = ([0; 32], [0; 48]);
            Hmac::<Sha256>::authenticate(key, &data, &mut tag256).unwrap();
            Hmac::<Sha384>::authenticate(key, &data, &mut tag384).unwrap();
            assert_eq!(&tag256[..], hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &data).as_ref(), "HMAC-SHA-256 of {len} B");
            assert_eq!(&tag384[..], hmac::sign(&hmac::Key::new(hmac::HMAC_SHA384, key), &data).as_ref(), "HMAC-SHA-384 of {len} B");
        }
    }
}

fn main() {
    let build = if cfg!(debug_assertions) { Build::Asserted } else { Build::Unasserted };
    println!("{}", IsaLine(build, &features::detected()));
    agrees_with_ring_at_every_length();
    let mut rows = Vec::new();
    for alg in [Tls::Aes128, Tls::Aes256, Tls::ChaCha] {
        tls_rows(alg, &mut rows);
    }
    ssh_rows(&mut rows);
    hash_rows(&mut rows);
    agreement_rows(&mut rows);
    signature_rows(&mut rows);
    ml_kem_rows(&mut rows);
    probe_rows(&mut rows);

    let start = Instant::now();
    let times = toyos_parity::measure(&mut rows, &mut || start.elapsed().as_nanos() as u64);
    for (row, times) in rows.iter().zip(&times) {
        println!("{}", SamplesLine(build, row.name(), times));
        println!("{}", RowLine(build, &Summary::of(row.name(), times)));
    }
    println!("{}", EndLine(build, rows.len()));
}

/// A row of three explicit backends, each running `ops` operations.
fn probe(name: &str, run: [Box<dyn FnMut(u64)>; 3]) -> Box<dyn Row> {
    Box::new(Trio { name: name.into(), run })
}

/// The exploratory probe's rows (never lands): M1 per-temporary erasure
/// against boundary erasure, M2 kernels, M3 the system's random bytes, M4
/// X25519 split, M9 RSA split. In an M1 or M2 row ours is the candidate and
/// A and B are the baseline it is held against.
fn probe_rows(rows: &mut Vec<Box<dyn Row>>) {
    use toyos_crypto::{ecc, ed25519, rsa, x25519};
    use toyos_crypto_wiped as wiped;

    // M1: the same arithmetic, boundary erasure (ours) against per-temporary
    // erasure (A and B).
    let mut peer = [9u8; 32];
    peer[0] = 0x2a;
    let peer = x25519::PublicKey(peer);
    let wiped_peer = wiped::x25519::PublicKey(peer.0);
    assert_eq!(
        x25519::SecretKey::from_bytes(SEED).agree(&peer).unwrap().as_bytes(),
        wiped::x25519::SecretKey::from_bytes(SEED).agree(&wiped_peer).unwrap().as_bytes(),
        "the M1 variants agree"
    );
    let wiped_x25519 = move || -> Box<dyn FnMut(u64)> {
        Box::new(move |ops| {
            for _ in 0..ops {
                let key = wiped::x25519::SecretKey::generate(&mut WipedSystem).expect("a key");
                keep(key.public_key());
                keep(key.agree(&wiped_peer).expect("the agreement").as_bytes()[0]);
            }
        })
    };
    rows.push(probe(
        "m1/x25519/ecdh",
        [
            Box::new(move |ops| {
                for _ in 0..ops {
                    let key = x25519::SecretKey::generate(&mut System).expect("a key");
                    keep(key.public_key());
                    keep(key.agree(&peer).expect("the agreement").as_bytes()[0]);
                }
            }),
            wiped_x25519(),
            wiped_x25519(),
        ],
    ));
    let signing = ed25519::SigningKey::from_seed(SEED);
    let public = *signing.public_key().as_bytes();
    let signed = signing.sign(&MESSAGE).unwrap();
    let wiped_signing = wiped::ed25519::SigningKey::from_seed(SEED);
    assert_eq!(wiped_signing.sign(&MESSAGE).unwrap(), signed, "the M1 variants sign alike");
    let wiped_verify = move || -> Box<dyn FnMut(u64)> {
        Box::new(move |ops| {
            for _ in 0..ops {
                wiped::ed25519::PublicKey::from_bytes(public).unwrap().verify(&MESSAGE, &signed).unwrap();
            }
        })
    };
    rows.push(probe(
        "m1/ed25519/verify/64",
        [
            Box::new(move |ops| {
                for _ in 0..ops {
                    ed25519::PublicKey::from_bytes(public).unwrap().verify(&MESSAGE, &signed).unwrap();
                }
            }),
            wiped_verify(),
            wiped_verify(),
        ],
    ));
    let wiped_sign = || -> Box<dyn FnMut(u64)> {
        let key = wiped::ed25519::SigningKey::from_seed(SEED);
        Box::new(move |ops| {
            for _ in 0..ops {
                keep(key.sign(&MESSAGE).unwrap());
            }
        })
    };
    rows.push(probe(
        "m1/ed25519/sign/64",
        [
            Box::new(move |ops| {
                for _ in 0..ops {
                    keep(signing.sign(&MESSAGE).unwrap());
                }
            }),
            wiped_sign(),
            wiped_sign(),
        ],
    ));
    let p256_peer = ecc::p256::SecretKey::from_bytes([0x11; 32]).unwrap().public_key().as_sec1().to_vec();
    let wiped_p256 = |peer: Vec<u8>| -> Box<dyn FnMut(u64)> {
        Box::new(move |ops| {
            for _ in 0..ops {
                let key = wiped::ecc::p256::SecretKey::generate(&mut WipedSystem).expect("a key");
                keep(key.public_key());
                let peer = wiped::ecc::p256::PublicKey::from_sec1(&peer).expect("the peer");
                keep(key.agree(&peer).expect("the agreement").as_bytes()[0]);
            }
        })
    };
    let ours_peer = p256_peer.clone();
    rows.push(probe(
        "m1/p-256/ecdh",
        [
            Box::new(move |ops| {
                for _ in 0..ops {
                    let key = ecc::p256::SecretKey::generate(&mut System).expect("a key");
                    keep(key.public_key());
                    let peer = ecc::p256::PublicKey::from_sec1(&ours_peer).expect("the peer");
                    keep(key.agree(&peer).expect("the agreement").as_bytes()[0]);
                }
            }),
            wiped_p256(p256_peer.clone()),
            wiped_p256(p256_peer),
        ],
    ));

    // M3: the system's random bytes, ours through getrandom and ring's.
    for len in [32usize, 128] {
        let ring_fill = move || -> Box<dyn FnMut(u64)> {
            let rng = rand::SystemRandom::new();
            Box::new(move |ops| {
                let mut buffer = vec![0; len];
                for _ in 0..ops {
                    rand::SecureRandom::fill(&rng, &mut buffer).expect("random bytes");
                    keep(buffer[0]);
                }
            })
        };
        rows.push(probe(
            &format!("m3/random/{len}"),
            [
                Box::new(move |ops| {
                    let mut buffer = vec![0; len];
                    for _ in 0..ops {
                        getrandom::getrandom(&mut buffer).expect("random bytes");
                        keep(buffer[0]);
                    }
                }),
                ring_fill(),
                ring_fill(),
            ],
        ));
    }

    // M4: X25519's key generation and agreement apart.
    let ring_peer = {
        let key = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rand::SystemRandom::new()).unwrap();
        key.compute_public_key().unwrap().as_ref().to_vec()
    };
    let ours_peer = x25519::PublicKey(ring_peer.as_slice().try_into().unwrap());
    let ring_keygen = || -> Box<dyn FnMut(u64)> {
        let rng = rand::SystemRandom::new();
        Box::new(move |ops| {
            for _ in 0..ops {
                let key = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).expect("a key");
                keep(key.compute_public_key().expect("its public key"));
            }
        })
    };
    rows.push(probe(
        "m4/x25519/keygen",
        [
            Box::new(|ops| {
                for _ in 0..ops {
                    keep(x25519::SecretKey::generate(&mut System).expect("a key").public_key());
                }
            }),
            ring_keygen(),
            ring_keygen(),
        ],
    ));
    let ring_agree = |peer: Vec<u8>| -> Box<dyn FnMut(u64)> {
        let rng = rand::SystemRandom::new();
        Box::new(move |ops| {
            for _ in 0..ops {
                let key = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).expect("a key");
                let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, &peer);
                agreement::agree_ephemeral(key, &peer, |secret| black_box(secret[0])).expect("the agreement");
            }
        })
    };
    rows.push(probe(
        "m4/x25519/agree",
        [
            Box::new(move |ops| {
                for _ in 0..ops {
                    let key = x25519::SecretKey::generate(&mut System).expect("a key");
                    keep(key.agree(&ours_peer).expect("the agreement").as_bytes()[0]);
                }
            }),
            ring_agree(ring_peer.clone()),
            ring_agree(ring_peer),
        ],
    ));

    // M9: RSA-2048 key construction and exponentiation apart; ring's row is
    // whole.
    {
        let pair = signature::RsaKeyPair::from_pkcs8(include_bytes!("../../../../../lib/crypto/tests/keys/rsa-2048.pk8"))
            .expect("the key");
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = pair.public().into();
        let mut sig = vec![0; pair.public().modulus_len()];
        pair.sign(&signature::RSA_PKCS1_SHA256, &rand::SystemRandom::new(), &MESSAGE, &mut sig).expect("sign");
        let ring_whole = |n: Vec<u8>, e: Vec<u8>, sig: Vec<u8>| -> Box<dyn FnMut(u64)> {
            Box::new(move |ops| {
                for _ in 0..ops {
                    let key = signature::RsaPublicKeyComponents { n: &n, e: &e };
                    key.verify(&signature::RSA_PKCS1_2048_8192_SHA256, &MESSAGE, &sig).expect("verify");
                }
            })
        };
        let (n, e) = (public.n.clone(), public.e.clone());
        rows.push(probe(
            "m9/rsa-2048/key",
            [
                Box::new(move |ops| {
                    for _ in 0..ops {
                        keep(rsa::PublicKey::from_components(&n, &e).expect("the key"));
                    }
                }),
                ring_whole(public.n.clone(), public.e.clone(), sig.clone()),
                ring_whole(public.n.clone(), public.e.clone(), sig.clone()),
            ],
        ));
        let key = rsa::PublicKey::from_components(&public.n, &public.e).expect("the key");
        let signed = sig.clone();
        rows.push(probe(
            "m9/rsa-2048/exp",
            [
                Box::new(move |ops| {
                    for _ in 0..ops {
                        key.verify_pkcs1_v1_5(rsa::Hash::Sha256, &MESSAGE, &signed).expect("verify");
                    }
                }),
                ring_whole(public.n.clone(), public.e.clone(), sig.clone()),
                ring_whole(public.n.clone(), public.e.clone(), sig),
            ],
        ));
    }

    #[cfg(target_arch = "x86_64")]
    m2_rows(rows);
}

struct WipedSystem;

impl toyos_crypto_wiped::Random for WipedSystem {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto_wiped::Error> {
        getrandom::getrandom(destination).map_err(|_| toyos_crypto_wiped::Error::Entropy)
    }
}

/// M2: each multiplication kernel as u128, as mulx with addcarry intrinsics,
/// and as asm! with adcx/adox carry chains. Each kernel is first checked
/// against the u128 one on pseudorandom inputs.
#[cfg(target_arch = "x86_64")]
fn m2_rows(rows: &mut Vec<Box<dyn Row>>) {
    if !(std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("adx")) {
        println!("probe: no BMI2 and ADX; M2 rows skipped");
        return;
    }
    let mut seed = 0x0123_4567_89ab_cdefu64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // 2^255 - 19 results lie in [0, 2^256); compare them modulo p.
    let canonical = |x: [u64; 4]| {
        let p = [0xffff_ffff_ffff_ffedu64, u64::MAX, u64::MAX, 0x7fff_ffff_ffff_ffff];
        let mut x = x;
        for _ in 0..2 {
            let mut d = [0u64; 4];
            let mut borrow = false;
            for i in 0..4 {
                let (v, b1) = x[i].overflowing_sub(p[i]);
                let (v, b2) = v.overflowing_sub(u64::from(borrow));
                d[i] = v;
                borrow = b1 | b2;
            }
            if !borrow {
                x = d;
            }
        }
        x
    };
    let below_p256 = |mut x: [u64; 4]| {
        x[3] %= 0xffff_ffff_0000_0001;
        x
    };
    // A disagreeing kernel is reported and its rows skipped, so the boot's
    // table survives it.
    let mut wrong: Vec<&'static str> = Vec::new();
    fn agree<T: PartialEq>(wrong: &mut Vec<&'static str>, got: T, want: T, name: &'static str) {
        if got != want && !wrong.contains(&name) {
            wrong.push(name);
        }
    }
    for _ in 0..10_000 {
        let a = [next(), next(), next(), next()];
        let b = [next(), next(), next(), next()];
        let want = canonical(kernels::f25519_mul_u128(&a, &b));
        agree(&mut wrong, canonical(unsafe { kernels::f25519_mul_mulx(&a, &b) }), want, "25519 mulx");
        agree(&mut wrong, canonical(unsafe { kernels::f25519_mul_asm(&a, &b) }), want, "25519 asm");
        let (a, b) = (below_p256(a), below_p256(b));
        let want = kernels::p256_mul_u128(&a, &b);
        agree(&mut wrong, unsafe { kernels::p256_mul_mulx(&a, &b) }, want, "P-256 mulx");
        agree(&mut wrong, unsafe { kernels::p256_mul_asm(&a, &b) }, want, "P-256 asm");
    }
    let mut m = [0u64; kernels::W];
    for word in &mut m {
        *word = next();
    }
    m[0] |= 1;
    m[kernels::W - 1] |= 1 << 63;
    let mut inverse = 1u64;
    for _ in 0..6 {
        inverse = inverse.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inverse)));
    }
    let n0 = inverse.wrapping_neg();
    let y4 = [next(), next(), next(), next() >> 1];
    let x4 = [next(), next(), next(), next() >> 1];
    let mut residue = || {
        let mut x = [0u64; kernels::W];
        for word in &mut x {
            *word = next();
        }
        x[kernels::W - 1] >>= 1;
        x
    };
    for _ in 0..1000 {
        let (a, b) = (residue(), residue());
        let want = kernels::mont_u128(&a, &b, &m, n0);
        agree(&mut wrong, unsafe { kernels::mont_mulx(&a, &b, &m, n0) }, want, "2048-bit mulx");
        agree(&mut wrong, unsafe { kernels::mont_asm(&a, &b, &m, n0) }, want, "2048-bit asm");
    }
    if !wrong.is_empty() {
        println!("probe: M2 kernels disagree with their u128 form: {wrong:?}; M2 rows skipped");
        return;
    }
    println!("probe: every M2 kernel agrees with its u128 form");
    macro_rules! chain_row {
        ($name:expr, $ours:path, $base:path) => {
            rows.push(probe(
                $name,
                [
                    Box::new(move |ops| {
                        let mut x = x4;
                        unsafe { $ours(ops, &mut x, &y4) };
                        keep(x[0]);
                    }),
                    Box::new(move |ops| {
                        let mut x = x4;
                        #[allow(unused_unsafe)]
                        unsafe { $base(ops, &mut x, &y4) };
                        keep(x[0]);
                    }),
                    Box::new(move |ops| {
                        let mut x = x4;
                        #[allow(unused_unsafe)]
                        unsafe { $base(ops, &mut x, &y4) };
                        keep(x[0]);
                    }),
                ],
            ))
        };
    }
    chain_row!("m2/f25519-mul/mulx-over-u128", kernels::f25519_chain_mulx, kernels::f25519_chain_u128);
    chain_row!("m2/f25519-mul/asm-over-mulx", kernels::f25519_chain_asm, kernels::f25519_chain_mulx);
    chain_row!("m2/p256-mul/mulx-over-u128", kernels::p256_chain_mulx, kernels::p256_chain_u128);
    chain_row!("m2/p256-mul/asm-over-mulx", kernels::p256_chain_asm, kernels::p256_chain_mulx);
    let (x, y) = (residue(), residue());
    let mont = |variant: u8| -> Box<dyn FnMut(u64)> {
        Box::new(move |ops| {
            let mut v = x;
            kernels::mont_chain(variant, ops, &mut v, &y, &m, n0);
            keep(v[0]);
        })
    };
    rows.push(probe("m2/mont-2048/mulx-over-u128", [mont(1), mont(0), mont(0)]));
    rows.push(probe("m2/mont-2048/asm-over-mulx", [mont(2), mont(1), mont(1)]));
}
