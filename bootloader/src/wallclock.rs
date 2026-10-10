//! The wall clock as firmware keeps it: `GetTime` (UEFI 2.11 §8.3.1), a
//! runtime service the kernel never calls, asked here before
//! `ExitBootServices`. The hardware clock keeps UTC, so `EFI_TIME::TimeZone`
//! is not read; a date that does not exist is refused here, at the firmware's
//! boundary, and never reaches the kernel.

use core::fmt;

use toyos_wallclock::Civil;
use uefi::prelude::*;

/// Why firmware gave no time.
pub enum Refusal {
    Failed(uefi::Status),
    NotADate(Civil),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(status) => write!(f, "firmware's GetTime failed ({status:?})"),
            Self::NotADate(civil) => write!(f, "firmware's GetTime answered {civil:?}, which is not a date"),
        }
    }
}

/// Firmware's time, and [`crate::arch::counter`] as it answered.
pub fn now(system_table: &SystemTable<Boot>) -> Result<(Civil, u64), Refusal> {
    let time = system_table.runtime_services().get_time().map_err(|e| Refusal::Failed(e.status()))?;
    let counter = crate::arch::counter();
    let civil = Civil {
        year: u64::from(time.year()),
        month: u64::from(time.month()),
        day: u64::from(time.day()),
        hour: u64::from(time.hour()),
        min: u64::from(time.minute()),
        sec: u64::from(time.second()),
    };
    if !civil.is_valid() {
        return Err(Refusal::NotADate(civil));
    }
    Ok((civil, counter))
}
