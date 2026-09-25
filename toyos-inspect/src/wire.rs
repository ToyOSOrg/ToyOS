//! The snapshot's wire form.
//!
//! ```text
//! snapshot := VERSION entry*
//! entry    := path_len:u8 path tag:u8 value
//! value    := u64 (little-endian)          tag 0
//!           | 0 | 1                        tag 1
//!           | len:u16 (little-endian) utf8 tag 2
//! ```
//!
//! Every path is under the owner's root and appears once, and text carries no
//! control character — a value is rendered after `=` on one line, so a newline
//! inside one would be a second line the owner did not answer with. The owner
//! builds with [`Snapshot`]; the reader takes what arrived through [`decode`],
//! which refuses every departure from the above by name.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::path::{check_path, PathError};
use crate::{Owner, MAX_SNAPSHOT_BYTES};

/// The first byte of every snapshot.
const VERSION: u8 = 1;

const TAG_U64: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_TEXT: u8 = 2;

/// One value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    U64(u64),
    Bool(bool),
    Text(String),
}

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Self::U64(v)
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::U64(v.into())
    }
}

impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Self::U64(v as u64)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Text(v.into())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::U64(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Text(v) => f.write_str(v),
        }
    }
}

/// An owner's answer, as it builds it.
pub struct Snapshot {
    root: &'static str,
    entries: BTreeMap<String, Value>,
}

impl Snapshot {
    pub fn new(owner: Owner) -> Self {
        Self { root: owner.root, entries: BTreeMap::new() }
    }

    /// Add `value` at `root.relative`.
    ///
    /// **A bad path, a repeated one or text with a control character panics**:
    /// every path is a literal in the owner's own source and every text value
    /// is the owner's own formatting, so each is a bug in the owner and never
    /// input it was handed.
    pub fn put(&mut self, relative: &str, value: impl Into<Value>) {
        let path = alloc::format!("{}.{relative}", self.root);
        if let Err(why) = check_path(&path) {
            panic!("inspect: {path:?} is not a path: {why}");
        }
        let value = value.into();
        if let Value::Text(text) = &value {
            if let Some(c) = text.chars().find(|c| c.is_control()) {
                panic!("inspect: {path} would carry the control character {c:?}");
            }
            assert!(u16::try_from(text.len()).is_ok(), "inspect: {path}'s text is too long");
        }
        let repeated = self.entries.insert(path.clone(), value);
        assert!(repeated.is_none(), "inspect: {path} is put twice");
    }

    /// The wire form, or [`EncodeError::TooLarge`] when it would not fit one
    /// frame.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = alloc::vec![VERSION];
        for (path, value) in &self.entries {
            out.push(path.len() as u8);
            out.extend_from_slice(path.as_bytes());
            match value {
                Value::U64(v) => {
                    out.push(TAG_U64);
                    out.extend_from_slice(&v.to_le_bytes());
                }
                Value::Bool(v) => {
                    out.push(TAG_BOOL);
                    out.push(u8::from(*v));
                }
                Value::Text(v) => {
                    out.push(TAG_TEXT);
                    out.extend_from_slice(&(v.len() as u16).to_le_bytes());
                    out.extend_from_slice(v.as_bytes());
                }
            }
        }
        if out.len() > MAX_SNAPSHOT_BYTES {
            return Err(EncodeError::TooLarge(out.len()));
        }
        Ok(out)
    }
}

/// Why a snapshot could not be encoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EncodeError {
    /// Its encoding, in bytes, is past [`MAX_SNAPSHOT_BYTES`].
    TooLarge(usize),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(len) => {
                write!(f, "{len} bytes is past one frame's {MAX_SNAPSHOT_BYTES}")
            }
        }
    }
}

