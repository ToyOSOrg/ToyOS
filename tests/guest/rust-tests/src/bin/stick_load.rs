//! Writes the boot stick through usbd for ever, so the boot deadline resets
//! the machine with a write in flight: `stick_survives_a_reset_under_load`'s
//! load. Lines to `/log` ([`stick_lines`]), which logkeeper writes to the
//! stick and syncs every round.

#[path = "../stick_lines.rs"]
mod stick_lines;

fn main() {
    stick_lines::say("stick_load", u64::MAX, || true);
}
