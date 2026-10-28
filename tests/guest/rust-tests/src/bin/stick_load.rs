//! Writes the boot's log disk for ever — QEMU's stick, a bench's NVMe — so the
//! boot deadline resets the machine with a write in flight:
//! `the_disk_survives_a_reset_under_load`'s load. Lines to `/log`
//! ([`log_lines`]), which logkeeper writes to that disk and syncs every
//! round.

#[path = "../log_lines.rs"]
mod log_lines;

/// About 180 KB a second: a write and a sync on the disk every logkeeper
/// round, and a boot's log that stays inside logkeeper's retention (16 files
/// of a mebibyte) until the deadline ends it, so the file read back is
/// whole. At ten times the rate the boot rotated 55 of its own parts away.
const PER_SECOND: u64 = 200;

fn main() {
    log_lines::say("stick_load", u64::MAX, PER_SECOND, || true);
}
