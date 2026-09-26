//! The platform watchdog: on an ACPI Arm machine, an SBSA generic watchdog
//! the GTDT names, the port's stage 6.

use crate::drivers::pci::PciDevice;

pub fn init(_devices: &[PciDevice]) {
    owed!("the platform watchdog", "no stage yet")
}

pub fn feed(_now: u64) {
    owed!("the platform watchdog", "no stage yet")
}

pub fn disarm() {
    owed!("the platform watchdog", "no stage yet")
}
