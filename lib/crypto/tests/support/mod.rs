//! What the vector, differential and fuzz tests share: the committed data,
//! hex, a seeded generator and a caller-supplied `Random` of fixed bytes.
#![allow(dead_code, reason = "each test binary takes the part it needs")]

use std::path::PathBuf;

/// A file under this crate's `tests/data/`.
pub fn data(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A NIST ACVP vector file under the repository's `tests/acvp/`, outside
/// this crate's directory: test input, which the licence gate would count as
/// shipping inside it.
pub fn acvp(name: &str) -> serde_json::Value {
    serde_json::from_str(&acvp_text(name)).unwrap()
}

/// [`acvp`]'s file as text.
pub fn acvp_text(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/acvp").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A response file under the repository's `tests/cavp/`.
pub fn cavp(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cavp").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "{text:?} is not whole bytes of hex");
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex")).collect()
}

/// A hexadecimal integer, whose digits may be odd in number.
pub fn integer(text: &str) -> Vec<u8> {
    hex(&if text.len() % 2 == 1 { format!("0{text}") } else { text.to_string() })
}

/// A big-endian integer as exactly `n` bytes: leading zeros dropped or
/// added, `None` where its value needs more.
pub fn fixed_be(bytes: &[u8], n: usize) -> Option<Vec<u8>> {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let value = &bytes[first..];
    (value.len() <= n).then(|| {
        let mut out = vec![0; n - value.len()];
        out.extend_from_slice(value);
        out
    })
}

/// A big-endian integer without its leading zero bytes.
pub fn minimal(bytes: &[u8]) -> Vec<u8> {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[first..].to_vec()
}

/// SEC 1 §4.1.4's bits2int as a digest the curve's width: the leftmost bits
/// of a longer hash, a shorter one left-padded.
pub fn bits2int(digest: &[u8], width: usize) -> Vec<u8> {
    if digest.len() >= width {
        digest[..width].to_vec()
    } else {
        let mut out = vec![0; width - digest.len()];
        out.extend_from_slice(digest);
        out
    }
}

/// The crate's hashes by Wycheproof's and NIST's names.
pub fn sha2(name: &str, message: &[u8]) -> Option<Vec<u8>> {
    Some(match name {
        "SHA-256" | "SHA2-256" => toyos_crypto::hash::Sha256::digest(message).unwrap().to_vec(),
        "SHA-384" | "SHA2-384" => toyos_crypto::hash::Sha384::digest(message).unwrap().to_vec(),
        "SHA-512" | "SHA2-512" => toyos_crypto::hash::Sha512::digest(message).unwrap().to_vec(),
        _ => return None,
    })
}

/// An ECDSA verdict for a SEC 1 key, a hash's digest and a DER signature, on
/// the curve the key's length names; `Err` where the key is refused.
pub fn ecdsa(sec1: &[u8], hash: &str, message: &[u8], der: &[u8]) -> Result<bool, toyos_crypto::Error> {
    use toyos_crypto::ecc::{p256, p384};
    let digest = sha2(hash, message).expect("a SHA-2 hash");
    match sec1.len() {
        65 => {
            let key = p256::PublicKey::from_sec1(sec1)?;
            Ok(key.verify_digest(bits2int(&digest, 32).as_slice().try_into().unwrap(), der).is_ok())
        }
        97 => {
            let key = p384::PublicKey::from_sec1(sec1)?;
            Ok(key.verify_digest(bits2int(&digest, 48).as_slice().try_into().unwrap(), der).is_ok())
        }
        _ => Err(toyos_crypto::Error::Length),
    }
}

/// SplitMix64: the fuzz's and the differential's one seeded source, so a red
/// names its case.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            *byte = self.next() as u8;
        }
    }

    pub fn array<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        self.fill(&mut out);
        out
    }

    pub fn exactly(&mut self, n: usize) -> Vec<u8> {
        let mut out = vec![0; n];
        self.fill(&mut out);
        out
    }

    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let mut out = vec![0; self.below(max + 1)];
        self.fill(&mut out);
        out
    }
}

