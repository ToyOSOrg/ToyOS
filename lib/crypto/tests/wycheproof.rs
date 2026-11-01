//! Every Wycheproof file for an interface the crate offers (`tests/data`,
//! `NOTICE`). Each case is judged against the file's result, or counted
//! outside the interface and refused: by an error where the interface can be
//! handed the input, by its types where it cannot. The other files are other
//! interfaces: other algorithms, curves and hashes, RSA moduli past 4096
//! bits, ASN.1, PEM, JWK and P1363 encodings, and expanded and semi-expanded
//! ML-KEM decapsulation keys. A primitive with no instruction path is the
//! same code in every form, so its files run in the `portable` form alone.

mod support;

use serde_json::Value;
use support::{Given, Tally, data, ecdsa, fixed_be, hex, minimal};
use toyos_crypto::{
    Error,
    aead::{OpeningKey, SealingKey},
    hash::{Sha256, Sha384, Sha512},
    kdf::{Hkdf, Hmac},
};

fn h(v: &Value, key: &str) -> Vec<u8> {
    hex(v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")))
}

fn result(t: &Value) -> &str {
    t["result"].as_str().unwrap()
}

/// Every case of `file`, with its group, and the count it declares.
fn load(file: &str) -> (Vec<(Value, Value)>, usize) {
    let d = data(file);
    let mut out = Vec::new();
    for g in d["testGroups"].as_array().unwrap() {
        for t in g["tests"].as_array().unwrap() {
            out.push((g.clone(), t.clone()));
        }
    }
    (out, d["numberOfTests"].as_u64().unwrap() as usize)
}

fn case(file: &str, t: &Value) -> String {
    format!("{file} tcId {} {}", t["tcId"], t["flags"])
}

#[derive(Clone, Copy)]
enum Aead {
    Aes128,
    Aes256,
    ChaCha,
}

impl Aead {
    fn of(file: &str, key: &[u8]) -> Option<Self> {
        match (file.starts_with("chacha"), key.len()) {
            (true, 32) => Some(Self::ChaCha),
            (false, 16) => Some(Self::Aes128),
            (false, 32) => Some(Self::Aes256),
            _ => None,
        }
    }
    fn opening(self, key: &[u8]) -> OpeningKey {
        match self {
            Self::Aes128 => OpeningKey::aes128_gcm(key.try_into().unwrap()),
            Self::Aes256 => OpeningKey::aes256_gcm(key.try_into().unwrap()),
            Self::ChaCha => OpeningKey::chacha20_poly1305(key.try_into().unwrap()),
        }
    }
    /// The first record of a sealing key whose IV is `nonce` is sealed under
    /// `nonce` itself.
    fn sealing(self, key: &[u8], nonce: [u8; 12]) -> SealingKey {
        match self {
            Self::Aes128 => SealingKey::aes128_gcm(key.try_into().unwrap(), nonce),
            Self::Aes256 => SealingKey::aes256_gcm(key.try_into().unwrap(), nonce),
            Self::ChaCha => SealingKey::chacha20_poly1305(key.try_into().unwrap(), nonce),
        }
    }
}

/// One AEAD case's inputs and expected output.
struct Record<'a> {
    key: &'a [u8],
    nonce: [u8; 12],
    aad: &'a [u8],
    ct: &'a [u8],
    tag: &'a [u8],
    msg: &'a [u8],
}

/// Opens `ct` under `tag`, checking that a refusal zeroes the buffer and that
/// an open recovers `msg`; seals `msg` too where the file says valid.
fn aead_case(tally: &mut Tally, file: &str, t: &Value, alg: Aead, r: Record) {
    let Record { key, nonce, aad, ct, tag, msg } = r;
    let id = case(file, t);
    let mut opened = ct.to_vec();
    let refusal = alg.opening(key).open(&nonce, aad, &mut opened, tag);
    if tag.len() != 16 {
        assert_eq!(refusal, Err(Error::Length), "{id}: a {}-byte tag", tag.len());
        assert_eq!(opened, ct, "{id}");
        tally.outside("tag not 16 bytes");
        return;
    }
    if refusal.is_ok() {
        assert_eq!(opened, msg, "{id}");
    } else {
        assert!(opened.iter().all(|&b| b == 0), "{id}: a refusal left more than zeros");
    }
    tally.judged(result(t), refusal.is_ok(), &id);
    if result(t) == "valid" {
        let mut sealed = msg.to_vec();
        let (used, sealed_tag) = alg.sealing(key, nonce).seal(aad, &mut sealed).unwrap();
        assert_eq!((used, sealed.as_slice(), sealed_tag.as_slice()), (nonce, ct, tag), "{id}: sealing");
    }
}

