//! The one printer: `eprintln!` in every target of this package is the macro
//! below, so each statement the build system or the harness makes on stderr
//! opens with the UTC time of day it was made at — the clock `logd` stamps
//! `/log` with. A statement of several lines is stamped once, on its first.
//! What a child process writes to the terminal, and what is written to
//! stdout, pass through no printer and carry no stamp.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toyos_wallclock::Civil;

#[macro_export]
macro_rules! eprintln {
    () => { $crate::printer::say(::std::format_args!("")) };
    ($($arg:tt)*) => { $crate::printer::say(::std::format_args!($($arg)*)) };
}

/// Write `said` and a newline to stderr, with now's time of day and a space
/// before its first line that is not blank. One string, because stderr is
/// unbuffered and each piece of a format is a write of its own.
pub fn say(said: fmt::Arguments) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("the host's clock reads before 1970");
    let at = Civil::from_unix_secs(now.as_secs()).time_of_day();
    let text = said.to_string();
    let said = text.trim_start_matches('\n');
    let statement = if said.is_empty() {
        format!("{text}\n")
    } else {
        format!("{}{at} {said}\n", &text[..text.len() - said.len()])
    };
    std::eprint!("{statement}");
}

/// `line` without the stamp [`say`] opened it with: what a reader of this
/// package's output judges.
pub fn unstamped(line: &str) -> &str {
    toyos_wallclock::after_time_of_day(line).and_then(|said| said.strip_prefix(' ')).unwrap_or(line)
}

/// One task's line when it starts, a test's and a build's alike.
pub fn started(word: &str, name: &str) -> String {
    format!("  {word:<5} {name}")
}

/// One finished task's line, a test's and a build's alike: what became of it,
/// its name, and how long it took.
pub fn outcome(word: &str, name: &str, took: Duration) -> String {
    format!("{}  ({took:.0?})", started(word, name))
}
