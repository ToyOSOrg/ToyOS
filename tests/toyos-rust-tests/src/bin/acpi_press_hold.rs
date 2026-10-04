//! Hold the boot open for the owner's press of the power button, which stops
//! the machine through `/system/bin/acpiserver`, and exit if none came: the
//! `acpi_power_button_pressed` metal row then finds no press and reds. It
//! asserts nothing.

use std::thread::sleep;
use std::time::Duration;

/// The span the owner is given to press, the stage's own ask: a boot held open
/// at least two minutes for one brief press.
const HOLD: Duration = Duration::from_secs(180);

fn main() {
    println!("acpi_press_hold: holding the boot open {} s for a press of the power button", HOLD.as_secs());
    sleep(HOLD);
    println!("acpi_press_hold: no press in {} s", HOLD.as_secs());
}
