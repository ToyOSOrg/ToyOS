//! What memory type firmware gave a physical range.
//!
//! Read-only: firmware owns these registers, the kernel programs none. A
//! mapping with [`CachePolicy::DeferToMtrr`](crate::mm::paging::CachePolicy)
//! selects PAT entry 0 (WB), so what this module reports is the effective
//! type; the exception is [`effective_under_wc`], where WC outvotes the MTRR
//! instead of deferring to it.

use crate::arch::cpu;

const IA32_MTRRCAP: u32 = 0xFE;
const IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
const IA32_MTRR_PHYSBASE0: u32 = 0x200;
/// Bit 11 of `IA32_MTRR_DEF_TYPE`: clear means the whole address space is UC.
const DEF_TYPE_ENABLE: u64 = 1 << 11;
/// Bit 11 of an `IA32_MTRR_PHYSMASK`.
const PHYSMASK_VALID: u64 = 1 << 11;
/// Physical address bits of a PHYSBASE/PHYSMASK: 4 KiB-aligned, masked to the
/// 52-bit architectural ceiling, never narrower than a CPU's real width.
const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// A memory type in the MTRRs' architectural encoding, matching the MSR values.
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// Why a range has no single answer, reported rather than resolved: picking one
/// type here would be inventing an answer firmware never gave.
pub enum Unknown {
    /// A variable MTRR holds an encoding the architecture does not define.
    ReservedEncoding,
    /// Overlapping MTRRs whose types the architecture leaves undefined.
    Conflicting,
    /// Part of the range is covered and part is not.
    PartiallyCovered,
}

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

/// The memory type firmware gave `[base, base + size)`; fixed MTRRs (first
/// 1 MiB) are not consulted.
pub fn range_type(base: u64, size: u64) -> Effective {
    let def_type = cpu::rdmsr(IA32_MTRR_DEF_TYPE);
    if def_type & DEF_TYPE_ENABLE == 0 {
        return Effective::MtrrsDisabled;
    }
    let default = match MemoryType::from_encoding(def_type as u8) {
        Some(t) => t,
        None => return Effective::Unknown(Unknown::ReservedEncoding),
    };

    let end = base + size;
    let mut covering: Option<MemoryType> = None;
    for i in 0..(cpu::rdmsr(IA32_MTRRCAP) & 0xFF) as u32 {
        let mask = cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2 + 1);
        if mask & PHYSMASK_VALID == 0 {
            continue;
        }
        // A PHYSMASK's contiguous high bits size the region: PHYSBASE plus
        // 1 << its lowest set bit.
        let phys_mask = mask & PHYS_MASK;
        let base_msr = cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2);
        let region_start = base_msr & phys_mask;
        let region_end = region_start + (1u64 << phys_mask.trailing_zeros());
        if region_end <= base || region_start >= end {
            continue;
        }
        if region_start > base || region_end < end {
            // Not uniform under this MTRR; picking a winner would be
            // inventing an answer.
            return Effective::Unknown(Unknown::PartiallyCovered);
        }
        let t = match MemoryType::from_encoding(base_msr as u8) {
            Some(t) => t,
            None => return Effective::Unknown(Unknown::ReservedEncoding),
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
