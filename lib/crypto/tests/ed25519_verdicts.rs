//! The Ed25519 verifier's rules (`src/ed25519.rs`) as explicit verdicts, with
//! RustCrypto's `ed25519-dalek` 2.2 `verify_strict` and ring 0.17 as
//! cross-checks. Each expected verdict comes from the rule, and each
//! oracle's verdict is pinned beside it, so a red names who moved. ring
//! departs from the rules on small-order and non-canonical keys;
//! `verify_strict` on a small-order R and on a key whose y is p or more.

mod support;

use num_bigint::BigUint;
use sha2::Digest;
use toyos_crypto::{Error, ed25519::PublicKey};

fn order() -> BigUint {
    BigUint::parse_bytes(b"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed", 16).unwrap()
}

fn le(n: &BigUint) -> [u8; 32] {
    let mut out = [0; 32];
    let bytes = n.to_bytes_le();
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

/// k = SHA-512(R || A || M) mod L, over the bytes as given.
fn challenge(r: &[u8; 32], a: &[u8; 32], message: &[u8]) -> BigUint {
    let digest = sha2::Sha512::new().chain_update(r).chain_update(a).chain_update(message).finalize();
    BigUint::from_bytes_le(&digest) % order()
}

/// The canonical encoding of [a]B and a mod L for the RFC 8032 secret scalar
/// a of `seed`, through ring's key derivation.
fn scalar_and_point(seed: [u8; 32]) -> (BigUint, [u8; 32]) {
    use ring::signature::KeyPair;
    let mut h: [u8; 64] = sha2::Sha512::digest(seed).into();
    h[0] &= 248;
    h[31] = (h[31] & 63) | 64;
    let a = BigUint::from_bytes_le(&h[..32]) % order();
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    (a, pair.public_key().as_ref().try_into().unwrap())
}

fn signature(r: &[u8; 32], s: &[u8; 32]) -> [u8; 64] {
    let mut out = [0; 64];
    out[..32].copy_from_slice(r);
    out[32..].copy_from_slice(s);
    out
}

/// Ours, `verify_strict` and ring on one signature; a refused key counts as
/// a refused signature.
#[derive(Debug, PartialEq)]
struct Verdicts {
    ours: Result<(), Error>,
    strict: bool,
    ring: bool,
}

fn verdicts(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> Verdicts {
    Verdicts {
        ours: PublicKey::from_bytes(*public).and_then(|key| key.verify(message, signature)),
        strict: ed25519_dalek::VerifyingKey::from_bytes(public)
            .is_ok_and(|key| key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature)).is_ok()),
        ring: ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public).verify(message, signature).is_ok(),
    }
}

/// Whether [S]B = R + [k]A holds for the decoded points: ed25519-dalek's
/// non-strict `verify`, a witness that a constructed signature satisfies
/// the cofactorless equation, so that a refusal is the rule's.
fn equation_holds(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::Verifier;
    ed25519_dalek::VerifyingKey::from_bytes(public).is_ok_and(|key| key.verify(message, &ed25519_dalek::Signature::from_bytes(signature)).is_ok())
}

const ACCEPTED: Result<(), Error> = Ok(());
const BAD_SIGNATURE: Result<(), Error> = Err(Error::InvalidSignature);
const SMALL_KEY: Result<(), Error> = Err(Error::InvalidKey);
const UNDECODABLE_KEY: Result<(), Error> = Err(Error::InvalidPoint);

