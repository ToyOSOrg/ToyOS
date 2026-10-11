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
//! register's value. An event, a collection and an LPI are each a type made
//! from what the ITS, its tables and the distributor say there is of them.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod command;
pub mod lpi;

use toyos_phys::Phys;

/// Offsets in the ITS's control frame (§12.18, Table 12-33).
pub const GITS_CTLR: usize = 0x0000;
pub const GITS_TYPER: usize = 0x0008;
pub const GITS_CBASER: usize = 0x0080;
pub const GITS_CWRITER: usize = 0x0088;
pub const GITS_CREADR: usize = 0x0090;

/// `GITS_BASER<n>`, n = 0 to 7 (§12.19.1): one per table the ITS may ask
/// memory for.
pub const fn gits_baser(n: usize) -> Option<usize> {
    if n < 8 {
        Some(0x0100 + 8 * n)
    } else {
        None
    }
}

/// The translation frame is the second 64 KiB frame (§12.18), and
/// `GITS_TRANSLATER` is at 0x0040 of it (Table 12-34): a function's message
/// is one 32-bit write of its EventID there, and the bus says which device
/// wrote it.
pub const TRANSLATION_FRAME: u64 = 0x1_0000;
pub const GITS_TRANSLATER: u64 = 0x0040;

/// `GITS_CTLR.Enabled` [0] (§12.19.4).
pub const CTLR_ENABLED: u32 = 1 << 0;

/// What an ITS lacks that the driver needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lacks {
    /// `GITS_TYPER.Physical` clear: no physical LPIs.
    PhysicalLpis,
}

/// An ITS as its type register describes it (§12.19.13).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Its {
    /// `Devbits` [17:13] plus one: the widest DeviceID, and so how many
    /// entries a flat device table holds.
    pub device_bits: u8,
    /// `ID_bits` [12:8] plus one: the widest EventID.
    event_bits: u8,
    /// `ITT_entry_size` [7:4] plus one: bytes of one translation table entry.
    itt_entry_bytes: u8,
    /// `HCC` [31:24]: collections the ITS holds without a collection table.
    held: u8,
    /// `CCT` [2]: those count beside a collection table's.
    cumulative: bool,
    /// The width of an ICID: 16 bits where `CIL` [36] is clear, else
    /// `CIDbits` [35:32] plus one.
    collection_bits: u8,
    /// `PTA` [19]: a collection's target is its redistributor's address
    /// rather than its processor number.
    physical_targets: bool,
}

pub const fn probe(typer: u64) -> Result<Its, Lacks> {
    if typer & 1 == 0 {
        return Err(Lacks::PhysicalLpis);
    }
    Ok(Its {
        device_bits: (typer >> 13 & 0x1F) as u8 + 1,
        event_bits: (typer >> 8 & 0x1F) as u8 + 1,
        itt_entry_bytes: (typer >> 4 & 0xF) as u8 + 1,
        held: (typer >> 24) as u8,
        cumulative: typer & 1 << 2 != 0,
        collection_bits: if typer & 1 << 36 == 0 { 16 } else { (typer >> 32 & 0xF) as u8 + 1 },
        physical_targets: typer & 1 << 19 != 0,
    })
}

/// How many EventIDs one device's translation table holds, as `MAPD` says
/// it: made by [`Its::events`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EventBits(u8);

/// An EventID inside the translation table it was made for:
/// [`EventBits::event`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event(u32);

impl EventBits {
    /// `id` as an event of a device whose table holds this many.
    pub const fn event(self, id: u32) -> Option<Event> {
        if (id as u64) >> self.0 == 0 {
            Some(Event(id))
        } else {
            None
        }
    }
}

/// How many collections an ITS has: made by [`Its::collections`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Collections(u32);

/// A collection the ITS has, by its ICID: [`Collections::collection`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Collection(u16);

