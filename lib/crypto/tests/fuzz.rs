//! **Structured fuzzing of every decoder**, [`CASES`] seeded inputs each, so
//! a red names its case: SEC 1 points, DER signatures, RSA components,
//! Ed25519 encodings, ML-KEM keys and ciphertexts, and the rustls provider's
//! key shares, public keys and records.
//!
//! Each input is a valid encoding bent along its grammar — a length, a tag, a
//! coordinate at or past the modulus, a padding byte, a coefficient past q —
//! or random bytes. Every input ends in a value or a refusal, never a panic,
//! and the verdict is an independent decoder's: RustCrypto's or ring's, or
//! the interface's own documented rule where the two differ by design. A
//! decoder with no instruction path is the same code in every form, so it
//! runs in the `portable` form alone.

mod support;

use num_bigint::BigUint;
use support::{Rng, seeded};

/// Seeded inputs per decoder.
const CASES: usize = 4_000;

/// A generic change: a flipped bit, a byte set, a cut, an extension, an
/// insertion or deletion, or same-length random bytes.
fn bend(rng: &mut Rng, bytes: &mut Vec<u8>) {
    match rng.below(7) {
        0 if !bytes.is_empty() => {
            let i = rng.below(bytes.len());
            bytes[i] ^= 1 << rng.below(8);
        }
        1 if !bytes.is_empty() => {
            let i = rng.below(bytes.len());
            bytes[i] = rng.next() as u8;
        }
        2 => bytes.truncate(rng.below(bytes.len() + 1)),
        3 => {
            let more = rng.bytes(4);
            bytes.extend(more);
        }
        4 => {
            let i = rng.below(bytes.len() + 1);
            bytes.insert(i, rng.next() as u8);
        }
        5 if !bytes.is_empty() => {
            let i = rng.below(bytes.len());
            bytes.remove(i);
        }
        _ => rng.fill(bytes),
    }
}

/// A big-endian integer as `n` bytes, or `None` where it is wider.
fn be(value: &BigUint, n: usize) -> Option<Vec<u8>> {
    support::fixed_be(&value.to_bytes_be(), n)
}

/// The point of y^2 = x^3 - 3x + b with the least x, for p = 3 mod 4,
/// whose square roots are a power.
fn small_x(p: &BigUint, b: &BigUint) -> (BigUint, BigUint) {
    let root = (p + 1u8) >> 2;
    (0u32..)
        .map(BigUint::from)
        .find_map(|x| {
            let rhs = (&x * &x * &x + p * 3u8 - &x * 3u8 + b) % p;
            let y = rhs.modpow(&root, p);
            (&y * &y % p == rhs).then_some((x, y))
        })
        .unwrap()
}

