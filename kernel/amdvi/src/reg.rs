//! The MMIO registers this driver writes or reads (§3.4), by offset, and the
//! fields of each it uses.

use toyos_phys::Phys;

use crate::dte::{DEVICE_IDS, ENTRY_BYTES};

pub const DEVICE_TABLE: u64 = 0x0000;
pub const COMMAND_BASE: u64 = 0x0008;
pub const EVENT_BASE: u64 = 0x0010;
pub const CONTROL: u64 = 0x0018;
pub const EXCLUSION_BASE: u64 = 0x0020;
pub const EXCLUSION_LIMIT: u64 = 0x0028;
pub const EXTENDED_FEATURES: u64 = 0x0030;
pub const COMMAND_HEAD: u64 = 0x2000;
pub const COMMAND_TAIL: u64 = 0x2008;
pub const EVENT_HEAD: u64 = 0x2010;
pub const EVENT_TAIL: u64 = 0x2018;
pub const STATUS: u64 = 0x2020;
/// The first SMI filter register (MMIO 0060h–00D8h, Table 80), 8 bytes each.
pub const SMI_FILTER: u64 = 0x0060;

/// One SMI filter register (MMIO 00[60–D8]h): `SmiDID` 15:0, `SmiDV` 16,
/// `SmiFLock` 17.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SmiFilter(pub u64);

/// `registers` as the filter is to pass SMIs from the ids firmware left
/// valid and from each of `platform`, every register locked (§2.1.5.3 steps
/// 3–6): an unlocked register firmware left invalid takes an id it left out,
/// and the rest are locked invalid. Refused with the first platform id no
/// such register is left for: a filter without it would refuse that
/// source's SMIs.
pub fn smi_filter(registers: &mut [SmiFilter], platform: &[u16]) -> Result<(), u16> {
    for &id in platform {
        if registers.iter().any(|register| register.passes() == Some(id)) {
            continue;
        }
        let free = registers.iter_mut().find(|register| !register.locked() && register.passes().is_none()).ok_or(id)?;
        *free = SmiFilter::passing(id);
    }
    for register in registers.iter_mut().filter(|register| !register.locked()) {
        register.0 = register.passes().map_or(SmiFilter::LOCK, |id| SmiFilter::passing(id).0);
    }
    Ok(())
}

impl SmiFilter {
    const VALID: u64 = 1 << 16;
    const LOCK: u64 = 1 << 17;

    /// Passes SMIs from `id`, locked.
    pub const fn passing(id: u16) -> Self {
        Self(id as u64 | Self::VALID | Self::LOCK)
    }

    /// The requester id an SMI passes from, where the register is valid.
    pub const fn passes(self) -> Option<u16> {
        if self.0 & Self::VALID != 0 {
            Some(self.0 as u16)
        } else {
            None
        }
    }

    pub const fn locked(self) -> bool {
        self.0 & Self::LOCK != 0
    }
}

/// `CONTROL` (MMIO 0018h).
pub mod control {
    pub const IOMMU_EN: u64 = 1 << 0;
    pub const EVENT_LOG_EN: u64 = 1 << 2;
    pub const EVENT_INT_EN: u64 = 1 << 3;
    /// Device table fetches snooped; reset 1.
    pub const COHERENT: u64 = 1 << 10;
    pub const CMD_BUF_EN: u64 = 1 << 12;
    pub const PPR_LOG_EN: u64 = 1 << 13;
    pub const PPR_INT_EN: u64 = 1 << 14;
    /// Upstream SMIs pass only from the ids the SMI filter registers name
    /// (§2.1.5.1).
    pub const SMIF_EN: u64 = 1 << 22;
    /// An SMI the filter refuses is written to the event log as an
    /// `IO_PAGE_FAULT` at its SMI address.
    pub const SMIF_LOG_EN: u64 = 1 << 24;
    /// The 128-bit remapping entry (Table 21), with `GAMEn` 000b.
    pub const GA_EN: u64 = 1 << 17;
    pub const GA_LOG_EN: u64 = 1 << 28;
    /// A remapping entry's 32-bit destination (§2.2.5.3); needs `GA_EN`.
    pub const XT_EN: u64 = 1 << 50;
    pub const GA_INT_EN: u64 = 1 << 29;
}

/// `STATUS` (MMIO 2020h).
pub mod status {
    /// Write 1 to clear.
    pub const EVENT_OVERFLOW: u32 = 1 << 0;
    /// Write 1 to clear.
    pub const EVENT_LOG_INT: u32 = 1 << 1;
    pub const EVENT_LOG_RUN: u32 = 1 << 3;
    pub const CMD_BUF_RUN: u32 = 1 << 4;
    pub const PPR_LOG_RUN: u32 = 1 << 7;
    pub const GA_LOG_RUN: u32 = 1 << 8;
}

/// The extended feature register (MMIO 0030h), as far as this driver asks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Features(pub u64);

impl Features {
    /// `IASup`: the unit takes `INVALIDATE_IOMMU_ALL`.
    pub const fn invalidate_all(self) -> bool {
        self.0 & 1 << 6 != 0
    }

    /// `XTSup`: a remapping entry can name an x2APIC destination.
    pub const fn x2apic(self) -> bool {
        self.0 & 1 << 2 != 0
    }

    /// `GASup`: the 128-bit remapping entry can be selected.
    pub const fn guest_apic(self) -> bool {
        self.0 & 1 << 7 != 0
    }

    /// `SmiFSup` 01b: the SMI filter exists, with this many registers
    /// (`SmiFRC`: 1, 2, 4, 8 or 16); `None` where SMIs always pass, or for a
    /// reserved encoding of either field.
    pub const fn smi_filters(self) -> Option<usize> {
        if (self.0 >> 16) & 0b11 != 0b01 {
            return None;
        }
        match (self.0 >> 18) & 0b111 {
            count @ 0..=4 => Some(1 << count),
            _ => None,
        }
    }

    /// `HATS`, the host table levels the unit walks: 4, 5 or 6, or `None`
    /// for the reserved encoding.
    pub const fn host_levels(self) -> Option<u8> {
        match (self.0 >> 10) & 0b11 {
            0b11 => None,
            n => Some(4 + n as u8),
        }
    }
}

/// `DEVICE_TABLE`'s value for a whole table at `base`: `Size` 1FFh, 2 MiB.
pub const fn device_table(base: Phys<12>) -> u64 {
    base.get() | ((DEVICE_IDS * ENTRY_BYTES / 4096 - 1) as u64)
}

/// `COMMAND_BASE`'s or `EVENT_BASE`'s value for a 4 KiB ring at `base`:
/// length 1000b, 256 entries.
pub const fn ring(base: Phys<12>) -> u64 {
    base.get() | 0b1000 << 56
}

/// The byte offset a head or tail register holds for entry `slot`, and back.
pub const fn offset(slot: usize) -> u64 {
    slot as u64 * 16
}

pub const fn slot(offset: u64) -> usize {
    ((offset >> 4) & 0x7FFF) as usize
}
