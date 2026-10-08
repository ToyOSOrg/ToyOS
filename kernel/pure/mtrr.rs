//! What memory type the processor's variable range registers give a physical
//! range, decided from the words read off them: `IA32_MTRR_DEF_TYPE` and each
//! `IA32_MTRR_PHYSBASEn`, `IA32_MTRR_PHYSMASKn` pair.
//!
//! The fixed range registers, which type the first 1 MiB, are not among
//! them: an answer for a range below 1 MiB is the variable registers' alone.
//! Nor is anything outside the range registers that types memory, a
//! processor's own configuration bit for RAM above 4 GiB among them.
//!
//! A range has one type or it has none ([`Unknown`]): picking one where the
//! registers give two would be inventing an answer firmware never gave.
//!
//! The registers are each CPU's own. [`beside`] says how one CPU's stand
//! beside the boot processor's, where firmware wrote what it typed the
//! machine's memory.

#![forbid(unsafe_code)]

/// Bit 11 of `IA32_MTRR_DEF_TYPE`: clear means the whole address space is UC.
const DEF_TYPE_ENABLE: u64 = 1 << 11;
/// Bit 11 of an `IA32_MTRR_PHYSMASK`.
const PHYSMASK_VALID: u64 = 1 << 11;
/// Physical address bits of a PHYSBASE/PHYSMASK: 4 KiB-aligned, masked to the
/// 52-bit architectural ceiling, never narrower than a CPU's real width.
const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// A memory type in the MTRRs' architectural encoding, matching the MSR values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryType {
    Uncacheable,
    WriteCombining,
    WriteThrough,
    WriteProtected,
    WriteBack,
}

impl MemoryType {
    fn from_encoding(raw: u8) -> Option<Self> {
        match raw {
            0x00 => Some(Self::Uncacheable),
            0x01 => Some(Self::WriteCombining),
            0x04 => Some(Self::WriteThrough),
            0x05 => Some(Self::WriteProtected),
            0x06 => Some(Self::WriteBack),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Uncacheable => "UC",
            Self::WriteCombining => "WC",
            Self::WriteThrough => "WT",
            Self::WriteProtected => "WP",
            Self::WriteBack => "WB",
        }
    }
}

/// Why a range has no single answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unknown {
    /// A register holds an encoding the architecture does not define.
    ReservedEncoding,
    /// Overlapping MTRRs whose types the architecture leaves undefined.
    Conflicting,
    /// Part of the range is covered and part is not.
    PartiallyCovered,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effective {
    Known(MemoryType),
    Unknown(Unknown),
    /// MTRRs are off, so the whole address space is UC by architecture.
    MtrrsDisabled,
}

impl Effective {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Known(t) => t.name(),
            Self::MtrrsDisabled => "UC (MTRRs disabled)",
            Self::Unknown(Unknown::ReservedEncoding) => "unknown (reserved MTRR encoding)",
            Self::Unknown(Unknown::Conflicting) => "unknown (overlapping MTRRs disagree)",
            Self::Unknown(Unknown::PartiallyCovered) => "unknown (range only partly covered)",
        }
    }

    /// Whether the range registers type the range uncacheable, which is what
    /// tells a register from RAM. Registers that are off type nothing: all
    /// RAM is uncacheable under them too, so that answer tells neither.
    pub fn typed_uncacheable(&self) -> bool {
        matches!(self, Self::Known(MemoryType::Uncacheable))
    }
}

/// Effective type of a WC-PAT page over range `mtrr`: WC wins even over an
/// MTRR's UC (SDM Vol. 3A Table 11-7); `None` only when `mtrr` has no single
/// answer.
pub fn effective_under_wc(mtrr: &Effective) -> Option<MemoryType> {
    match mtrr {
        Effective::Known(_) | Effective::MtrrsDisabled => Some(MemoryType::WriteCombining),
        Effective::Unknown(_) => None,
    }
}

/// Two MTRRs over one address: UC beats anything, WT beats WB, else undefined.
fn combine(a: MemoryType, b: MemoryType) -> Option<MemoryType> {
    use MemoryType::{Uncacheable, WriteBack, WriteThrough};
    match (a, b) {
        (x, y) if x == y => Some(x),
        (Uncacheable, _) | (_, Uncacheable) => Some(Uncacheable),
        (WriteThrough, WriteBack) | (WriteBack, WriteThrough) => Some(WriteThrough),
        _ => None,
    }
}

