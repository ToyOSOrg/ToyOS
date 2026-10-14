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
    /// `INVALIDATE_INTERRUPT_TABLE` (Figure 48): everything the unit cached
    /// of this id's remapping table.
    InvalidateInterrupts(u16),
    /// `INVALIDATE_IOMMU_ALL` (Figure 51): every cache the unit holds.
    InvalidateAll,
}

impl Command {
    pub const fn words(self) -> [u64; 2] {
        match self {
            Self::CompletionWait { store, data } => [1 << CODE_SHIFT | store.get() | 1, data],
            Self::InvalidateDevice(id) => [2 << CODE_SHIFT | id as u64, 0],
            Self::InvalidateDomain(domain) => [3 << CODE_SHIFT | (domain as u64) << 32, 0x7_FFFF_FFFF_FFFF << 12 | 0b11],
            Self::InvalidateInterrupts(id) => [5 << CODE_SHIFT | id as u64, 0],
            Self::InvalidateAll => [8 << CODE_SHIFT, 0],
        }
    }
}

/// One event record, decoded as far as what it ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// A record a requester caused, which the unit has already refused or
    /// dropped: `IO_PAGE_FAULT` (Figure 56), `IOTLB_INV_TIMEOUT` (Figure 61),
    /// `INVALID_DEVICE_REQUEST` (Figure 62), and `INVALID_PPR_REQUEST` with
    /// `RX` clear (Figure 64). `write` is the access where the record states
    /// one; `address` is the record's, whatever it names.
    Requester { code: u8, device: u16, address: u64, write: Option<bool> },
    /// Every other record: an entry or command of this driver's the unit
    /// refused, or the unit's own hardware failing.
    Unit { code: u8 },
}

pub const fn event(words: [u64; 2]) -> Event {
    let device = words[0] as u16;
    let code = (words[0] >> CODE_SHIFT) as u8;
    let write = match code {
        // `I` [51]: an interrupt carries no read or write; else `RW` [53].
        0b0010 if words[0] & 1 << 51 != 0 => None,
        0b0010 => Some(words[0] & 1 << 53 != 0),
        // `RX` [48] set is a `COMPLETE_PPR_REQUEST` of this driver's failing.
        0b1001 if words[0] & 1 << 48 != 0 => return Event::Unit { code },
        // `WP` [53]: the write permission the peripheral asked for.
        0b1001 => Some(words[0] & 1 << 53 != 0),
        0b0111 | 0b1000 => None,
        _ => return Event::Unit { code },
    };
    Event::Requester { code, device, address: words[1], write }
}

/// Whether a record's address is an SMI's (§2.1.5.2, Figure 3 and Table 4):
/// bits 47:40 zero, `FDF8h` at 39:24, `MT` 0010b at 7 and 4:2, `DM` and
/// bits 1:0 zero. The SMI filter records a refused SMI as an
/// `IO_PAGE_FAULT` at that address (§2.1.5.1).
pub const fn smi(address: u64) -> bool {
    address & 0xFFFF_FFFF_FF00_00C3 == 0x0000_00FD_F800_0000 && (address >> 2) & 0b111 == 0b010
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
