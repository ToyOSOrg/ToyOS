//! Hold the boot open for as long as the host needs to reach this machine over
//! the cable, and exit. It asserts nothing; the host judges the records netd
//! wrote inside this window.

use std::thread::sleep;
use std::time::Duration;

/// **netd's own lease bound**, so this boot cannot hand the machine back before
/// netd has settled whether it has an address.
const HOLD: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

fn main() {
    sleep(HOLD);
}