macro_rules! sec1_case {
    ($curve:ident, $width:literal, $prime:literal, $b:literal, $case:expr, $rng:expr) => {{
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let (case, rng): (usize, &mut Rng) = ($case, $rng);
        let p = BigUint::parse_bytes($prime, 16).unwrap();
        let secret = loop {
            if let Ok(key) = $curve::SecretKey::from_slice(&rng.array::<$width>()) {
                break key;
            }
        };
        let valid = secret.public_key().to_encoded_point(false).as_bytes().to_vec();
        let y = BigUint::from_bytes_be(&valid[1 + $width..]);
        let edges = || [p.clone(), &p + 1u8, &p - 1u8, (BigUint::from(1u8) << (8 * $width)) - 1u8, BigUint::from(0u8)];
        let mut input = valid.clone();
        match rng.below(8) {
            0 => {}
            // The same x with the other y: on the curve.
            1 => input[1 + $width..].copy_from_slice(&be(&(&p - &y), $width).unwrap()),
            // A coordinate at, past or just below the modulus, or zero.
            2 => {
                let e = edges()[rng.below(5)].clone();
                input[1..1 + $width].copy_from_slice(&be(&e, $width).unwrap());
            }
            3 => {
                let e = edges()[rng.below(5)].clone();
                input[1 + $width..].copy_from_slice(&be(&e, $width).unwrap());
            }
            // A point whose x is small enough to be encoded plus p: the same
            // residue, not a canonical encoding. Or the point itself.
            4 => {
                let (small, y) = small_x(&p, &BigUint::parse_bytes($b, 16).unwrap());
                let x = if rng.chance(50) { &small + &p } else { small };
                input[1..1 + $width].copy_from_slice(&be(&x, $width).unwrap());
                input[1 + $width..].copy_from_slice(&be(&y, $width).unwrap());
            }
            5 => input = secret.public_key().to_encoded_point(true).as_bytes().to_vec(),
            6 => input[0] = [0, 2, 3, 5, 6, 7][rng.below(6)],
            _ => bend(rng, &mut input),
        }
        let oracle = $curve::PublicKey::from_sec1_bytes(&input).is_ok() && input.len() == 1 + 2 * $width && input[0] == 4;
        let ours = toyos_crypto::ecc::$curve::PublicKey::from_sec1(&input);
        assert_eq!(ours.is_ok(), oracle, "case {case}: {input:02x?}");
        if let Ok(key) = ours {
            assert_eq!(key.as_sec1().as_slice(), input, "case {case}");
        }
    }};
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn sec1_points() {
    seeded(CASES, 0x5ec1, |case, rng| {
        sec1_case!(
            p256,
            32,
            b"ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
            b"5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
            case,
            rng
        )
    });
    seeded(CASES, 0x5ec2, |case, rng| {
        sec1_case!(
            p384,
            48,
            b"fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
            b"b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
            case,
            rng
        )
    });
}

/// A DER signature bent along X.690's grammar: its tags and lengths, a
/// long-form length, an integer's padding and sign, r or s at zero or the
/// order, the other s, a trailing byte, and generic changes.
fn bend_der(rng: &mut Rng, der: &mut Vec<u8>, order: &BigUint) {
    let r_len = der[3] as usize;
    let s_at = 4 + r_len;
    let integer = |value: &BigUint| {
        let mut bytes = value.to_bytes_be();
        if bytes[0] & 0x80 != 0 {
            bytes.insert(0, 0);
        }
        bytes
    };
    let rebuild = |r: &[u8], s: &[u8]| {
        let mut body = vec![2, r.len() as u8];
        body.extend_from_slice(r);
        body.extend([2, s.len() as u8]);
        body.extend_from_slice(s);
        let mut out = vec![0x30, body.len() as u8];
        out.extend(body);
        out
    };
    let (r, s) = (der[4..4 + r_len].to_vec(), der[s_at + 2..].to_vec());
    match rng.below(12) {
        0 => {}
        1 => der[0] = [0x31, 0x10, 0x20, 0xa0][rng.below(4)],
        2 => der[1] = der[1].wrapping_add([1, 255, 128][rng.below(3)]),
        3 => {
            let at = [2, s_at][rng.below(2)];
            der[at] = [0, 3, 4, 0x42][rng.below(4)];
        }
        4 => {
            let at = [3, s_at + 1][rng.below(2)];
            der[at] = der[at].wrapping_add([1, 255][rng.below(2)]);
        }
        // The outer length in long form: valid BER, refused by DER.
        5 => {
            der[1] = 0x81;
            der.insert(2, (der.len() - 2) as u8);
        }
        // A zero byte before an integer that needs none, or a high bit
        // without the zero that makes it positive.
        6 => *der = if rng.chance(50) { rebuild(&[&[0][..], &r].concat(), &s) } else { rebuild(&r, &[&[0][..], &s].concat()) },
        7 => {
            let stripped: Vec<u8> = s.iter().copied().skip_while(|&b| b == 0).collect();
            *der = rebuild(&r, &stripped);
        }
        8 => {
            let value = [BigUint::from(0u8), order.clone(), order + 1u8, order - 1u8][rng.below(4)].clone();
            let encoded = if value == BigUint::from(0u8) { vec![0] } else { integer(&value) };
            *der = if rng.chance(50) { rebuild(&encoded, &s) } else { rebuild(&r, &encoded) };
        }
        // n - s: the other valid signature.
        9 => *der = rebuild(&r, &integer(&(order - BigUint::from_bytes_be(&s)))),
        10 => der.push(rng.next() as u8),
        _ => {
            bend(rng, der);
            if der.is_empty() {
                der.push(0x30);
            }
        }
    }
}

macro_rules! der_case {
    ($curve:ident, $width:literal, $ring:ident, $order:literal, $case:expr, $rng:expr) => {{
        use p256::ecdsa::signature::Signer as _;
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        use ring::signature;
        let (case, rng): (usize, &mut Rng) = ($case, $rng);
        let order = BigUint::parse_bytes($order, 16).unwrap();
        let secret = loop {
            if let Ok(key) = $curve::SecretKey::from_slice(&rng.array::<$width>()) {
                break key;
            }
        };
        let public = secret.public_key().to_encoded_point(false);
        let message = rng.bytes(32);
        let signed: $curve::ecdsa::Signature = $curve::ecdsa::SigningKey::from(&secret).sign(&message);
        let mut der = signed.to_der().as_bytes().to_vec();
        bend_der(rng, &mut der, &order);
        let ours = toyos_crypto::ecc::$curve::PublicKey::from_sec1(public.as_bytes()).unwrap().verify(&message, &der);
        let ring = signature::UnparsedPublicKey::new(&signature::$ring, public.as_bytes()).verify(&message, &der).is_ok();
        assert_eq!(ours.is_ok(), ring, "case {case}: {der:02x?} {ours:?}");
    }};
}

#[test]
fn der_signatures() {
    seeded(CASES, 0xde5, |case, rng| {
        der_case!(p256, 32, ECDSA_P256_SHA256_ASN1, b"ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551", case, rng)
    });
    seeded(CASES, 0xde6, |case, rng| {
        der_case!(
            p384,
            48,
            ECDSA_P384_SHA384_ASN1,
            b"ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
            case,
            rng
        )
    });
}

/// A committed key: its modulus and exponent, a PKCS #1 v1.5 signature by
/// it, and its `RSAPublicKey` DER.
struct RsaKey {
    n: Vec<u8>,
    e: Vec<u8>,
    sig: Vec<u8>,
    #[cfg(feature = "rustls-provider")]
    der: Vec<u8>,
}

fn rsa_keys() -> Vec<RsaKey> {
    use ring::signature::{self, RsaKeyPair, RsaPublicKeyComponents};
    [&include_bytes!("keys/rsa-2048.pk8")[..], include_bytes!("keys/rsa-4096.pk8")]
        .iter()
        .map(|der| {
            let private = RsaKeyPair::from_pkcs8(der).unwrap();
            let public: RsaPublicKeyComponents<Vec<u8>> = private.public().into();
            let mut sig = vec![0; private.public().modulus_len()];
            private.sign(&signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), b"ToyOS", &mut sig).unwrap();
            RsaKey {
                n: public.n,
                e: public.e,
                sig,
                #[cfg(feature = "rustls-provider")]
                der: signature::KeyPair::public_key(&private).as_ref().to_vec(),
            }
        })
        .collect()
}