impl Collections {
    pub const fn collection(self, id: u16) -> Option<Collection> {
        if (id as u32) < self.0 {
            Some(Collection(id))
        } else {
            None
        }
    }
}

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

    /// The collections this ITS has once its collection table holds
    /// `in_table` entries, none where no `GITS_BASER<n>` backs one (§5.2.2,
    /// §5.3.1): the table's, those the ITS holds itself where it has no
    /// table or counts them beside it, and no more than an ICID numbers.
    pub const fn collections(&self, in_table: u64) -> Collections {
        let held = if in_table == 0 || self.cumulative { self.held as u64 } else { 0 };
        let numbered = 1u64 << self.collection_bits;
        let total = held.saturating_add(in_table);
        Collections(if total < numbered { total } else { numbered } as u32)
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

/// `GITS_BASER<n>` as read (§12.19.1): the table it backs and the page size
/// it holds — which an ITS may fix, so it is read back after
/// [`Backing::baser`] is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Backing {
    pub table: Table,
    pub page: PageSize,
    /// `Entry_Size` [52:48] plus one: the bytes of one entry.
    entry_bytes: u8,
}

pub const fn table(baser: u64) -> Backing {
    let table = match baser >> 56 & 0b111 {
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
    Backing { table, page, entry_bytes: (baser >> 48 & 0x1F) as u8 + 1 }
}

/// `InnerCache` `0b111`, read-allocate write-allocate write-back, with
/// `OuterCache` zero, as the inner; `Shareability` [11:10] `0b01`, inner
/// shareable. `GITS_BASER<n>` and `GITS_CBASER` hold them at the same bits.
const CACHED: u64 = 0b111 << 59 | 0b01 << 10;
const VALID: u64 = 1 << 63;

impl Backing {
    /// The entries a flat table of `pages` of its pages holds.
    pub const fn entries(self, pages: u16) -> u64 {
        pages as u64 * self.page.bytes() / self.entry_bytes as u64
    }

    /// The fewest pages that hold `entries`. `None` for none, or more than
    /// the 256 `Size` counts.
    pub const fn pages(self, entries: u64) -> Option<u16> {
        let Some(bytes) = entries.checked_mul(self.entry_bytes as u64) else {
            return None;
        };
        let pages = bytes.div_ceil(self.page.bytes());
        if pages >= 1 && pages <= 256 {
            Some(pages as u16)
        } else {
            None
        }
    }

    /// `GITS_BASER<n>` for a flat table of `pages` of its pages at `at`:
    /// `Valid` [63], `Indirect` [62] clear, `Physical_Address` [47:12],
    /// `Page_Size` [9:8], `Size` [7:0] the pages minus one. `None` for no
    /// page or more than the 256 the field counts.
    pub const fn baser(self, at: Phys<16>, pages: u16) -> Option<u64> {
        if pages == 0 || pages > 256 {
            return None;
        }
        let size = match self.page {
            PageSize::K4 => 0b00,
            PageSize::K16 => 0b01,
            PageSize::K64 => 0b10,
        };
        Some(VALID | CACHED | at.get() | size << 8 | (pages - 1) as u64)
    }
}

/// The ITS's command queue: `pages` 4 KiB pages of 32-byte commands
/// (§5.2.8). `GITS_CWRITER` and `GITS_CREADR` each hold a byte offset into
/// it, `Offset` [19:5], which is all of either that names a command.
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

    /// What `GITS_CWRITER` is written with once the command at its offset is
    /// in memory: the offset after, wrapping at the queue's end.
    pub const fn after(self, cwriter: u64) -> u64 {
        ((self.offset(cwriter) + 32) % self.bytes) as u64
    }

    /// The ITS has read every command: the two registers' offsets are equal.
    pub const fn is_empty(self, cwriter: u64, creadr: u64) -> bool {
        self.offset(cwriter) == self.offset(creadr)
    }

    /// One more command would make the writer's offset the reader's, which
    /// reads as empty: the queue holds one command fewer than its bytes do.
    pub const fn is_full(self, cwriter: u64, creadr: u64) -> bool {
        self.after(cwriter) == self.offset(creadr) as u64
    }
}