/// The memory type of `first..=last` under the default-type word `def_type`
/// and `pairs`, each variable register's `(PHYSBASE, PHYSMASK)`.
pub fn range_type(def_type: u64, pairs: impl IntoIterator<Item = (u64, u64)>, first: u64, last: u64) -> Effective {
    if def_type & DEF_TYPE_ENABLE == 0 {
        return Effective::MtrrsDisabled;
    }
    let Some(default) = MemoryType::from_encoding(def_type as u8) else {
        return Effective::Unknown(Unknown::ReservedEncoding);
    };

    let mut covering: Option<MemoryType> = None;
    for (base, mask) in pairs {
        if mask & PHYSMASK_VALID == 0 {
            continue;
        }
        // A PHYSMASK's contiguous high bits size the region: from PHYSBASE
        // under the mask to every address below the mask's lowest set bit
        // above it. A mask with no address bit matches every address.
        let phys_mask = mask & PHYS_MASK;
        let region_first = base & phys_mask;
        let region_last = region_first | (phys_mask & phys_mask.wrapping_neg()).wrapping_sub(1);
        if region_last < first || region_first > last {
            continue;
        }
        if region_first > first || region_last < last {
            return Effective::Unknown(Unknown::PartiallyCovered);
        }
        let Some(t) = MemoryType::from_encoding(base as u8) else {
            return Effective::Unknown(Unknown::ReservedEncoding);
        };
        covering = Some(match covering {
            None => t,
            Some(prev) => match combine(prev, t) {
                Some(merged) => merged,
                None => return Effective::Unknown(Unknown::Conflicting),
            },
        });
    }
    Effective::Known(covering.unwrap_or(default))
}

/// How a CPU's range registers stand beside the boot processor's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Beside {
    /// Word for word the boot processor's: this CPU reads every range as the
    /// type [`range_type`] gives it under those.
    Same,
    /// Not the boot processor's, and off: this CPU reads every range
    /// uncached, whatever firmware typed it.
    Off,
    /// Not the boot processor's, and on: nothing the boot processor's say of
    /// a range is known of a read this CPU makes there.
    Different,
}

