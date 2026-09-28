//! A thread the kernel's `quiesce-last-park` actuator can hold, and a reboot.
//!
//! It carries [`toyos_quiesce::LAST_THREAD`]'s name and parks for longer than
//! any stop's budget; `quiesce-last-park` holds it inside its `SYS_NANOSLEEP`.
//! The kernel holds the reset itself until it is held, so this program orders
//! nothing.
//!
//! Nothing here asserts: `common::power::quiesce_wakes_on_the_last_park` reads
//! the kernel's hold line and its `stop:` record.

use std::time::Duration;

use toyos::power::Stop;
use toyos_quiesce::LAST_THREAD;

/// Longer than any stop's budget: a park the timer ends inside the stop would
/// come back to Ring 3 and be banded there, and that band's post would wake the
/// stop in place of the park's.
const PARKED_FOR: Duration = Duration::from_secs(3_600);

fn main() {
    std::thread::Builder::new()
        .name(LAST_THREAD.into())
        .spawn(park)
        .expect("spawn a thread the kernel may hold");

    // Comes back only refused.
    let refused = toyos::power::stop(Stop::Reboot);
    eprintln!("quiesce_last: the reboot was refused ({refused:?})");
    std::process::exit(1);
}

fn park() {
    std::thread::sleep(PARKED_FOR);
}
