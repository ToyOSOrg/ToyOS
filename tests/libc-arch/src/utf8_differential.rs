//! libc's UTF-8 reader against `core::str::from_utf8`, over every sequence of
//! one to four bytes: each is fed a byte at a time, and where the reader ends a
//! character, refuses, or waits for more, `from_utf8` of the bytes so far must
//! say the same. A sequence's verdict is its first decided prefix's, so the
//! walk extends only the prefixes both call undecided, and every byte after
//! each of them.

use crate::utf8::{Byte, MbState};

#[derive(Debug, PartialEq)]
enum Verdict {
    /// A character ends at the last byte: its code point.
    Char(u32),
    /// No continuation makes the bytes a character.
    Refused,
    /// Some continuation does.
    Prefix,
}

/// What the reader says of the last of `bytes`, fed from the initial state;
/// every byte before it left a prefix.
fn reader(bytes: &[u8]) -> Verdict {
    let mut st = MbState::INITIAL;
    let (last, before) = bytes.split_last().expect("one byte at least");
    for &b in before {
        assert!(matches!(st.feed(b), Byte::Continues), "{bytes:02x?}: decided before its last byte");
    }
    match st.feed(*last) {
        Byte::Ends(cp) => Verdict::Char(cp),
        Byte::Refused => Verdict::Refused,
        Byte::Continues => Verdict::Prefix,
    }
}

/// What `from_utf8` says of `bytes`, whose every proper prefix it calls
/// incomplete: `error_len` is `None` exactly when the input ended inside a
/// sequence some continuation completes.
fn oracle(bytes: &[u8]) -> Verdict {
    match core::str::from_utf8(bytes) {
        Ok(s) => Verdict::Char(s.chars().next().expect("one byte at least") as u32),
        Err(e) if e.error_len().is_some() => Verdict::Refused,
        Err(_) => Verdict::Prefix,
    }
}

#[test]
fn every_sequence_to_four_bytes_agrees_with_from_utf8() {
    let (mut prefixes, mut judged, mut chars) = (vec![Vec::new()], 0u64, 0u32);
    while let Some(prefix) = prefixes.pop() {
        for b in 0..=u8::MAX {
            let mut bytes = prefix.clone();
            bytes.push(b);
            let verdict = oracle(&bytes);
            assert_eq!(reader(&bytes), verdict, "{bytes:02x?}");
            judged += 1;
            match verdict {
                Verdict::Prefix => {
                    assert!(bytes.len() < 4, "{bytes:02x?}: four bytes and still a prefix");
                    prefixes.push(bytes);
                }
                Verdict::Char(_) => chars += 1,
                Verdict::Refused => {}
            }
        }
    }
    // Every scalar value but none of the surrogates, each in one form.
    assert_eq!(chars, 0x11_0000 - 0x800);
    assert!(judged > 0x11_0000, "{judged}");
}

/// A refusal leaves the initial state: the next byte starts afresh.
#[test]
fn a_refusal_starts_the_next_character_afresh() {
    let mut st = MbState::INITIAL;
    for b in [0xe0, 0x80] {
        st.feed(b);
    }
    assert!(!st.is_partial());
    assert!(matches!(st.feed(b'a'), Byte::Ends(0x61)));
}