/// `rsa::PublicKey::from_components`, held to its contract: minimal
/// big-endian integers, an odd modulus of 2048 to 4096 bits, and an odd
/// exponent from 3 that fits 32 bits. A key accepted verifies the
/// signature only where it is the signer's.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn rsa_components() {
    let keys = rsa_keys();
    seeded(CASES, 0x45a, |case, rng| {
        let RsaKey { n, e, sig, .. } = &keys[case % 2];
        let (mut modulus, mut exponent) = (n.clone(), e.clone());
        match rng.below(8) {
            0 => {}
            1 => modulus.insert(0, 0),
            2 => *modulus.last_mut().unwrap() ^= 1,
            3 => modulus[0] = [0x7f, 0x80, 0x01, 0xff][rng.below(4)],
            4 => {
                let n = [255, 256, 511, 512, 513][rng.below(5)];
                modulus = rng.exactly(n);
            }
            5 => exponent = [&[][..], &[0], &[1], &[2], &[3], &[0, 1, 0, 1], &[1, 0, 0], &[0xff; 4], &[1, 0, 0, 0, 1]][rng.below(9)].to_vec(),
            6 => bend(rng, &mut exponent),
            _ => bend(rng, &mut modulus),
        }
        let bits = modulus.first().map_or(0, |&top| modulus.len() * 8 - top.leading_zeros() as usize);
        let value = (exponent.len() <= 4).then(|| exponent.iter().fold(0u64, |e, &b| (e << 8) | u64::from(b)));
        let lawful = modulus.first().is_some_and(|&b| b != 0)
            && (2048..=4096).contains(&bits)
            && modulus.last().is_some_and(|&b| b & 1 == 1)
            && exponent.first().is_some_and(|&b| b != 0)
            && value.is_some_and(|e| e >= 3 && e & 1 == 1);
        let key = toyos_crypto::rsa::PublicKey::from_components(&modulus, &exponent);
        assert_eq!(key.is_ok(), lawful, "case {case}: {} modulus bytes, exponent {exponent:02x?}", modulus.len());
        if let Ok(key) = key {
            let verified = key.verify_pkcs1_v1_5(toyos_crypto::rsa::Hash::Sha256, b"ToyOS", sig).is_ok();
            assert_eq!(verified, (&modulus, &exponent) == (n, e), "case {case}");
        }
    });
}

