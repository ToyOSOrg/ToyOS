//! Hold the boot open, and exit. It asserts nothing; `dump_nmi_probe`'s metal
//! row judges the dump its actuator stages inside this window.

use std::thread::sleep;
use std::time::Duration;

const HOLD: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

fn main() {
    sleep(HOLD);
}
