//! A program that writes its output far faster than the log can take it: no
//! write waits, and every line reaches `/log` in order or is counted by `logd`.
//!
//! One `write` per whole line, numbered, so the host can count them in the
//! file. The total is many times what one ring holds, so a `logd` that fell
//! behind has lines refused rather than this program waiting. The verdict is
//! the host's; `log_program_flood` runs it.

use std::io::Write;

/// Lines written, many times the records one log ring holds, each [`WIDTH`]
/// bytes: what fills a ring is its slots, and a narrow line keeps the stop's
/// flush of a full one inside init's bound.
const LINES: usize = 16_384;
const WIDTH: usize = 64;

fn main() {
    let mut out = std::io::stdout().lock();
    for i in 0..LINES {
        let head = format!("flood {i:06} ");
        let line = format!("{head}{}\n", "x".repeat(WIDTH - head.len() - 1));
        out.write_all(line.as_bytes()).expect("a write to the log never fails");
        out.flush().expect("a write to the log never fails");
    }
    let _ = writeln!(
        out,
        "flood done lines={LINES} bytes={}",
        LINES * WIDTH,
    );
}
