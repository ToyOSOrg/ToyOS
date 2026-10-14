use serde_json::Value;
use toyos_crypto::{
    aead::OpeningKey,
    hash::{Sha256, Sha384, Sha512},
    kdf::{Hkdf, Hmac},
};
fn data(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap(),
    )
    .unwrap()
}
fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}
fn h(v: &Value, key: &str) -> Vec<u8> {
    hex(v[key].as_str().unwrap())
}
fn valid(v: &Value) -> bool {
    v["result"] == "valid"
}
#[test]
fn wycheproof_aead() {
    for file in ["aes_gcm_test.json", "chacha20_poly1305_test.json"] {
        let d = data(file);
        let mut executed = 0;
        let mut unsupported = 0;
        for g in d["testGroups"].as_array().unwrap() {
            for t in g["tests"].as_array().unwrap() {
                let key = h(t, "key");
                let iv = h(t, "iv");
                let tag = h(t, "tag");
                if iv.len() != 12
                    || tag.len() != 16
                    || (file == "aes_gcm_test.json" && key.len() == 24)
                {
                    unsupported += 1;
                    continue;
                }
                let key = if file == "chacha20_poly1305_test.json" {
                    OpeningKey::chacha20_poly1305(key.try_into().unwrap())
                } else if key.len() == 16 {
                    OpeningKey::aes128_gcm(key.try_into().unwrap())
                } else {
                    OpeningKey::aes256_gcm(key.try_into().unwrap())
                };
                let aad = h(t, "aad");
                let mut encrypted = h(t, "ct");
                let original = encrypted.clone();
                let result = key.open(&iv.try_into().unwrap(), &aad, &mut encrypted, &tag);
                assert_eq!(result.is_ok(), valid(t), "{file} tcId {}", t["tcId"]);
                if valid(t) {
                    assert_eq!(encrypted, h(t, "msg"))
                } else {
                    assert_eq!(encrypted, original)
                }
                executed += 1;
            }
        }
        println!(
            "{file}: {executed} cases; {unsupported} outside fixed 96-bit nonce/128-bit tag/AES-128-or-256 contract"
        );
    }
}
#[test]
fn wycheproof_x25519() {
    use toyos_crypto::x25519::{PublicKey, SecretKey};
    let d = data("x25519_test.json");
    let mut count = 0;
    for g in d["testGroups"].as_array().unwrap() {
        for t in g["tests"].as_array().unwrap() {
            let secret = SecretKey::from_bytes(h(t, "private").try_into().unwrap());
            let public = PublicKey(h(t, "public").try_into().unwrap());
            let result = secret.agree(&public);
            let shared = h(t, "shared");
            if shared.iter().all(|&b| b == 0) {
                assert!(result.is_err(), "tcId {}", t["tcId"])
            } else {
                assert_eq!(
                    result.unwrap().as_bytes().as_slice(),
                    shared,
                    "tcId {}",
                    t["tcId"]
                )
            }
            count += 1;
        }
    }
    println!("X25519: {count} cases");
}
#[test]
fn wycheproof_ed25519() {
    let d = data("ed25519_test.json");
    let mut count = 0;
    for g in d["testGroups"].as_array().unwrap() {
        let public = toyos_crypto::ed25519::PublicKey::from_bytes(
            h(&g["publicKey"], "pk").try_into().unwrap(),
        );
        for t in g["tests"].as_array().unwrap() {
            let result = public
                .as_ref()
                .map(|p| p.verify(&h(t, "msg"), &h(t, "sig")).is_ok())
                .unwrap_or(false);
            assert_eq!(result, valid(t), "tcId {} flags {}", t["tcId"], t["flags"]);
            count += 1;
        }
    }
    println!("Ed25519: {count} cases");
}
#[test]
fn wycheproof_ecdsa() {
    for file in [
        "ecdsa_secp256r1_sha256_test.json",
        "ecdsa_secp384r1_sha384_test.json",
    ] {
        let d = data(file);
        let mut count = 0;
        for g in d["testGroups"].as_array().unwrap() {
            let public = h(&g["publicKey"], "uncompressed");
            for t in g["tests"].as_array().unwrap() {
                let signature = h(t, "sig");
                let msg = h(t, "msg");
                let result = if file.contains("256r1") {
                    toyos_crypto::ecc::p256::PublicKey::from_sec1(&public)
                        .and_then(|p| p.verify(&msg, &signature))
                } else {
                    toyos_crypto::ecc::p384::PublicKey::from_sec1(&public)
                        .and_then(|p| p.verify(&msg, &signature))
                };
                assert_eq!(result.is_ok(), valid(t), "{file} tcId {}", t["tcId"]);
                count += 1;
            }
        }
        println!("{file}: {count} cases");
    }
}
#[test]
fn wycheproof_rsa() {
    for file in [
        "rsa_signature_2048_sha256_test.json",
        "rsa_pss_2048_sha256_mgf1_32_test.json",
    ] {
        let d = data(file);
        let mut count = 0;
        for g in d["testGroups"].as_array().unwrap() {
            let mut modulus = h(&g["publicKey"], "modulus");
            if modulus[0] == 0 {
                modulus.remove(0);
            }
            let public = toyos_crypto::rsa::PublicKey::from_components(
                &modulus,
                &h(&g["publicKey"], "publicExponent"),
            )
            .unwrap();
            for t in g["tests"].as_array().unwrap() {
                let result = if file.contains("pss") {
                    public.verify_pss(toyos_crypto::rsa::Hash::Sha256, &h(t, "msg"), &h(t, "sig"))
                } else {
                    public.verify_pkcs1_v1_5(
                        toyos_crypto::rsa::Hash::Sha256,
                        &h(t, "msg"),
                        &h(t, "sig"),
                    )
                };
                assert_eq!(result.is_ok(), valid(t), "{file} tcId {}", t["tcId"]);
                count += 1;
            }
        }
        println!("{file}: {count} cases");
    }
}
#[test]
fn wycheproof_hmac_and_hkdf() {
    macro_rules! run {
        ($hash:ident,$hmac:literal,$hkdf:literal) => {
            let d = data($hmac);
            let mut count = 0;
            let mut unsupported = 0;
            for g in d["testGroups"].as_array().unwrap() {
                for t in g["tests"].as_array().unwrap() {
                    let tag = h(t, "tag");
                    if g["tagSize"].as_u64().unwrap() as usize
                        != <$hash as toyos_crypto::hash::Digest>::OUTPUT_LEN * 8
                    {
                        unsupported += 1;
                        continue;
                    }
                    let mut mac = Hmac::<$hash>::new(&h(t, "key")).unwrap();
                    mac.update(&h(t, "msg")).unwrap();
                    assert_eq!(
                        mac.verify(&tag).is_ok(),
                        valid(t),
                        "{} tcId {}",
                        $hmac,
                        t["tcId"]
                    );
                    count += 1;
                }
            }
            println!(
                "{}: {count}; {unsupported} truncated-tag cases outside API",
                $hmac
            );
            let d = data($hkdf);
            let mut count = 0;
            for g in d["testGroups"].as_array().unwrap() {
                for t in g["tests"].as_array().unwrap() {
                    let mut out = vec![0; t["size"].as_u64().unwrap() as usize];
                    let result = Hkdf::<$hash>::extract(&h(t, "salt"), &h(t, "ikm"))
                        .unwrap()
                        .expand(&[&h(t, "info")], &mut out);
                    assert_eq!(result.is_ok(), valid(t), "{} tcId {}", $hkdf, t["tcId"]);
                    if result.is_ok() {
                        assert_eq!(out, h(t, "okm"))
                    }
                    count += 1;
                }
            }
            println!("{}: {count} cases", $hkdf);
        };
    }
    run!(Sha256, "hmac_sha256_test.json", "hkdf_sha256_test.json");
    run!(Sha384, "hmac_sha384_test.json", "hkdf_sha384_test.json");
    run!(Sha512, "hmac_sha512_test.json", "hkdf_sha512_test.json");
}

