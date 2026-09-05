//! Return the machine to firmware.
//!
//! **The endowment is the whole of the authority**, on the shutdown applet's
//! terms: this is `/system/bin/toybox` under another name, so what it holds is
//! what the image's `[programs.toybox]` row declares.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

pub fn main(_args: Vec<String>) {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("reboot: this program was endowed no system capability");
        std::process::exit(1);
    };
    // Comes back only refused: on the other path the machine is already at its firmware.
    let refused = cap.reboot();
    eprintln!("reboot: refused ({refused:?}) — no POWER on this capability, or no reset register");
    std::process::exit(1);
}
