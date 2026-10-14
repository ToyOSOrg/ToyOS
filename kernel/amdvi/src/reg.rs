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
    pub const GA_LOG_EN: u64 = 1 << 28;
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
