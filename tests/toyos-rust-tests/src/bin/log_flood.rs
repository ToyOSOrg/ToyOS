//! A program that writes its output far faster than the log can take it: no
//! write waits, and every line reaches `/log` in order or is counted by `logd`.
//!
//! One `write` per whole line, numbered, so the host can count them in the
//! file. The total is many times what one ring holds, so a `logd` that fell
//! behind has lines refused rather than this program waiting. The verdict is
//! the host's; `log_program_flood` runs it.

use std::io::Write;
use std::time::Instant;

/// Lines written, many times the records one log ring holds, unless the first
/// argument names another count. Each is [`WIDTH`] bytes — one record's text,
/// nearly — so the lines `logd` does take are megabytes a second.
const LINES: usize = 16_384;
const WIDTH: usize = 960;

fn main() {
    let lines = std::env::args().nth(1).map_or(LINES, |n| n.parse().expect("log_flood: a line count"));
    let mut out = std::io::stdout().lock();
    let began = Instant::now();
    let mut slowest = 0u128;
    for i in 0..lines {
        let head = format!("flood {i:06} ");
        let line = format!("{head}{}\n", "x".repeat(WIDTH - head.len() - 1));
        let at = Instant::now();
        out.write_all(line.as_bytes()).expect("a write to the log never fails");
        out.flush().expect("a write to the log never fails");
        slowest = slowest.max(at.elapsed().as_micros());
    }
    let _ = writeln!(
        out,
        "flood done lines={lines} bytes={} ms={} slowest_write_us={slowest}",
        lines * WIDTH,
        began.elapsed().as_millis()
    );
}