/// `cases` cases of `run` over every CPU, case `i` drawing from a generator
/// of its own seeded by `seed` and `i`, so it is the same case whichever
/// thread takes it.
pub fn seeded(cases: usize, seed: u64, run: impl Fn(usize, &mut Rng) + Sync) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for first in 0..threads {
            let run = &run;
            scope.spawn(move || {
                for case in (first..cases).step_by(threads) {
                    run(case, &mut Rng(seed ^ (case as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)));
                }
            });
        }
    });
}

/// The caller's entropy as given bytes, refused once they run out.
pub struct Given {
    pub bytes: Vec<u8>,
    pub at: usize,
}

impl Given {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self { bytes: bytes.into(), at: 0 }
    }
}

impl toyos_crypto::Random for Given {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        let end = self.at + out.len();
        out.copy_from_slice(self.bytes.get(self.at..end).ok_or(toyos_crypto::Error::Entropy)?);
        self.at = end;
        Ok(())
    }
}

/// A DER `SEQUENCE` of two positive `INTEGER`s, as ECDSA signs: each minimal,
/// with a zero byte before a set high bit.
pub fn der_signature(r: &[u8], s: &[u8]) -> Vec<u8> {
    fn integer(out: &mut Vec<u8>, value: &[u8]) {
        let first = value.iter().position(|&b| b != 0).unwrap_or(value.len() - 1);
        let value = &value[first..];
        let pad = value[0] & 0x80 != 0;
        out.push(2);
        out.push((value.len() + usize::from(pad)) as u8);
        if pad {
            out.push(0);
        }
        out.extend_from_slice(value);
    }
    let mut body = Vec::new();
    integer(&mut body, r);
    integer(&mut body, s);
    assert!(body.len() < 128, "a P-256 or P-384 signature has a short length");
    let mut out = vec![0x30, body.len() as u8];
    out.extend(body);
    out
}

/// How one vector file's cases came out. Every case is counted once: as an
/// agreement with the file's verdict, as an `acceptable` case either way, or
/// as outside the interface by a named reason: refused by a call that
/// returned an error, by a type that cannot carry its input, or (for PSS)
/// made under parameters the interface does not take.
#[derive(Default)]
pub struct Tally {
    pub valid: usize,
    pub invalid: usize,
    pub acceptable_accepted: usize,
    pub acceptable_refused: usize,
    pub outside: std::collections::BTreeMap<&'static str, usize>,
}

impl Tally {
    pub fn outside(&mut self, why: &'static str) {
        *self.outside.entry(why).or_default() += 1;
    }

    /// One case the interface takes: `accepted` against the file's `result`.
    pub fn judged(&mut self, result: &str, accepted: bool, case: impl std::fmt::Display) {
        match result {
            "valid" => {
                assert!(accepted, "{case}: valid and refused");
                self.valid += 1;
            }
            "invalid" => {
                assert!(!accepted, "{case}: invalid and accepted");
                self.invalid += 1;
            }
            "acceptable" if accepted => self.acceptable_accepted += 1,
            "acceptable" => self.acceptable_refused += 1,
            other => panic!("{case}: no result {other:?}"),
        }
    }

    pub fn total(&self) -> usize {
        self.valid + self.invalid + self.acceptable_accepted + self.acceptable_refused + self.outside.values().sum::<usize>()
    }

    /// Printed, and held against the count the file declares.
    pub fn report(&self, file: &str, declared: Option<usize>) {
        if let Some(declared) = declared {
            assert_eq!(self.total(), declared, "{file}: a case was neither judged nor counted outside");
        }
        println!(
            "{file}: {} cases; {} valid and {} invalid agreed; acceptable {} accepted, {} refused; outside the interface: {:?}",
            self.total(),
            self.valid,
            self.invalid,
            self.acceptable_accepted,
            self.acceptable_refused,
            self.outside
        );
    }
}
