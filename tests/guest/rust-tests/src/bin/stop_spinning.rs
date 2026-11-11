//! Ask for the power-off with a thread of this process spinning in userland
//! on another CPU, which enters the kernel only when the stop kicks it.
//! `machine_shutdown_with_a_thread_spinning` runs it.

use toyos::power::{stop, Mode, Pending, Stop};

fn main() {
    std::thread::spawn(|| loop {
        std::hint::spin_loop();
    });
    let refused = stop(Stop::Shutdown, Mode::Terminate).map(Pending::outcome);
    panic!("stop_spinning: the power-off was refused: {refused:?}");
}
