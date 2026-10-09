//! The register map (§6.2) and the fields of the registers the driver reads
//! and writes (§6.3), and [`probe`]: what a unit's identification registers
//! let this crate's one configuration be built on.

use crate::{Asid, Phys};

/// Offsets in register page 0 (§6.2.1).
pub const IDR0: usize = 0x000;
pub const IDR1: usize = 0x004;
pub const IDR5: usize = 0x014;
pub const CR0: usize = 0x020;
pub const CR0ACK: usize = 0x024;
pub const CR1: usize = 0x028;
pub const CR2: usize = 0x02C;
pub const GBPA: usize = 0x044;
pub const IRQ_CTRL: usize = 0x050;
pub const IRQ_CTRLACK: usize = 0x054;
pub const GERROR: usize = 0x060;
pub const GERRORN: usize = 0x064;
pub const STRTAB_BASE: usize = 0x080;
pub const STRTAB_BASE_CFG: usize = 0x088;
pub const CMDQ_BASE: usize = 0x090;
pub const CMDQ_PROD: usize = 0x098;
pub const CMDQ_CONS: usize = 0x09C;
pub const EVENTQ_BASE: usize = 0x0A0;
/// The event queue's indexes are in register page 1, 64 KiB above page 0
/// (§6.1, §6.2.2): `SMMU_EVENTQ_PROD` and `SMMU_EVENTQ_CONS` at 0xA8 and 0xAC of it.
pub const EVENTQ_PROD: usize = 0x1_00A8;
pub const EVENTQ_CONS: usize = 0x1_00AC;

/// `SMMU_CR0` (§6.3.9), and `SMMU_CR0ACK`, which reads each bit back once
/// the unit has acted on it.
pub const CR0_SMMUEN: u32 = 1 << 0;
pub const CR0_EVENTQEN: u32 = 1 << 2;
pub const CR0_CMDQEN: u32 = 1 << 3;

/// `SMMU_CR1` (§6.3.11): the unit reads its stream table and its queues as
/// inner-shareable write-back memory — `TABLE_SH` [11:10], `TABLE_OC` [9:8],
/// `TABLE_IC` [7:6], `QUEUE_SH` [5:4], `QUEUE_OC` [3:2], `QUEUE_IC` [1:0],
/// each shareability `0b11` and each cacheability `0b01`.
pub const CR1_WRITE_BACK: u32 = 0b11 << 10 | 0b01 << 8 | 0b01 << 6 | 0b11 << 4 | 0b01 << 2 | 0b01;

/// `SMMU_CR2` (§6.3.12): `RECINVSID` [1], so a transaction under a StreamID
/// past the stream table is recorded as well as aborted, and `PTM` [2], so
/// no CPU's broadcast TLB invalidation reaches the unit's entries: its ASIDs
/// are its own. `E2H` [0] stays clear.
pub const CR2_RECORD_PRIVATE: u32 = 1 << 1 | 1 << 2;

/// `SMMU_GBPA` (§6.3.15): what happens to every transaction while `SMMUEN`
/// is clear. `ABORT` [20] aborts them; a write takes effect by carrying
/// `Update` [31], which reads back set until the unit has taken it (§6.3.15.1).
pub const GBPA_ABORT: u32 = 1 << 20;
pub const GBPA_UPDATE: u32 = 1 << 31;

/// `SMMU_IRQ_CTRL` (§6.3.17), and `SMMU_IRQ_CTRLACK` as `SMMU_CR0ACK`.
pub const IRQ_GERROR: u32 = 1 << 0;
pub const IRQ_EVENTQ: u32 = 1 << 2;

/// `SMMU_GERROR` (§6.3.19): an error is active while its bit differs from
/// `SMMU_GERRORN`'s, and writing `GERRORN` equal acknowledges it (§7.5).
pub const fn active_errors(gerror: u32, gerrorn: u32) -> u32 {
    gerror ^ gerrorn
}

