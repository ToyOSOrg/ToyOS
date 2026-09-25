//! The path grammar: dotted segments of `a-z`, `0-9`, `_`, `-` and `:`.

use core::fmt;

/// The longest path, in bytes. It travels behind a one-byte length.
pub const MAX_PATH: usize = 128;

/// Why a string is not a path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PathError {
    Empty,
    TooLong(usize),
    /// Two dots in a row, or a dot at either end.
    EmptySegment,
    /// A character no segment may hold, and its byte offset.
    Char { ch: char, at: usize },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a path has at least one segment"),
            Self::TooLong(len) => write!(f, "{len} bytes is past the {MAX_PATH}-byte bound"),
            Self::EmptySegment => f.write_str("a segment is empty (two dots, or a dot at an end)"),
            Self::Char { ch, at } => {
                write!(f, "{ch:?} at byte {at} is not one of a-z 0-9 _ - :")
            }
        }
    }
}

/// Whether `c` may appear in a segment.
pub(crate) fn segment_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | ':')
}

/// Check `path` against the grammar. `*` is not a path character; a selector
/// is checked by [`crate::Selector::parse`].
pub fn check_path(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH {
        return Err(PathError::TooLong(path.len()));
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(PathError::EmptySegment);
        }
    }
    if let Some((at, ch)) = path.char_indices().find(|&(_, c)| c != '.' && !segment_char(c)) {
        return Err(PathError::Char { ch, at });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_refused_by_name() {
        assert_eq!(check_path("net.link.speed_mbps"), Ok(()));
        assert_eq!(check_path("dev.pci.00:1f.6.driver"), Ok(()));
        assert_eq!(check_path("net"), Ok(()));
        assert_eq!(check_path(""), Err(PathError::Empty));
        assert_eq!(check_path("net..x"), Err(PathError::EmptySegment));
        assert_eq!(check_path(".net"), Err(PathError::EmptySegment));
        assert_eq!(check_path("net."), Err(PathError::EmptySegment));
        assert_eq!(check_path("net.Link"), Err(PathError::Char { ch: 'L', at: 4 }));
        assert_eq!(check_path("net.*"), Err(PathError::Char { ch: '*', at: 4 }));
        assert_eq!(check_path("net link"), Err(PathError::Char { ch: ' ', at: 3 }));
        assert_eq!(check_path("net=x"), Err(PathError::Char { ch: '=', at: 3 }));
        let long = "a".repeat(MAX_PATH + 1);
        assert_eq!(check_path(&long), Err(PathError::TooLong(MAX_PATH + 1)));
        assert_eq!(check_path(&long[1..]), Ok(()));
    }
}