#[test]
fn aead() {
    for file in ["aes_gcm_test.json", "chacha20_poly1305_test.json"] {
        let (cases, declared) = load(file);
        let mut tally = Tally::default();
        for (_, t) in &cases {
            let key = h(t, "key");
            let Some(alg) = Aead::of(file, &key) else {
                tally.outside("AES-192");
                continue;
            };
            let Ok(nonce) = <[u8; 12]>::try_from(h(t, "iv")) else {
                tally.outside("nonce not 12 bytes");
                continue;
            };
            let (aad, ct, tag, msg) = (h(t, "aad"), h(t, "ct"), h(t, "tag"), h(t, "msg"));
            aead_case(&mut tally, file, t, alg, Record { key: &key, nonce, aad: &aad, ct: &ct, tag: &tag, msg: &msg });
        }
        tally.report(file, Some(declared));
    }
}

/// GMAC is GCM over AAD alone: an empty ciphertext, the message as AAD.
#[test]
fn aes_gmac() {
    let file = "aes_gmac_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let key = h(t, "key");
        let Some(alg) = Aead::of(file, &key) else {
            tally.outside("AES-192");
            continue;
        };
        let Ok(nonce) = <[u8; 12]>::try_from(h(t, "iv")) else {
            tally.outside("nonce not 12 bytes");
            continue;
        };
        let (aad, tag) = (h(t, "msg"), h(t, "tag"));
        aead_case(&mut tally, file, t, alg, Record { key: &key, nonce, aad: &aad, ct: &[], tag: &tag, msg: &[] });
    }
    tally.report(file, Some(declared));
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn x25519() {
    use toyos_crypto::x25519::{PublicKey, SecretKey};
    let file = "x25519_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let secret = SecretKey::from_bytes(h(t, "private").try_into().unwrap());
        let agreed = secret.agree(&PublicKey(h(t, "public").try_into().unwrap()));
        let shared = h(t, "shared");
        // RFC 7748 §6.1: an all-zero result is refused, and nothing else is.
        if shared.iter().all(|&b| b == 0) {
            assert!(agreed.is_err(), "{}", case(file, t));
        } else {
            assert_eq!(agreed.as_ref().map(|s| s.as_bytes().to_vec()), Ok(shared), "{}", case(file, t));
        }
        tally.judged(result(t), agreed.is_ok(), case(file, t));
    }
    tally.report(file, Some(declared));
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn ed25519() {
    let file = "ed25519_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (g, t) in &cases {
        let public = toyos_crypto::ed25519::PublicKey::from_bytes(h(&g["publicKey"], "pk").try_into().unwrap());
        let accepted = public.as_ref().is_ok_and(|p| p.verify(&h(t, "msg"), &h(t, "sig")).is_ok());
        tally.judged(result(t), accepted, case(file, t));
    }
    tally.report(file, Some(declared));
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn ecdsa_der() {
    for file in [
        "ecdsa_secp256r1_sha256_test.json",
        "ecdsa_secp256r1_sha512_test.json",
        "ecdsa_secp384r1_sha256_test.json",
        "ecdsa_secp384r1_sha384_test.json",
        "ecdsa_secp384r1_sha512_test.json",
    ] {
        let (cases, declared) = load(file);
        let mut tally = Tally::default();
        for (g, t) in &cases {
            let public = h(&g["publicKey"], "uncompressed");
            let hash = g["sha"].as_str().unwrap();
            let accepted = ecdsa(&public, hash, &h(t, "msg"), &h(t, "sig")).unwrap_or(false);
            // The hash each curve's own `verify` takes, beside `verify_digest`.
            if matches!((public.len(), hash), (65, "SHA-256") | (97, "SHA-384")) {
                let own = match public.len() {
                    65 => toyos_crypto::ecc::p256::PublicKey::from_sec1(&public).and_then(|k| k.verify(&h(t, "msg"), &h(t, "sig"))),
                    _ => toyos_crypto::ecc::p384::PublicKey::from_sec1(&public).and_then(|k| k.verify(&h(t, "msg"), &h(t, "sig"))),
                };
                assert_eq!(own.is_ok(), accepted, "{}", case(file, t));
            }
            tally.judged(result(t), accepted, case(file, t));
        }
        tally.report(file, Some(declared));
    }
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn ecdh_sec1() {
    use toyos_crypto::ecc::{p256, p384};
    for (file, width) in [("ecdh_secp256r1_ecpoint_test.json", 32), ("ecdh_secp384r1_ecpoint_test.json", 48)] {
        let (cases, declared) = load(file);
        let mut tally = Tally::default();
        for (_, t) in &cases {
            let public = h(t, "public");
            let private = fixed_be(&h(t, "private"), width).expect("a scalar below the order");
            let id = case(file, t);
            let agreed = match width {
                32 => p256::PublicKey::from_sec1(&public).and_then(|peer| {
                    let secret = p256::SecretKey::from_bytes(private.as_slice().try_into().unwrap())?;
                    Ok(secret.agree(&peer)?.as_bytes().to_vec())
                }),
                _ => p384::PublicKey::from_sec1(&public).and_then(|peer| {
                    let secret = p384::SecretKey::from_bytes(private.as_slice().try_into().unwrap())?;
                    Ok(secret.agree(&peer)?.as_bytes().to_vec())
                }),
            };
            // Only uncompressed points are accepted (`ecc`'s contract).
            if public.first().is_some_and(|&b| b == 2 || b == 3) {
                assert!(agreed.is_err(), "{id}: a compressed point accepted");
                tally.outside("compressed point");
                continue;
            }
            if let Ok(shared) = &agreed {
                assert_eq!(shared, &h(t, "shared"), "{id}");
            }
            tally.judged(result(t), agreed.is_ok(), &id);
        }
        tally.report(file, Some(declared));
    }
}

/// The group's key: every file here is of a size the crate takes.
fn rsa_key(g: &Value) -> toyos_crypto::rsa::PublicKey {
    let modulus = minimal(&h(&g["publicKey"], "modulus"));
    toyos_crypto::rsa::PublicKey::from_components(&modulus, &h(&g["publicKey"], "publicExponent")).unwrap()
}

#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn rsa_pkcs1() {
    use toyos_crypto::rsa::Hash;
    for file in [
        "rsa_signature_2048_sha256_test.json",
        "rsa_signature_2048_sha384_test.json",
        "rsa_signature_2048_sha512_test.json",
        "rsa_signature_3072_sha256_test.json",
        "rsa_signature_3072_sha384_test.json",
        "rsa_signature_3072_sha512_test.json",
        "rsa_signature_4096_sha256_test.json",
        "rsa_signature_4096_sha384_test.json",
        "rsa_signature_4096_sha512_test.json",
    ] {
        let (cases, declared) = load(file);
        let mut tally = Tally::default();
        for (g, t) in &cases {
            let key = rsa_key(g);
            let hash = match g["sha"].as_str().unwrap() {
                "SHA-256" => Hash::Sha256,
                "SHA-384" => Hash::Sha384,
                "SHA-512" => Hash::Sha512,
                other => panic!("{file}: {other}"),
            };
            let accepted = key.verify_pkcs1_v1_5(hash, &h(t, "msg"), &h(t, "sig")).is_ok();
            tally.judged(result(t), accepted, case(file, t));
        }
        tally.report(file, Some(declared));
    }
}

