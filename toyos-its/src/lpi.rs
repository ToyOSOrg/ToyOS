//! LPIs (§5.1): their INTIDs, the configuration table every redistributor
//! reads and the pending table each one owns, and the redistributor registers
//! that name them.

use toyos_phys::Phys;

/// The first LPI's INTID (§2.2): everything below is an SGI, a PPI, an SPI
/// or special.
pub const FIRST: u32 = 8192;

/// Offsets in a redistributor's `RD_base` frame (§12.10, Table 12-27).
pub const GICR_PROPBASER: usize = 0x0070;
pub const GICR_PENDBASER: usize = 0x0078;

/// `GICR_CTLR.EnableLPIs` [0] (§12.11.2): from here the redistributor reads
/// both tables, and neither base register may be written.
pub const CTLR_ENABLE_LPIS: u32 = 1 << 0;

/// `GICR_TYPER.PLPIS` [0] (§12.11.37): the redistributor takes physical LPIs.
pub const TYPER_PLPIS: u64 = 1 << 0;

/// `GICR_TYPER.Processor_Number` [23:8]: how an ITS whose `GITS_TYPER.PTA`
/// is clear names this redistributor.
pub const fn processor_number(typer: u64) -> u16 {
    (typer >> 8) as u16
}

/// An LPI's INTID, one the distributor and its redistributor both take:
/// [`Space::lpi`].
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
}

/// One LPI's byte in the configuration table (Table 5-1): the priority's
/// upper six bits in [7:2], bit [1] RES1, and the enable in bit [0].
pub const fn configuration(priority: u8, enabled: bool) -> u8 {
    priority & 0xFC | 0b10 | enabled as u8
}

/// The two tables laid out for INTIDs of `id_bits` bits, which the
/// distributor has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    id_bits: u8,
    lpis: u32,
}

/// `InnerCache` [9:7] `0b111`, read-allocate write-allocate write-back, with
/// `OuterCache` [58:56] zero, as the inner; `Shareability` [11:10] `0b01`,
/// inner shareable. Both base registers hold them at the same bits.
const CACHED: u64 = 0b111 << 7 | 0b01 << 10;

impl Layout {
    /// `GICD_TYPER` (§12.9.38) judged: `None` where the distributor takes no
    /// LPI (`LPIS` [17] clear), and for `id_bits` below 14, which reach no
    /// LPI, or above its INTIDs' (`IDbits` [23:19] plus one). Its `num_LPIs`
    /// [15:11], where not zero, counts the LPIs there are from 8192 on.
    pub const fn new(gicd_typer: u32, id_bits: u8) -> Option<Self> {
        let widest = (gicd_typer >> 19 & 0x1F) as u8 + 1;
        if gicd_typer & 1 << 17 == 0 || id_bits < 14 || id_bits > widest {
            return None;
        }
        let numbered = (1u64 << id_bits) - FIRST as u64;
        let counted = match gicd_typer >> 11 & 0x1F {
            0 => numbered,
            count => 2u64 << count,
        };
        Some(Self { id_bits, lpis: if counted < numbered { counted } else { numbered } as u32 })
    }

    /// One byte for every INTID from 8192 that `id_bits` bits number.
    pub const fn configuration_bytes(self) -> u64 {
        (1 << self.id_bits) - FIRST as u64
    }

    /// One bit for every INTID, the first 8192 included.
    pub const fn pending_bytes(self) -> u64 {
        (1 << self.id_bits) / 8
    }

    /// `GICR_PROPBASER` (§12.11.33) for the configuration table at `table`:
    /// `Physical_Address` [51:12] and `IDbits` [4:0], the bits minus one.
    pub const fn propbaser(self, table: Phys<12>) -> u64 {
        CACHED | table.get() | (self.id_bits - 1) as u64
    }

    /// The LPIs of a redistributor that reads `propbaser` back: `None` where
    /// its `IDbits` is not what [`Layout::propbaser`] wrote, which a
    /// redistributor that holds the register as its own answers.
    pub const fn space(self, propbaser: u64) -> Option<Space> {
        if (propbaser & 0x1F) as u8 == self.id_bits - 1 {
            Some(Space { lpis: self.lpis })
        } else {
            None
        }
    }
}

/// The LPIs there are: those a [`Layout`]'s tables hold, that the
/// distributor counts, in a redistributor that took the layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Space {
    lpis: u32,
}

impl Space {
    /// `intid` as an LPI of this space.
    pub const fn lpi(self, intid: u32) -> Option<Lpi> {
        if intid >= FIRST && intid - FIRST < self.lpis {
            Some(Lpi(intid))
        } else {
            None
        }
    }
}

/// `GICR_PENDBASER.PTZ` [62]: the pending table is all zeroes, so the
/// redistributor need not read it. Write-only: it reads as zero (§12.11.32).
pub const PENDBASER_PTZ: u64 = 1 << 62;

/// `GICR_PENDBASER` (§12.11.32) for the pending table at `table`, which is
/// all zeroes: `Physical_Address` [51:16], and [`PENDBASER_PTZ`].
pub const fn pendbaser(table: Phys<16>) -> u64 {
    PENDBASER_PTZ | CACHED | table.get()
}
