//! Two threads the kernel's `quiesce-last-*` actuators can hold, and a reboot.
//!
//! Both carry [`toyos_quiesce::LAST_THREAD`]'s name. One parks for longer than
//! any stop's budget and the other exits at once. `quiesce-last-park` holds
//! the first inside its `SYS_NANOSLEEP`, and `quiesce-last-exit` holds the
//! second inside its `SYS_THREAD_EXIT`. The kernel holds the reset itself until
//! one of them is held, so this program orders nothing.
//!
//! Nothing here asserts: `common::power::quiesce_wakes_on_the_last_park` and
//! `quiesce_wakes_on_the_last_exit` read the kernel's hold line and its `stop:`
//! record.

use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_quiesce::LAST_THREAD;

/// Longer than any stop's budget: a park the timer ends inside the stop would
/// come back to Ring 3 and be banded there, and that band's post would wake the
/// stop in place of the park's.
const PARKED_FOR: Duration = Duration::from_secs(3_600);

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_last: this program was endowed no system capability");
        std::process::exit(1);
    };
    for body in [park as fn(), exit] {
        std::thread::Builder::new()
            .name(LAST_THREAD.into())
            .spawn(body)
            .expect("spawn a thread the kernel may hold");
    }

    // Comes back only refused.
    let refused = cap.reboot();
    eprintln!("quiesce_last: the reboot was refused ({refused:?})");
    std::process::exit(1);
}

fn park() {
    std::thread::sleep(PARKED_FOR);
}

fn exit() {}
