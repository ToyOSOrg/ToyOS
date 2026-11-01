use super::*;
use alloc::boxed::Box;
use rustls::ProtocolVersion;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection,
    crypto::{GetRandomFailed, SecureRandom, cipher::*},
    version::{TLS12, TLS13},
};
use std::io::{Cursor, Read, Write};
#[derive(Debug)]
struct Entropy;
impl SecureRandom for Entropy {
    fn fill(&self, out: &mut [u8]) -> Result<(), GetRandomFailed> {
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(out)
    }
}
static BACKEND: Backend = Backend::new(&Entropy);
fn key() -> AeadKey {
    AeadKey::from([7; 32])
}
#[test]
fn record_adapters_interoperate_and_refuse_reused_sequences() {
    for suite in BACKEND.provider().cipher_suites {
        let (mut seal, mut open, explicit, version13, len) = match suite {
            SupportedCipherSuite::Tls13(a) => (
                a.aead_alg.encrypter(key(), Iv::from([3; 12])),
                a.aead_alg.decrypter(key(), Iv::from([3; 12])),
                false,
                true,
                a.aead_alg.key_len(),
            ),
            SupportedCipherSuite::Tls12(a) => {
                let shape = a.aead_alg.key_block_shape();
                let iv = &[3; 12][..shape.fixed_iv_len];
                let extra = &[5; 8][..shape.explicit_nonce_len];
                (
                    a.aead_alg.encrypter(key(), iv, extra),
                    a.aead_alg.decrypter(key(), iv),
                    shape.explicit_nonce_len != 0,
                    false,
                    shape.enc_key_len,
                )
            }
        };
        let algorithm = if len == 16 {
            &ring::aead::AES_128_GCM
        } else if matches!(
            suite.suite(),
            CipherSuite::TLS13_AES_256_GCM_SHA384
                | CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
                | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
        ) {
            &ring::aead::AES_256_GCM
        } else {
            &ring::aead::CHACHA20_POLY1305
        };
        let reference = ring::aead::LessSafeKey::new(
            ring::aead::UnboundKey::new(algorithm, &[7; 32][..len]).unwrap(),
        );
        for (seq, len) in [0, 1, 15, 16, 17, 63, 64, 65, 16384]
            .into_iter()
            .enumerate()
        {
            let plain = vec![0x5a; len];
            let msg = || OutboundPlainMessage {
                typ: rustls::ContentType::ApplicationData,
                version: ProtocolVersion::TLSv1_2,
                payload: OutboundChunks::Single(&plain),
            };
            let encrypted = seal.encrypt(msg(), seq as u64).unwrap();
            assert!(seal.encrypt(msg(), seq as u64).is_err());
            let offset = if explicit { 8 } else { 0 };
            let mut nonce = [3; 12];
            if explicit {
                nonce[4..].copy_from_slice(&[5; 8]);
            }
            for (n, s) in nonce[4..].iter_mut().zip((seq as u64).to_be_bytes()) {
                *n ^= s;
            }
            if explicit {
                assert_eq!(&encrypted.payload.as_ref()[..8], &nonce[4..]);
            }
            let aad = if version13 {
                make_tls13_aad(encrypted.payload.as_ref().len()).to_vec()
            } else {
                make_tls12_aad(seq as u64, encrypted.typ, encrypted.version, len).to_vec()
            };
            let mut expected = plain.clone();
            if version13 {
                expected.push(rustls::ContentType::ApplicationData.into());
            }
            reference
                .seal_in_place_append_tag(
                    ring::aead::Nonce::assume_unique_for_key(nonce),
                    ring::aead::Aad::from(&aad),
                    &mut expected,
                )
                .unwrap();
            assert_eq!(&encrypted.payload.as_ref()[offset..], expected);
            let mut opened = expected.clone();
            assert_eq!(
                reference
                    .open_in_place(
                        ring::aead::Nonce::assume_unique_for_key(nonce),
                        ring::aead::Aad::from(&aad),
                        &mut opened
                    )
                    .unwrap(),
                if version13 {
                    let mut p = plain.clone();
                    p.push(rustls::ContentType::ApplicationData.into());
                    p
                } else {
                    plain.clone()
                }
            );
            let mut payload = encrypted.payload.as_ref().to_vec();
            let last = payload.len() - 1;
            payload[last] ^= 1;
            let before = payload.clone();
            assert!(
                open.decrypt(
                    InboundOpaqueMessage::new(encrypted.typ, encrypted.version, &mut payload),
                    seq as u64
                )
                .is_err()
            );
            // A refusal zeroes the record's data.
            let mut left = before.clone();
            left[offset..last - 15].fill(0);
            assert_eq!(payload, left);
            let mut payload = before;
            payload[last] ^= 1;
            assert_eq!(
                open.decrypt(
                    InboundOpaqueMessage::new(encrypted.typ, encrypted.version, &mut payload),
                    seq as u64
                )
                .unwrap()
                .payload,
                plain
            );
        }
    }
}

