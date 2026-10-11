//! What a recording and its replay share: the authorizer, a scripted driver
//! whose every answer is a function of the events alone, and the transcript
//! file. `examples/record.rs` includes this file, so a replay drives the
//! server exactly as the recording did.

use std::fmt::Write as _;

use toyos_ssh::{Authorizer, Event, HostKey, Server};

/// The user every recording logs in as.
pub const USER: &str = "toyos";

/// The exit status the scripted driver reports, so a client that read it
/// exits with it.
pub const STATUS: u32 = 3;

/// The host key every recording was made with: a throwaway `ssh-keygen` key.
pub fn host_key() -> HostKey {
    HostKey::from_openssh(include_str!("../fixtures/host_ed25519")).expect("the fixture host key")
}

/// The key blob of an OpenSSH `.pub` line.
pub fn public_blob(line: &str) -> Vec<u8> {
    decode_base64(line.split(' ').nth(1).expect("a .pub line"))
}

/// The Ed25519 public key of an OpenSSH `.pub` line.
pub fn public_key(line: &str) -> [u8; 32] {
    let blob = public_blob(line);
    blob[blob.len() - 32..].try_into().unwrap()
}

/// The recordings' user key, the one the authorizer names.
pub fn user_key() -> [u8; 32] {
    public_key(include_str!("../fixtures/user_ed25519.pub"))
}

/// Names [`USER`] with the keys it holds; after `answers` questions it names
/// nobody, as a key removed from the file between two of them.
pub struct Keys {
    pub keys: Vec<[u8; 32]>,
    pub asked: usize,
    pub answers: usize,
}

impl Keys {
    pub fn new(keys: Vec<[u8; 32]>) -> Self {
        Self { keys, asked: 0, answers: usize::MAX }
    }

    pub fn recorded() -> Self {
        Self::new(vec![user_key()])
    }
}

impl Authorizer for Keys {
    fn authorizes(&mut self, user: &str, key: &[u8; 32]) -> bool {
        self.asked += 1;
        self.asked <= self.answers && user == USER && self.keys.contains(key)
    }
}

/// Answer every event: an `exec` is greeted with a line naming its command,
/// stdin is echoed back, and EOF ends the program with [`STATUS`].
pub fn drive<R: ring::rand::SecureRandom>(server: &mut Server<Keys, R>) -> Vec<Event> {
    let mut seen = Vec::new();
    while let Some(event) = server.poll() {
        match &event {
            Event::Exec { channel, command } => {
                let line = [b"exec: ", &command[..], b"\n"].concat();
                assert_eq!(server.send(*channel, &line), Ok(line.len()));
            }
            Event::Data { channel, data } => {
                assert_eq!(server.send(*channel, data), Ok(data.len()), "the client's window is wide enough for an echo");
                server.consumed(*channel, data.len()).unwrap();
            }
            Event::Eof { channel } => server.exit(*channel, STATUS).unwrap(),
            Event::Authenticated { .. }
            | Event::Closed { .. }
            | Event::Writable { .. }
            | Event::Declined(_)
            | Event::Disconnected => {}
        }
        seen.push(event);
    }
    seen
}

/// Which side a transcript chunk came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Server,
}

/// A recorded session: the seed byte of its randomness and every chunk in the
/// order the recorder saw it.
pub struct Transcript {
    pub seed: u8,
    pub chunks: Vec<(Side, Vec<u8>)>,
}

impl Transcript {
    /// The file's text: `seed <hex>`, then one `c` or `s` line per 64 bytes.
    pub fn write(&self, comment: &str) -> String {
        let mut text = format!("# {comment}\nseed {:02x}\n", self.seed);
        for (side, bytes) in &self.chunks {
            let tag = match side {
                Side::Client => 'c',
                Side::Server => 's',
            };
            for line in bytes.chunks(64) {
                text.push(tag);
                text.push(' ');
                for b in line {
                    write!(text, "{b:02x}").unwrap();
                }
                text.push('\n');
            }
        }
        text
    }

    pub fn read(text: &str) -> Self {
        let mut seed = None;
        let mut chunks: Vec<(Side, Vec<u8>)> = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let (tag, hex) = line.split_once(' ').expect("a tagged line");
            let bytes: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            let side = match tag {
                "seed" => {
                    seed = Some(bytes[0]);
                    continue;
                }
                "c" => Side::Client,
                "s" => Side::Server,
                other => panic!("a transcript line tagged {other}"),
            };
            match chunks.last_mut() {
                Some((last, chunk)) if *last == side => chunk.extend_from_slice(&bytes),
                _ => chunks.push((side, bytes)),
            }
        }
        Self { seed: seed.expect("a seed line"), chunks }
    }

    /// Every byte one side sent, in order.
    pub fn side(&self, side: Side) -> Vec<u8> {
        self.chunks.iter().filter(|(s, _)| *s == side).flat_map(|(_, b)| b.iter().copied()).collect()
    }
}

fn decode_base64(text: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("{c:#x} is not base64"),
        }
    };
    let digits: Vec<u8> = text.trim().bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &c)| acc | value(c) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}
