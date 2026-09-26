//! The wall clock at boot: on an ACPI Arm machine, the UEFI runtime's
//! `GetTime` or a PL031 the tables name, the port's stage 6.

use core::fmt;

use toyos_wallclock::Civil;

/// Why this machine did not say what time it is. None yet: the read is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcFault {}

impl fmt::Display for RtcFault {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

pub fn read(_century_reg: Option<u8>) -> Result<Civil, RtcFault> {
    owed!("the wall clock", "no stage yet")
}