fn handshake(
    suite: SupportedCipherSuite,
    group: NamedGroup,
    cert_algorithm: &'static rcgen::SignatureAlgorithm,
) {
    let key_pair = rcgen::KeyPair::generate_for(cert_algorithm).unwrap();
    handshake_with_key(suite, group, &key_pair, false);
}
fn handshake_with_key(
    suite: SupportedCipherSuite,
    group: NamedGroup,
    key_pair: &rcgen::KeyPair,
    default_offer: bool,
) {
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Provider Test Root");
    let ca = ca_params.self_signed(key_pair).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .signed_by(key_pair, &ca, key_pair)
        .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let version = if suite.tls13().is_some() {
        &TLS13
    } else {
        &TLS12
    };
    let mut provider = BACKEND.provider();
    provider.cipher_suites = vec![suite];
    if !default_offer {
        provider.kx_groups.retain(|g| g.name() == group);
    }
    let client = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut server_provider = rustls::crypto::ring::default_provider();
    if group == NamedGroup::X25519MLKEM768 {
        server_provider.kx_groups = vec![&ReferenceHybrid];
    }
    let server = ServerConfig::builder_with_provider(Arc::new(server_provider))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        )
        .unwrap();
    let mut client =
        ClientConnection::new(Arc::new(client), "localhost".try_into().unwrap()).unwrap();
    let mut server = ServerConnection::new(Arc::new(server)).unwrap();
    for _ in 0..32 {
        let mut wire = vec![];
        client.write_tls(&mut wire).unwrap();
        server.read_tls(&mut Cursor::new(wire)).unwrap();
        server.process_new_packets().unwrap();
        let mut wire = vec![];
        server.write_tls(&mut wire).unwrap();
        client.read_tls(&mut Cursor::new(wire)).unwrap();
        client.process_new_packets().unwrap();
        if !client.is_handshaking() && !server.is_handshaking() {
            break;
        }
    }
    assert!(!client.is_handshaking() && !server.is_handshaking());
    assert_eq!(client.negotiated_cipher_suite().unwrap(), suite);
    assert_eq!(
        client.negotiated_key_exchange_group().unwrap().name(),
        group
    );
    client
        .writer()
        .write_all(b"client application record")
        .unwrap();
    let mut wire = vec![];
    client.write_tls(&mut wire).unwrap();
    server.read_tls(&mut Cursor::new(wire)).unwrap();
    server.process_new_packets().unwrap();
    let mut out = [0; 25];
    let n = server.reader().read(&mut out).unwrap();
    assert_eq!(&out[..n], b"client application record");
    server
        .writer()
        .write_all(b"server application record")
        .unwrap();
    let mut wire = vec![];
    server.write_tls(&mut wire).unwrap();
    client.read_tls(&mut Cursor::new(wire)).unwrap();
    client.process_new_packets().unwrap();
    let n = client.reader().read(&mut out).unwrap();
    assert_eq!(&out[..n], b"server application record");
    if suite.tls13().is_some() {
        client.refresh_traffic_keys().unwrap();
        client.writer().write_all(b"new client key").unwrap();
        let mut wire = vec![];
        client.write_tls(&mut wire).unwrap();
        server.read_tls(&mut Cursor::new(wire)).unwrap();
        server.process_new_packets().unwrap();
        let n = server.reader().read(&mut out).unwrap();
        assert_eq!(&out[..n], b"new client key");
        server.refresh_traffic_keys().unwrap();
        server.writer().write_all(b"new server key").unwrap();
        let mut wire = vec![];
        server.write_tls(&mut wire).unwrap();
        client.read_tls(&mut Cursor::new(wire)).unwrap();
        client.process_new_packets().unwrap();
        let n = client.reader().read(&mut out).unwrap();
        assert_eq!(&out[..n], b"new server key");
    }
}
#[test]
fn tls13_client_interoperates_with_ring_for_every_cipher_group_and_ec_certificate() {
    for suite in BACKEND
        .provider()
        .cipher_suites
        .into_iter()
        .filter(|s| s.tls13().is_some())
    {
        for group in [
            NamedGroup::X25519,
            NamedGroup::secp256r1,
            NamedGroup::secp384r1,
        ] {
            for cert in [
                &rcgen::PKCS_ECDSA_P256_SHA256,
                &rcgen::PKCS_ECDSA_P384_SHA384,
                &rcgen::PKCS_ED25519,
            ] {
                handshake(suite, group, cert);
            }
        }
    }
}
#[test]
fn tls12_ecdsa_client_interoperates_with_ring_for_every_cipher_and_group() {
    for suite in
        BACKEND.provider().cipher_suites.into_iter().filter(
            |s| matches!(s,SupportedCipherSuite::Tls12(a) if core::ptr::eq(a.sign,EC_SCHEMES)),
        )
    {
        for group in [
            NamedGroup::X25519,
            NamedGroup::secp256r1,
            NamedGroup::secp384r1,
        ] {
            handshake(suite, group, &rcgen::PKCS_ECDSA_P256_SHA256);
        }
    }
}
#[test]
fn key_exchange_propagates_entropy_failure() {
    #[derive(Debug)]
    struct Failed;
    impl SecureRandom for Failed {
        fn fill(&self, _: &mut [u8]) -> Result<(), GetRandomFailed> {
            Err(GetRandomFailed)
        }
    }
    static FAILED: Backend = Backend::new(&Failed);
    for group in FAILED.provider().kx_groups {
        assert!(matches!(group.start(), Err(Error::FailedToGetRandomBytes)));
    }
}
#[test]
fn transcript_forks_keep_their_own_prefix_and_key_schedule_matches_ring() {
    use rustls::crypto::hash::Hash;
    for (ours, other) in [
        (
            &hash::SHA256 as &dyn Hash,
            rustls::crypto::ring::default_provider()
                .cipher_suites
                .iter()
                .find_map(|s| {
                    s.tls13().filter(|s| {
                        s.common.hash_provider.algorithm()
                            == rustls::crypto::hash::HashAlgorithm::SHA256
                    })
                })
                .unwrap()
                .common
                .hash_provider,
        ),
        (
            &hash::SHA384 as &dyn Hash,
            rustls::crypto::ring::default_provider()
                .cipher_suites
                .iter()
                .find_map(|s| {
                    s.tls13().filter(|s| {
                        s.common.hash_provider.algorithm()
                            == rustls::crypto::hash::HashAlgorithm::SHA384
                    })
                })
                .unwrap()
                .common
                .hash_provider,
        ),
    ] {
        let mut h = ours.start();
        h.update(b"prefix");
        let mut fork = h.fork();
        fork.update(b"branch");
        h.update(b"main");
        assert_eq!(fork.finish().as_ref(), other.hash(b"prefixbranch").as_ref());
        assert_eq!(h.fork_finish().as_ref(), ours.hash(b"prefixmain").as_ref());
    }
    let ring = rustls::crypto::ring::default_provider();
    for suite in [&TLS13_AES128, &TLS13_AES256] {
        let other = ring
            .cipher_suites
            .iter()
            .find_map(|s| s.tls13().filter(|s| s.common.suite == suite.common.suite))
            .unwrap();
        let mut a = [0; 121];
        let mut b = [0; 121];
        suite
            .hkdf_provider
            .extract_from_secret(Some(b"salt"), b"secret")
            .expand_slice(&[b"info", b"label"], &mut a)
            .unwrap();
        other
            .hkdf_provider
            .extract_from_secret(Some(b"salt"), b"secret")
            .expand_slice(&[b"info", b"label"], &mut b)
            .unwrap();
        assert_eq!(a, b);
    }
}

