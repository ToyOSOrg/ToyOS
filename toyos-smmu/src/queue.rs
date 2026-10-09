//! The two queues: their index arithmetic (§3.5.1), the commands the driver
//! writes (chapter 4) and the event records it reads (§7.3).

use crate::Asid;

/// The indexes of a circular queue of `1 << log2size` entries. An index
/// register holds the entry's index in its low `log2size` bits and a wrap
/// flag in the next one up, which toggles each time the index passes the top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Indexes {
    pub(crate) log2size: u8,
}

impl Indexes {
    /// The index and its wrap flag, which is all of an index register that
    /// names a position.
    const fn position(self, register: u32) -> u32 {
        register & ((2 << self.log2size) - 1)
    }

    const fn slot(self, register: u32) -> usize {
        (register & ((1 << self.log2size) - 1)) as usize
    }

    /// The indexes and their wrap flags are equal.
    const fn is_empty(self, prod: u32, cons: u32) -> bool {
        self.position(prod) == self.position(cons)
    }

    /// The position after `register`'s, wrap flag toggled where it passes
    /// the top.
    const fn after(self, register: u32) -> u32 {
        self.position(self.position(register) + 1)
    }
}

/// The command queue's indexes, as the driver that produces into it reads
/// `SMMU_CMDQ_PROD` and `SMMU_CMDQ_CONS`: made by
/// [`Unit::command_queue`](crate::unit::Unit::command_queue).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Commands(pub(crate) Indexes);

impl Commands {
    /// The entry the next command is written to.
    pub const fn slot(self, prod: u32) -> usize {
        self.0.slot(prod)
    }

    /// No command can be written: the indexes are equal and the wrap flags
    /// differ.
    pub const fn is_full(self, prod: u32, cons: u32) -> bool {
        self.0.position(prod) ^ self.0.position(cons) == 1 << self.0.log2size
    }

    /// The unit has consumed every command written.
    pub const fn is_empty(self, prod: u32, cons: u32) -> bool {
        self.0.is_empty(prod, cons)
    }

    /// What `SMMU_CMDQ_PROD` is written with once the command at `prod` is
    /// in memory: the position after it, index and wrap flag at once.
    pub const fn after(self, prod: u32) -> u32 {
        self.0.after(prod)
    }
}

/// `SMMU_EVENTQ_PROD.OVFLG` and `SMMU_EVENTQ_CONS.OVACKFLG`, bit [31] of each.
const OVERFLOW: u32 = 1 << 31;

/// The event queue's indexes, as the driver that consumes from it reads
/// `SMMU_EVENTQ_PROD` and `SMMU_EVENTQ_CONS`: made by
/// [`Unit::event_queue`](crate::unit::Unit::event_queue).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events(pub(crate) Indexes);

impl Events {
    /// The entry the next record is read from.
    pub const fn slot(self, cons: u32) -> usize {
        self.0.slot(cons)
    }

    /// No record to read.
    pub const fn is_empty(self, prod: u32, cons: u32) -> bool {
        self.0.is_empty(prod, cons)
    }

    /// Records were dropped on a full queue and that is not yet
    /// acknowledged: `OVFLG` differs from `OVACKFLG` (§7.4).
    pub const fn overflowed(self, prod: u32, cons: u32) -> bool {
        (prod ^ cons) & OVERFLOW != 0
    }

    /// What `SMMU_EVENTQ_CONS` is written with once the record at `cons` is
    /// read: the position after it, and `OVACKFLG` equal to the `OVFLG`
    /// `prod` was read with. That acknowledges an overflow seen in `prod`,
    /// and no value this answers raises one the unit did not report.
    pub const fn after(self, cons: u32, prod: u32) -> u32 {
        self.0.after(cons) | prod & OVERFLOW
    }
}

/// How a `CMD_SYNC` says it is done (§4.7.3, `CS`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// `SIG_NONE`: `SMMU_CMDQ_CONS` passing it is the only sign.
    None,
    /// `SIG_IRQ` with `MSIAddress` zero: the unit's wired sync interrupt.
    Irq,
}

/// One 16-byte command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// `CMD_CFGI_STE` (§4.3.1), `Leaf` set: forget one stream's entry and
    /// every context descriptor reached through it.
    ForgetStream(u32),
    /// `CMD_CFGI_ALL` (§4.3.9): forget every stream's configuration.
    ForgetAll,
    /// `CMD_TLBI_NH_ASID` (§4.4.2.2), VMID 0: drop every non-global
    /// translation cached under one ASID.
    InvalidateAsid(Asid),
    /// `CMD_TLBI_NSNH_ALL` (§4.4.4.1): drop every Non-secure translation.
    InvalidateAll,
    /// `CMD_SYNC` (§4.7.3): consumed only once every command before it has
    /// taken effect.
    Sync(Signal),
}

