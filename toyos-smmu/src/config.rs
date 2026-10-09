//! The stream table entry (§5.2) and the context descriptor (§5.4): 64 bytes
//! each, eight little-endian doublewords, a field's bit `n` being bit
//! `n % 64` of doubleword `n / 64`.

use toyos_phys::Phys;

use crate::unit::Unit;
use crate::Asid;

/// A stream table entry: [`Ste::ABORT`] or [`Ste::stage1`], and no other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ste([u64; 8]);

/// `V`, bit [0].
const STE_V: u64 = 1 << 0;
/// `Config`, bits [3:1]: `0b101` is stage 1 translate, stage 2 bypass.
const STE_STAGE1: u64 = 0b101 << 1;
/// Doubleword 1: `S1CIR` [67:66] and `S1COR` [69:68] `0b01`, write-back
/// read-allocate, and `S1CSH` [71:70] `0b11`, inner shareable — how the unit
/// reads the context descriptor.
const STE_CD_CACHED: u64 = 0b01 << 2 | 0b01 << 4 | 0b11 << 6;
/// `S1STALLD`, bit [91].
const STE_S1STALLD: u64 = 1 << 27;

impl Ste {
    /// `V` set and `Config` `0b000`: every transaction of the stream is
    /// aborted, and no event is recorded for it. An entry of all zeroes
    /// aborts too, and records `C_BAD_STE` each time.
    pub const ABORT: Self = Self([STE_V, 0, 0, 0, 0, 0, 0, 0]);

    /// The stream translated by stage 1 through the one context descriptor
    /// at `context`: `S1ContextPtr` [55:6], `S1CDMax` [63:59] zero so a
    /// transaction carrying a SubstreamID is aborted, `STRW` [95:94] zero for
    /// the Non-secure EL1 regime, `EATS` [93:92] zero so ATS is refused, and
    /// no stage 2 field. `None` where the descriptor is past the unit's
    /// output size.
    pub const fn stage1(context: Phys<6>, unit: &Unit) -> Option<Self> {
        if !unit.reaches(context.get(), 64) {
            return None;
        }
        let stall = if unit.stall_disable() { STE_S1STALLD } else { 0 };
        Some(Self([STE_V | STE_STAGE1 | context.get(), STE_CD_CACHED | stall, 0, 0, 0, 0, 0, 0]))
    }

    /// The entry's eight doublewords, as the stream table holds them.
    pub const fn words(&self) -> [u64; 8] {
        self.0
    }
}

/// A context descriptor: [`Cd::new`]'s, and no other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cd([u64; 8]);

/// `T0SZ`, bits [5:0]: a 48-bit input, walked from level 0.
const CD_T0SZ: u64 = 64 - crate::table::INPUT_BITS as u64;
/// `TG0` [7:6] zero is the 4 KiB granule; `IR0` [9:8] and `OR0` [11:10]
/// `0b01`, write-back write-allocate, and `SH0` [13:12] `0b11`, inner
/// shareable — how the unit walks the tables; `EPD0` [14] and `ENDI` [15]
/// clear, walk `TTB0` little-endian.
const CD_WALK: u64 = 0b01 << 8 | 0b01 << 10 | 0b11 << 12;
/// `EPD1`, bit [30]: no walk through `TTB1`, so the upper half faults.
const CD_EPD1: u64 = 1 << 30;
/// `V`, bit [31].
const CD_V: u64 = 1 << 31;
/// `AA64`, bit [41]: VMSAv8-64 descriptors.
const CD_AA64: u64 = 1 << 41;
/// `R` [45] records a translation-related fault and `A` [46] aborts its
/// transaction rather than completing it as read-as-zero, write-ignored;
/// `S` [44] stays clear, so none stalls (§5.5).
const CD_FAULTS: u64 = 1 << 45 | 1 << 46;
/// `ASET`, bit [47]: the ASID is the unit's own and no CPU process's.
const CD_ASET: u64 = 1 << 47;

impl Cd {
    /// The context whose tables start at `root`, tagged `asid`: `IPS`
    /// [34:32] from the unit's output size, `ASID` [63:48], `TTB0` [119:68]
    /// holding the root's bits [55:4], and `MAIR0` [223:192] the attributes
    /// [`crate::table`]'s leaves index. `AFFD` [35] stays clear, and every
    /// leaf written sets its access flag. `None` where the root table is
    /// past the unit's output size.
    pub const fn new(root: Phys<12>, asid: Asid, unit: &Unit) -> Option<Self> {
        if !unit.reaches(root.get(), 4096) {
            return None;
        }
        let word0 = CD_T0SZ
            | CD_WALK
            | CD_EPD1
            | CD_V
            | unit.ips() << 32
            | CD_AA64
            | CD_FAULTS
            | CD_ASET
            | (asid.get() as u64) << 48;
        Some(Self([word0, root.get(), 0, crate::table::MAIR0 as u64, 0, 0, 0, 0]))
    }

    /// The descriptor's eight doublewords.
    pub const fn words(&self) -> [u64; 8] {
        self.0
    }
}