/// The twelve cases of Chalkias, Garillot, Kondi and Nikolaenko, "Taming the
/// many EdDSAs" (2020), each with its expected verdict and the rule
/// that decides it.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn speccheck_cases() {
    #[rustfmt::skip]
    let expected: [(Result<(), Error>, bool, bool, &str); 12] = [
        (SMALL_KEY, false, true, "small-order A and R, S = 0: a small-order key is refused; ring takes it"),
        (SMALL_KEY, false, true, "small-order A, mixed-order R: a small-order key is refused; ring takes it"),
        (ACCEPTED, false, true, "mixed-order A, small-order R satisfying the equation: R is taken, as RFC 8032 takes it; verify_strict refuses it"),
        (ACCEPTED, true, true, "mixed-order A and R satisfying [S]B = R + [k mod L]A, as section 6 reduces k; section 5.1.7's unreduced k fails, as it may only for a mixed-order A"),
        (BAD_SIGNATURE, false, false, "mixed-order A and R satisfying only the cofactored equation: cofactorless"),
        (BAD_SIGNATURE, false, false, "mixed-order A, prime-order R, only the cofactored equation: cofactorless"),
        (BAD_SIGNATURE, false, false, "S of L or more (RFC 8032 section 5.1.7)"),
        (BAD_SIGNATURE, false, false, "S far past L, its high bits set (RFC 8032 section 5.1.7)"),
        (BAD_SIGNATURE, false, false, "non-canonical R (y = p - 1 with x = 0 signed), k over R's canonical bytes (section 5.1.3)"),
        (BAD_SIGNATURE, false, false, "non-canonical R, k over the bytes as sent: the equation holds, R does not decode (section 5.1.3)"),
        (UNDECODABLE_KEY, false, false, "A is x = 0 signed, a non-canonical (0, -1) (section 5.1.3 step 4); ring's k misses too"),
        (UNDECODABLE_KEY, false, true, "A is x = 0 signed, k over the bytes as sent: refused (section 5.1.3 step 4); ring takes it"),
    ];
    let cases = support::data("ed25519_speccheck_cases.json");
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), expected.len());
    for (i, (case, (ours, strict, ring, why))) in cases.iter().zip(expected).enumerate() {
        let field = |name: &str| support::hex(case[name].as_str().unwrap());
        let public: [u8; 32] = field("pub_key").try_into().unwrap();
        let signature: [u8; 64] = field("signature").try_into().unwrap();
        assert_eq!(verdicts(&public, &field("message"), &signature), Verdicts { ours, strict, ring }, "case {i}: {why}");
    }
}

/// Every encoding of a point of order dividing 8, the eight canonical ones
/// and six non-canonical: y = 1 and y = -1 with x = 0 signed, and y = p and
/// y = p + 1 with either sign.
const SMALL_ORDER: [(&str, Result<(), Error>); 14] = [
    ("0100000000000000000000000000000000000000000000000000000000000000", SMALL_KEY),
    ("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", SMALL_KEY),
    ("0000000000000000000000000000000000000000000000000000000000000000", SMALL_KEY),
    ("0000000000000000000000000000000000000000000000000000000000000080", SMALL_KEY),
    ("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05", SMALL_KEY),
    ("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85", SMALL_KEY),
    ("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a", SMALL_KEY),
    ("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa", SMALL_KEY),
    ("0100000000000000000000000000000000000000000000000000000000000080", UNDECODABLE_KEY),
    ("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", UNDECODABLE_KEY),
    ("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", UNDECODABLE_KEY),
    ("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", UNDECODABLE_KEY),
    ("eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", UNDECODABLE_KEY),
    ("eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", UNDECODABLE_KEY),
];

/// A small-order key T has no secret: for any S, R = [S]B satisfies
/// [S]B = R + [k]T whenever 8 divides k, one message in eight, and the
/// identity's for every message. Such a signature is anyone's, so the key is
/// refused, canonical or not. ring takes every one; `verify_strict` refuses.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn a_small_order_key_is_refused_though_anyone_can_sign_for_it() {
    let (s, r) = scalar_and_point([5; 32]);
    let signature = signature(&r, &le(&s));
    for (encoding, ours) in SMALL_ORDER {
        let public: [u8; 32] = support::hex(encoding).try_into().unwrap();
        let message = (0u32..).map(u32::to_le_bytes).find(|m| (challenge(&r, &public, m) % 8u32) == BigUint::ZERO).unwrap();
        assert!(equation_holds(&public, &message, &signature), "{encoding}: the constructed signature");
        assert_eq!(verdicts(&public, &message, &signature), Verdicts { ours, strict: false, ring: true }, "{encoding}");
    }
}

