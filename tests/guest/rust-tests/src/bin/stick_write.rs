//! Eight mebibytes of lines to `/log` on the boot stick through usbd
//! ([`stick_lines`]), then an end: the stick-write load `mask_windows` reads
//! the windows under.

#[path = "../stick_lines.rs"]
mod stick_lines;

/// About eight mebibytes.
const LINES: u64 = (8 << 20) / stick_lines::LINE_BYTES as u64;

fn main() {
    let said = stick_lines::say("stick_write", LINES, || true);
    println!("stick_write: {said} lines of {} bytes said", stick_lines::LINE_BYTES);
}
