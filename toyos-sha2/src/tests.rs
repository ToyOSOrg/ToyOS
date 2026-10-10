//! The oracles: NIST's CAVP SHAVS response files (`tests/cavp/`, outside every
//! shipped package's directory; `NOTICE`), and RustCrypto's `sha2` over every
//! message length to 4096 bytes in random splits.

use super::*;

mod shavs;
use shavs::*;

fn sha256(msg: &[u8]) -> Vec<u8> {
    Sha256::digest(msg).to_vec()
}

fn sha512(msg: &[u8]) -> Vec<u8> {
    Sha512::digest(msg).to_vec()
}

fn sha256_bytewise(msg: &[u8]) -> Vec<u8> {
    let mut hash = Sha256::new();
    msg.iter().for_each(|b| hash.update([*b]));
    hash.finalize().to_vec()
}

fn sha512_bytewise(msg: &[u8]) -> Vec<u8> {
    let mut hash = Sha512::new();
    msg.iter().for_each(|b| hash.update([*b]));
    hash.finalize().to_vec()
}

#[test]
fn sha256_byte_vectors() {
    byte_file("SHA256ShortMsg.rsp", sha256, sha256_bytewise);
    byte_file("SHA256LongMsg.rsp", sha256, sha256_bytewise);
}

#[test]
fn sha512_byte_vectors() {
    byte_file("SHA512ShortMsg.rsp", sha512, sha512_bytewise);
    byte_file("SHA512LongMsg.rsp", sha512, sha512_bytewise);
}

#[test]
fn sha256_monte_carlo() {
    monte_file("SHA256Monte.rsp", sha256);
}

#[test]
fn sha512_monte_carlo() {
    monte_file("SHA512Monte.rsp", sha512);
}

/// **The differential**: every length from empty to 4096 bytes, of random
/// bytes, digested whole and streamed in three random splits each, against
/// RustCrypto's `sha2` — which shares no code with this crate.
#[test]
fn every_length_to_4096_in_random_splits_agrees_with_sha2() {
    use sha2::Digest as _;
    let mut draws = Draws(0x5eed_70e0_5a2b_0001);
    for len in 0..=4096usize {
        let msg: Vec<u8> = (0..len).map(|_| draws.next() as u8).collect();
        let want256: [u8; 32] = sha2::Sha256::digest(&msg).into();
        let want512: [u8; 64] = sha2::Sha512::digest(&msg).into();
        assert_eq!(Sha256::digest(&msg), want256, "SHA-256 of {len} bytes");
        assert_eq!(Sha512::digest(&msg), want512, "SHA-512 of {len} bytes");
        for _ in 0..3 {
            let cuts = draws.splits(len);
            let (mut a, mut b) = (Sha256::new(), Sha512::new());
            let mut at = 0;
            for cut in cuts.iter().copied().chain([len]) {
                a.update(&msg[at..cut]);
                b.update(&msg[at..cut]);
                at = cut;
            }
            assert_eq!(
                a.finalize(),
                want256,
                "SHA-256 of {len} bytes cut at {cuts:?}"
            );
            assert_eq!(
                b.finalize(),
                want512,
                "SHA-512 of {len} bytes cut at {cuts:?}"
            );
        }
    }
}
