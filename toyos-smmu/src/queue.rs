//! The two queues: their index arithmetic (§3.5.1), the commands the driver
//! writes (chapter 4) and the event records it reads (§7.3).

use crate::Asid;

/// A circular queue of `1 << log2size` entries. An index register holds the
/// entry's index in its low `log2size` bits and a wrap flag in the next one
/// up, which toggles each time the index passes the top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Queue {
    log2size: u8,
}

impl Queue {
    /// `None` past the 19 bits an index register's `WR` and `RD` fields with
    /// their wrap flag can hold (`IDR1.CMDQS`, `EVENTQS`).
    pub const fn new(log2size: u8) -> Option<Self> {
        if log2size <= 19 {
            Some(Self { log2size })
        } else {
            None
        }
    }

    /// The index and its wrap flag, which is all of an index register that
    /// names a position.
    const fn position(self, index: u32) -> u32 {
        index & ((2 << self.log2size) - 1)
    }

    /// The entry an index register names.
    pub const fn slot(self, index: u32) -> usize {
        (index & ((1 << self.log2size) - 1)) as usize
    }

    /// Nothing to consume: the indexes and their wrap flags are equal.
    pub const fn is_empty(self, prod: u32, cons: u32) -> bool {
        self.position(prod) == self.position(cons)
    }

    /// Nothing can be produced: the indexes are equal and the wrap flags differ.
    pub const fn is_full(self, prod: u32, cons: u32) -> bool {
        self.position(prod) ^ self.position(cons) == 1 << self.log2size
    }

    /// The position after `index`, wrap flag toggled where it passes the
    /// top: what the owner of an index writes back, index and flag at once.
    pub const fn after(self, index: u32) -> u32 {
        self.position(self.position(index) + 1)
    }
}

/// How a `CMD_SYNC` says it is done (§4.7.3, `CS`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// `SIG_NONE`: `SMMU_CMDQ_CONS` passing it is the only sign.
    None,
    /// `SIG_IRQ` with `MSIAddress` zero: the unit's wired sync interrupt.
    Irq,
    /// `SIG_SEV`: an event to the CPUs, where `IDR0.SEV` is set.
    Sev,
}

/// One 16-byte command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// `CMD_CFGI_STE` (§4.3.1), `Leaf` set: forget one stream's entry and
    /// every context descriptor reached through it.
    ForgetStream(u32),
    /// `CMD_CFGI_CD` (§4.3.3), SubstreamID 0 and `Leaf` set: forget the one
    /// context descriptor a stream's entry names.
    ForgetContext(u32),
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
            // StreamID [63:32], SubstreamID [31:12] zero; Leaf [64].
            Self::ForgetContext(stream) => [0x05 | (stream as u64) << 32, 1],
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
                    Signal::Sev => 0b10,
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
