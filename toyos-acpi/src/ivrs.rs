//! The IVRS (signature `IVRS`): AMD's I/O Virtualization Reporting Structure,
//! AMD I/O Virtualization Technology (IOMMU) Specification 48882 §5.2 — which
//! AMD-Vi units the machine has, where each one's registers are, which
//! requesters each one serves, and which memory a requester must keep reaching.
//!
//! [`ivrs`] answers a table whose every block it has walked: a block the table
//! cannot hold or shorter than its type's header, an IVMD whose range runs
//! backwards or past the address space, more IVHDs than it bounds,
//! and two IVHDs of one type naming one unit are its refusals. A unit's device
//! entries are judged by the [`Ivrs::devices`] walk that reads them, so a
//! malformed list refuses that walk and leaves every unit's header readable.
//!
//! Firmware describes a unit once per IVHD type it publishes for it — 10h,
//! 11h, 40h, each a superset of the one before — and [`Ivrs::blocks`] answers
//! each unit, keyed by its segment and requester id, once at the highest of
//! them. A block of any other type is answered by its type alone.

use crate::{find_table, Phys, Table, TableError, SDT_HEADER_LEN, SDT_REVISION};

/// The IVinfo word at 36, eight reserved bytes, and the first block at 48.
const IVINFO: usize = SDT_HEADER_LEN;
const FIRST_BLOCK: usize = SDT_HEADER_LEN + 12;

/// Every block opens with its type (0), flags (1), length (2..4) and a
/// requester id (4..6).
const BLOCK_HEADER: usize = 6;
/// IVHD 10h's header: capability offset (6), register base (8..16), segment
/// (16), IOMMU info (18), IOMMU Feature Reporting (20..24). 11h and 40h put
/// the IOMMU Attributes at 20 and the two EFR images at 24 and 32.
const IVHD_10_LEN: usize = 24;
const IVHD_11_LEN: usize = 40;
/// IVMD: requester id (4), auxiliary id (6), segment (8), start (16..24),
/// length (24..32).
const IVMD_LEN: usize = 32;

const IVHD_10: u8 = 0x10;
const IVHD_11: u8 = 0x11;
const IVHD_40: u8 = 0x40;
const IVMD_ALL: u8 = 0x20;
const IVMD_ONE: u8 = 0x21;
const IVMD_RANGE: u8 = 0x22;

/// The most IVHDs a table may hold: a unit is described at most once per
/// type, and the bound is what keeps the pairwise check of them short.
const MAX_DEFINITIONS: usize = 64;

/// A device entry's type: bits 7:6 encode its length as 4, 8, 16 or 32
/// bytes, except F0h's, which carries its own.
const PAD_4: u8 = 0x00;
const ALL: u8 = 0x01;
const SELECT: u8 = 0x02;
const START: u8 = 0x03;
const END: u8 = 0x04;
const PAD_8: u8 = 0x40;
const ALIAS: u8 = 0x42;
const ALIAS_START: u8 = 0x43;
const EXTENDED: u8 = 0x46;
const EXTENDED_START: u8 = 0x47;
const SPECIAL: u8 = 0x48;
const HID: u8 = 0xF0;
/// F0h's fixed part: HID (4..12), CID (12..20), UID format (20) and length
/// (21), the UID from 22.
const HID_FIXED: usize = 22;

/// Why an IVRS, or one unit's device entries, cannot be used. `at` is an
/// offset in the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IvrsRefused {
    Table(TableError),
    /// The block at `at` declares a length shorter than its type's header, or
    /// one the table cannot hold; a header the table cannot hold ends here
    /// with `declared` zero.
    Block { at: usize, declared: usize },
    /// More IVHDs than `most`.
    Definitions { most: usize },
    /// The IVHDs at `at` and `other` are of one type and name one unit.
    Duplicate { at: usize, other: usize },
    /// The IVMD at `at` reaches past the 64-bit address space.
    Memory { at: usize, start: u64, length: u64 },
    /// A range of requester ids, an IVMD's or a device entry's, whose last id
    /// is below its first.
    Range { at: usize, first: u16, last: u16 },
    /// The device entry at `at` runs past the end of its block.
    Entry { at: usize },
    /// A device entry type whose length implementations disagree on (the 16-
    /// and 32-byte ones), or F0h outside an IVHD 40h, which alone carries it.
    EntryType { at: usize, kind: u8 },
    /// An end of range with no start of range open.
    Unopened { at: usize },
    /// The start of range at `at` is still open at another start or at the
    /// end of its block.
    Unclosed { at: usize },
    /// An F0h entry's UID of a format this decoder does not know, or of a
    /// length its format does not take.
    Uid { at: usize, format: u8, len: u8 },
}