/// PSS as TLS 1.3 requires it (RFC 8446 §4.2.3): MGF1 over the message hash
/// and a salt one digest long. A signature valid under another salt or mask
/// hash is refused; another message hash cannot be named.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn rsa_pss() {
    use toyos_crypto::rsa::Hash;
    for file in [
        "rsa_pss_2048_sha256_mgf1_0_test.json",
        "rsa_pss_2048_sha256_mgf1_32_test.json",
        "rsa_pss_2048_sha256_mgf1sha1_20_test.json",
        "rsa_pss_2048_sha384_mgf1_48_test.json",
        "rsa_pss_3072_sha256_mgf1_32_test.json",
        "rsa_pss_4096_sha256_mgf1_32_test.json",
        "rsa_pss_4096_sha384_mgf1_48_test.json",
        "rsa_pss_4096_sha512_mgf1_32_test.json",
        "rsa_pss_4096_sha512_mgf1_64_test.json",
        "rsa_pss_misc_test.json",
    ] {
        let (cases, declared) = load(file);
        let mut tally = Tally::default();
        for (g, t) in &cases {
            let (hash, length) = match g["sha"].as_str().unwrap() {
                "SHA-256" => (Hash::Sha256, 32),
                "SHA-384" => (Hash::Sha384, 48),
                "SHA-512" => (Hash::Sha512, 64),
                _ => {
                    tally.outside("message hash not SHA-2 256, 384 or 512");
                    continue;
                }
            };
            let key = rsa_key(g);
            let accepted = key.verify_pss(hash, &h(t, "msg"), &h(t, "sig")).is_ok();
            let tls = g["mgf"] == "MGF1" && g["mgfSha"] == g["sha"] && g["sLen"].as_u64() == Some(length);
            // A valid signature under the file's other parameters is refused.
            // An invalid one may be valid under TLS's: "s_len changed to 32".
            if !tls {
                if result(t) == "valid" {
                    assert!(!accepted, "{}: outside TLS's parameters and accepted", case(file, t));
                    tally.outside(if g["mgfSha"] == g["sha"] { "salt not one digest long" } else { "MGF1 hash not the message hash" });
                } else {
                    tally.outside("another salt or MGF1 hash, and invalid there");
                }
                continue;
            }
            tally.judged(result(t), accepted, case(file, t));
        }
        tally.report(file, Some(declared));
    }
}

