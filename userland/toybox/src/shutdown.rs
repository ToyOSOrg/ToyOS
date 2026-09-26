//! Power the machine off, by asking `/system/bin/init`, which has the log made
//! whole first ([`toyos::power`]).
//!
//! **The `power` connector is the whole of the authority.** `/system/bin/shutdown`
//! is `/system/bin/toybox` under another name, so what this holds is what the
//! image's `[programs.toybox]` row declares — a config that does not have it
//! receive `power` builds an image whose shutdown applet says it cannot and
//! changes nothing else.

use toyos::power::{self, Stop};

pub fn main(_args: Vec<String>) {
    // Comes back only refused: on the other path the power is already off.
    let refused = power::stop(Stop::Shutdown);
    eprintln!("shutdown: refused ({refused:?})");
    std::process::exit(1);
}