impl From<TableError> for IvrsRefused {
    fn from(error: TableError) -> Self {
        Self::Table(error)
    }
}

/// The IVinfo word, whole and decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IvInfo {
    pub raw: u32,
    /// Bit 0, `EFRSup`: every IVHD 11h and 40h carries its unit's EFR image.
    pub efr_images: bool,
    /// Bit 1: firmware protected memory from DMA before the operating system
    /// ran, so a unit may be handed over translating.
    pub dma_remap: bool,
    /// Bits 14:8 and 21:15: the physical and virtual address bits a unit
    /// translates.
    pub physical_bits: u8,
    pub virtual_bits: u8,
}

/// What an IVHD reports of its unit beyond its header's first 20 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Features {
    /// IVHD 10h: the IOMMU Feature Reporting word.
    Reported(u32),
    /// IVHD 11h and 40h: the IOMMU Attributes and the images of the unit's
    /// two Extended Feature Registers.
    Image { attributes: u32, efr: u64, efr2: u64 },
}

/// One AMD-Vi unit as an IVHD describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unit {
    /// The block's offset in the table, which [`Ivrs::devices`] walks from.
    pub at: usize,
    /// The IVHD type: 10h, 11h or 40h.
    pub kind: u8,
    pub flags: u8,
    /// The unit's own requester id, as a PCI function of `segment`.
    pub device: u16,
    /// Where the unit's capability block sits in that function's
    /// configuration space.
    pub capability: u16,
    /// The unit's register window.
    pub base: u64,
    pub segment: u16,
    /// The IOMMU info word: MSI message number (4:0) and UnitID (12:8).
    pub info: u16,
    pub features: Features,
}

/// The requester ids an IVMD names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Requesters {
    /// IVMD 20h.
    All,
    /// IVMD 21h.
    One(u16),
    /// IVMD 22h, both ends included.
    Range { first: u16, last: u16 },
}

/// An IVMD: memory its requesters must keep reaching, or never reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ivmd {
    pub requesters: Requesters,
    /// Unity mapping (0), read (1), write (2), exclusion range (3).
    pub flags: u8,
    pub segment: u16,
    pub start: u64,
    /// `start + length` does not wrap: [`ivrs`] refused every one that does.
    pub length: u64,
}

/// One block, in the shape its reader acts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IvrsBlock {
    /// The IVHD that describes its unit at the highest type the table holds
    /// for it.
    Unit(Unit),
    /// An IVHD a higher-typed one of the same unit replaces.
    Superseded(Unit),
    Memory(Ivmd),
    /// A type not decoded here, by its type, offset and length.
    Other { kind: u8, at: usize, len: usize },
}

/// An F0h entry's UID: the ACPI `_UID` of the device it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uid {
    Absent,
    Integer(u64),
    /// `len` bytes at `at` in the table, read by [`Ivrs::bytes`].
    String { at: usize, len: u8 },
}

/// One device entry, ranges already paired. `data` is the entry's DTE
/// setting byte; a range's is its start's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceEntry {
    /// Type 1: every requester id of the unit's segment.
    All { data: u8 },
    /// Type 2.
    Select { id: u16, data: u8 },
    /// Types 3 and 4.
    Range { first: u16, last: u16, data: u8 },
    /// Type 42h: `id`'s transactions reach the unit under `used`.
    Alias { id: u16, used: u16, data: u8 },
    /// Types 43h and 4.
    AliasRange { first: u16, last: u16, used: u16, data: u8 },
    /// Type 46h, with its extended setting word.
    Extended { id: u16, data: u8, extended: u32 },
    /// Types 47h and 4.
    ExtendedRange { first: u16, last: u16, data: u8, extended: u32 },
    /// Type 48h: an I/O APIC (`variety` 1) or HPET (2), which no PCI walk
    /// reaches, numbered `handle` and reaching the unit under `used`.
    Special { handle: u8, used: u16, variety: u8, data: u8 },
    /// Type F0h: an ACPI device, by its `_HID` and `_CID` as eight bytes
    /// each and its `_UID`, reaching the unit under `id`.
    Hid { id: u16, data: u8, hid: [u8; 8], cid: [u8; 8], uid: Uid },
    /// A 4- or 8-byte type not decoded here, padding excepted.
    Other(u8),
}

