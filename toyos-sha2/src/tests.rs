//! The oracles: NIST's CAVP SHAVS response files (`cavp/`, `NOTICE`), and
//! RustCrypto's `sha2` over every message length to 4096 bytes in random
//! splits.

use super::*;

/// A response file under `cavp/`.
fn rsp(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cavp")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "{text:?} is not whole bytes of hex");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Every `key = value` line of a response file, in order.
fn fields(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        line.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split_once(" = ")
    })
}

/// A digest, held to the length its section's `[L = n]` header names.
fn md(name: &str, l: Option<usize>, value: &str) -> Vec<u8> {
    let md = unhex(value);
    assert_eq!(
        Some(md.len()),
        l,
        "{name}: an MD that is not its section's L bytes"
    );
    md
}

/// A message file's vectors: its length in bits, its bytes and its digest.
fn messages(name: &str) -> Vec<(usize, Vec<u8>, Vec<u8>)> {
    let text = rsp(name);
    let mut out = Vec::new();
    let mut len = None;
    let mut msg = None;
    let mut l = None;
    for (key, value) in fields(&text) {
        match key {
            "Len" => len = Some(value.parse().expect("a length")),
            "Msg" => msg = Some(unhex(value)),
            "L" => l = Some(value.parse().expect("a digest length")),
            "MD" => out.push((
                len.take().expect("Len"),
                msg.take().expect("Msg"),
                md(name, l, value),
            )),
            _ => panic!("{name}: no field {key:?} in a message file"),
        }
    }
    assert!(!out.is_empty(), "{name} holds no vectors");
    out
}

/// A Monte Carlo file's seed and its hundred checkpoints.
fn monte(name: &str) -> (Vec<u8>, Vec<Vec<u8>>) {
    let text = rsp(name);
    let mut seed = None;
    let mut want = Vec::new();
    let mut l = None;
    for (key, value) in fields(&text) {
        match key {
            "Seed" => seed = Some(unhex(value)),
            "COUNT" => assert_eq!(
                value.parse::<usize>().expect("a count"),
                want.len(),
                "{name}"
            ),
            "L" => l = Some(value.parse().expect("a digest length")),
            "MD" => want.push(md(name, l, value)),
            _ => panic!("{name}: no field {key:?} in a Monte Carlo file"),
        }
    }
    assert_eq!(want.len(), 100, "{name}");
    (seed.expect("Seed"), want)
}

/// Every vector of a byte-oriented file, whole and streamed a byte at a time.
fn byte_file(name: &str, digest: impl Fn(&[u8]) -> Vec<u8>, streamed: impl Fn(&[u8]) -> Vec<u8>) {
    for (len, msg, want) in messages(name) {
        assert_eq!(len % 8, 0, "{name}: Len = {len} in a byte-oriented file");
        let msg = &msg[..len / 8];
        assert_eq!(hexed(&digest(msg)), hexed(&want), "{name}: Len = {len}");
        assert_eq!(
            hexed(&streamed(msg)),
            hexed(&want),
            "{name}: Len = {len}, a byte at a time"
        );
    }
}

fn hexed(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHAVS §6.4's Monte Carlo test: each digest is of the three before it, and
/// every thousandth is a checkpoint and the next round's seed.
fn monte_file(name: &str, digest: impl Fn(&[u8]) -> Vec<u8>) {
    let (mut seed, want) = monte(name);
    for (count, want) in want.iter().enumerate() {
        let mut md = [seed.clone(), seed.clone(), seed];
        for _ in 3..1003 {
            let next = digest(&md.concat());
            md = [md[1].clone(), md[2].clone(), next];
        }
        seed = md[2].clone();
        assert_eq!(hexed(&seed), hexed(want), "{name}: COUNT = {count}");
    }
}

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
    byte_file(
        "shabytetestvectors/SHA256ShortMsg.rsp",
        sha256,
        sha256_bytewise,
    );
    byte_file(
        "shabytetestvectors/SHA256LongMsg.rsp",
        sha256,
        sha256_bytewise,
    );
}

