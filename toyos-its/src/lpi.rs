//! LPIs (§5.1): their INTIDs, the configuration table every redistributor
//! reads and the pending table each one owns, and the redistributor registers
//! that name them.

use crate::Phys;

/// The first LPI's INTID (§2.2): everything below is an SGI, a PPI, an SPI
/// or special.
pub const FIRST: u32 = 8192;

/// Offsets in a redistributor's `RD_base` frame (§12.10, Table 12-27).
pub const GICR_CTLR: usize = 0x0000;
pub const GICR_TYPER: usize = 0x0008;
pub const GICR_PROPBASER: usize = 0x0070;
pub const GICR_PENDBASER: usize = 0x0078;

/// `GICR_CTLR.EnableLPIs` [0] (§12.11): from here the redistributor reads
/// both tables, and neither base register may be written.
pub const CTLR_ENABLE_LPIS: u32 = 1 << 0;

/// `GICR_TYPER.PLPIS` [0] (§12.11.37): the redistributor takes physical LPIs.
pub const TYPER_PLPIS: u64 = 1 << 0;

/// `GICR_TYPER.Processor_Number` [23:8]: how an ITS whose `GITS_TYPER.PTA`
/// is clear names this redistributor.
pub const fn processor_number(typer: u64) -> u16 {
    (typer >> 8) as u16
}

/// An LPI's INTID, inside the space it was made in: [`Space::lpi`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lpi(u32);

impl Lpi {
    pub const fn intid(self) -> u32 {
        self.0
    }

    /// Where its byte is in the configuration table: the table's first byte
    /// is INTID 8192's (§5.1.1).
    pub const fn configuration_index(self) -> usize {
        (self.0 - FIRST) as usize
    }

    /// Where its bit is in a pending table, which is indexed by the INTID
    /// itself: the byte, and the bit in it (§5.1.2).
    pub const fn pending_bit(self) -> (usize, u8) {
        ((self.0 / 8) as usize, (self.0 % 8) as u8)
    }
}

/// One LPI's byte in the configuration table (Table 5-1): the priority's
/// upper six bits in [7:2], bit [1] RES1, and the enable in bit [0].
pub const fn configuration(priority: u8, enabled: bool) -> u8 {
    priority & 0xFC | 0b10 | enabled as u8
}

/// The INTIDs of `id_bits` bits the tables are laid out for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Space {
    id_bits: u8,
}

/// `InnerCache` [9:7] `0b111`, read-allocate write-allocate write-back, with
/// `OuterCache` [58:56] zero, as the inner; `Shareability` [11:10] `0b01`,
/// inner shareable. Both base registers hold them at the same bits.
const CACHED: u64 = 0b111 << 7 | 0b01 << 10;

impl Space {
    /// `None` below 14 bits, which reach no LPI, and above the 24 that keep
    /// the configuration table within 16 MiB.
    pub const fn new(id_bits: u8) -> Option<Self> {
        if id_bits >= 14 && id_bits <= 24 {
            Some(Self { id_bits })
        } else {
            None
        }
    }

    /// `intid` as an LPI of this space.
    pub const fn lpi(self, intid: u32) -> Option<Lpi> {
        if intid >= FIRST && intid >> self.id_bits == 0 {
            Some(Lpi(intid))
        } else {
            None
        }
    }

    /// One byte for every LPI.
    pub const fn configuration_bytes(self) -> usize {
        (1 << self.id_bits) - FIRST as usize
    }

    /// One bit for every INTID, the first 8192 included.
    pub const fn pending_bytes(self) -> usize {
        (1 << self.id_bits) / 8
    }

    /// `GICR_PROPBASER` (§12.11.33) for the configuration table at `table`:
    /// `Physical_Address` [51:12] and `IDbits` [4:0], the bits minus one.
    pub const fn propbaser(self, table: Phys<12>) -> u64 {
        CACHED | table.get() | (self.id_bits - 1) as u64
    }
}

/// `GICR_PENDBASER` (§12.11.32) for the pending table at `table`, which is
/// all zeroes: `Physical_Address` [51:16], and `PTZ` [62], which says so and
/// lets the redistributor not read it.
pub const fn pendbaser(table: Phys<16>) -> u64 {
    1 << 62 | CACHED | table.get()
}
