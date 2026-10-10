//! The SSH data types (RFC 4251 §5), read from the peer's bytes and written for
//! it. A [`Reader`] checks every length against what remains and against a cap
//! its caller names, and refuses with the field's name; it never panics.

use std::fmt;

/// Why the server ended a session: the field or the rule the peer's bytes
/// broke. After one, the session is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The field ends past the bytes that carry it.
    Truncated(&'static str),
    /// A length above the cap named for its field.
    TooLong { field: &'static str, len: usize, cap: usize },
    /// Bytes after the last field of a message.
    Trailing(&'static str),
    /// A field that is not UTF-8.
    NotUtf8(&'static str),
    /// A field whose value this server does not take.
    Malformed(&'static str),
    /// A message this phase does not take, by number.
    Unexpected { phase: &'static str, message: u8 },
    /// No algorithm in this list that both sides name.
    NoCommonAlgorithm(&'static str),
    /// The client's first KEXINIT does not offer `kex-strict-c-v00@openssh.com`,
    /// and no session runs without the sequence-number reset (CVE-2023-48795).
    NotStrict,
    /// The client's X25519 key, refused because the secret it makes is zero
    /// (RFC 7748 §6.1).
    KeyAgreement,
    /// A packet whose tag does not verify. The peer is told nothing.
    Integrity,
    /// A sealed packet's length, decrypted but not yet verified, above the
    /// cap or off the block. The peer is told nothing: what a length decrypts
    /// to is not said before its tag.
    SealedLength { len: usize, cap: usize },
    /// A failed authentication past the cap.
    TooManyAttempts,
    /// A sequence number that would wrap under one key, reusing a nonce.
    SequenceExhausted,
    /// Input after the session ended.
    Ended,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(field) => write!(f, "{field} ends early"),
            Self::TooLong { field, len, cap } => write!(f, "{field} is {len} bytes, above the cap of {cap}"),
            Self::Trailing(field) => write!(f, "bytes after the last field of {field}"),
            Self::NotUtf8(field) => write!(f, "{field} is not UTF-8"),
            Self::Malformed(why) => f.write_str(why),
            Self::Unexpected { phase, message } => write!(f, "message {message} during {phase}"),
            Self::NoCommonAlgorithm(list) => write!(f, "no algorithm in {list} this server offers"),
            Self::NotStrict => f.write_str("the client does not offer kex-strict-c-v00@openssh.com"),
            Self::KeyAgreement => f.write_str("the client's X25519 key makes a zero secret"),
            Self::Integrity => f.write_str("a packet whose tag does not verify"),
            Self::SealedLength { len, cap } => {
                write!(f, "a sealed packet_length of {len}, above the cap of {cap} or off the block, before its tag")
            }
            Self::TooManyAttempts => f.write_str("too many failed authentication attempts"),
            Self::SequenceExhausted => f.write_str("a sequence number would wrap under one key"),
            Self::Ended => f.write_str("input after the session ended"),
        }
    }
}

/// The longest name-list read, in bytes: OpenSSH's longest, its host key
/// algorithms, is under a kilobyte.
const NAME_LIST_CAP: usize = 4096;

/// The longest algorithm, method or request name (RFC 4251 §6).
const NAME_CAP: usize = 64;

/// The peer's bytes, read front to back.
pub(crate) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    pub(crate) fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], Refusal> {
        if n > self.0.len() {
            return Err(Refusal::Truncated(field));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], Refusal> {
        let (head, rest) = self.0.split_first_chunk::<N>().ok_or(Refusal::Truncated(field))?;
        self.0 = rest;
        Ok(*head)
    }

    pub(crate) fn byte(&mut self, field: &'static str) -> Result<u8, Refusal> {
        let [b] = self.array(field)?;
        Ok(b)
    }

    /// A `boolean`: every value but zero is true (RFC 4251 §5).
    pub(crate) fn boolean(&mut self, field: &'static str) -> Result<bool, Refusal> {
        Ok(self.byte(field)? != 0)
    }

    pub(crate) fn u32(&mut self, field: &'static str) -> Result<u32, Refusal> {
        Ok(u32::from_be_bytes(self.array(field)?))
    }

    /// A `string` of at most `cap` bytes.
    pub(crate) fn string(&mut self, field: &'static str, cap: usize) -> Result<&'a [u8], Refusal> {
        let len = self.u32(field)? as usize;
        if len > cap {
            return Err(Refusal::TooLong { field, len, cap });
        }
        self.take(len, field)
    }

    /// A `string` of exactly `N` bytes.
    pub(crate) fn fixed<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], Refusal> {
        let bytes = self.string(field, N)?;
        bytes.try_into().map_err(|_| Refusal::Truncated(field))
    }

    /// A UTF-8 `string` of at most `cap` bytes.
    pub(crate) fn text(&mut self, field: &'static str, cap: usize) -> Result<&'a str, Refusal> {
        std::str::from_utf8(self.string(field, cap)?).map_err(|_| Refusal::NotUtf8(field))
    }

    /// One algorithm, method or request name: printable US-ASCII without a
    /// comma or a space, at most 64 bytes (RFC 4251 §6).
    pub(crate) fn name(&mut self, field: &'static str) -> Result<&'a str, Refusal> {
        let name = self.text(field, NAME_CAP)?;
        if name.is_empty() || !name.bytes().all(name_byte) {
            return Err(Refusal::Malformed("a name that is empty or not printable US-ASCII"));
        }
        Ok(name)
    }

    /// A `name-list` (RFC 4251 §5): names as [`Reader::name`] reads one,
    /// joined by commas.
    pub(crate) fn name_list(&mut self, field: &'static str) -> Result<NameList<'a>, Refusal> {
        let list = self.text(field, NAME_LIST_CAP)?;
        if !list.is_empty() {
            for name in list.split(',') {
                if name.is_empty() || name.len() > NAME_CAP || !name.bytes().all(name_byte) {
                    return Err(Refusal::Malformed("a name-list with an empty, long or unprintable name"));
                }
            }
        }
        Ok(NameList(list))
    }

    /// How many bytes are left.
    pub(crate) fn rest_len(&self) -> usize {
        self.0.len()
    }

    /// The message ends here.
    pub(crate) fn end(self, message: &'static str) -> Result<(), Refusal> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Refusal::Trailing(message))
        }
    }
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_graphic() && b != b','
}

/// A name-list [`Reader::name_list`] has checked.
#[derive(Clone, Copy)]
pub(crate) struct NameList<'a>(&'a str);

impl<'a> NameList<'a> {
    pub(crate) fn names(self) -> impl Iterator<Item = &'a str> {
        self.0.split(',').filter(|name| !name.is_empty())
    }

    pub(crate) fn contains(self, name: &str) -> bool {
        self.names().any(|n| n == name)
    }
}

pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_bool(out: &mut Vec<u8>, value: bool) {
    out.push(u8::from(value));
}

/// A `string`. Every one the server writes is its own and far below 4 GiB.
pub(crate) fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, u32::try_from(bytes.len()).expect("a string the server writes fits a u32"));
    out.extend_from_slice(bytes);
}

/// An `mpint` holding the unsigned big-endian integer `magnitude`: no leading
/// zero bytes, and one zero byte where the top bit would read as a sign
/// (RFC 4251 §5).
pub(crate) fn put_mpint(out: &mut Vec<u8>, magnitude: &[u8]) {
    let first = magnitude.iter().position(|&b| b != 0).unwrap_or(magnitude.len());
    let digits = &magnitude[first..];
    let sign = digits.first().is_some_and(|&b| b & 0x80 != 0);
    put_u32(out, u32::try_from(digits.len() + usize::from(sign)).expect("an mpint the server writes fits a u32"));
    if sign {
        out.push(0);
    }
    out.extend_from_slice(digits);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4251 §5's own `mpint` examples.
    #[test]
    fn an_mpint_is_written_as_rfc_4251_spells_it() {
        let mpint = |magnitude: &[u8]| {
            let mut out = Vec::new();
            put_mpint(&mut out, magnitude);
            out
        };
        assert_eq!(mpint(&[0, 0]), [0, 0, 0, 0]);
        assert_eq!(mpint(&[0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]), [0, 0, 0, 8, 0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]);
        assert_eq!(mpint(&[0x80]), [0, 0, 0, 2, 0, 0x80]);
        assert_eq!(mpint(&[0, 0, 0x7f]), [0, 0, 0, 1, 0x7f]);
    }

    #[test]
    fn a_reader_refuses_by_the_fields_name() {
        let mut r = Reader::new(&[0, 0, 0, 9, b'a']);
        assert_eq!(r.string("user name", 8), Err(Refusal::TooLong { field: "user name", len: 9, cap: 8 }));
        let mut r = Reader::new(&[0, 0, 0, 2, b'a']);
        assert_eq!(r.string("user name", 8), Err(Refusal::Truncated("user name")));
        let mut r = Reader::new(&[0, 0, 0, 2, 0xff, 0xfe]);
        assert_eq!(r.text("user name", 8), Err(Refusal::NotUtf8("user name")));
        let mut r = Reader::new(&[0, 0, 0, 3, b'a', b',', b'b', 1]);
        assert_eq!(r.name_list("list").map(|l| l.names().collect::<Vec<_>>()), Ok(vec!["a", "b"]));
        assert_eq!(r.end("message"), Err(Refusal::Trailing("message")));
        for bad in [&b"a,,b"[..], b",a", b"a b", b"a\x01"] {
            let mut bytes = vec![0, 0, 0, bad.len() as u8];
            bytes.extend_from_slice(bad);
            assert!(matches!(Reader::new(&bytes).name_list("list"), Err(Refusal::Malformed(_))), "{bad:?}");
        }
        assert_eq!(Reader::new(&[]).fixed::<2>("key"), Err(Refusal::Truncated("key")));
        assert_eq!(Reader::new(&[0, 0, 0, 1, 7]).fixed::<2>("key"), Err(Refusal::Truncated("key")));
    }
}