#[test]
fn hmac_and_hkdf() {
    macro_rules! run {
        ($hash:ident, $hmac:literal, $hkdf:literal) => {
            let (cases, declared) = load($hmac);
            let mut tally = Tally::default();
            for (g, t) in &cases {
                let mac = || {
                    let mut mac = Hmac::<$hash>::new(&h(t, "key")).unwrap();
                    mac.update(&h(t, "msg")).unwrap();
                    mac
                };
                let tag = h(t, "tag");
                if g["tagSize"].as_u64().unwrap() as usize != <$hash as toyos_crypto::hash::Digest>::OUTPUT_LEN * 8 {
                    assert_eq!(mac().verify(&tag), Err(Error::Length), "{}", case($hmac, t));
                    tally.outside("truncated tag");
                    continue;
                }
                tally.judged(result(t), mac().verify(&tag).is_ok(), case($hmac, t));
            }
            tally.report($hmac, Some(declared));
            let (cases, declared) = load($hkdf);
            let mut tally = Tally::default();
            for (_, t) in &cases {
                let mut out = vec![0; t["size"].as_u64().unwrap() as usize];
                let derived = Hkdf::<$hash>::extract(&h(t, "salt"), &h(t, "ikm")).unwrap().expand(&[&h(t, "info")], &mut out);
                if derived.is_ok() {
                    assert_eq!(out, h(t, "okm"), "{}", case($hkdf, t));
                }
                tally.judged(result(t), derived.is_ok(), case($hkdf, t));
            }
            tally.report($hkdf, Some(declared));
        };
    }
    run!(Sha256, "hmac_sha256_test.json", "hkdf_sha256_test.json");
    run!(Sha384, "hmac_sha384_test.json", "hkdf_sha384_test.json");
    run!(Sha512, "hmac_sha512_test.json", "hkdf_sha512_test.json");
}

