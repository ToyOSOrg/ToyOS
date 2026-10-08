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

/// Never called, as [`release`]: the four are reached only with a claim.
pub fn access(_row: &crate::isa::Row, _request: &mut toyos_abi::acpi::Access) -> Result<(), toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

pub fn lock_take(_row: &crate::isa::Row) -> Result<bool, toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

pub fn lock_release(_row: &crate::isa::Row) -> Result<(), toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

pub fn s5(_row: &crate::isa::Row, _word: u64) -> Result<(), toyos_abi::syscall::SyscallError> {
    unreachable!("AArch64 has no ACPI row")
}

#[cfg(feature = "test-actuators")]
pub fn debug_firmware_lock(_act: u64) -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}
