//! The ECDT (ACPI 6.5 §5.2.15, Table 5.54): the embedded controller's two
//! registers, its GPE and its device's path, for an OS that makes the
//! controller's space "available before executing any control method"
//! (§6.5.4).

use core::ops::Range;

use crate::{Phys, Table, SDT_HEADER_LEN};

/// Table 5.54: `EC_CONTROL` and `EC_DATA` are Generic Address Structures,
/// then `UID` (4), `GPE_BIT` (1) and `EC_ID`, ASCII up to a NUL.
const EC_CONTROL: usize = SDT_HEADER_LEN;
const EC_DATA: usize = EC_CONTROL + 12;
const GPE_BIT: usize = EC_DATA + 12 + 4;
const EC_ID: usize = GPE_BIT + 1;

/// The bytes [`ecdt`] reads at least: through `EC_ID`'s NUL, where its path
/// is empty.
pub const ECDT_NEEDED: usize = EC_ID + 1;

const SPACE_SYSTEM_IO: u8 = 1;

/// The embedded controller as the ECDT names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ec {
    /// The command register when written and the status register when read.
    pub command: u16,
    pub data: u16,
    /// The GPE it raises.
    pub gpe: u8,
    /// Where `EC_ID`'s path lies in the table, its NUL left out: "a fully
    /// qualified reference to the namespace object that is this embedded
    /// controller device".
    pub id: Range<usize>,
}

/// Which register an [`EcRefused`] is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EcRegister {
    Command,
    Data,
}

/// Why the ECDT names no controller this decode hands out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EcRefused {
    /// The table ends before `EC_ID`.
    Short { len: usize },
    /// Not in the System I/O space, the controller's two ports of §12.11.
    NotSystemIo { register: EcRegister, space: u8 },
    /// Not one byte wide at bit 0.
    Width { register: EcRegister, bit_width: u8, bit_offset: u8 },
    /// Zero, or past the 16-bit port space.
    Address { register: EcRegister, address: u64 },
    /// `EC_ID` runs to the table's end without its NUL, or holds a byte
    /// that is not printable ASCII.
    Id,
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
    let command = port(EcRegister::Command, EC_CONTROL)?;
    let data = port(EcRegister::Data, EC_DATA)?;
    let gpe = table.byte(GPE_BIT).ok_or(short)?;
    let mut end = EC_ID;
    loop {
        match table.byte(end) {
            Some(0) => break,
            Some(0x21..=0x7E) => end += 1,
            _ => return Err(EcRefused::Id),
        }
    }
    Ok(Ec { command, data, gpe, id: EC_ID..end })
}
