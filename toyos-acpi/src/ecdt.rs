//! The ECDT: ACPI 6.5 §5.2.16, Table 5.88 — the embedded controller's ports
//! and its GPE, named for an OS that reaches the controller before any AML.

use crate::{Phys, Table, SDT_HEADER_LEN};

/// Table 5.88: `EC_CONTROL` and `EC_DATA` are Generic Address Structures,
/// then `UID` (4) and `GPE_BIT` (1).
const EC_CONTROL: usize = SDT_HEADER_LEN;
const EC_DATA: usize = EC_CONTROL + 12;
const GPE_BIT: usize = EC_DATA + 12 + 4;

/// The bytes [`ecdt`] reads to the end of.
pub const ECDT_NEEDED: usize = GPE_BIT + 1;

const SPACE_SYSTEM_IO: u8 = 1;

/// The embedded controller as the ECDT names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ec {
    /// The command register when written and the status register when read.
    pub command: u16,
    pub data: u16,
    /// The GPE it raises.
    pub gpe: u8,
}

/// Which register an [`EcRefused`] is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Register {
    Command,
    Data,
}

/// Why the ECDT names no controller this kernel hands out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EcRefused {
    /// The table ends before `GPE_BIT`.
    Short { len: usize },
    /// Not in the System I/O space: an MMIO controller is not served.
    NotSystemIo { register: Register, space: u8 },
    /// Not one byte wide at bit 0.
    Width { register: Register, bit_width: u8, bit_offset: u8 },
    /// Zero, or past the 16-bit port space.
    Address { register: Register, address: u64 },
}

/// The controller the ECDT names.
pub fn ecdt<P: Phys>(table: &Table<P>) -> Result<Ec, EcRefused> {
    let short = EcRefused::Short { len: table.len() };
    let port = |register, at: usize| {
        let field = |offset| table.byte(at + offset).ok_or(short);
        let (space, bit_width, bit_offset) = (field(0)?, field(1)?, field(2)?);
        let address = table.u64_at(at + 4).ok_or(short)?;
        if space != SPACE_SYSTEM_IO {
            return Err(EcRefused::NotSystemIo { register, space });
        }
        if (bit_width, bit_offset) != (8, 0) {
            return Err(EcRefused::Width { register, bit_width, bit_offset });
        }
        match u16::try_from(address) {
            Ok(port) if port != 0 => Ok(port),
            _ => Err(EcRefused::Address { register, address }),
        }
    };
    Ok(Ec {
        command: port(Register::Command, EC_CONTROL)?,
        data: port(Register::Data, EC_DATA)?,
        gpe: table.byte(GPE_BIT).ok_or(short)?,
    })
}
