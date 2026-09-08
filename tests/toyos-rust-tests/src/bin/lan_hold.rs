//! Hold the boot open for as long as the host needs to reach this machine over
//! the cable, and exit.
//!
//! **A metal boot ends itself**, and the whole of a `tests/lancase` boot is
//! about a second: the job list runs and the last job hands the machine back to
//! firmware. Nothing on the network could be asked of a machine that is up for
//! that long — the I219's link takes seconds to negotiate before a DHCP
//! discover can even go out — so this job is the window, and its exit record is
//! what says the machine stayed up for the whole of it.
//!
//! It asserts nothing. What it is evidence *for* is judged on the host, out of
//! the records netd wrote inside this window and out of whether the host's own
//! `ping` was answered while it was open; a bare `sleep` cannot be wrong about
//! either. It is on `RUST_SKIP` for the same reason: on any boot but that one
//! it is twenty seconds of nothing.

use std::thread::sleep;
use std::time::Duration;

/// How long this machine stays up for the host.
///
/// The host polls once a second and the link and the lease come first, so what
/// this has to cover is a gigabit auto-negotiation, a DHCP exchange with the
/// router and several polls after both. `tests/metal-profile.toml`'s
/// `list.lancase.job_ms` is this number plus what a spawn costs, and moving one
/// without the other is a job list the runner's own deadline cuts short.
const HOLD: Duration = Duration::from_secs(20);

fn main() {
    sleep(HOLD);
}