/// Why an answer is not a snapshot from the owner that was asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    Empty,
    TooLarge(usize),
    Version(u8),
    /// The bytes end inside an entry that started at this offset.
    Truncated { at: usize },
    PathNotUtf8 { at: usize },
    Path { at: usize, why: PathError },
    /// A path under a root this owner does not speak for.
    OutsideRoot(String),
    Repeated(String),
    Tag { at: usize, tag: u8 },
    Bool { at: usize, byte: u8 },
    TextNotUtf8 { at: usize },
    TextControl { at: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the answer is empty"),
            Self::TooLarge(len) => {
                write!(f, "{len} bytes is past one frame's {MAX_SNAPSHOT_BYTES}")
            }
            Self::Version(v) => write!(f, "version {v} is not {VERSION}"),
            Self::Truncated { at } => write!(f, "the entry at byte {at} is cut short"),
            Self::PathNotUtf8 { at } => write!(f, "the path at byte {at} is not UTF-8"),
            Self::Path { at, why } => write!(f, "the path at byte {at}: {why}"),
            Self::OutsideRoot(path) => {
                write!(f, "{path} is under another owner's root")
            }
            Self::Repeated(path) => write!(f, "{path} is answered twice"),
            Self::Tag { at, tag } => write!(f, "value tag {tag} at byte {at} names no type"),
            Self::Bool { at, byte } => write!(f, "the bool at byte {at} is {byte}, not 0 or 1"),
            Self::TextNotUtf8 { at } => write!(f, "the text at byte {at} is not UTF-8"),
            Self::TextControl { at } => {
                write!(f, "the text at byte {at} carries a control character")
            }
        }
    }
}

/// Read what `owner` answered, sorted by path.
pub fn decode(bytes: &[u8], owner: Owner) -> Result<BTreeMap<String, Value>, DecodeError> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(DecodeError::TooLarge(bytes.len()));
    }
    let (&version, mut rest) = bytes.split_first().ok_or(DecodeError::Empty)?;
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let mut out = BTreeMap::new();
    while !rest.is_empty() {
        let at = bytes.len() - rest.len();
        let mut reader = Reader { bytes: rest, at };
        let path_len = reader.take(1)?[0] as usize;
        let path_bytes = reader.take(path_len)?;
        let path =
            core::str::from_utf8(path_bytes).map_err(|_| DecodeError::PathNotUtf8 { at })?;
        check_path(path).map_err(|why| DecodeError::Path { at, why })?;
        match path.split_once('.') {
            Some((root, _)) if root == owner.root => {}
            _ => return Err(DecodeError::OutsideRoot(path.into())),
        }
        let tag = reader.take(1)?[0];
        let value = match tag {
            TAG_U64 => {
                let raw: [u8; 8] = reader.take(8)?.try_into().expect("took eight");
                Value::U64(u64::from_le_bytes(raw))
            }
            TAG_BOOL => match reader.take(1)?[0] {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                byte => return Err(DecodeError::Bool { at, byte }),
            },
            TAG_TEXT => {
                let len = reader.take(2)?;
                let len = u16::from_le_bytes([len[0], len[1]]) as usize;
                let text = core::str::from_utf8(reader.take(len)?)
                    .map_err(|_| DecodeError::TextNotUtf8 { at })?;
                if text.chars().any(char::is_control) {
                    return Err(DecodeError::TextControl { at });
                }
                Value::Text(text.into())
            }
            tag => return Err(DecodeError::Tag { at, tag }),
        };
        if out.insert(String::from(path), value).is_some() {
            return Err(DecodeError::Repeated(path.into()));
        }
        rest = reader.bytes;
    }
    Ok(out)
}

