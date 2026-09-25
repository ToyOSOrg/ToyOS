//! A program that writes its output far faster than the log can take it, for
//! the pipe's backpressure: every line must reach `/log`, in order, once.
//!
//! One `write` per whole line, numbered, so the host can count them in the
//! file. The total is several times what one pipe holds, so a `logd` that fell
//! behind makes this program wait rather than lose a line. The verdict is the
//! host's; `log_program_flood` runs it.

use std::io::Write;
use std::time::Instant;

/// Lines written. Each is [`WIDTH`] bytes, so the whole is 5 MiB — two and a
/// half times the 2 MiB a pipe's ring holds (`kernel/src/pipe.rs`'s
/// `PIPE_SIZE`).
const LINES: usize = 81_920;
const WIDTH: usize = 64;

fn main() {
    let mut out = std::io::stdout().lock();
    let began = Instant::now();
    let mut slowest = 0u128;
    for i in 0..LINES {
        let head = format!("flood {i:06} ");
        let line = format!("{head}{}\n", "x".repeat(WIDTH - head.len() - 1));
        let at = Instant::now();
        out.write_all(line.as_bytes()).expect("the log takes every line");
        out.flush().expect("the log takes every line");
        slowest = slowest.max(at.elapsed().as_micros());
    }
    let _ = writeln!(
        out,
        "flood done lines={LINES} bytes={} ms={} slowest_write_us={slowest}",
        LINES * WIDTH,
        began.elapsed().as_millis()
    );
}
