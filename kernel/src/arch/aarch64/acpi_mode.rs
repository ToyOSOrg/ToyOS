//! The machine's ACPI mode: an AArch64 machine is hardware-reduced, with no
//! fixed hardware and no legacy mode to leave, so there is no row to claim.

use toyos_abi::acpi::AcpiInfo;

use crate::device::ClaimError;

pub fn claim() -> Result<(crate::isa::Row, AcpiInfo), ClaimError> {
    Err(ClaimError::Absent)
}

/// Never called: nothing is released where nothing can be claimed.
pub fn release() {
    unreachable!("AArch64 has no ACPI row")
}