#[test]
fn rsa_authenticated_tls12_and_tls13_interoperate_with_ring() {
    let key = rcgen::KeyPair::from_der_and_sign_algo(
        &rustls::pki_types::PrivatePkcs8KeyDer::from(
            &include_bytes!("../../tests/keys/rsa-2048.pk8")[..],
        )
        .into(),
        &rcgen::PKCS_RSA_SHA256,
    )
    .unwrap();
    for suite in BACKEND.provider().cipher_suites.into_iter().filter(|s| {
        s.tls13().is_some()
            || matches!(s,SupportedCipherSuite::Tls12(a) if core::ptr::eq(a.sign,RSA_SCHEMES))
    }) {
        for group in [
            NamedGroup::X25519,
            NamedGroup::secp256r1,
            NamedGroup::secp384r1,
        ] {
            handshake_with_key(suite, group, &key, false);
        }
    }
}
#[test]
fn hybrid_wire_exchange_matches_independent_kem_and_curve_oracles() {
    use ml_kem::{Decapsulate, KeyExport};
    let provider = BACKEND.provider();
    let group = provider
        .kx_groups
        .iter()
        .find(|g| g.name() == NamedGroup::X25519MLKEM768)
        .unwrap();
    let client = group.start().unwrap();
    assert_eq!(client.pub_key().len(), 1216);
    let peer_kem = ml_kem::EncapsulationKey::<ml_kem::MlKem768>::new(
        client.pub_key()[..1184].try_into().unwrap(),
    )
    .unwrap();
    let (ciphertext, kem_secret) = peer_kem.encapsulate_deterministic(&[13; 32].into());
    let x = x25519_dalek::StaticSecret::from([19; 32]);
    let xp = x25519_dalek::PublicKey::from(&x);
    let xs = x.diffie_hellman(&x25519_dalek::PublicKey::from(
        <[u8; 32]>::try_from(&client.pub_key()[1184..]).unwrap(),
    ));
    let mut share = ciphertext.to_vec();
    share.extend_from_slice(xp.as_bytes());
    let secret = client.complete(&share).unwrap();
    assert_eq!(&secret.secret_bytes()[..32], kem_secret.as_slice());
    assert_eq!(&secret.secret_bytes()[32..], xs.as_bytes());
    let dk = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(
        [[21; 32], [23; 32]].concat().as_slice().try_into().unwrap(),
    );
    let ek = dk.encapsulation_key().to_bytes();
    let x = x25519_dalek::StaticSecret::from([29; 32]);
    let xp = x25519_dalek::PublicKey::from(&x);
    let mut share = ek.to_vec();
    share.extend_from_slice(xp.as_bytes());
    let server = group.start_and_complete(&share).unwrap();
    assert_eq!(server.pub_key.len(), 1120);
    let kem_secret = dk.decapsulate(server.pub_key[..1088].try_into().unwrap());
    let xs = x.diffie_hellman(&x25519_dalek::PublicKey::from(
        <[u8; 32]>::try_from(&server.pub_key[1088..]).unwrap(),
    ));
    assert_eq!(&server.secret.secret_bytes()[..32], kem_secret.as_slice());
    assert_eq!(&server.secret.secret_bytes()[32..], xs.as_bytes());
    assert!(!group.usable_for_version(ProtocolVersion::TLSv1_2));
    for len in [0, 31, 1119, 1121, 1215, 1217] {
        assert!(group.start().unwrap().complete(&vec![0; len]).is_err());
        assert!(group.start_and_complete(&vec![0; len]).is_err());
    }
    let mut invalid = share.clone();
    invalid[..2].copy_from_slice(&[255; 2]);
    assert!(group.start_and_complete(&invalid).is_err());
    invalid = share;
    invalid[1184..].fill(0);
    assert!(group.start_and_complete(&invalid).is_err());
    assert!(group.start().unwrap().complete(&[0; 1120]).is_err());
}

