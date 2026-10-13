//! The command buffer's commands (§2.4) and the event log's records (§2.5):
//! 128 bits each, a four-bit code in bits 63:60 of the first doubleword.

use toyos_phys::Phys;

/// Entries a 4 KiB command buffer or event log holds, its length field's
/// minimum (MMIO 0008h and 0010h, `1000b`).
pub const ENTRIES: usize = 256;

const CODE_SHIFT: u32 = 60;

/// A command this driver sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// `COMPLETION_WAIT` (Figure 44) with `s`: once every earlier command has
    /// completed, the unit stores `data` at `store`.
    CompletionWait { store: Phys<3>, data: u64 },
    /// `INVALIDATE_DEVTAB_ENTRY` (Figure 45): the unit rereads this id's entry.
    InvalidateDevice(u16),
    /// `INVALIDATE_IOMMU_PAGES` (Figure 46) over every address of one domain,
    /// directories with them: `S` and `PDE` set, address 7_FFFF_FFFF_FFFFh.
    InvalidateDomain(u16),
    /// `INVALIDATE_IOMMU_ALL` (Figure 51): every cache the unit holds.
    InvalidateAll,
}

impl Command {
    pub const fn words(self) -> [u64; 2] {
        match self {
            Self::CompletionWait { store, data } => [1 << CODE_SHIFT | store.get() | 1, data],
            Self::InvalidateDevice(id) => [2 << CODE_SHIFT | id as u64, 0],
            Self::InvalidateDomain(domain) => [3 << CODE_SHIFT | (domain as u64) << 32, 0x7_FFFF_FFFF_FFFF << 12 | 0b11],
            Self::InvalidateAll => [8 << CODE_SHIFT, 0],
        }
    }
}

/// One event record, decoded as far as what it ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// `IO_PAGE_FAULT` (Figure 56, Table 57): a requester's access the unit
    /// refused, a memory one or an interrupt.
    PageFault { device: u16, domain: u16, address: u64, write: bool, interrupt: bool },
    /// `INVALID_DEVICE_REQUEST` (Figure 62): a requester asked for something
    /// its entry forbids outright.
    DeviceRequest { device: u16, address: u64 },
    /// Every other code: a table or command of this driver's the unit
    /// refused, or the unit's own hardware failing.
    Other { code: u8 },
}

pub const fn event(words: [u64; 2]) -> Event {
    let device = words[0] as u16;
    match (words[0] >> CODE_SHIFT) as u8 {
        0b0010 => Event::PageFault {
            device,
            domain: (words[0] >> 32) as u16,
            address: words[1],
            write: words[0] & 1 << 53 != 0,
            interrupt: words[0] & 1 << 51 != 0,
        },
        0b1000 => Event::DeviceRequest { device, address: words[1] },
        code => Event::Other { code },
    }
}

/// The name 48882 gives an event code, for a record's line.
pub const fn name(code: u8) -> &'static str {
    match code {
        0b0001 => "ILLEGAL_DEV_TABLE_ENTRY",
        0b0010 => "IO_PAGE_FAULT",
        0b0011 => "DEV_TAB_HARDWARE_ERROR",
        0b0100 => "PAGE_TAB_HARDWARE_ERROR",
        0b0101 => "ILLEGAL_COMMAND_ERROR",
        0b0110 => "COMMAND_HARDWARE_ERROR",
        0b0111 => "IOTLB_INV_TIMEOUT",
        0b1000 => "INVALID_DEVICE_REQUEST",
        0b1001 => "INVALID_PPR_REQUEST",
        _ => "reserved",
    }
}
