//! Every word a GICv3 Interrupt Translation Service and its driver exchange,
//! as Arm IHI 0069 issue H.b lays it out: the ITS's registers and what its
//! type register says of it (this module), its commands ([`command`]), and
//! the two tables a redistributor reads an LPI's configuration and pending
//! state from ([`lpi`]).
//!
//! The device table and each device's interrupt translation table are the
//! ITS's own: their entry formats are IMPLEMENTATION DEFINED (§5.2.3,
//! §5.2.4), software gives the ITS zeroed memory of the size its registers
//! ask for and writes an entry only by a command. So no entry is encoded
//! here; the sizes are.
//!
//! Only physical LPIs through flat tables are spelled: no vPE, no indirect
//! table. What an ITS reports about itself is a device's word and is refused
//! by name where the driver could not act on it; nothing here panics on a
//! register's value.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod command;
pub mod lpi;

/// A physical address the ITS or a redistributor is given, aligned to
/// `1 << ALIGN` bytes and below 2^48: inside every address field written
/// here, whichever page size the table it names is read in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Phys<const ALIGN: u32>(u64);

impl<const ALIGN: u32> Phys<ALIGN> {
    pub const fn new(address: u64) -> Option<Self> {
        if address >> 48 == 0 && address & ((1 << ALIGN) - 1) == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Offsets in the ITS's control frame (§12.18).
pub const GITS_CTLR: usize = 0x0000;
pub const GITS_TYPER: usize = 0x0008;
pub const GITS_CBASER: usize = 0x0080;
pub const GITS_CWRITER: usize = 0x0088;
pub const GITS_CREADR: usize = 0x0090;

/// `GITS_BASER<n>`, n = 0 to 7: one per table the ITS may ask memory for.
pub const fn gits_baser(n: usize) -> Option<usize> {
    if n < 8 {
        Some(0x0100 + 8 * n)
    } else {
        None
    }
}

/// The translation frame is the second 64 KiB frame, and `GITS_TRANSLATER`
/// is at 0x0040 of it: a function's message is one 32-bit write of its
/// EventID there, and the bus says which device wrote it.
pub const TRANSLATION_FRAME: u64 = 0x1_0000;
pub const GITS_TRANSLATER: u64 = 0x0040;

/// `GITS_CTLR` (§12.19.4): `Enabled` [0]; `Quiescent` [31] reads set once a
/// disabled ITS has nothing in progress.
pub const CTLR_ENABLED: u32 = 1 << 0;
pub const CTLR_QUIESCENT: u32 = 1 << 31;

/// What an ITS lacks that the driver needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lacks {
    /// `GITS_TYPER.Physical` clear: no physical LPIs.
    PhysicalLpis,
}

/// An ITS as its type register describes it (§12.19.13).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Its {
    /// `ID_bits` [12:8] plus one: the widest EventID.
    pub event_bits: u8,
    /// `Devbits` [17:13] plus one: the widest DeviceID.
    pub device_bits: u8,
    /// `ITT_entry_size` [7:4] plus one: bytes of one translation table entry.
    pub itt_entry_bytes: u8,
    /// `HCC` [31:24]: collections the ITS holds without a collection table.
    pub collections_held: u8,
    /// `PTA` [19]: a collection's target is its redistributor's address
    /// rather than its processor number.
    physical_targets: bool,
}

pub const fn probe(typer: u64) -> Result<Its, Lacks> {
    if typer & 1 == 0 {
        return Err(Lacks::PhysicalLpis);
    }
    Ok(Its {
        event_bits: (typer >> 8 & 0x1F) as u8 + 1,
        device_bits: (typer >> 13 & 0x1F) as u8 + 1,
        itt_entry_bytes: (typer >> 4 & 0xF) as u8 + 1,
        collections_held: (typer >> 24) as u8,
        physical_targets: typer & 1 << 19 != 0,
    })
}

/// How many EventIDs one device's translation table holds, as `MAPD` says
/// it: made by [`Its::events`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EventBits(u8);

/// The redistributor a collection's interrupts go to, in the form this ITS
/// takes it (`RDbase`, §5.3.1): made by [`Its::target`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target(u64);

impl Its {
    /// A translation table of `1 << bits` events. `None` for no bit, or more
    /// than the ITS's EventIDs have.
    pub const fn events(&self, bits: u8) -> Option<EventBits> {
        if bits >= 1 && bits <= self.event_bits {
            Some(EventBits(bits))
        } else {
            None
        }
    }

    /// The zeroed bytes `MAPD` is given for that table, at a 256-byte
    /// aligned address.
    pub const fn itt_bytes(&self, events: EventBits) -> u64 {
        (self.itt_entry_bytes as u64) << events.0
    }

