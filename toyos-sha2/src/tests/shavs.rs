//! SHAVS's response files (`tests/cavp/`) read and driven, and the seeded
//! draws a differential splits its messages with: this crate's tests', and
//! `toyos-sha2-hw`'s, which include this file by path.

/// A response file under the repository's `tests/cavp/`.
fn rsp(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/cavp")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn unhex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "{text:?} is not whole bytes of hex"
    );
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
pub fn byte_file(name: &str, digest: impl Fn(&[u8]) -> Vec<u8>, streamed: impl Fn(&[u8]) -> Vec<u8>) {
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
pub fn monte_file(name: &str, digest: impl Fn(&[u8]) -> Vec<u8>) {
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

/// SplitMix64: the splits' source, seeded so a red names a reproducible one.
pub struct Draws(pub u64);

impl Draws {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Cut points that split `0..len` into random pieces, empty ones among them.
    pub fn splits(&mut self, len: usize) -> Vec<usize> {
        let mut cuts: Vec<usize> = (0..self.next() % 8)
            .map(|_| (self.next() % (len as u64 + 1)) as usize)
            .collect();
        cuts.sort_unstable();
        cuts
    }
}
