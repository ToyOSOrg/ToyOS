//! The platform watchdog: on an ACPI Arm machine, an SBSA generic watchdog
//! the GTDT names, which no stage of the port has taken on yet. So one is
//! never armed, and a boot that asks for one is refused by name.

use crate::drivers::pci::PciDevice;

pub fn init(_devices: &[PciDevice]) {
    if crate::params::watchdog() {
        owed!("the platform watchdog (the SBSA generic watchdog the GTDT names)", "no stage yet");
    }
}

/// Nothing is armed to feed.
pub fn feed(_now: u64) {}

/// Nothing is armed to disarm.
pub fn disarm() {}