    /// The redistributor at `frame` whose `GICR_TYPER.Processor_Number` is
    /// `number`, as this ITS names it.
    pub const fn target(&self, number: u16, frame: Phys<16>) -> Target {
        Target(if self.physical_targets { frame.get() >> 16 } else { number as u64 })
    }
}

/// What a `GITS_BASER<n>` asks memory for (`Type`, bits [58:56]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    /// `0b000`: the register backs no table.
    Unimplemented,
    /// `0b001`: one entry for every DeviceID.
    Devices,
    /// `0b100`: one entry for every collection past those the ITS holds.
    Collections,
    /// vPEs (`0b010`), or a reserved type.
    Other(u8),
}

/// The page a table is read in (`Page_Size`, bits [9:8]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageSize {
    K4,
    K16,
    K64,
}

impl PageSize {
    pub const fn bytes(self) -> u64 {
        match self {
            Self::K4 => 4 << 10,
            Self::K16 => 16 << 10,
            Self::K64 => 64 << 10,
        }
    }
}

/// `GITS_BASER<n>` as read (§12.19.1): the table it backs, the bytes of one
/// entry (`Entry_Size` [52:48] plus one) and the page size it holds — which
/// an ITS may fix, so it is read back after [`baser`] is written.
pub const fn table(baser: u64) -> (Table, u8, PageSize) {
    let kind = match baser >> 56 & 0b111 {
        0b000 => Table::Unimplemented,
        0b001 => Table::Devices,
        0b100 => Table::Collections,
        other => Table::Other(other as u8),
    };
    let page = match baser >> 8 & 0b11 {
        0b00 => PageSize::K4,
        0b01 => PageSize::K16,
        // `0b11` is reserved and treated as `0b10`.
        _ => PageSize::K64,
    };
    (kind, (baser >> 48 & 0x1F) as u8 + 1, page)
}

/// `InnerCache` `0b111`, read-allocate write-allocate write-back, with
/// `OuterCache` zero, as the inner; `Shareability` [11:10] `0b01`, inner
/// shareable. `GITS_BASER<n>` and `GITS_CBASER` hold them at the same bits.
const CACHED: u64 = 0b111 << 59 | 0b01 << 10;
const VALID: u64 = 1 << 63;

/// `GITS_BASER<n>` for a flat table of `pages` pages of `page` at `at`:
/// `Valid` [63], `Indirect` [62] clear, `Physical_Address` [47:12],
/// `Page_Size` [9:8], `Size` [7:0] the pages minus one. `None` for no page or
/// more than the 256 the field counts.
pub const fn baser(at: Phys<16>, page: PageSize, pages: u16) -> Option<u64> {
    if pages == 0 || pages > 256 {
        return None;
    }
    let size = match page {
        PageSize::K4 => 0b00,
        PageSize::K16 => 0b01,
        PageSize::K64 => 0b10,
    };
    Some(VALID | CACHED | at.get() | size << 8 | (pages - 1) as u64)
}

/// The ITS's command queue: `pages` 4 KiB pages of 32-byte commands
/// (§5.2.8). `GITS_CWRITER` and `GITS_CREADR` each hold a byte offset into
/// it, `Offset` [19:5].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommandQueue {
    bytes: u32,
}

/// `GITS_CREADR.Stalled` [0]: the ITS stopped on a command it refused.
pub const CREADR_STALLED: u64 = 1 << 0;

impl CommandQueue {
    /// `None` for no page or more than the 256 `GITS_CBASER.Size` counts.
    pub const fn new(pages: u16) -> Option<Self> {
        if pages == 0 || pages > 256 {
            None
        } else {
            Some(Self { bytes: pages as u32 * 4096 })
        }
    }

    /// `GITS_CBASER` (§12.19.2) for the queue at `at`: `Valid` [63],
    /// `Physical_Address` [51:12] with its low four bits zero, `Size` [7:0]
    /// the pages minus one.
    pub const fn cbaser(self, at: Phys<16>) -> u64 {
        VALID | CACHED | at.get() | (self.bytes / 4096 - 1) as u64
    }

    /// The byte offset an offset register names: its `Offset` field alone,
    /// and inside the queue whatever the ITS wrote there.
    pub const fn offset(self, register: u64) -> u32 {
        (register & 0xF_FFE0) as u32 % self.bytes
    }

    /// The offset after `offset`, wrapping at the queue's end.
    pub const fn after(self, offset: u32) -> u32 {
        (self.offset(offset as u64) + 32) % self.bytes
    }

    /// The ITS has read every command: the two offsets are equal.
    pub const fn is_empty(self, writer: u32, reader: u32) -> bool {
        writer == reader
    }

    /// One more command would make the writer equal the reader, which reads
    /// as empty: the queue holds one command fewer than its bytes do.
    pub const fn is_full(self, writer: u32, reader: u32) -> bool {
        self.after(writer) == reader
    }
}
