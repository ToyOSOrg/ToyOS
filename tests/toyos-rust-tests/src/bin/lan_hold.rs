//! Hold the boot open for as long as the host needs to reach this machine over
//! the cable, and exit.
//!
//! It asserts nothing: what it is evidence *for* is judged on the host, out of
//! the records netd wrote inside this window and out of whether the host's own
//! `ping` was answered while it was open.

use std::thread::sleep;
use std::time::Duration;

/// How long this machine stays up for the host. `list.lancase.job_ms` in
/// `tests/metal-profile.toml` is this number plus what a spawn costs, and
/// moving one without the other is a job list the runner's own deadline cuts
/// short.
const HOLD: Duration = Duration::from_secs(20);

fn main() {
    sleep(HOLD);
}
