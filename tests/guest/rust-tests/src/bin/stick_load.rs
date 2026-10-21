//! Writes the boot stick through usbd for ever, so the boot deadline resets
//! the machine with a write in flight: `stick_survives_a_reset_under_load`'s
//! load. Lines to `/log` ([`stick_lines`]), which logkeeper writes to the
//! stick and syncs every round.

#[path = "../stick_lines.rs"]
mod stick_lines;

/// About 180 KB a second: a write and a sync on the stick every logkeeper
/// round, and a boot's log that stays inside logkeeper's retention (16 files
/// of a mebibyte) until the deadline ends it, so the file read back is
/// whole. At ten times the rate the boot rotated 55 of its own parts away.
const PER_SECOND: u64 = 200;

fn main() {
    stick_lines::say("stick_load", u64::MAX, PER_SECOND, || true);
}