#[test]
fn sha512_byte_vectors() {
    byte_file(
        "shabytetestvectors/SHA512ShortMsg.rsp",
        sha512,
        sha512_bytewise,
    );
    byte_file(
        "shabytetestvectors/SHA512LongMsg.rsp",
        sha512,
        sha512_bytewise,
    );
}

#[test]
fn sha256_monte_carlo() {
    monte_file("shabytetestvectors/SHA256Monte.rsp", sha256);
    monte_file("shabittestvectors/SHA256Monte.rsp", sha256);
}

#[test]
fn sha512_monte_carlo() {
    monte_file("shabytetestvectors/SHA512Monte.rsp", sha512);
    monte_file("shabittestvectors/SHA512Monte.rsp", sha512);
}

/// `len` bits of `msg` padded as §5.1.1 and §5.1.2 pad a message of any bit
/// length — written here against the text, where this crate pads only whole
/// bytes — into blocks of `block` bytes ending in a `length`-byte field.
fn padded(msg: &[u8], len: usize, block: usize, length: usize) -> Vec<u8> {
    let mut out = msg[..len.div_ceil(8)].to_vec();
    match len % 8 {
        0 => out.push(0x80),
        used => {
            let last = out.last_mut().expect("a partial byte");
            *last = (*last & (0xff << (8 - used))) | (0x80 >> used);
        }
    }
    while out.len() % block != block - length {
        out.push(0);
    }
    out.extend_from_slice(&(len as u128).to_be_bytes()[16 - length..]);
    out
}

/// Every vector of a bit-oriented message file, through the compression
/// function under [`padded`]; one whose length is whole bytes through the
/// public digest too.
fn bit_file<W: Copy>(
    name: &str,
    block: usize,
    length: usize,
    init: [W; 8],
    compress: impl Fn(&mut [W; 8], &[u8]),
    be: impl Fn(W) -> Vec<u8>,
    digest: impl Fn(&[u8]) -> Vec<u8>,
) {
    let vectors = messages(name);
    assert!(
        vectors.iter().any(|(len, ..)| len % 8 != 0),
        "{name} holds no partial byte"
    );
    for (len, msg, want) in vectors {
        let mut state = init;
        for chunk in padded(&msg, len, block, length).chunks_exact(block) {
            compress(&mut state, chunk);
        }
        let got: Vec<u8> = state.into_iter().flat_map(&be).collect();
        assert_eq!(hexed(&got), hexed(&want), "{name}: Len = {len}");
        if len % 8 == 0 {
            assert_eq!(
                hexed(&digest(&msg[..len / 8])),
                hexed(&want),
                "{name}: Len = {len}, whole bytes"
            );
        }
    }
}

#[test]
fn sha256_bit_vectors() {
    bit_file(
        "shabittestvectors/SHA256ShortMsg.rsp",
        64,
        8,
        H256,
        |state, block| compress256(state, block.try_into().expect("a block")),
        |w: u32| w.to_be_bytes().to_vec(),
        sha256,
    );
}

#[test]
fn sha512_bit_vectors() {
    bit_file(
        "shabittestvectors/SHA512ShortMsg.rsp",
        128,
        16,
        H512,
        |state, block| compress512(state, block.try_into().expect("a block")),
        |w: u64| w.to_be_bytes().to_vec(),
        sha512,
    );
}

/// SplitMix64: the splits' source, seeded so a red names a reproducible one.
struct Draws(u64);

impl Draws {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Cut points that split `0..len` into random pieces, empty ones among them.
    fn splits(&mut self, len: usize) -> Vec<usize> {
        let mut cuts: Vec<usize> = (0..self.next() % 8)
            .map(|_| (self.next() % (len as u64 + 1)) as usize)
            .collect();
        cuts.sort_unstable();
        cuts
    }
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
