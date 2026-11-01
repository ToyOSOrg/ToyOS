//! Lines written to `/log`, which a boot's logkeeper keeps on its log volume
//! and makes durable every round.
//!
//! Each line is one record of [`LINE_BYTES`], paced at the caller's rate,
//! under logkeeper's allowance for a program's children (4096 records a
//! second), so none is suppressed and every one reaches the file. What
//! reached the medium is counted by the judge in the file read back off the
//! log volume, never here.

use std::time::{Duration, Instant};

/// The bytes one line carries, its prefix and number included.
pub const LINE_BYTES: usize = 900;
/// Say lines under `name`, `per_second` of them, until `go_on` answers false or
/// `most` are said; answers how many were.
pub fn say(name: &str, most: u64, per_second: u64, go_on: impl Fn() -> bool) -> u64 {
    let began = Instant::now();
    let mut said = 0u64;
    while said < most && go_on() {
        let head = format!("{name}: line {said} ");
        println!("{head}{}", "x".repeat(LINE_BYTES.saturating_sub(head.len())));
        said += 1;
        // Paced to the rate, not slept by a fixed amount: a line late does
        // not make the next one later.
        let due = began + Duration::from_micros(said * 1_000_000 / per_second);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    said
}
