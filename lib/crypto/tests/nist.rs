//! NIST's vectors (`NOTICE`): CAVP's response files under the repository's
//! `tests/cavp/` for SHA-2, AES-GCM and ECDSA, and ACVP's for HMAC and
//! ML-KEM under its `tests/acvp/`. A case whose parameters the crate does not
//! offer is counted outside the interface by the reason it names, and each
//! file's cases are held to a count taken from its text apart from the
//! parse: its `Count = `, `Len = `, `Msg = ` or `Qx = ` lines, or its
//! `"tcId"`s. ECDSA and ML-KEM, which have no instruction path, run in the
//! `portable` form alone.

mod support;

use std::collections::BTreeMap;

use support::{Given, Tally, acvp, cavp, der_signature, ecdsa, fixed_be, hex, integer};
use toyos_crypto::{
    aead::SealingKey,
    hash::{Sha256, Sha384, Sha512},
    kdf::Hmac,
};

/// One record of a response file: the `[...]` headers above it and its own
/// `name = value` lines, with a bare `FAIL` as `FAIL = `.
struct Record {
    headers: Vec<String>,
    fields: BTreeMap<String, String>,
}

impl Record {
    fn get(&self, name: &str) -> &str {
        self.fields.get(name).unwrap_or_else(|| panic!("no {name} in {:?}", self.fields))
    }
    fn bytes(&self, name: &str) -> Vec<u8> {
        hex(self.get(name))
    }
}

/// The lines of a response file that open with `prefix`: a count of its
/// records the parser below does not take part in.
fn lines(name: &str, prefix: &str) -> usize {
    cavp(name).lines().filter(|line| line.trim_start().starts_with(prefix)).count()
}

/// Records of `name`, held to the count of lines opening with `prefix`.
fn counted(name: &str, prefix: &str) -> Vec<Record> {
    let out = records(name);
    assert_eq!(out.len(), lines(name, prefix), "{name}: the parser merged or lost a record");
    out
}

fn records(name: &str) -> Vec<Record> {
    let text = cavp(name);
    let mut out = Vec::new();
    let mut headers: Vec<String> = Vec::new();
    let mut fresh_headers = false;
    let mut fields = BTreeMap::new();
    let mut flush = |fields: &mut BTreeMap<String, String>, headers: &Vec<String>| {
        if !fields.is_empty() {
            out.push(Record { headers: headers.clone(), fields: std::mem::take(fields) });
        }
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            flush(&mut fields, &headers);
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&mut fields, &headers);
            if !fresh_headers {
                headers.clear();
            }
            headers.push(header.to_string());
            fresh_headers = true;
            continue;
        }
        fresh_headers = false;
        match line.split_once(" = ").or_else(|| line.strip_suffix(" =").map(|k| (k, ""))) {
            Some((key, value)) => {
                fields.insert(key.to_string(), value.to_string());
            }
            None if line == "FAIL" => {
                fields.insert("FAIL".to_string(), String::new());
            }
            None => panic!("{name}: {line:?}"),
        }
    }
    flush(&mut fields, &headers);
    out
}

/// SHAVS §6.2–6.4 for every SHA-2 the crate has: each message whole and a
/// byte at a time, and the Monte Carlo chain.
#[test]
fn cavp_shavs() {
    fn run<const N: usize>(name: &str, digest: fn(&[u8]) -> [u8; N], streamed: fn(&[u8]) -> [u8; N]) {
        let mut count = 0;
        for kind in ["ShortMsg", "LongMsg"] {
            let file = format!("{name}{kind}.rsp");
            let want = lines(&file, "Len = ");
            let before = count;
            for r in records(&file) {
                if !r.fields.contains_key("MD") {
                    continue;
                }
                let len: usize = r.get("Len").parse().unwrap();
                let msg = &r.bytes("Msg")[..len / 8];
                assert_eq!(digest(msg).as_slice(), r.bytes("MD"), "{file} Len = {len}");
                assert_eq!(streamed(msg).as_slice(), r.bytes("MD"), "{file} Len = {len}, a byte at a time");
                count += 1;
            }
            assert_eq!(count - before, want, "{file}: the parser merged or lost a message");
        }
        let file = format!("{name}Monte.rsp");
        let mut checkpoints = records(&file).into_iter().filter(|r| r.fields.contains_key("MD") || r.fields.contains_key("Seed"));
        let mut seed = checkpoints.next().expect("Seed").bytes("Seed");
        let mut monte = 0;
        for r in checkpoints {
            let mut md = [seed.clone(), seed.clone(), seed];
            for _ in 3..1003 {
                let next = digest(&md.concat()).to_vec();
                md = [md[1].clone(), md[2].clone(), next];
            }
            seed = md[2].clone();
            assert_eq!(seed, r.bytes("MD"), "{file} COUNT = {}", r.get("COUNT"));
            monte += 1;
        }
        assert_eq!(monte, lines(&file, "MD = "), "{file}");
        assert_eq!(monte, 100, "{file}");
        println!("{name}: {count} messages, {monte} Monte Carlo checkpoints");
    }
    macro_rules! streamed {
        ($hash:ident) => {
            |msg| {
                let mut h = $hash::new();
                for byte in msg.chunks(1) {
                    h.update(byte).unwrap();
                }
                h.finalize()
            }
        };
    }
    run("SHA256", |m| Sha256::digest(m).unwrap(), streamed!(Sha256));
    run("SHA384", |m| Sha384::digest(m).unwrap(), streamed!(Sha384));
    run("SHA512", |m| Sha512::digest(m).unwrap(), streamed!(Sha512));
}

