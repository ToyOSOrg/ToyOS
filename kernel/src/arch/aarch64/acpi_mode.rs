//! The machine's ACPI mode: an AArch64 machine is hardware-reduced, with no
//! fixed hardware and no legacy mode to leave, so there is no row to claim.

use toyos_abi::acpi::AcpiInfo;

use crate::device::ClaimError;

pub fn claim() -> Result<(usize, AcpiInfo), ClaimError> {
    Err(ClaimError::Absent)
}

/// Never called: nothing is released where nothing can be claimed.
pub fn release(_row: usize) {
    unreachable!("AArch64 has no ACPI row")
}

/// Never called, as [`release`]: the three are reached only with a claim.
pub fn access(_row: usize, _request: &mut toyos_abi::acpi::Access) -> Result<(), toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

pub fn lock_take() -> Result<bool, toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

pub fn lock_release() -> Result<(), toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

#[cfg(feature = "test-actuators")]
pub fn debug_firmware_lock(_own: bool) -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}