impl Command {
    /// The command's two doublewords: the opcode in bits [7:0] of the first.
    pub const fn words(self) -> [u64; 2] {
        match self {
            // StreamID [63:32]; Leaf [64].
            Self::ForgetStream(stream) => [0x03 | (stream as u64) << 32, 1],
            // `CMD_CFGI_STE_RANGE` with Range [68:64] of 31.
            Self::ForgetAll => [0x04, 31],
            // ASID [63:48], VMID [47:32].
            Self::InvalidateAsid(asid) => [0x11 | (asid.get() as u64) << 48, 0],
            Self::InvalidateAll => [0x30, 0],
            // CS [13:12].
            Self::Sync(signal) => {
                let cs = match signal {
                    Signal::None => 0b00,
                    Signal::Irq => 0b01,
                };
                [0x46 | cs << 12, 0]
            }
        }
    }
}

/// An event's number, bits [7:0] of its record (§7.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Code {
    /// `F_UUT` (0x01).
    UnsupportedTransaction,
    /// `C_BAD_STREAMID` (0x02): past the stream table.
    BadStream,
    /// `F_STE_FETCH` (0x03).
    EntryFetch,
    /// `C_BAD_STE` (0x04): an invalid or ILLEGAL stream table entry.
    BadEntry,
    /// `F_BAD_ATS_TREQ` (0x05).
    AtsRequest,
    /// `F_STREAM_DISABLED` (0x06).
    StreamDisabled,
    /// `F_TRANSL_FORBIDDEN` (0x07).
    TranslatedForbidden,
    /// `C_BAD_SUBSTREAMID` (0x08).
    BadSubstream,
    /// `F_CD_FETCH` (0x09).
    ContextFetch,
    /// `C_BAD_CD` (0x0A): an invalid or ILLEGAL context descriptor.
    BadContext,
    /// `F_WALK_EABT` (0x0B).
    WalkAbort,
    /// `F_TRANSLATION` (0x10): no valid descriptor for the address.
    Translation,
    /// `F_ADDR_SIZE` (0x11).
    AddressSize,
    /// `F_ACCESS` (0x12).
    AccessFlag,
    /// `F_PERMISSION` (0x13).
    Permission,
    /// `F_TLB_CONFLICT` (0x20).
    TlbConflict,
    /// `F_CFG_CONFLICT` (0x21).
    ConfigurationConflict,
    /// `E_PAGE_REQUEST` (0x24).
    PageRequest,
    /// `F_VMS_FETCH` (0x25).
    VmsFetch,
    /// A number this issue gives no record or the unit defines for itself.
    Other(u8),
}

/// What a device tried, where the record says: the four translation-related
/// faults carry it (§7.3.13 to §7.3.16).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attempt {
    /// `InputAddr`, bits [191:128]: the device address.
    pub address: u64,
    /// `RnW`, bit [99], clear.
    pub write: bool,
}

/// One 32-byte event record, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    /// `StreamID`, bits [63:32].
    pub stream: u32,
    pub code: Code,
    pub attempt: Option<Attempt>,
}

pub const fn event(record: [u64; 4]) -> Event {
    let code = match record[0] as u8 {
        0x01 => Code::UnsupportedTransaction,
        0x02 => Code::BadStream,
        0x03 => Code::EntryFetch,
        0x04 => Code::BadEntry,
        0x05 => Code::AtsRequest,
        0x06 => Code::StreamDisabled,
        0x07 => Code::TranslatedForbidden,
        0x08 => Code::BadSubstream,
        0x09 => Code::ContextFetch,
        0x0A => Code::BadContext,
        0x0B => Code::WalkAbort,
        0x10 => Code::Translation,
        0x11 => Code::AddressSize,
        0x12 => Code::AccessFlag,
        0x13 => Code::Permission,
        0x20 => Code::TlbConflict,
        0x21 => Code::ConfigurationConflict,
        0x24 => Code::PageRequest,
        0x25 => Code::VmsFetch,
        other => Code::Other(other),
    };
    let attempt = match code {
        Code::Translation | Code::AddressSize | Code::AccessFlag | Code::Permission => {
            Some(Attempt { address: record[2], write: record[1] & 1 << 35 == 0 })
        }
        _ => None,
    };
    Event { stream: (record[0] >> 32) as u32, code, attempt }
}
