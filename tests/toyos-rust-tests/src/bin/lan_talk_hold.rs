//! Hold the boot open while the host talks to this machine over the cable, and
//! exit when the runner's own bound is near. It asserts nothing.
//!
//! **The host is meant to end this boot first**, with `reboot` over ssh; the
//! machine going back to its firmware kills this process on the way. Exiting
//! here is the fallback that hands the machine back when the host never asks —
//! through the runner's `reboot` job, which is the boot's ordinary end — and
//! its exit record is how a judge tells the two apart.

use std::thread::sleep;
use std::time::Duration;

/// Milliseconds of boot time this hold ends at: the runner's bound less the
/// tenth `toyos_build::metalprofile::AROUND_THE_LIST_MS` keeps for the boot
/// around the list, so the runner's own `reboot` still runs inside its bound.
const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    sleep(Duration::from_millis(UNTIL_MS.saturating_sub(since_boot)));
}
