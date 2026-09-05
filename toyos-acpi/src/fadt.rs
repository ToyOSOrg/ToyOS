//! The FADT (signature `FACP`): ACPI 6.5 §5.2.9, Table 5.9.

use crate::{find_table, Phys, Table, TableError, SDT_REVISION};

/// Table 5.9 offsets, from the start of the table.
pub const FADT_DSDT: usize = 40;
pub const FADT_PM1A_CNT_BLK: usize = 64;
const FADT_CENTURY: usize = 108;
const FADT_IAPC_BOOT_ARCH: usize = 109;
const FADT_FLAGS: usize = 112;
/// A Generic Address Structure (§5.2.3.2): space at +0, bit width at +1, bit offset at +2, address at +4.
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
pub const FADT_X_DSDT: usize = 140;

/// Fixed feature flags bit 10, `RESET_REG_SUP` (Table 5.10).
const RESET_REG_SUP: u32 = 1 << 10;
/// Address space 1, System I/O (Table 5.1).
const SPACE_SYSTEM_IO: u8 = 1;

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

pub fn century_of(index: u8) -> Century {
    match index {
        0 => Century::Absent,
        i if CMOS_RAM.contains(&i) => Century::At(i),
        i => Century::OutOfRange(i),
    }
}

/// What the FADT says about returning the machine to firmware: a register, or
/// the field that refused one. The alternative to naming them is a guessed
/// port, and a guess writes a byte to whatever else lives there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reset {
    /// An 8-bit System I/O port, and the byte the firmware asks be written to it.
    Port { port: u16, value: u8 },
    Unsupported,
    Space(u8),
    Field { bit_width: u8, bit_offset: u8 },
    Address(u64),
}

/// The reset register the FADT names, or which of its fields refused.
pub fn reset_register<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Reset, TableError> {
    const NEEDED: usize = FADT_RESET_VALUE + 1;
    let fadt = find_table(phys, rsdp_addr, b"FACP", NEEDED)?;
    let short = || TableError::Length { declared: fadt.len() as u32, needed: NEEDED };
    let flags = fadt.u32_at(FADT_FLAGS).ok_or_else(short)?;
    if flags & RESET_REG_SUP == 0 {
        return Ok(Reset::Unsupported);
    }
    let space = fadt.byte(FADT_RESET_REG).ok_or_else(short)?;
    if space != SPACE_SYSTEM_IO {
        return Ok(Reset::Space(space));
    }
    // The bit width, not the GAS access size: firmware may leave that 0 (undefined).
    let bit_width = fadt.byte(FADT_RESET_REG + 1).ok_or_else(short)?;
    let bit_offset = fadt.byte(FADT_RESET_REG + 2).ok_or_else(short)?;
    if (bit_width, bit_offset) != (8, 0) {
        return Ok(Reset::Field { bit_width, bit_offset });
    }
    let address = fadt.u64_at(FADT_RESET_REG + 4).ok_or_else(short)?;
    let value = fadt.byte(FADT_RESET_VALUE).ok_or_else(short)?;
    match u16::try_from(address) {
        Ok(port) => Ok(Reset::Port { port, value }),
        Err(_) => Ok(Reset::Address(address)),
    }
}

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
