use std::thread::sleep;
use std::time::Duration;

const HOLD: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

fn main() {
    sleep(HOLD);
}
