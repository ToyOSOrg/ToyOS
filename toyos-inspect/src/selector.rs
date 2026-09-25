//! The reader's selector grammar: a path whose segments may be `*`, and a `*`
//! matches one or more whole segments.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::path::{segment_char, MAX_PATH};

/// A parsed selector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Selector {
    segments: Vec<Segment>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Segment {
    Literal(String),
    /// One or more whole segments.
    Any,
}

/// Why a string is not a selector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SelectorError {
    Empty,
    TooLong(usize),
    /// Two dots in a row, or a dot at either end.
    EmptySegment,
    /// A `*` sharing a segment with anything else, and that segment.
    PartialWildcard(String),
    /// A character no segment may hold, and its byte offset.
    Char { ch: char, at: usize },
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a selector has at least one segment"),
            Self::TooLong(len) => write!(f, "{len} bytes is past the {MAX_PATH}-byte bound"),
            Self::EmptySegment => f.write_str("a segment is empty (two dots, or a dot at an end)"),
            Self::PartialWildcard(segment) => write!(
                f,
                "{segment:?}: a `*` is a whole segment and matches one or more of them; it is \
                 not a glob inside one"
            ),
            Self::Char { ch, at } => {
                write!(f, "{ch:?} at byte {at} is not one of a-z 0-9 _ - : or a whole-segment *")
            }
        }
    }
}

impl Selector {
    /// Everything: one `*`.
    pub fn all() -> Self {
        Self { segments: alloc::vec![Segment::Any] }
    }

    pub fn parse(text: &str) -> Result<Self, SelectorError> {
        if text.is_empty() {
            return Err(SelectorError::Empty);
        }
        if text.len() > MAX_PATH {
            return Err(SelectorError::TooLong(text.len()));
        }
        let mut segments = Vec::new();
        let mut offset = 0;
        for segment in text.split('.') {
            if segment.is_empty() {
                return Err(SelectorError::EmptySegment);
            }
            if segment == "*" {
                segments.push(Segment::Any);
            } else if segment.contains('*') {
                return Err(SelectorError::PartialWildcard(segment.to_string()));
            } else {
                if let Some((at, ch)) = segment.char_indices().find(|&(_, c)| !segment_char(c)) {
                    return Err(SelectorError::Char { ch, at: offset + at });
                }
                segments.push(Segment::Literal(segment.to_string()));
            }
            offset += segment.len() + 1;
        }
        Ok(Self { segments })
    }

    /// Whether `path` is one this selector names. `path` is taken as already
    /// checked: the reader matches only what [`crate::decode`] accepted.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('.').collect();
        matches_from(&self.segments, &parts)
    }

    /// Whether any path under `root` could match, which is whether the reader
    /// has to ask that owner at all.
    pub fn reaches(&self, root: &str) -> bool {
        match &self.segments[0] {
            Segment::Literal(first) => first == root,
            Segment::Any => true,
        }
    }
}

/// `segments` against `parts`, a `*` taking one part and then either stopping
/// or taking another. Backtracking, and bounded: a path has at most
/// [`MAX_PATH`] / 2 segments.
fn matches_from(segments: &[Segment], parts: &[&str]) -> bool {
    match segments.split_first() {
        None => parts.is_empty(),
        Some((Segment::Literal(lit), rest)) => match parts.split_first() {
            Some((part, more)) => part == lit && matches_from(rest, more),
            None => false,
        },
        Some((Segment::Any, rest)) => {
            (1..=parts.len()).any(|taken| matches_from(rest, &parts[taken..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATHS: &[&str] = &[
        "net.link.state",
        "net.link.speed_mbps",
        "net.errors.crc",
        "net.errors",
        "sound.device",
        "sound.hda.codec",
        "sound.hda.errors",
        "log.volume.bytes",
        "display.windows",
    ];

    fn hits(selector: &str) -> Vec<&'static str> {
        let s = Selector::parse(selector).unwrap();
        PATHS.iter().copied().filter(|p| s.matches(p)).collect()
    }

    /// The whole of what `*` means, as sets: every selector here is compared
    /// against the full list, so a `*` that matches too much or too little is a
    /// set that differs, whichever way it differs.
    #[test]
    fn a_star_is_one_or_more_whole_segments() {
        assert_eq!(
            hits("net.*"),
            ["net.link.state", "net.link.speed_mbps", "net.errors.crc", "net.errors"]
        );
        assert_eq!(hits("sound.hda.*"), ["sound.hda.codec", "sound.hda.errors"]);
        assert_eq!(hits("*.errors"), ["net.errors", "sound.hda.errors"]);
        assert_eq!(hits("net.*.state"), ["net.link.state"]);
        assert_eq!(hits("*.link.*"), ["net.link.state", "net.link.speed_mbps"]);
        assert_eq!(hits("*"), PATHS);
        // Exact: no `*` is no prefix match.
        assert_eq!(hits("net.link.state"), ["net.link.state"]);
        assert!(hits("net").is_empty());
        assert!(hits("net.link").is_empty());
        // A `*` takes at least one segment: nothing is exactly `net`.
        assert!(hits("net.*.*.*").is_empty());
        assert_eq!(hits("*.*.*"), [
            "net.link.state",
            "net.link.speed_mbps",
            "net.errors.crc",
            "sound.hda.codec",
            "sound.hda.errors",
            "log.volume.bytes",
        ]);
        // A literal is the whole segment and never a prefix of one.
        assert!(hits("ne.*").is_empty());
        assert!(hits("net.link.stat").is_empty());
    }

    #[test]
    fn a_selector_reaches_only_the_owners_it_can_name() {
        assert!(Selector::parse("net.*").unwrap().reaches("net"));
        assert!(!Selector::parse("net.*").unwrap().reaches("sound"));
        assert!(Selector::parse("*.errors").unwrap().reaches("sound"));
        assert!(Selector::all().reaches("log"));
        assert_eq!(Selector::all(), Selector::parse("*").unwrap());
    }

    #[test]
    fn a_malformed_selector_is_refused_by_name() {
        assert_eq!(Selector::parse(""), Err(SelectorError::Empty));
        assert_eq!(Selector::parse("net..x"), Err(SelectorError::EmptySegment));
        assert_eq!(Selector::parse(".net"), Err(SelectorError::EmptySegment));
        assert_eq!(Selector::parse("net."), Err(SelectorError::EmptySegment));
        assert_eq!(Selector::parse("ne*"), Err(SelectorError::PartialWildcard("ne*".into())));
        assert_eq!(Selector::parse("net.**"), Err(SelectorError::PartialWildcard("**".into())));
        assert_eq!(Selector::parse("net.Link"), Err(SelectorError::Char { ch: 'L', at: 4 }));
        assert_eq!(Selector::parse("net.li?k"), Err(SelectorError::Char { ch: '?', at: 6 }));
        assert_eq!(Selector::parse("net link"), Err(SelectorError::Char { ch: ' ', at: 3 }));
        let long = "a".repeat(MAX_PATH + 1);
        assert_eq!(Selector::parse(&long), Err(SelectorError::TooLong(MAX_PATH + 1)));
        // The refusal names the problem in words, not only as a variant.
        let said = alloc::format!("{}", Selector::parse("ne*").unwrap_err());
        assert!(said.contains("whole segment"), "{said}");
    }
}
