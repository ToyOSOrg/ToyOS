//! The reader's command line: `inspect [--json] [SELECTOR]`.
//!
//! **There are no filter flags and there is no `--watch`.** Filtering is the
//! selector's and then `grep`'s, and a reading over time is the log's.

use alloc::string::{String, ToString};
use core::fmt;

use crate::selector::{Selector, SelectorError};

/// What one run of the reader was asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    pub selector: Selector,
    /// One JSON object instead of `path = value` lines.
    pub json: bool,
}

/// Why a command line is not one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UsageError {
    /// A word starting with `-` that is not `--json`.
    Flag(String),
    /// A second selector: a run reads one.
    Second(String),
    Selector(SelectorError),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag(flag) => write!(f, "{flag} is not a flag; the only one is --json"),
            Self::Second(extra) => write!(f, "{extra}: one selector per run"),
            Self::Selector(why) => write!(f, "{why}"),
        }
    }
}

impl Invocation {
    /// The words after the program's name. No selector is `*`.
    pub fn parse<'a>(words: impl IntoIterator<Item = &'a str>) -> Result<Self, UsageError> {
        let mut json = false;
        let mut selector: Option<Selector> = None;
        for word in words {
            if word == "--json" {
                json = true;
            } else if word.starts_with('-') {
                return Err(UsageError::Flag(word.to_string()));
            } else if selector.is_some() {
                return Err(UsageError::Second(word.to_string()));
            } else {
                selector = Some(Selector::parse(word).map_err(UsageError::Selector)?);
            }
        }
        Ok(Self { selector: selector.unwrap_or_else(Selector::all), json })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_is_one_optional_selector_and_one_optional_flag() {
        let run = Invocation::parse([]).unwrap();
        assert_eq!(run, Invocation { selector: Selector::all(), json: false });
        let run = Invocation::parse(["net.*", "--json"]).unwrap();
        assert_eq!(run, Invocation { selector: Selector::parse("net.*").unwrap(), json: true });
        assert_eq!(Invocation::parse(["--watch"]), Err(UsageError::Flag("--watch".into())));
        assert_eq!(Invocation::parse(["-v", "net.*"]), Err(UsageError::Flag("-v".into())));
        assert_eq!(Invocation::parse(["net.*", "sound.*"]), Err(UsageError::Second("sound.*".into())));
        assert_eq!(
            Invocation::parse(["net*"]),
            Err(UsageError::Selector(SelectorError::PartialWildcard("net*".into())))
        );
    }
}