/// Ed25519 encodings: y near zero and near p, so canonical and not, with the
/// identity and the points of order 2 and 4 among them, each sign; and
/// random y. Ours refuses non-canonical y,
/// x = 0 with the sign set and small order, so it decodes exactly what
/// dalek decodes canonically and finds not weak.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn ed25519_encodings() {
    let p = (BigUint::from(1u8) << 255) - 19u8;
    seeded(CASES, 0xed5, |case, rng| {
        let y = match rng.below(4) {
            0 => BigUint::from(rng.below(40)),
            1 => &p - 20u8 + BigUint::from(rng.below(40)),
            2 => (BigUint::from(1u8) << 255) - 1u8 - BigUint::from(rng.below(20)),
            _ => BigUint::from_bytes_le(&rng.array::<32>()) % (BigUint::from(1u8) << 255),
        };
        let mut encoding: [u8; 32] = support::fixed_be(&y.to_bytes_be(), 32).unwrap().into_iter().rev().collect::<Vec<_>>().try_into().unwrap();
        encoding[31] |= (rng.below(2) as u8) << 7;
        let canonical = y < p;
        let dalek = ed25519_dalek::VerifyingKey::from_bytes(&encoding).is_ok_and(|k| !k.is_weak() && k.to_bytes() == encoding);
        let ours = toyos_crypto::ed25519::PublicKey::from_bytes(encoding).is_ok();
        assert_eq!(ours, dalek && canonical, "case {case}: {encoding:02x?}");
    });
}

/// An encapsulation key with a coefficient set past q, or other changes:
/// FIPS 203 §7.2's modulus check, as RustCrypto applies it.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn mlkem_encapsulation_keys() {
    use ml_kem::KeyExport;
    seeded(CASES, 0xe4, |case, rng| {
        let seed: [u8; 64] = rng.array();
        let mut ek = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(seed.into()).encapsulation_key().to_bytes().to_vec();
        match rng.below(4) {
            0 => {}
            1 | 2 => {
                let i = rng.below(768);
                let value = if rng.chance(70) { 3329 + rng.below(4096 - 3329) } else { rng.below(3329) } as u16;
                for bit in 0..12 {
                    let at = 12 * i + bit;
                    ek[at / 8] = (ek[at / 8] & !(1 << (at % 8))) | ((((value >> bit) & 1) as u8) << (at % 8));
                }
            }
            _ => bend(rng, &mut ek),
        }
        let oracle = <&[u8; 1184]>::try_from(ek.as_slice())
            .ok()
            .and_then(|bytes| ml_kem::EncapsulationKey::<ml_kem::MlKem768>::new(&(*bytes).into()).ok());
        let ours = toyos_crypto::mlkem::EncapsulationKey::from_bytes(&ek);
        assert_eq!(ours.is_ok(), oracle.is_some(), "case {case}");
        if let (Ok(ours), Some(oracle)) = (ours, oracle) {
            let m: [u8; 32] = rng.array();
            let (c, k) = ours.encapsulate(&mut support::Given::new(m)).unwrap();
            let (want_c, want_k) = oracle.encapsulate_deterministic(&m.into());
            assert_eq!((c.as_slice(), k.as_bytes().as_slice()), (want_c.as_slice(), want_k.as_slice()), "case {case}");
        }
    });
}