/// RFC 8032 section 5.1.3 step 1 refuses y of p or more, every one of the
/// 38 encodings y = p + j, j < 19. `verify_strict` decodes the twenty whose
/// y - p is on the curve and not small-order: the only keys on which the
/// two differ. No signature for such a key is known to anyone: it would
/// take the discrete log of a point named by its y-coordinate, so no
/// signature verdict can show the difference. ring decodes y = p and
/// p + 1 (the small-order test) and is not consulted here.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn a_key_with_y_of_p_or_more_is_refused_where_verify_strict_decodes_it() {
    // y - p of the encodings verify_strict decodes to a point not of small order.
    const DECODED_BY_STRICT: [u8; 10] = [3, 4, 5, 6, 9, 10, 14, 15, 16, 18];
    for j in 0..19u8 {
        for sign in [0, 0x80] {
            let mut encoding = [0xff; 32];
            encoding[0] = 0xed + j;
            encoding[31] = 0x7f | sign;
            assert_eq!(PublicKey::from_bytes(encoding).err(), Some(Error::InvalidPoint), "y = p + {j}, sign {sign:#x}");
            let strict = ed25519_dalek::VerifyingKey::from_bytes(&encoding).is_ok_and(|key| !key.is_weak());
            assert_eq!(strict, DECODED_BY_STRICT.contains(&j), "y = p + {j}, sign {sign:#x}: verify_strict's decoding");
        }
    }
}

/// Each canonical small-order R under a key the RFC 8032 key generation
/// makes, with S = k a, the S that makes [S]B - [k]A the identity O. R = O
/// is accepted: RFC 8032 signing emits exactly this signature when r is a
/// multiple of L, improbable but possible, and only the key's holder can
/// make it. ring takes it; `verify_strict` refuses every small-order R. The
/// other seven fail the equation for any S, because [S]B - [k]A is then a
/// multiple of B, never a point of order 2, 4 or 8.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn r_of_order_dividing_8_is_taken_where_the_equation_holds() {
    let (a, public) = scalar_and_point([9; 32]);
    let message = b"r = 0";
    for (encoding, _) in &SMALL_ORDER[..8] {
        let r: [u8; 32] = support::hex(encoding).try_into().unwrap();
        let signature = signature(&r, &le(&(challenge(&r, &public, message) * &a % order())));
        let identity = r == le(&BigUint::from(1u32));
        assert_eq!(equation_holds(&public, message, &signature), identity, "R {encoding}: the constructed signature");
        let ours = if identity { ACCEPTED } else { BAD_SIGNATURE };
        assert_eq!(verdicts(&public, message, &signature), Verdicts { ours, strict: false, ring: identity }, "R {encoding}");
    }
}

/// R must decode (RFC 8032 section 5.1.3): the three non-canonical
/// encodings of O as R are refused by everyone though [S]B - [k]A is O. So
/// is an honest signature's S + L (section 5.1.7).
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn non_canonical_r_and_s_past_l_are_refused() {
    let (a, public) = scalar_and_point([9; 32]);
    let message = b"no nonce";
    for alias in [
        "0100000000000000000000000000000000000000000000000000000000000080",
        "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    ] {
        let r: [u8; 32] = support::hex(alias).try_into().unwrap();
        let forged = signature(&r, &le(&(challenge(&r, &public, message) * &a % order())));
        assert_eq!(verdicts(&public, message, &forged), Verdicts { ours: BAD_SIGNATURE, strict: false, ring: false }, "R {alias}");
    }

    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let honest: [u8; 64] = pair.sign(message).as_ref().try_into().unwrap();
    assert_eq!(verdicts(&public, message, &honest), Verdicts { ours: ACCEPTED, strict: true, ring: true });
    let mut past = honest;
    past[32..].copy_from_slice(&le(&(BigUint::from_bytes_le(&honest[32..]) + order())));
    assert_eq!(verdicts(&public, message, &past), Verdicts { ours: BAD_SIGNATURE, strict: false, ring: false }, "S + L");
}