/// An IVRS whose every block [`ivrs`] has walked.
#[derive(Clone, Copy)]
pub struct Ivrs<P> {
    table: Table<P>,
}

/// A block's header, bounded by the table.
#[derive(Clone, Copy)]
struct Raw {
    at: usize,
    kind: u8,
    flags: u8,
    len: usize,
    device: u16,
}

/// The IVRS at `rsdp_addr`, every block of it checked.
pub fn ivrs<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Ivrs<P>, IvrsRefused> {
    let ivrs = Ivrs { table: find_table(phys, rsdp_addr, b"IVRS", FIRST_BLOCK)? };
    let mut definitions = 0usize;
    for raw in ivrs.raw_blocks() {
        let raw = raw?;
        match raw.kind {
            IVHD_10 | IVHD_11 | IVHD_40 => {
                definitions += 1;
                if definitions > MAX_DEFINITIONS {
                    return Err(IvrsRefused::Definitions { most: MAX_DEFINITIONS });
                }
                let unit = ivrs.unit(raw).ok_or(IvrsRefused::Block { at: raw.at, declared: raw.len })?;
                if let Some(other) = ivrs.units().find(|o| o.at < unit.at && o.kind == unit.kind && same(o, &unit)) {
                    return Err(IvrsRefused::Duplicate { at: unit.at, other: other.at });
                }
            }
            IVMD_ALL | IVMD_ONE | IVMD_RANGE => {
                let memory = ivrs.memory(raw).ok_or(IvrsRefused::Block { at: raw.at, declared: raw.len })?;
                if let Requesters::Range { first, last } = memory.requesters {
                    if last < first {
                        return Err(IvrsRefused::Range { at: raw.at, first, last });
                    }
                }
                if memory.start.checked_add(memory.length).is_none() {
                    return Err(IvrsRefused::Memory { at: raw.at, start: memory.start, length: memory.length });
                }
            }
            _ => {}
        }
    }
    Ok(ivrs)
}

fn same(a: &Unit, b: &Unit) -> bool {
    (a.segment, a.device) == (b.segment, b.device)
}

impl<P: Phys> Ivrs<P> {
    pub fn revision(&self) -> u8 {
        self.table.byte(SDT_REVISION).unwrap_or(0)
    }

    pub fn info(&self) -> IvInfo {
        let raw = self.table.u32_at(IVINFO).unwrap_or(0);
        IvInfo {
            raw,
            efr_images: raw & 1 != 0,
            dma_remap: raw & (1 << 1) != 0,
            physical_bits: ((raw >> 8) & 0x7F) as u8,
            virtual_bits: ((raw >> 15) & 0x7F) as u8,
        }
    }