/// `SMMU_GERROR.CMDQ_ERR`: the command queue stopped on [`CommandError`].
pub const GERROR_CMDQ: u32 = 1 << 0;
/// `SMMU_GERROR.EVENTQ_ABT_ERR`: a write to the event queue aborted, and
/// records were lost.
pub const GERROR_EVENTQ_ABORT: u32 = 1 << 2;
/// `SMMU_GERROR.SFM_ERR`: the unit entered Service Failure Mode.
pub const GERROR_SERVICE_FAILURE: u32 = 1 << 8;

/// `SMMU_CMDQ_CONS.ERR` [30:24] (§6.3.28, §7.1): why the command at `RD`
/// was not consumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandError {
    None,
    /// `CERROR_ILL`: a reserved opcode or an invalid parameter.
    Illegal,
    /// `CERROR_ABT`: reading the command aborted.
    Abort,
    /// `CERROR_ATC_INV_SYNC`.
    AtcInvalidation,
    Reserved(u8),
}

pub const fn command_error(cons: u32) -> CommandError {
    match (cons >> 24 & 0x7F) as u8 {
        0 => CommandError::None,
        1 => CommandError::Illegal,
        2 => CommandError::Abort,
        3 => CommandError::AtcInvalidation,
        other => CommandError::Reserved(other),
    }
}

/// `SMMU_EVENTQ_PROD.OVFLG` and `SMMU_EVENTQ_CONS.OVACKFLG` [31] (§7.4):
/// records were dropped on a full queue while the two differ, and the unit
/// reports no further overflow until the consumer writes its flag equal.
pub const EVENTQ_OVERFLOW: u32 = 1 << 31;

/// What a unit lacks that this crate's one configuration needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lacks {
    /// `IDR0.S1P` clear: no stage 1 translation.
    Stage1,
    /// `IDR0.TTF[1]` clear: no VMSAv8-64 tables.
    Aarch64Tables,
    /// `IDR0.TTENDIAN` is big-endian only.
    LittleEndianTables,
    /// `IDR0.STALL_MODEL` is `0b10`, under which every fault stalls its
    /// transaction until software answers it; or a reserved value.
    Termination(u8),
    /// `IDR5.GRAN4K` clear.
    Granule4K,
    /// `IDR1.TABLES_PRESET` or `IDR1.QUEUES_PRESET`: the unit's table or
    /// queues sit at an address of its own choosing.
    FixedStructures,
    /// `IDR1.SIDSIZE`, `CMDQS` or `EVENTQS` holds a reserved value.
    Size(u8),
}

/// A unit the one configuration can be built on, and its sizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unit {
    /// `IDR1.SIDSIZE`: how many bits of StreamID the unit takes, at most 32.
    pub stream_bits: u8,
    /// `IDR1.CMDQS` and `EVENTQS`: the largest each queue may be, as log2 of
    /// its entries.
    pub command_queue_log2: u8,
    pub event_queue_log2: u8,
    /// `IDR5.OAS` in bits: no address the unit is given may reach past it.
    pub output_bits: u8,
    /// `IDR0.COHACC`, which the IORT's SMMUv3 node may override.
    pub coherent: bool,
    asid16: bool,
    /// `IDR0.STALL_MODEL` is `0b00`: stalling is there to be disabled.
    stall: bool,
}