/// SP 800-38D's encryption validation vectors. The interface takes a 96-bit
/// IV and AES-128 or AES-256; a tag shorter than 128 bits is compared as the
/// prefix of the sealed one.
#[test]
fn cavp_gcm() {
    for file in ["gcm/gcmEncryptExtIV128.rsp", "gcm/gcmEncryptExtIV256.rsp"] {
        let mut tally = Tally::default();
        let records = counted(file, "Count = ");
        for r in &records {
            let id = format!("{file} {:?} Count = {}", r.headers, r.get("Count"));
            let Ok(nonce) = <[u8; 12]>::try_from(r.bytes("IV")) else {
                tally.outside("IV not 96 bits");
                continue;
            };
            let key = r.bytes("Key");
            let (aad, tag) = (r.bytes("AAD"), r.bytes("Tag"));
            let mut sealed = r.bytes("PT");
            let (_, full) = match key.len() {
                16 => SealingKey::aes128_gcm(key.try_into().unwrap(), nonce),
                _ => SealingKey::aes256_gcm(key.try_into().unwrap(), nonce),
            }
            .seal(&aad, &mut sealed)
            .unwrap();
            assert_eq!((sealed, &full[..tag.len()]), (r.bytes("CT"), tag.as_slice()), "{id}");
            tally.judged("valid", true, &id);
        }
        tally.report(file, Some(records.len()));
    }
}

/// FIPS 186-4's signature and public-key validation vectors, on P-256 and
/// P-384 with SHA-256, SHA-384 and SHA-512, the digest as SEC 1's bits2int.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn cavp_ecdsa() {
    let curve = |r: &Record| -> Option<usize> {
        match r.headers[0].split(',').next().unwrap() {
            "P-256" => Some(32),
            "P-384" => Some(48),
            _ => None,
        }
    };
    let point = |r: &Record, width| -> Option<Vec<u8>> {
        let mut sec1 = vec![4];
        sec1.extend(fixed_be(&integer(r.get("Qx")), width)?);
        sec1.extend(fixed_be(&integer(r.get("Qy")), width)?);
        Some(sec1)
    };
    let passed = |r: &Record| match r.get("Result").chars().next() {
        Some('P') => "valid",
        Some('F') => "invalid",
        _ => panic!("Result = {}", r.get("Result")),
    };

    let file = "ecdsa/SigVer.rsp";
    let mut tally = Tally::default();
    let all = counted(file, "Msg = ");
    for r in &all {
        let id = format!("{file} {:?} Qx = {}", r.headers, r.get("Qx"));
        let Some(width) = curve(r) else {
            tally.outside("curve not P-256 or P-384");
            continue;
        };
        let hash = r.headers[0].split(',').nth(1).unwrap();
        if support::sha2(hash, b"").is_none() {
            tally.outside("hash not SHA-256, SHA-384 or SHA-512");
            continue;
        }
        let sec1 = point(r, width).expect("a signed key's coordinates fit");
        let der = der_signature(&integer(r.get("R")), &integer(r.get("S")));
        let accepted = ecdsa(&sec1, hash, &r.bytes("Msg"), &der).unwrap_or(false);
        tally.judged(passed(r), accepted, &id);
    }
    tally.report(file, Some(all.len()));

    let file = "ecdsa/PKV.rsp";
    let mut tally = Tally::default();
    let all = counted(file, "Qx = ");
    for r in &all {
        let id = format!("{file} {:?} Qx = {}", r.headers, r.get("Qx"));
        let Some(width) = curve(r) else {
            tally.outside("curve not P-256 or P-384");
            continue;
        };
        let Some(sec1) = point(r, width) else {
            assert_eq!(passed(r), "invalid", "{id}");
            tally.outside("coordinate wider than the field, which SEC 1 cannot carry");
            continue;
        };
        let accepted = match width {
            32 => toyos_crypto::ecc::p256::PublicKey::from_sec1(&sec1).is_ok(),
            _ => toyos_crypto::ecc::p384::PublicKey::from_sec1(&sec1).is_ok(),
        };
        tally.judged(passed(r), accepted, &id);
    }
    tally.report(file, Some(all.len()));
}