/// One CPU's registers, `other`, beside the boot processor's: each the
/// default-type word and every variable register's `(PHYSBASE, PHYSMASK)`.
/// The words whole, a register whose valid bit is clear included: two
/// snapshots that type every range alike and are not the same words are
/// [`Beside::Different`], which refuses and never passes.
pub fn beside(boot: (u64, &[(u64, u64)]), other: (u64, &[(u64, u64)])) -> Beside {
    if other == boot {
        Beside::Same
    } else if other.0 & DEF_TYPE_ENABLE == 0 {
        Beside::Off
    } else {
        Beside::Different
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryType::{Uncacheable as UC, WriteBack as WB, WriteCombining as WC, WriteProtected as WP, WriteThrough as WT};
    use super::*;

    const ON: u64 = DEF_TYPE_ENABLE;
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    /// A valid pair typing `size` bytes at `base`, a power of two of them, on
    /// a processor of 39 physical address bits.
    fn pair(base: u64, size: u64, encoding: u64) -> (u64, u64) {
        assert!(size.is_power_of_two() && base % size == 0);
        (base | encoding, !(size - 1) & ((1 << 39) - 1) & PHYS_MASK | PHYSMASK_VALID)
    }

    fn known(def_type: u64, pairs: &[(u64, u64)], first: u64, last: u64) -> Effective {
        range_type(def_type, pairs.iter().copied(), first, last)
    }

    #[test]
    fn a_range_no_register_covers_has_the_default_type() {
        for (encoding, ty) in [(0, UC), (1, WC), (4, WT), (5, WP), (6, WB)] {
            assert_eq!(known(ON | encoding, &[], 0x10_0000, 0x10_0fff), Effective::Known(ty));
            // A pair elsewhere, and one whose valid bit is clear over the range itself.
            let pairs = [pair(GIB, GIB, 0), (2 * GIB, !(GIB - 1) & PHYS_MASK)];
            assert_eq!(known(ON | encoding, &pairs, 2 * GIB, 2 * GIB + 7), Effective::Known(ty));
        }
    }

    #[test]
    fn a_default_or_a_covering_register_of_no_defined_encoding_is_unknown() {
        for encoding in [2u64, 3, 7, 0xFF] {
            assert_eq!(known(ON | encoding, &[], 0, 7), Effective::Unknown(Unknown::ReservedEncoding), "default {encoding}");
            assert_eq!(
                known(ON, &[pair(GIB, GIB, encoding)], GIB, GIB + 7),
                Effective::Unknown(Unknown::ReservedEncoding),
                "a register's {encoding}"
            );
            // The same register over another range decides nothing of this one.
            assert_eq!(known(ON, &[pair(GIB, GIB, encoding)], 2 * GIB, 2 * GIB + 7), Effective::Known(UC));
        }
    }

    #[test]
    fn a_register_types_its_region_to_the_last_byte_and_no_further() {
        // Write-back by default, and the 256 MiB under 4 GiB uncacheable.
        let hole = [pair(4 * GIB - 256 * MIB, 256 * MIB, 0)];
        let (start, end) = (4 * GIB - 256 * MIB, 4 * GIB);
        assert_eq!(known(ON | 6, &hole, start, start), Effective::Known(UC));
        assert_eq!(known(ON | 6, &hole, end - 8, end - 1), Effective::Known(UC));
        assert_eq!(known(ON | 6, &hole, start, end - 1), Effective::Known(UC));
        assert_eq!(known(ON | 6, &hole, start - 1, start - 1), Effective::Known(WB));
        assert_eq!(known(ON | 6, &hole, end, end + 7), Effective::Known(WB));
        // One byte out of it, by either end, is two types and so none.
        assert_eq!(known(ON | 6, &hole, start - 1, start), Effective::Unknown(Unknown::PartiallyCovered));
        assert_eq!(known(ON | 6, &hole, end - 1, end), Effective::Unknown(Unknown::PartiallyCovered));
        assert_eq!(known(ON | 6, &hole, start - 8, end + 7), Effective::Unknown(Unknown::PartiallyCovered));
    }

    #[test]
    fn two_registers_over_one_range_answer_as_the_architecture_orders_them() {
        let over = |a: u64, b: u64| known(ON | 6, &[pair(GIB, GIB, a), pair(GIB, 256 * MIB, b)], GIB, GIB + 7);
        // Uncacheable over anything, whichever register says it.
        for other in [1, 4, 5, 6] {
            assert_eq!(over(0, other), Effective::Known(UC));
            assert_eq!(over(other, 0), Effective::Known(UC));
        }
        assert_eq!(over(4, 6), Effective::Known(WT));
        assert_eq!(over(6, 4), Effective::Known(WT));
        for same in [(0, UC), (1, WC), (4, WT), (5, WP), (6, WB)] {
            assert_eq!(over(same.0, same.0), Effective::Known(same.1));
        }
        // Every other pair of types is undefined.
        for (a, b) in [(1, 4), (1, 5), (1, 6), (4, 5), (5, 6)] {
            assert_eq!(over(a, b), Effective::Unknown(Unknown::Conflicting), "{a} over {b}");
            assert_eq!(over(b, a), Effective::Unknown(Unknown::Conflicting), "{b} over {a}");
        }
        // A third register that is uncacheable does not undo a conflict the first two made.
        let three = [pair(GIB, GIB, 1), pair(GIB, GIB, 6), pair(GIB, GIB, 0)];
        assert_eq!(known(ON | 6, &three, GIB, GIB + 7), Effective::Unknown(Unknown::Conflicting));
        // Past the smaller register the larger decides alone.
        assert_eq!(known(ON | 6, &[pair(GIB, GIB, 1), pair(GIB, 256 * MIB, 5)], GIB + 256 * MIB, GIB + 256 * MIB + 7), Effective::Known(WC));
    }

    #[test]
    fn registers_that_are_off_type_nothing_uncacheable() {
        // The enable bit clear, whatever the default type and the pairs say.
        for def_type in [0u64, 6, 0xFF, 1 << 10] {
            let off = known(def_type, &[pair(GIB, GIB, 0)], GIB, GIB + 7);
            assert_eq!(off, Effective::MtrrsDisabled);
            assert!(!off.typed_uncacheable(), "registers that are off were read as typing a range uncacheable");
        }
        assert!(Effective::Known(UC).typed_uncacheable());
        for ty in [WC, WT, WP, WB] {
            assert!(!Effective::Known(ty).typed_uncacheable(), "{ty:?}");
        }
        for unknown in [Unknown::ReservedEncoding, Unknown::Conflicting, Unknown::PartiallyCovered] {
            assert!(!Effective::Unknown(unknown).typed_uncacheable(), "{unknown:?}");
        }
    }

    #[test]
    fn a_mask_with_no_address_bit_and_a_region_at_the_top_decide_without_overflow() {
        // A valid mask of no address bit matches every address.
        let all = [(0, PHYSMASK_VALID)];
        assert_eq!(known(ON | 6, &all, 0, u64::MAX), Effective::Known(UC));
        assert_eq!(known(ON | 6, &all, u64::MAX, u64::MAX), Effective::Known(UC));
        // The last 4 KiB the registers can name, and the byte after it.
        let top = [(PHYS_MASK, PHYS_MASK | PHYSMASK_VALID)];
        assert_eq!(known(ON | 6, &top, PHYS_MASK, PHYS_MASK | 0xFFF), Effective::Known(UC));
        assert_eq!(known(ON | 6, &top, (PHYS_MASK | 0xFFF) + 1, u64::MAX), Effective::Known(WB));
    }

    /// Firmware's registers on a machine of write-back RAM and an uncacheable
    /// hole under 4 GiB.
    const BOOT: (u64, [(u64, u64); 2]) = (ON | 6, [(0xC000_0000, 0x7F_C000_0800), (0, 0)]);

    #[test]
    fn registers_that_are_word_for_word_the_boot_processors_are_the_same() {
        assert_eq!(beside((BOOT.0, &BOOT.1), (BOOT.0, &BOOT.1)), Beside::Same);
        // Off on both, the same words: the same, and typing nothing.
        assert_eq!(beside((6, &BOOT.1), (6, &BOOT.1)), Beside::Same);
        assert_eq!(beside((ON, &[]), (ON, &[])), Beside::Same);
    }

    #[test]
    fn registers_that_are_off_and_not_the_boot_processors_are_off() {
        // As a CPU comes out of reset: every word zero.
        assert_eq!(beside((BOOT.0, &BOOT.1), (0, &[(0, 0); 2])), Beside::Off);
        // The enable bit alone clear, every other word the boot processor's.
        assert_eq!(beside((BOOT.0, &BOOT.1), (6, &BOOT.1)), Beside::Off);
        // Off on both and not the same words.
        assert_eq!(beside((6, &BOOT.1), (0, &BOOT.1)), Beside::Off);
    }

    #[test]
    fn registers_that_are_on_and_not_the_boot_processors_are_different() {
        let boot = (BOOT.0, &BOOT.1[..]);
        // Another default type; the fixed registers' enable bit; a pair's
        // base, its type, its mask and its valid bit; a pair more, and one fewer.
        let others: [(u64, &[(u64, u64)]); 8] = [
            (ON, &BOOT.1),
            (ON | 6 | 1 << 10, &BOOT.1),
            (ON | 6, &[(0x8000_0000, 0x7F_C000_0800), (0, 0)]),
            (ON | 6, &[(0xC000_0006, 0x7F_C000_0800), (0, 0)]),
            (ON | 6, &[(0xC000_0000, 0x7F_8000_0800), (0, 0)]),
            (ON | 6, &[(0xC000_0000, 0x7F_C000_0000), (0, 0)]),
            (ON | 6, &[(0xC000_0000, 0x7F_C000_0800), (0, 0), (0, 0)]),
            (ON | 6, &[(0xC000_0000, 0x7F_C000_0800)]),
        ];
        for other in others {
            assert_eq!(beside(boot, other), Beside::Different, "{other:x?}");
        }
        // On where the boot processor's are off.
        assert_eq!(beside((6, &BOOT.1), boot), Beside::Different);
    }
}