/// Ciphertexts changed or cut: a length is refused, and every 1088-byte
/// input decapsulates to RustCrypto's key, rejected implicitly or not.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn mlkem_ciphertexts() {
    use ml_kem::Decapsulate;
    seeded(CASES, 0xc7, |case, rng| {
        let seed: [u8; 64] = rng.array();
        let oracle = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(seed.into());
        let (valid, _) = oracle.encapsulation_key().encapsulate_deterministic(&rng.array::<32>().into());
        let mut ct = valid.to_vec();
        match rng.below(4) {
            0 => {}
            1 => ct.fill([0, 0xff][rng.below(2)]),
            _ => bend(rng, &mut ct),
        }
        let ours = toyos_crypto::mlkem::DecapsulationKey::generate(&mut support::Given::new(seed)).unwrap().decapsulate(&ct);
        match <&[u8; 1088]>::try_from(ct.as_slice()) {
            Ok(bytes) => assert_eq!(ours.unwrap().as_bytes().as_slice(), oracle.decapsulate(&(*bytes).into()).as_slice(), "case {case}"),
            Err(_) => assert_eq!(ours.err(), Some(toyos_crypto::Error::Length), "case {case}"),
        }
    });
}

#[cfg(feature = "rustls-provider")]
mod provider {
    use super::*;
    use rustls::crypto::{GetRandomFailed, SecureRandom};
    use toyos_crypto::rustls_provider::Backend;

    #[derive(Debug)]
    struct Entropy;
    impl SecureRandom for Entropy {
        fn fill(&self, out: &mut [u8]) -> Result<(), GetRandomFailed> {
            rustls::crypto::ring::default_provider().secure_random.fill(out)
        }
    }
    static BACKEND: Backend = Backend::new(&Entropy);

    /// Whether X25519 with this u-coordinate yields zero, which RFC 7748
    /// §6.1 refuses: a clamped scalar is a multiple of 8, so it does so for
    /// every scalar or for none.
    fn low_order(u: &[u8]) -> bool {
        x25519_dalek::x25519([0x5a; 32], u.try_into().unwrap()) == [0; 32]
    }

    /// The order-8 u-coordinates and their non-canonical equivalents.
    fn small_order_u(rng: &mut Rng) -> [u8; 32] {
        let p: BigUint = (BigUint::from(1u8) << 255u32) - 19u8;
        let points = [
            BigUint::from(0u8),
            BigUint::from(1u8),
            BigUint::parse_bytes(b"57119fd0dd4e22d8868e1c58c45c44045bef839c55b1d0b1248c50a3bc959c5f", 16).unwrap(),
            BigUint::parse_bytes(b"00b8495f16056286fdb1329ceb8d09da6ac49ff1fae35616aeb8413b7c7aebe0", 16).unwrap(),
            &p - 1u8,
            p.clone(),
            &p + 1u8,
        ];
        let u = points[rng.below(points.len())].clone();
        support::fixed_be(&u.to_bytes_be(), 32).unwrap().into_iter().rev().collect::<Vec<_>>().try_into().unwrap()
    }