/// An ACVP file's cases, counted in its text rather than its parse.
fn tc_ids(name: &str) -> usize {
    support::acvp_text(name).matches("\"tcId\"").count()
}

/// ACVP's HMAC vectors. A `macLen` below the hash's is the prefix of the
/// whole tag, which is what the interface computes.
#[test]
fn acvp_hmac() {
    fn run(file: &str, mac: impl Fn(&[u8], &[u8]) -> Vec<u8>) {
        let mut count = 0;
        for g in acvp(file)["testGroups"].as_array().unwrap() {
            for t in g["tests"].as_array().unwrap() {
                let field = |name: &str| hex(t[name].as_str().unwrap());
                let want = field("mac");
                assert_eq!(want.len() * 8, t["macLen"].as_u64().unwrap() as usize);
                let tag = mac(&field("key"), &field("msg"));
                assert_eq!(&tag[..want.len()], want, "{file} tcId {}", t["tcId"]);
                count += 1;
            }
        }
        assert_eq!(count, tc_ids(file), "{file}: a case was not run");
        println!("{file}: {count} cases");
    }
    macro_rules! hmac {
        ($hash:ident, $len:literal) => {
            |key: &[u8], msg: &[u8]| {
                let mut out = [0; $len];
                Hmac::<$hash>::authenticate(key, msg, &mut out).unwrap();
                out.to_vec()
            }
        };
    }
    run("nist-hmac-sha256.json", hmac!(Sha256, 32));
    run("nist-hmac-sha384.json", hmac!(Sha384, 48));
    run("nist-hmac-sha512.json", hmac!(Sha512, 64));
    #[cfg(feature = "wpa2")]
    run("nist-hmac-sha1.json", |key, msg| toyos_crypto::wpa2::hmac_sha1(key, &[msg]).unwrap().to_vec());
}

/// ACVP's ML-KEM-768 vectors. The crate's decapsulation key is made only
/// from its seed, so the expanded-key cases are outside the interface.
#[test]
#[cfg_attr(not(feature = "portable"), ignore = "the same code in every form: run in the portable one")]
fn acvp_mlkem768() {
    use toyos_crypto::mlkem::{DecapsulationKey, EncapsulationKey};
    let field = |t: &serde_json::Value, name: &str| hex(t[name].as_str().unwrap());
    let seed = |t: &serde_json::Value| [field(t, "d"), field(t, "z")].concat();
    let mut tally = Tally::default();
    for g in acvp("nist-mlkem-keygen.json")["testGroups"].as_array().unwrap() {
        for t in g["tests"].as_array().unwrap() {
            if g["parameterSet"] != "ML-KEM-768" {
                tally.outside("not ML-KEM-768");
                continue;
            }
            let key = DecapsulationKey::generate(&mut Given::new(seed(t))).unwrap();
            assert_eq!(key.public_key().as_bytes().as_slice(), field(t, "ek"), "tcId {}", t["tcId"]);
            tally.judged("valid", true, "");
        }
    }
    tally.report("nist-mlkem-keygen.json", Some(tc_ids("nist-mlkem-keygen.json")));

    let mut tally = Tally::default();
    for g in acvp("nist-mlkem-encapdecap.json")["testGroups"].as_array().unwrap() {
        for t in g["tests"].as_array().unwrap() {
            let id = format!("tcId {} {}", t["tcId"], t["reason"]);
            if g["parameterSet"] != "ML-KEM-768" {
                tally.outside("not ML-KEM-768");
                continue;
            }
            match g["function"].as_str().unwrap() {
                "encapsulation" => {
                    let key = EncapsulationKey::from_bytes(&field(t, "ek")).unwrap();
                    let (c, k) = key.encapsulate(&mut Given::new(field(t, "m"))).unwrap();
                    assert_eq!((c.to_vec(), k.as_bytes().to_vec()), (field(t, "c"), field(t, "k")), "{id}");
                    tally.judged("valid", true, &id);
                }
                "decapsulation" if g["keyFormat"] == "seed" => {
                    let key = DecapsulationKey::generate(&mut Given::new(seed(t))).unwrap();
                    assert_eq!(key.decapsulate(&field(t, "c")).unwrap().as_bytes().as_slice(), field(t, "k"), "{id}");
                    tally.judged("valid", true, &id);
                }
                "encapsulationKeyCheck" => {
                    let verdict = if t["testPassed"].as_bool().unwrap() { "valid" } else { "invalid" };
                    tally.judged(verdict, EncapsulationKey::from_bytes(&field(t, "ek")).is_ok(), &id);
                }
                _ => tally.outside("expanded decapsulation key"),
            }
        }
    }
    tally.report("nist-mlkem-encapdecap.json", Some(tc_ids("nist-mlkem-encapdecap.json")));
}
