//! The SPCR (Microsoft "Serial Port Console Redirection Table", revision 2
//! and later): which UART firmware's console is on, and where.

use crate::{find_table, Phys, TableError, SDT_HEADER_LEN};

/// Interface Type (1) at 36, reserved to 40, Base Address (a 12-byte Generic
/// Address Structure) at 40, Interrupt Type at 52, IRQ at 53, Global System
/// Interrupt at 54..58.
const INTERFACE_TYPE: usize = SDT_HEADER_LEN;
const BASE_ADDRESS: usize = 40;
const GSIV: usize = 54;
/// Every field this decoder reads lies below the GSIV's end.
pub const SPCR_NEEDED: usize = GSIV + 4;

/// ACPI 6.5 §5.2.3.2, Table 5.1: a register block's place and access shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gas {
    /// 0 is system memory, 1 system I/O.
    pub space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    /// 1..=4 are byte, word, dword and qword access; 0 is undefined.
    pub access_size: u8,
    pub address: u64,
}

/// The Generic Address Structure's system-memory space id.
pub const GAS_SYSTEM_MEMORY: u8 = 0;

/// The UART kinds the DBG2 table's serial subtypes name, as far as a kernel
/// that drives them needs to tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SerialInterface {
    /// 0x00: a full 16550.
    Ns16550,
    /// 0x03: an Arm PL011.
    Pl011,
    /// 0x0E: the Arm SBSA generic UART, a PL011 subset firmware configured.
    SbsaGeneric,
    /// 0x0D: the same, restricted to 32-bit accesses.
    SbsaGeneric32,
    /// Any other subtype, by number.
    Other(u8),
}

impl SerialInterface {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0x00 => Self::Ns16550,
            0x03 => Self::Pl011,
            0x0D => Self::SbsaGeneric32,
            0x0E => Self::SbsaGeneric,
            other => Self::Other(other),
        }
    }
}

/// What the SPCR says about firmware's console.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spcr {
    pub interface: SerialInterface,
    pub base: Gas,
    /// The UART's interrupt as a global system interrupt.
    pub gsiv: u32,
}

/// The SPCR at `rsdp_addr`, decoded.
pub fn spcr<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Spcr, TableError> {
    let table = find_table(phys, rsdp_addr, b"SPCR", SPCR_NEEDED)?;
    let short = TableError::Length { declared: table.len() as u32, needed: SPCR_NEEDED };
    let byte = |at| table.byte(at).ok_or(short);
    Ok(Spcr {
        interface: SerialInterface::from_raw(byte(INTERFACE_TYPE)?),
        base: Gas {
            space: byte(BASE_ADDRESS)?,
            bit_width: byte(BASE_ADDRESS + 1)?,
            bit_offset: byte(BASE_ADDRESS + 2)?,
            access_size: byte(BASE_ADDRESS + 3)?,
            address: table.u64_at(BASE_ADDRESS + 4).ok_or(short)?,
        },
        gsiv: table.u32_at(GSIV).ok_or(short)?,
    })
}