#[derive(Debug)]
struct ReferenceHybrid;
impl rustls::crypto::SupportedKxGroup for ReferenceHybrid {
    fn name(&self) -> NamedGroup {
        NamedGroup::X25519MLKEM768
    }
    fn usable_for_version(&self, v: ProtocolVersion) -> bool {
        v == ProtocolVersion::TLSv1_3
    }
    fn ffdhe_group(&self) -> Option<rustls::ffdhe_groups::FfdheGroup<'static>> {
        None
    }
    fn start(&self) -> Result<Box<dyn rustls::crypto::ActiveKeyExchange>, Error> {
        Err(Error::General("server-only test oracle".into()))
    }
    fn start_and_complete(
        &self,
        peer: &[u8],
    ) -> Result<rustls::crypto::CompletedKeyExchange, Error> {
        assert_eq!(peer.len(), 1216);
        let provider = rustls::crypto::ring::default_provider();
        let x = provider
            .kx_groups
            .iter()
            .find(|g| g.name() == NamedGroup::X25519)
            .unwrap()
            .start()
            .unwrap();
        let public = x.pub_key().to_vec();
        let ecdh = x.complete(&peer[1184..])?;
        let kem = ml_kem::EncapsulationKey::<ml_kem::MlKem768>::new(
            peer[..1184].try_into().unwrap(),
        )
        .unwrap();
        let mut coins = [0; 32];
        Entropy.fill(&mut coins).unwrap();
        let (ciphertext, shared) = kem.encapsulate_deterministic(&coins.into());
        let mut public_share = ciphertext.to_vec();
        public_share.extend_from_slice(&public);
        let mut secret = shared.to_vec();
        secret.extend_from_slice(ecdh.secret_bytes());
        Ok(rustls::crypto::CompletedKeyExchange {
            group: self.name(),
            pub_key: public_share,
            secret: rustls::crypto::SharedSecret::from(secret),
        })
    }
}
#[test]
fn hybrid_tls13_handshakes_interoperate_with_independent_server_primitives() {
    for suite in BACKEND
        .provider()
        .cipher_suites
        .into_iter()
        .filter(|s| s.tls13().is_some())
    {
        handshake(
            suite,
            NamedGroup::X25519MLKEM768,
            &rcgen::PKCS_ECDSA_P256_SHA256,
        );
    }
}