    /// A peer's key share for each group, bent: lengths, the order-8
    /// X25519 points, SEC 1's grammar and ML-KEM's modulus. The verdict is
    /// the share's own decoders': RustCrypto's for SEC 1 and ML-KEM keys,
    /// dalek's ladder for the zero X25519 result.
    #[test]
    #[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
    fn key_shares() {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        use rustls::NamedGroup;
        let provider = BACKEND.provider();
        seeded(CASES, 0x6e5, |case, rng| {
            let group = provider.kx_groups[case % provider.kx_groups.len()];
            let x25519 = |rng: &mut Rng| -> Vec<u8> {
                match rng.below(4) {
                    0 => small_order_u(rng).to_vec(),
                    _ => rng.array::<32>().to_vec(),
                }
            };
            let id = format!("case {case}, {:?}", group.name());
            match group.name() {
                NamedGroup::X25519 => {
                    let mut peer = x25519(rng);
                    if rng.chance(20) {
                        bend(rng, &mut peer);
                    }
                    let accepted = group.start().unwrap().complete(&peer).is_ok();
                    assert_eq!(accepted, peer.len() == 32 && !low_order(&peer), "{id}: {peer:02x?}");
                }
                NamedGroup::secp256r1 | NamedGroup::secp384r1 => {
                    let p256 = group.name() == NamedGroup::secp256r1;
                    let mut peer = if p256 {
                        p256::SecretKey::from_slice(&rng.array::<32>()).unwrap().public_key().to_encoded_point(rng.chance(10)).as_bytes().to_vec()
                    } else {
                        p384::SecretKey::from_slice(&rng.array::<48>()).unwrap().public_key().to_encoded_point(rng.chance(10)).as_bytes().to_vec()
                    };
                    if rng.chance(50) {
                        bend(rng, &mut peer);
                    }
                    let oracle = peer.first() == Some(&4)
                        && if p256 { p256::PublicKey::from_sec1_bytes(&peer).is_ok() && peer.len() == 65 } else { p384::PublicKey::from_sec1_bytes(&peer).is_ok() && peer.len() == 97 };
                    assert_eq!(group.start().unwrap().complete(&peer).is_ok(), oracle, "{id}: {peer:02x?}");
                }
                NamedGroup::X25519MLKEM768 => {
                    use ml_kem::KeyExport;
                    if rng.chance(50) {
                        // The client's share, as a server takes it: an
                        // encapsulation key, then an X25519 key.
                        let seed: [u8; 64] = rng.array();
                        let ek = ml_kem::DecapsulationKey::<ml_kem::MlKem768>::from_seed(seed.into()).encapsulation_key().to_bytes().to_vec();
                        let mut peer = [ek, x25519(rng)].concat();
                        if rng.chance(50) {
                            bend(rng, &mut peer);
                        }
                        let ek_valid = peer.get(..1184).is_some_and(|ek| {
                            ml_kem::EncapsulationKey::<ml_kem::MlKem768>::new(&<[u8; 1184]>::try_from(ek).unwrap().into()).is_ok()
                        });
                        let oracle = peer.len() == 1216 && ek_valid && !low_order(&peer[1184..]);
                        assert_eq!(group.start_and_complete(&peer).is_ok(), oracle, "{id}");
                    } else {
                        // The server's share, as a client takes it: any
                        // ciphertext decapsulates, then an X25519 key.
                        let mut peer = [rng.exactly(1088), x25519(rng)].concat();
                        if rng.chance(30) {
                            bend(rng, &mut peer);
                        }
                        let oracle = peer.len() == 1120 && !low_order(&peer[1088..]);
                        assert_eq!(group.start().unwrap().complete(&peer).is_ok(), oracle, "{id}");
                    }
                }
                other => panic!("{other:?}"),
            }
        });
    }

