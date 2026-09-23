//! Hold the boot open until the host hands the machine back. It asserts nothing.
//!
//! **The host ends this boot**, with `reboot` over ssh once its swap is judged
//! (`toyos-metal --swap … --hand-back`); the machine going back to its firmware
//! kills this process on the way. It never exits on its own: a host that never
//! says it is done meets the runner's bound, whose line in the log is the
//! finding.

use std::thread::sleep;
use std::time::Duration;

fn main() {
    loop {
        sleep(Duration::from_secs(3600));
    }
}
