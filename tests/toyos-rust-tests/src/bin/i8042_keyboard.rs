//! Claims the keyboard and prints what arrives, one line per event.
//!
//! Driven by host tests that boot a guest with no USB HID at all and
//! inject through QMP once the ready line appears. Not a standalone test: on
//! its own it would time out with nothing to report, which is why it is in
//! RUST_SKIP.
//!
//! It holds a [`Translator`] because the kernel no longer does: the claim carries
//! a HID usage and a modifier mask, and what those type is a layout, which is
//! userland's. This is the same type and the same call `/system/bin/console` and
//! every window client make, so `tr=` below is what a real surface would put
//! on a real shell's stdin.

use std::time::Duration;
use toyos::device::Keyboard;
use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{DeviceType, SYSCAP_LABEL};
use toyos_abi::input::RawKeyEvent;

const EVENT_SIZE: usize = std::mem::size_of::<RawKeyEvent>();

/// The host's end-of-run marker: the HID usage for the End key. None of this
/// binary's callers' own injections presses it, so its release is
/// unambiguous — the same shape as `input_events.rs`'s right-button release.
/// No deadline: a lost sentinel is a hang the host's ceiling reds.
const SENTINEL: u8 = 0x4D;

fn main() {
    let keyboard: Keyboard =
        capability().claim(DeviceType::Keyboard).expect("i8042_keyboard: no keyboard device");
    let mut translator = window::configured_translator();
    println!("===I8042_READY===");

    let mut buf = [0u8; 512];
    let mut seen = 0;
    let mut ended = false;
    while !ended {
        let n = keyboard.read_nonblock(&mut buf).unwrap_or(0);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        for chunk in buf[..n].chunks_exact(EVENT_SIZE) {
            let key = window::KeyEvent { keycode: chunk[0], modifiers: chunk[1] };
            let translated = if key.pressed() {
                translator.press(key.keycode, key.mods())
            } else {
                window::Emit::EMPTY
            };
            println!(
                "kev usage=0x{:02x} mods=0x{:02x} tr={:?}",
                key.keycode,
                key.modifiers,
                translated.as_str()
            );
            seen += 1;
            if key.keycode == SENTINEL && !key.pressed() {
                ended = true;
            }
        }
    }
    println!("kev done seen={seen}");
}

/// The device-minting capability the test estate is endowed. A claim is
/// `/system/bin/init`'s to mint everywhere else; here test-runner passes a `DEVICE`
/// duplicate down, so a boot can run several binaries that each need an input
/// device.
fn capability() -> SysCap {
    Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability")
}