struct Input {
    bytes: Vec<u8>,
    position: usize,
}
impl toyos_crypto::Random for Input {
    fn fill(&mut self, dst: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        let end = self.position + dst.len();
        dst.copy_from_slice(
            self.bytes
                .get(self.position..end)
                .ok_or(toyos_crypto::Error::Entropy)?,
        );
        self.position = end;
        Ok(())
    }
}
#[test]
fn nist_mlkem768_kats() {
    use toyos_crypto::mlkem::{DecapsulationKey, EncapsulationKey};
    let d = data("nist-mlkem-keygen.json");
    let mut kg = 0;
    for g in d["testGroups"].as_array().unwrap() {
        if g["parameterSet"] != "ML-KEM-768" {
            continue;
        }
        for t in g["tests"].as_array().unwrap() {
            let mut seed = h(t, "d");
            seed.extend(h(t, "z"));
            let key = DecapsulationKey::generate(&mut Input {
                bytes: seed,
                position: 0,
            })
            .unwrap();
            assert_eq!(key.public_key().as_bytes().as_slice(), h(t, "ek"));
            kg += 1;
        }
    }
    let d = data("nist-mlkem-encapdecap.json");
    let mut enc = 0;
    let mut dec = 0;
    let mut checks = 0;
    let mut unsupported = 0;
    for g in d["testGroups"].as_array().unwrap() {
        if g["parameterSet"] != "ML-KEM-768" {
            continue;
        }
        for t in g["tests"].as_array().unwrap() {
            match g["function"].as_str().unwrap() {
                "encapsulation" => {
                    let key = EncapsulationKey::from_bytes(&h(t, "ek")).unwrap();
                    let (c, k) = key
                        .encapsulate(&mut Input {
                            bytes: h(t, "m"),
                            position: 0,
                        })
                        .unwrap();
                    assert_eq!(c.as_slice(), h(t, "c"));
                    assert_eq!(k.as_bytes().as_slice(), h(t, "k"));
                    enc += 1
                }
                "decapsulation" if g["keyFormat"] == "seed" => {
                    let mut seed = h(t, "d");
                    seed.extend(h(t, "z"));
                    let key = DecapsulationKey::generate(&mut Input {
                        bytes: seed,
                        position: 0,
                    })
                    .unwrap();
                    let k = key.decapsulate(&h(t, "c")).unwrap();
                    assert_eq!(
                        k.as_bytes().as_slice(),
                        h(t, "k"),
                        "tcId {} reason {}",
                        t["tcId"],
                        t["reason"]
                    );
                    dec += 1
                }
                "encapsulationKeyCheck" => {
                    assert_eq!(
                        EncapsulationKey::from_bytes(&h(t, "ek")).is_ok(),
                        t["testPassed"].as_bool().unwrap()
                    );
                    checks += 1
                }
                _ => unsupported += 1,
            }
        }
    }
    println!(
        "ML-KEM-768 ACVP: keygen {kg}, encapsulation {enc}, seed decapsulation {dec}, encapsulation-key checks {checks}; {unsupported} expanded-key cases outside API"
    );
}