    /// Every block, in the table's order.
    pub fn blocks(&self) -> impl Iterator<Item = IvrsBlock> + '_ {
        // `ivrs` walked the same headers and decoded the same blocks.
        self.raw_blocks().filter_map(|raw| {
            let raw = raw.ok()?;
            Some(match raw.kind {
                IVHD_10 | IVHD_11 | IVHD_40 => {
                    let unit = self.unit(raw)?;
                    if self.units().any(|o| o.kind > unit.kind && same(&o, &unit)) {
                        IvrsBlock::Superseded(unit)
                    } else {
                        IvrsBlock::Unit(unit)
                    }
                }
                IVMD_ALL | IVMD_ONE | IVMD_RANGE => IvrsBlock::Memory(self.memory(raw)?),
                kind => IvrsBlock::Other { kind, at: raw.at, len: raw.len },
            })
        })
    }

    /// The device entries of the IVHD at `unit.at`, in its order; none where
    /// no IVHD is there.
    pub fn devices(&self, unit: &Unit) -> DeviceEntries<'_, P> {
        let (kind, at, end) = match self.raw(unit.at) {
            Ok(raw) if raw.kind == IVHD_10 => (raw.kind, raw.at + IVHD_10_LEN, raw.at + raw.len),
            Ok(raw) if matches!(raw.kind, IVHD_11 | IVHD_40) => (raw.kind, raw.at + IVHD_11_LEN, raw.at + raw.len),
            _ => (0, 0, 0),
        };
        DeviceEntries { ivrs: self, kind, at, end, open: None, done: false }
    }

    /// `len` bytes at `at`, as a [`Uid::String`] names them.
    pub fn bytes(&self, at: usize, len: u8) -> impl Iterator<Item = u8> + '_ {
        (0..usize::from(len)).filter_map(move |i| self.table.byte(at.checked_add(i)?))
    }

    fn units(&self) -> impl Iterator<Item = Unit> + '_ {
        self.raw_blocks().filter_map(|raw| self.unit(raw.ok()?))
    }

    /// Walks by each block's own length, which [`Ivrs::raw`] bounds below by
    /// the block header and above by the table: at most `len / 6` steps.
    fn raw_blocks(&self) -> impl Iterator<Item = Result<Raw, IvrsRefused>> + '_ {
        let mut at = FIRST_BLOCK;
        let mut halted = false;
        core::iter::from_fn(move || {
            if halted || at >= self.table.len() {
                return None;
            }
            let raw = self.raw(at);
            match raw {
                Ok(raw) => at += raw.len,
                Err(_) => halted = true,
            }
            Some(raw)
        })
    }

    fn raw(&self, at: usize) -> Result<Raw, IvrsRefused> {
        let t = &self.table;
        let header = || Some((t.byte(at)?, t.byte(at + 1)?, t.u16_at(at + 2)?, t.u16_at(at + 4)?));
        let Some((kind, flags, declared, device)) = header() else {
            return Err(IvrsRefused::Block { at, declared: 0 });
        };
        let len = usize::from(declared);
        let floor = match kind {
            IVHD_10 => IVHD_10_LEN,
            IVHD_11 | IVHD_40 => IVHD_11_LEN,
            IVMD_ALL | IVMD_ONE | IVMD_RANGE => IVMD_LEN,
            _ => BLOCK_HEADER,
        };
        if len < floor || at + len > t.len() {
            return Err(IvrsRefused::Block { at, declared: len });
        }
        Ok(Raw { at, kind, flags, len, device })
    }

    /// An IVHD's fields, or `None` for any other block.
    fn unit(&self, raw: Raw) -> Option<Unit> {
        let t = &self.table;
        let at = raw.at;
        let features = match raw.kind {
            IVHD_10 => Features::Reported(t.u32_at(at + 20)?),
            IVHD_11 | IVHD_40 => {
                Features::Image { attributes: t.u32_at(at + 20)?, efr: t.u64_at(at + 24)?, efr2: t.u64_at(at + 32)? }
            }
            _ => return None,
        };
        Some(Unit {
            at,
            kind: raw.kind,
            flags: raw.flags,
            device: raw.device,
            capability: t.u16_at(at + 6)?,
            base: t.u64_at(at + 8)?,
            segment: t.u16_at(at + 16)?,
            info: t.u16_at(at + 18)?,
            features,
        })
    }

    /// An IVMD's fields, or `None` for any other block.
    fn memory(&self, raw: Raw) -> Option<Ivmd> {
        let t = &self.table;
        let at = raw.at;
        let requesters = match raw.kind {
            IVMD_ALL => Requesters::All,
            IVMD_ONE => Requesters::One(raw.device),
            IVMD_RANGE => Requesters::Range { first: raw.device, last: t.u16_at(at + 6)? },
            _ => return None,
        };
        Some(Ivmd {
            requesters,
            flags: raw.flags,
            segment: t.u16_at(at + 8)?,
            start: t.u64_at(at + 16)?,
            length: t.u64_at(at + 24)?,
        })
    }
}

/// A start of range waiting for its end.
#[derive(Clone, Copy)]
struct Start {
    at: usize,
    kind: u8,
    first: u16,
    data: u8,
    /// An alias start's used id, or an extended start's setting word.
    word: u32,
}

/// One unit's device entries; see [`Ivrs::devices`]. A refusal is its last
/// item.
pub struct DeviceEntries<'a, P> {
    ivrs: &'a Ivrs<P>,
    /// The IVHD type the entries are in.
    kind: u8,
    at: usize,
    end: usize,
    open: Option<Start>,
    done: bool,
}

impl<P: Phys> Iterator for DeviceEntries<'_, P> {
    type Item = Result<DeviceEntry, IvrsRefused>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.step();
        if !matches!(item, Some(Ok(_))) {
            self.done = true;
        }
        item
    }
}

