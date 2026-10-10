//! The instruction path against NIST's CAVP SHA-256 response files and, over
//! every message length to 4096 bytes in random splits, against `toyos-sha2`'s
//! scalar compression, which those files and RustCrypto's `sha2` hold.

use toyos_sha2::Sha256;

#[path = "../../toyos-sha2/src/tests/shavs.rs"]
mod shavs;
use shavs::*;

/// A hash on the instructions, which a host without them cannot test: it says
/// so rather than passing on the scalar path.
fn hashing() -> Sha256 {
    let compress = super::x86_64::compress()
        .expect("this host's CPU has no SHA extensions, so nothing here can test them");
    Sha256::with(compress)
}

fn sha256(msg: &[u8]) -> Vec<u8> {
    let mut hash = hashing();
    hash.update(msg);
    hash.finalize().to_vec()
}

fn sha256_bytewise(msg: &[u8]) -> Vec<u8> {
    let mut hash = hashing();
    msg.iter().for_each(|b| hash.update([*b]));
    hash.finalize().to_vec()
}

#[test]
fn sha256_byte_vectors() {
    byte_file("SHA256ShortMsg.rsp", sha256, sha256_bytewise);
    byte_file("SHA256LongMsg.rsp", sha256, sha256_bytewise);
}

#[test]
fn sha256_monte_carlo() {
    monte_file("SHA256Monte.rsp", sha256);
}

/// **The differential**: every length from empty to 4096 bytes, of random
/// bytes, digested whole and streamed in three random splits each.
#[test]
fn every_length_to_4096_in_random_splits_agrees_with_the_scalar_compression() {
    let mut draws = Draws(0x5eed_70e0_5a2b_0002);
    for len in 0..=4096usize {
        let msg: Vec<u8> = (0..len).map(|_| draws.next() as u8).collect();
        let want = Sha256::digest(&msg);
        assert_eq!(sha256(&msg), want, "{len} bytes");
        for _ in 0..3 {
            let cuts = draws.splits(len);
            let mut hash = hashing();
            let mut at = 0;
            for cut in cuts.iter().copied().chain([len]) {
                hash.update(&msg[at..cut]);
                at = cut;
            }
            assert_eq!(hash.finalize(), want, "{len} bytes cut at {cuts:?}");
        }
    }
}
