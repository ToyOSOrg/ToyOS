//! A program that writes its console far past its share of the record ring,
//! for the ring's per-program bound.
//!
//! Every line is one `write` of a whole line, numbered, so the host can tell
//! which of them became records and count the rest from the kernel's own
//! notes. The verdict is the host's: this binary owes it the lines, and a last
//! line that says how many it wrote.

use std::io::Write;
use std::time::Duration;

/// Written as fast as the console takes them: far past any burst, and past
/// one CPU's shard, so a kernel that recorded every line would lap its own.
const BURST_LINES: usize = 3000;

/// Written one per [`PACE`] after that, so the steady rate is what admits
/// them and each admitted line carries a count of the ones before it.
const PACED_LINES: usize = 1000;
const PACE: Duration = Duration::from_millis(2);

fn main() {
    let mut out = std::io::stdout().lock();
    for i in 0..BURST_LINES + PACED_LINES {
        if i >= BURST_LINES {
            std::thread::sleep(PACE);
        }
        // One `write` per line, so the rate is the syscall's and not a buffer's.
        let line = format!("flood {i:05}\n");
        out.write_all(line.as_bytes()).expect("the console takes every write");
        out.flush().expect("the console takes every write");
    }
    let _ = writeln!(out, "flood done lines={}", BURST_LINES + PACED_LINES);
}
