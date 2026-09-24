//! A program that keeps talking: one line every [`PACE`] for [`LINES`] lines,
//! so the panel is owed a program's line for the whole of a Ctrl+Alt+D hold.
//! `screen_held_dump_while_talking` runs it.

use std::io::Write;
use std::time::Duration;

const LINES: usize = 1000;
const PACE: Duration = Duration::from_millis(25);

fn main() {
    let mut out = std::io::stdout().lock();
    for i in 0..LINES {
        let _ = writeln!(out, "talk {i:05}");
        let _ = out.flush();
        std::thread::sleep(PACE);
    }
}
