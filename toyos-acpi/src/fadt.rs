//! The FADT (signature `FACP`): ACPI 6.5 §5.2.9, Table 5.9.
//!
//! What is read is bounded by the declared length; the revision is only ever a
//! preference, since a firmware claiming ACPI 2.0 does not prove the `X_`
//! fields are present.

use crate::{find_table, Phys, Table, TableError, SDT_REVISION};

/// Table 5.9 offsets, from the start of the table.
pub const FADT_DSDT: usize = 40;
pub const FADT_PM1A_CNT_BLK: usize = 64;
const FADT_CENTURY: usize = 108;
const FADT_IAPC_BOOT_ARCH: usize = 109;
pub const FADT_X_DSDT: usize = 140;

/// FADT revision and the IA-PC boot architecture flags.
// `Err` is not "absent" and must not be treated as one by the caller.
// Bit 1 of the flags is the port 60/64 keyboard-controller bit, defined only from FADT revision 3 onward.
pub fn iapc_boot_arch<P: Phys>(phys: P, rsdp_addr: u64) -> Result<(u8, u16), TableError> {
    const NEEDED: usize = FADT_IAPC_BOOT_ARCH + 2;
    let fadt = find_table(phys, rsdp_addr, b"FACP", NEEDED)?;
    let short = || TableError::Length { declared: fadt.len() as u32, needed: NEEDED };
    let revision = fadt.byte(SDT_REVISION).ok_or_else(short)?;
    let flags = fadt.u16_at(FADT_IAPC_BOOT_ARCH).ok_or_else(short)?;
    Ok((revision, flags))
}

/// What the FADT says about the RTC's century register.
///
/// Three answers rather than an `Option`, because "firmware names none" and
/// "firmware names one this kernel will not drive" are different facts and the
/// caller reports them differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Century {
    /// The century field is zero: this firmware names no register.
    Absent,
    /// Outside CMOS RAM, so it is not a century register whatever it is.
    OutOfRange(u8),
    At(u8),
}

/// 0x80+ selects with NMI-mask bit 7 set; below 0x0E is the RTC's own clock/status regs, not a century register.
pub const CMOS_RAM: core::ops::RangeInclusive<u8> = 0x0E..=0x7F;

/// Which CMOS register holds the RTC's century, as the FADT names it.
pub fn rtc_century<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Century, TableError> {
    const NEEDED: usize = FADT_CENTURY + 1;
    let fadt = find_table(phys, rsdp_addr, b"FACP", NEEDED)?;
    let declared = fadt
        .byte(FADT_CENTURY)
        .ok_or(TableError::Length { declared: fadt.len() as u32, needed: NEEDED })?;
    Ok(century_of(declared))
}

/// The classification on its own, for a caller that overrides the raw byte.
pub fn century_of(index: u8) -> Century {
    match index {
        0 => Century::Absent,
        i if CMOS_RAM.contains(&i) => Century::At(i),
        i => Century::OutOfRange(i),
    }
}

/// The DSDT address the FADT names: `X_DSDT` where the table is long enough and
/// the revision claims it, the 32-bit `DSDT` otherwise, and 0 for neither.
pub fn dsdt_address<P: Phys>(fadt: &Table<P>) -> u64 {
    let x_dsdt = match fadt.byte(SDT_REVISION) {
        Some(r) if r >= 2 => fadt.u64_at(FADT_X_DSDT).filter(|a| *a != 0),
        _ => None,
    };
    match x_dsdt {
        Some(addr) => addr,
        None => u64::from(fadt.u32_at(FADT_DSDT).unwrap_or(0)),
    }
}
