//! Eight mebibytes of lines to `/log` on the boot's log disk
//! ([`log_lines`]), then an end: the log-write load `mask_windows` reads the
//! windows under.

#[path = "../log_lines.rs"]
mod log_lines;

/// About eight mebibytes.
const LINES: u64 = (8 << 20) / log_lines::LINE_BYTES as u64;

fn main() {
    let said = log_lines::say("log_write", LINES, 2000, || true);
    println!("log_write: {said} lines of {} bytes said", log_lines::LINE_BYTES);
}