#[cfg(feature = "wpa2")]
#[test]
fn wpa2_hmac_sha1_pbkdf2_and_key_unwrap() {
    use toyos_crypto::wpa2;
    let file = "hmac_sha1_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (g, t) in &cases {
        if g["tagSize"].as_u64() != Some(160) {
            tally.outside("truncated tag, which no verifier takes");
            continue;
        }
        let tag = wpa2::hmac_sha1(&h(t, "key"), &[&h(t, "msg")]).unwrap();
        tally.judged(result(t), tag.as_slice() == h(t, "tag"), case(file, t));
    }
    tally.report(file, Some(declared));

    // The PMK is PBKDF2-HMAC-SHA1 at 4096 iterations, 32 bytes long, of an 8
    // to 63 byte passphrase and a 1 to 32 byte SSID (IEEE 802.11-2020
    // J.4.1). PBKDF2's blocks do not depend on the length asked for, so a
    // shorter or longer key shares its first bytes with the PMK.
    let file = "pbkdf2_hmacsha1_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        if t["iterationCount"].as_u64() != Some(4096) {
            tally.outside("iteration count not 4096");
            continue;
        }
        let (password, salt) = (h(t, "password"), h(t, "salt"));
        let pmk = wpa2::derive_pmk(&password, &salt);
        if !(8..=63).contains(&password.len()) || !(1..=32).contains(&salt.len()) {
            assert_eq!(pmk.err(), Some(Error::Length), "{}", case(file, t));
            tally.outside("passphrase or SSID length outside 802.11's");
            continue;
        }
        let dk = h(t, "dk");
        let n = dk.len().min(32);
        tally.judged(result(t), pmk.unwrap().as_bytes()[..n] == dk[..n], case(file, t));
    }
    tally.report(file, Some(declared));

    let file = "aes_wrap_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let Ok(key) = <[u8; 16]>::try_from(h(t, "key")) else {
            tally.outside("key-encryption key not AES-128");
            continue;
        };
        let ct = h(t, "ct");
        let mut out = vec![0; ct.len().saturating_sub(8)];
        let unwrapped = wpa2::KeyEncryptionKey::new(key).unwrap(&ct, &mut out);
        match unwrapped {
            Ok(()) => assert_eq!(out, h(t, "msg"), "{}", case(file, t)),
            Err(_) => assert!(out.iter().all(|&b| b == 0), "{}: a refusal left key bytes", case(file, t)),
        }
        tally.judged(result(t), unwrapped.is_ok(), case(file, t));
    }
    tally.report(file, Some(declared));
}

/// FIPS 203 ML-KEM-768 by seed: the crate's decapsulation key is made only
/// from `d || z`, so an expanded or semi-expanded key cannot be handed to it.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn mlkem768() {
    use toyos_crypto::mlkem::{DecapsulationKey, EncapsulationKey};
    let generate = |seed: Vec<u8>| DecapsulationKey::generate(&mut Given::new(seed)).unwrap();

    let file = "mlkem_768_keygen_seed_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let ek = generate(h(t, "seed"));
        tally.judged(result(t), ek.public_key().as_bytes().as_slice() == h(t, "ek"), case(file, t));
    }
    tally.report(file, Some(declared));

    let file = "mlkem_768_encaps_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let encapsulated = EncapsulationKey::from_bytes(&h(t, "ek")).map(|ek| ek.encapsulate(&mut Given::new(h(t, "m"))).unwrap());
        if let Ok((c, k)) = &encapsulated {
            assert_eq!((c.as_slice(), k.as_bytes().as_slice()), (h(t, "c").as_slice(), h(t, "K").as_slice()), "{}", case(file, t));
        }
        tally.judged(result(t), encapsulated.is_ok(), case(file, t));
    }
    tally.report(file, Some(declared));

    let file = "mlkem_768_test.json";
    let (cases, declared) = load(file);
    let mut tally = Tally::default();
    for (_, t) in &cases {
        let seed = h(t, "seed");
        if seed.len() != 64 {
            tally.outside("seed not 64 bytes");
            continue;
        }
        let key = generate(seed);
        assert_eq!(key.public_key().as_bytes().as_slice(), h(t, "ek"), "{}", case(file, t));
        let shared = key.decapsulate(&h(t, "c"));
        match &shared {
            Ok(k) => assert_eq!(k.as_bytes().as_slice(), h(t, "K"), "{}", case(file, t)),
            Err(e) => assert_eq!(*e, Error::Length, "{}", case(file, t)),
        }
        tally.judged(result(t), shared.is_ok(), case(file, t));
    }
    tally.report(file, Some(declared));
}
