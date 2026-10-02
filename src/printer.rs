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

/// Write `said` and a newline to stderr, stamped with now.
pub fn say(said: fmt::Arguments) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("the host's clock reads before 1970");
    std::eprintln!("{}", stamped(now.as_secs(), &said.to_string()));
}

/// `text` with `HH:MM:SS ` before its first line that is not blank.
fn stamped(unix_secs: u64, text: &str) -> String {
    let said = text.trim_start_matches('\n');
    if said.is_empty() {
        return text.to_string();
    }
    let at = Civil::from_unix_secs(unix_secs);
    let blank = &text[..text.len() - said.len()];
    format!("{blank}{:02}:{:02}:{:02} {said}", at.hour, at.min, at.sec)
}

/// `line` without the stamp [`say`] opened it with: what a reader of this
/// package's output judges.
pub fn unstamped(line: &str) -> &str {
    const SHAPE: &[u8] = b"dd:dd:dd ";
    let stamp = line.as_bytes().get(..SHAPE.len()).is_some_and(|head| {
        head.iter().zip(SHAPE).all(|(b, want)| match want {
            b'd' => b.is_ascii_digit(),
            c => b == c,
        })
    });
    if stamp {
        &line[SHAPE.len()..]
    } else {
        line
    }
}

/// One finished task's line, a test's and a build's alike: what became of it,
/// its name, and how long it took.
pub fn outcome(word: &str, name: &str, took: Duration) -> String {
    format!("  {word:<5} {name}  ({took:.0?})")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader strips exactly what the printer wrote: [`unstamped`] is
    /// `src/ci.rs`'s way to the suite's verdict lines.
    #[test]
    fn a_statement_is_stamped_once_and_read_back_whole() {
        // 2026-08-15 12:00:00 UTC, and 86_399 seconds into the same day.
        assert_eq!(stamped(1_786_795_200, "  PASS  a  (3s)"), "12:00:00   PASS  a  (3s)");
        assert_eq!(stamped(1_786_795_200 + 43_199, "x"), "23:59:59 x");
        assert_eq!(stamped(0, "\nrunning 2 tests\n"), "\n00:00:00 running 2 tests\n");
        assert_eq!(stamped(0, "FAIL a: short\n[kernel 0.1 cpu0] x"), "00:00:00 FAIL a: short\n[kernel 0.1 cpu0] x");
        assert_eq!(stamped(0, ""), "");
        assert_eq!(stamped(0, "\n"), "\n");

        for said in ["  PASS  a  (3s)", "FAIL a: short", "test result: ok. 1 passed, 1 total (9.0s)"] {
            assert_eq!(unstamped(&stamped(1_786_795_200, said)), said);
            assert_eq!(unstamped(said), said);
        }
        for no in ["12:00:00", "12:00:0x y", "1200:00:00 y", "[12:00:00] y"] {
            assert_eq!(unstamped(no), no);
        }
        assert_eq!(outcome("PASS", "a", Duration::from_millis(3_400)), "  PASS  a  (3s)");
        assert_eq!(outcome("STALL", "a", Duration::from_secs(90)), "  STALL a  (90s)");
    }
}