/// The unread tail of one entry, and where that entry began.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.bytes.len() < n {
            return Err(DecodeError::Truncated { at: self.at });
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LOG, NET, SOUND};

    fn sample() -> Snapshot {
        let mut s = Snapshot::new(NET);
        s.put("link.state", "up");
        s.put("link.speed_mbps", 1000u32);
        s.put("link.full_duplex", true);
        s.put("mac", "52:54:00:12:34:56");
        s.put("errors.crc", u64::MAX);
        s.put("lease.address", "");
        s
    }

    #[test]
    fn a_snapshot_round_trips() {
        let bytes = sample().encode().unwrap();
        let back = decode(&bytes, NET).unwrap();
        assert_eq!(back.len(), 6);
        assert_eq!(back["net.link.state"], Value::Text("up".into()));
        assert_eq!(back["net.link.speed_mbps"], Value::U64(1000));
        assert_eq!(back["net.link.full_duplex"], Value::Bool(true));
        assert_eq!(back["net.errors.crc"], Value::U64(u64::MAX));
        assert_eq!(back["net.lease.address"], Value::Text("".into()));
        // An empty snapshot is a version byte and is valid: an owner with
        // nothing to say says nothing.
        assert!(decode(&Snapshot::new(LOG).encode().unwrap(), LOG).unwrap().is_empty());
    }

    /// Every proper prefix of a good encoding is a refusal, and a named one:
    /// the reader never renders half an entry.
    #[test]
    fn every_truncation_is_refused() {
        let bytes = sample().encode().unwrap();
        let mut boundaries = 0;
        for cut in 0..bytes.len() {
            match decode(&bytes[..cut], NET) {
                Err(DecodeError::Empty) => assert_eq!(cut, 0),
                Err(DecodeError::Truncated { .. }) => {}
                // A cut that lands between two entries is a shorter snapshot.
                Ok(fewer) => {
                    assert!(fewer.len() < 6);
                    boundaries += 1;
                }
                Err(other) => panic!("a cut at {cut} was refused as {other:?}"),
            }
        }
        assert_eq!(boundaries, 6, "one version byte and five inner entry boundaries");
    }

    #[test]
    fn an_answer_for_another_owner_is_refused() {
        let bytes = sample().encode().unwrap();
        assert_eq!(decode(&bytes, SOUND), Err(DecodeError::OutsideRoot("net.errors.crc".into())));
        // A bare root is no path under it.
        let mut bytes = alloc::vec![VERSION, 3];
        bytes.extend_from_slice(b"net");
        bytes.extend_from_slice(&[TAG_BOOL, 1]);
        assert_eq!(decode(&bytes, NET), Err(DecodeError::OutsideRoot("net".into())));
    }

    fn one(path: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut bytes = alloc::vec![VERSION, path.len() as u8];
        bytes.extend_from_slice(path);
        bytes.extend_from_slice(tail);
        bytes
    }

    #[test]
    fn a_malformed_answer_is_refused_by_name() {
        assert_eq!(decode(&[2], NET), Err(DecodeError::Version(2)));
        assert_eq!(decode(&one(b"net.x", &[9]), NET), Err(DecodeError::Tag { at: 1, tag: 9 }));
        assert_eq!(
            decode(&one(b"net.x", &[TAG_BOOL, 2]), NET),
            Err(DecodeError::Bool { at: 1, byte: 2 })
        );
        assert_eq!(
            decode(&one(b"net.x", &[TAG_TEXT, 1, 0, b'\n']), NET),
            Err(DecodeError::TextControl { at: 1 })
        );
        assert_eq!(
            decode(&one(b"net.x", &[TAG_TEXT, 1, 0, 0xff]), NET),
            Err(DecodeError::TextNotUtf8 { at: 1 })
        );
        assert!(matches!(
            decode(&one(b"net.X", &[TAG_BOOL, 1]), NET),
            Err(DecodeError::Path { at: 1, why: PathError::Char { ch: 'X', .. } })
        ));
        assert!(matches!(
            decode(&one(b"net.\xff", &[TAG_BOOL, 1]), NET),
            Err(DecodeError::PathNotUtf8 { at: 1 })
        ));
        let mut twice = one(b"net.x", &[TAG_BOOL, 1]);
        twice.extend_from_slice(&twice.clone()[1..]);
        assert_eq!(decode(&twice, NET), Err(DecodeError::Repeated("net.x".into())));
        let big = alloc::vec![VERSION; MAX_SNAPSHOT_BYTES + 1];
        assert_eq!(decode(&big, NET), Err(DecodeError::TooLarge(MAX_SNAPSHOT_BYTES + 1)));
    }

    #[test]
    fn an_owner_that_answers_past_one_frame_is_told_so() {
        let mut s = Snapshot::new(LOG);
        for i in 0..200 {
            s.put(&alloc::format!("part{i}.path"), "x".repeat(40));
        }
        assert!(matches!(s.encode(), Err(EncodeError::TooLarge(_))));
    }

    #[test]
    #[should_panic(expected = "control character")]
    fn an_owner_cannot_put_a_second_line_in_a_value() {
        Snapshot::new(LOG).put("volume.path", "a\nlog.volume.state = fine");
    }

    #[test]
    #[should_panic(expected = "put twice")]
    fn an_owner_cannot_answer_one_path_twice() {
        let mut s = Snapshot::new(LOG);
        s.put("volume.bytes", 1u64);
        s.put("volume.bytes", 2u64);
    }

    #[test]
    #[should_panic(expected = "is not a path")]
    fn an_owner_cannot_put_a_path_outside_the_grammar() {
        Snapshot::new(LOG).put("Volume", 1u64);
    }
}
