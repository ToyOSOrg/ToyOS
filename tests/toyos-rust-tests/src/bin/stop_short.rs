//! Ask for the power-off with a thread of this process still spinning: on a
//! one-CPU guest whose stop sweeps once (`stop-budget-spent`), that thread is
//! queued behind the stop's caller, and the stop ends with it running.
//! `machine_shutdown_short_stop` runs it.

use toyos::power::{stop, Stop};

fn main() {
    std::thread::spawn(|| loop {
        std::hint::spin_loop();
    });
    let refused = stop(Stop::Shutdown);
    panic!("stop_short: the power-off was refused: {refused:?}");
}
