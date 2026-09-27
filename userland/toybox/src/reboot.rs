//! Return the machine to firmware, by asking `/system/bin/init`, which has the
//! log made whole first ([`toyos::power`]). **The `power` connector is the
//! whole of the authority**: this is `/system/bin/toybox` under another name,
//! holding what `[programs.toybox]` declares.

use toyos::power::{self, Stop};

pub fn main(_args: Vec<String>) {
    // Comes back only refused: on the other path the machine is already at its firmware.
    let refused = power::stop(Stop::Reboot);
    eprintln!("reboot: refused ({refused:?})");
    std::process::exit(1);
}