#[test]
fn certificate_cross_hash_ecdsa_uses_sec1_digest_width() {
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use rustls::pki_types::alg_id;
    let mut seed = [0; 32];
    seed[31] = 1;
    let key = p256::ecdsa::SigningKey::from_bytes((&seed).into()).unwrap();
    let digest = crate::hash::Sha384::digest(b"certificate").unwrap();
    let sig: p256::ecdsa::Signature = key.sign_prehash(&digest).unwrap();
    let alg = verify::SUPPORTED
        .all
        .iter()
        .find(|a| {
            a.public_key_alg_id() == alg_id::ECDSA_P256
                && a.signature_alg_id() == alg_id::ECDSA_SHA384
        })
        .unwrap();
    alg.verify_signature(
        key.verifying_key().to_encoded_point(false).as_bytes(),
        b"certificate",
        sig.to_der().as_bytes(),
    )
    .unwrap();
    assert!(
        alg.verify_signature(
            key.verifying_key().to_encoded_point(false).as_bytes(),
            b"changed",
            sig.to_der().as_bytes()
        )
        .is_err()
    );
    let mut seed = [0; 48];
    seed[47] = 1;
    let key = p384::ecdsa::SigningKey::from_bytes((&seed).into()).unwrap();
    let digest = crate::hash::Sha256::digest(b"certificate").unwrap();
    let sig: p384::ecdsa::Signature = key.sign_prehash(&digest).unwrap();
    let alg = verify::SUPPORTED
        .all
        .iter()
        .find(|a| {
            a.public_key_alg_id() == alg_id::ECDSA_P384
                && a.signature_alg_id() == alg_id::ECDSA_SHA256
        })
        .unwrap();
    alg.verify_signature(
        key.verifying_key().to_encoded_point(false).as_bytes(),
        b"certificate",
        sig.to_der().as_bytes(),
    )
    .unwrap();
    assert!(
        alg.verify_signature(
            key.verifying_key().to_encoded_point(false).as_bytes(),
            b"changed",
            sig.to_der().as_bytes()
        )
        .is_err()
    );
}

#[test]
fn default_hybrid_offer_retries_with_classical_ring_server() {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    for suite in BACKEND
        .provider()
        .cipher_suites
        .into_iter()
        .filter(|s| s.tls13().is_some())
    {
        handshake_with_key(suite, NamedGroup::X25519, &key, true);
    }
}