impl<P: Phys> DeviceEntries<'_, P> {
    /// Every step advances by at least four bytes, so a walk is at most a
    /// quarter of its block's length long.
    fn step(&mut self) -> Option<Result<DeviceEntry, IvrsRefused>> {
        loop {
            let at = self.at;
            if at >= self.end {
                return self.open.take().map(|start| Err(IvrsRefused::Unclosed { at: start.at }));
            }
            let t = &self.ivrs.table;
            let Some(kind) = t.byte(at) else { return Some(Err(IvrsRefused::Entry { at })) };
            let len = match kind {
                0x00..=0x7F => 4 << (kind >> 6),
                HID if self.kind != IVHD_40 => return Some(Err(IvrsRefused::EntryType { at, kind })),
                HID => match t.byte(at + HID_FIXED - 1) {
                    Some(uid) if at + HID_FIXED <= self.end => HID_FIXED + usize::from(uid),
                    _ => return Some(Err(IvrsRefused::Entry { at })),
                },
                _ => return Some(Err(IvrsRefused::EntryType { at, kind })),
            };
            if at + len > self.end {
                return Some(Err(IvrsRefused::Entry { at }));
            }
            self.at += len;
            match self.decode(at, kind, len) {
                Ok(Some(device)) => return Some(Ok(device)),
                // Padding, or a start of range waiting for its end.
                Ok(None) => continue,
                Err(refused) => return Some(Err(refused)),
            }
        }
    }

    /// The `len`-byte entry of type `kind` at `at`, which the block holds whole.
    fn decode(&mut self, at: usize, kind: u8, len: usize) -> Result<Option<DeviceEntry>, IvrsRefused> {
        let t = &self.ivrs.table;
        let entry = IvrsRefused::Entry { at };
        let id = t.u16_at(at + 1).ok_or(entry)?;
        let data = t.byte(at + 3).ok_or(entry)?;
        let word = if len == 8 { t.u32_at(at + 4).ok_or(entry)? } else { 0 };
        let used = (word >> 8) as u16;
        Ok(Some(match kind {
            PAD_4 | PAD_8 => return Ok(None),
            ALL => DeviceEntry::All { data },
            SELECT => DeviceEntry::Select { id, data },
            ALIAS => DeviceEntry::Alias { id, used, data },
            EXTENDED => DeviceEntry::Extended { id, data, extended: word },
            SPECIAL => DeviceEntry::Special { handle: word as u8, used, variety: (word >> 24) as u8, data },
            START | ALIAS_START | EXTENDED_START => {
                if let Some(open) = self.open.replace(Start { at, kind, first: id, data, word }) {
                    return Err(IvrsRefused::Unclosed { at: open.at });
                }
                return Ok(None);
            }
            END => {
                let Some(Start { at: start, kind, first, data, word }) = self.open.take() else {
                    return Err(IvrsRefused::Unopened { at });
                };
                let last = id;
                if last < first {
                    return Err(IvrsRefused::Range { at: start, first, last });
                }
                match kind {
                    START => DeviceEntry::Range { first, last, data },
                    ALIAS_START => DeviceEntry::AliasRange { first, last, used: (word >> 8) as u16, data },
                    _ => DeviceEntry::ExtendedRange { first, last, data, extended: word },
                }
            }
            HID => self.hid(at, id, data)?,
            other => DeviceEntry::Other(other),
        }))
    }

    /// An F0h entry the block holds whole.
    fn hid(&self, at: usize, id: u16, data: u8) -> Result<DeviceEntry, IvrsRefused> {
        let t = &self.ivrs.table;
        let entry = IvrsRefused::Entry { at };
        let eight = |from: usize| -> Option<[u8; 8]> {
            let mut bytes = [0u8; 8];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = t.byte(from + i)?;
            }
            Some(bytes)
        };
        let (format, len) = (t.byte(at + 20).ok_or(entry)?, t.byte(at + 21).ok_or(entry)?);
        let uid = match (format, len) {
            (0, 0) => Uid::Absent,
            (1, 1..=8) => {
                let mut value = 0u64;
                for i in 0..usize::from(len) {
                    value |= u64::from(t.byte(at + HID_FIXED + i).ok_or(entry)?) << (8 * i);
                }
                Uid::Integer(value)
            }
            (2, 1..) => Uid::String { at: at + HID_FIXED, len },
            _ => return Err(IvrsRefused::Uid { at, format, len }),
        };
        Ok(DeviceEntry::Hid { id, data, hid: eight(at + 4).ok_or(entry)?, cid: eight(at + 12).ok_or(entry)?, uid })
    }
}