/// §6.3.1, §6.3.2 and §6.3.6: the three identification registers, judged.
pub const fn probe(idr0: u32, idr1: u32, idr5: u32) -> Result<Unit, Lacks> {
    if idr0 & 1 << 1 == 0 {
        return Err(Lacks::Stage1);
    }
    if idr0 & 1 << 3 == 0 {
        return Err(Lacks::Aarch64Tables);
    }
    // `TTENDIAN` [22:21]: `0b00` mixed and `0b10` little-endian can be set to little-endian.
    if idr0 >> 21 & 0b11 == 0b11 {
        return Err(Lacks::LittleEndianTables);
    }
    // `STALL_MODEL` [25:24]: `0b00` stall and terminate, `0b01` terminate only.
    let stall = match idr0 >> 24 & 0b11 {
        0b00 => true,
        0b01 => false,
        other => return Err(Lacks::Termination(other as u8)),
    };
    if idr5 & 1 << 4 == 0 {
        return Err(Lacks::Granule4K);
    }
    if idr1 & (1 << 30 | 1 << 29) != 0 {
        return Err(Lacks::FixedStructures);
    }
    let stream_bits = (idr1 & 0x3F) as u8;
    let command_queue_log2 = (idr1 >> 21 & 0x1F) as u8;
    let event_queue_log2 = (idr1 >> 16 & 0x1F) as u8;
    if stream_bits > 32 {
        return Err(Lacks::Size(stream_bits));
    }
    if command_queue_log2 > 19 {
        return Err(Lacks::Size(command_queue_log2));
    }
    if event_queue_log2 > 19 {
        return Err(Lacks::Size(event_queue_log2));
    }
    Ok(Unit {
        stream_bits,
        command_queue_log2,
        event_queue_log2,
        output_bits: [32, 36, 40, 42, 44, 48, 52, 56][(idr5 & 0b111) as usize],
        coherent: idr0 & 1 << 4 != 0,
        asid16: idr0 & 1 << 12 != 0,
        stall,
    })
}

impl Unit {
    /// `tag` as an ASID of this unit: eight bits where `IDR0.ASID16` is
    /// clear, under which a wider one makes its context descriptor ILLEGAL.
    pub const fn asid(&self, tag: u16) -> Option<Asid> {
        if self.asid16 || tag <= 0xFF {
            Some(Asid(tag))
        } else {
            None
        }
    }

    /// Whether `STE.S1STALLD` is set: it must be where stalling is
    /// configurable and is ILLEGAL where it is not (§5.5).
    pub(crate) const fn stall_disable(&self) -> bool {
        self.stall
    }

    /// `CD.IPS`, the widest of `TCR_ELx.PS`'s sizes inside the unit's output
    /// size and the 48 bits these tables' descriptors carry.
    pub(crate) const fn ips(&self) -> u64 {
        match self.output_bits {
            32 => 0b000,
            36 => 0b001,
            40 => 0b010,
            42 => 0b011,
            44 => 0b100,
            _ => 0b101,
        }
    }

    /// `SMMU_STRTAB_BASE` and `SMMU_STRTAB_BASE_CFG` (§6.3.24, §6.3.25) for
    /// a linear table of `1 << log2size` entries at `table`: `RA` [62] and
    /// the address, then `FMT` [17:16] zero and `LOG2SIZE` [5:0]. `None`
    /// where the unit takes fewer StreamID bits, or the table is not aligned
    /// to its own size as the unit reads its address.
    pub const fn stream_table(&self, table: Phys<6>, log2size: u8) -> Option<(u64, u32)> {
        if log2size > self.stream_bits || table.get() & ((64 << log2size) - 1) != 0 {
            return None;
        }
        Some((1 << 62 | table.get(), log2size as u32))
    }

    /// `SMMU_CMDQ_BASE` (§6.3.26) for a queue of `1 << log2size` 16-byte
    /// commands at `queue`: `RA` [62], the address, `LOG2SIZE` [4:0]. `None`
    /// past the unit's `CMDQS`, or where the queue is not aligned to the
    /// larger of its size and 32 bytes.
    pub const fn command_queue(&self, queue: Phys<5>, log2size: u8) -> Option<u64> {
        if log2size > self.command_queue_log2 || queue.get() & ((16 << log2size) - 1) != 0 {
            return None;
        }
        Some(1 << 62 | queue.get() | log2size as u64)
    }

    /// `SMMU_EVENTQ_BASE` (§6.3.29) for a queue of `1 << log2size` 32-byte
    /// records: `WA` [62], the address, `LOG2SIZE` [4:0].
    pub const fn event_queue(&self, queue: Phys<5>, log2size: u8) -> Option<u64> {
        if log2size > self.event_queue_log2 || queue.get() & ((32 << log2size) - 1) != 0 {
            return None;
        }
        Some(1 << 62 | queue.get() | log2size as u64)
    }
}
