use std::thread::sleep;
use std::time::Duration;

/// Past the window `dump-deaf-cpu` opens a fixed time into the boot and the
/// dump it kicks and probes inside it: an empty job list ends the boot before
/// the actuator arms.
const PAST_THE_DEAF_WINDOW: Duration = Duration::from_secs(20);

fn main() {
    sleep(PAST_THE_DEAF_WINDOW);
}