    /// The provider's own decoder, `RSAPublicKey` DER, beside ring's, which
    /// reads the same structure (RFC 8017 A.1.1): bent lengths, tags,
    /// integer padding and trailing bytes. Only the signer's key, however
    /// spelled, can verify, and DER spells it one way.
    #[test]
    #[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
    fn rsa_public_key_der() {
        use ring::signature;
        use rustls::pki_types::alg_id;
        let provider = BACKEND.provider();
        let algorithm = provider
            .signature_verification_algorithms
            .all
            .iter()
            .find(|a| a.public_key_alg_id() == alg_id::RSA_ENCRYPTION && a.signature_alg_id() == alg_id::RSA_PKCS1_SHA256)
            .unwrap();
        let keys = rsa_keys();
        seeded(CASES, 0xd34, |case, rng| {
            let RsaKey { sig, der, .. } = &keys[case % 2];
            let mut key = der.clone();
            match rng.below(8) {
                0 => {}
                1 => key[rng.below(6)] ^= 1 << rng.below(8),
                // The outer length in a longer form than it needs.
                2 => {
                    key[1] = 0x83;
                    key.insert(2, 0);
                }
                3 => key.push(0),
                4 => key.truncate(key.len() - rng.below(8) - 1),
                // The exponent with a zero byte it does not need.
                5 => {
                    let at = key.len() - 4;
                    key[at] += 1;
                    key.insert(at + 1, 0);
                    key[3] += 1;
                }
                _ => bend(rng, &mut key),
            }
            let ours = algorithm.verify_signature(&key, b"ToyOS", sig).is_ok();
            let ring = signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, &key).verify(b"ToyOS", sig).is_ok();
            assert_eq!(ours, ring, "case {case}: {:02x?}", &key[..key.len().min(12)]);
            assert_eq!(ours, &key == der, "case {case}");
        });
    }

    /// Records sealed by the provider, then cut, extended, changed or opened
    /// at another sequence number: only the record as sealed opens, to its
    /// plaintext, and nothing panics.
    #[test]
    fn records() {
        use rustls::crypto::cipher::{AeadKey, InboundOpaqueMessage, Iv, OutboundChunks, OutboundPlainMessage};
        use rustls::{ContentType, ProtocolVersion, SupportedCipherSuite};
        let provider = BACKEND.provider();
        seeded(CASES, 0x7ec, |case, rng| {
            let suite = provider.cipher_suites[case % provider.cipher_suites.len()];
            let raw: [u8; 32] = rng.array();
            let (key, key2) = (AeadKey::from(raw), AeadKey::from(raw));
            let iv: [u8; 12] = rng.array();
            let (mut seal, mut open) = match suite {
                SupportedCipherSuite::Tls13(s) => (s.aead_alg.encrypter(key, Iv::from(iv)), s.aead_alg.decrypter(key2, Iv::from(iv))),
                SupportedCipherSuite::Tls12(s) => {
                    let shape = s.aead_alg.key_block_shape();
                    let extra = rng.array::<8>();
                    (
                        s.aead_alg.encrypter(key, &iv[..shape.fixed_iv_len], &extra[..shape.explicit_nonce_len]),
                        s.aead_alg.decrypter(key2, &iv[..shape.fixed_iv_len]),
                    )
                }
            };
            let plain = rng.bytes(100);
            let message = || OutboundPlainMessage { typ: ContentType::ApplicationData, version: ProtocolVersion::TLSv1_2, payload: OutboundChunks::Single(&plain) };
            // The sealer takes sequence numbers in order from 0.
            let seq = rng.below(3) as u64;
            for earlier in 0..seq {
                seal.encrypt(message(), earlier).unwrap();
            }
            let sealed = seal.encrypt(message(), seq).unwrap();
            let mut payload = sealed.payload.as_ref().to_vec();
            let (changed, at) = match rng.below(4) {
                0 => (false, seq),
                1 => (true, seq ^ 1),
                _ => {
                    let before = payload.clone();
                    bend(rng, &mut payload);
                    (payload != before, seq)
                }
            };
            let opened = open.decrypt(InboundOpaqueMessage::new(sealed.typ, sealed.version, &mut payload), at);
            match opened {
                Ok(m) => {
                    assert!(!changed, "case {case}, {:?}: a changed record opened", suite.suite());
                    assert_eq!(m.payload, plain.as_slice(), "case {case}");
                }
                Err(_) => assert!(changed, "case {case}, {:?}: the record as sealed refused", suite.suite()),
            }
        });
    }
}
