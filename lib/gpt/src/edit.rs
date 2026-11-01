//! A whole table: read from either copy, checked, and written back as UEFI
//! 2.10 §5.3.2 requires — the backup before the primary, every CRC resealed.
//!
//! Only the entry array every table in practice has is read or written,
//! [`ENTRIES`] entries of [`ENTRY_BYTES`]; a copy of another shape reads as
//! [`CopyError::Shape`]. The writer lays the arrays where §5.3.1 puts them:
//! the primary's at LBA 2, the backup's ending just before the device's last
//! LBA, which holds the backup header.

use crate::{check_protective_mbr, header_crc, parse_header, read, Block, Crc32, Disk, GptError, Guid, LbaSize, Sectors};

pub const ENTRIES: usize = 128;
pub const ENTRY_BYTES: usize = 128;
/// The array's bytes, which UEFI requires be at least 16,384 (§5.3.1).
pub const ARRAY_BYTES: usize = 16_384;
/// A name is at most 36 UTF-16 code units (§5.3.3).
pub const NAME_UNITS: usize = 36;
const HEADER_BYTES: u32 = 92;
const ENTRIES_U32: u32 = 128;
const ENTRY_BYTES_U32: u32 = 128;
const ARRAY_BYTES_U64: u64 = 16_384;

/// One entry, every field the table stores. A zero type is an unused entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub type_guid: Guid,
    pub unique_guid: Guid,
    pub first: u64,
    /// Inclusive, as GPT stores it.
    pub last: u64,
    pub attributes: u64,
    pub name: [u16; NAME_UNITS],
}

impl Slot {
    pub const UNUSED: Self =
        Self { type_guid: Guid::ZERO, unique_guid: Guid::ZERO, first: 0, last: 0, attributes: 0, name: [0; NAME_UNITS] };

    /// A used slot of no attributes; `None` for a name longer than
    /// [`NAME_UNITS`].
    pub fn new(type_guid: Guid, unique_guid: Guid, first: u64, last: u64, name: &str) -> Option<Self> {
        Some(Self { type_guid, unique_guid, first, last, attributes: 0, name: Self::named(name)? })
    }

    pub const fn is_used(&self) -> bool {
        !self.type_guid.is_zero()
    }

    /// The name up to its first zero unit.
    pub fn name(&self) -> &[u16] {
        self.name.split(|unit| *unit == 0).next().unwrap_or_default()
    }

    /// `text` as the name field, `None` for one longer than [`NAME_UNITS`].
    pub fn named(text: &str) -> Option<[u16; NAME_UNITS]> {
        let mut name = [0; NAME_UNITS];
        let mut units = text.encode_utf16();
        for unit in name.iter_mut() {
            *unit = units.next().unwrap_or(0);
        }
        units.next().is_none().then_some(name)
    }

    /// The number of blocks it spans, for a used slot whose range is ordered.
    pub fn lba_count(&self) -> Option<u64> {
        self.last.checked_sub(self.first)?.checked_add(1)
    }

    fn decode(raw: &[u8]) -> Option<Self> {
        let name = crate::name(raw)?;
        Some(Self {
            type_guid: crate::bytes(raw, 0).map(Guid)?,
            unique_guid: crate::bytes(raw, 16).map(Guid)?,
            first: crate::le_u64(raw, 32)?,
            last: crate::le_u64(raw, 40)?,
            attributes: crate::le_u64(raw, 48)?,
            name,
        })
    }

    fn encode(&self, out: &mut [u8]) {
        put(out, 0, &self.type_guid.0);
        put(out, 16, &self.unique_guid.0);
        put(out, 32, &self.first.to_le_bytes());
        put(out, 40, &self.last.to_le_bytes());
        put(out, 48, &self.attributes.to_le_bytes());
        if let Some(field) = out.get_mut(56..) {
            for (pair, unit) in field.as_chunks_mut::<2>().0.iter_mut().zip(self.name) {
                *pair = unit.to_le_bytes();
            }
        }
    }
}

/// A disk's size as the table is laid on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    lba: LbaSize,
    lba_count: u64,
    array_lbas: u64,
}

impl Geometry {
    /// A device of `lba_count` blocks of `lba_bytes`: `None` for a block size
    /// [`crate::LbaSize`] does not parse, or a device with no room for an MBR,
    /// both copies and one usable block.
    pub fn new(lba_bytes: u32, lba_count: u64) -> Option<Self> {
        let lba = LbaSize::of(lba_bytes)?;
        let array_lbas = ARRAY_BYTES_U64.checked_div(u64::from(lba_bytes))?;
        let table = array_lbas.checked_add(1)?.checked_mul(2)?.checked_add(1)?;
        (lba_count > table).then_some(Self { lba, lba_count, array_lbas })
    }

    pub const fn lba_bytes(&self) -> u32 {
        self.lba.bytes()
    }

    pub const fn lba_count(&self) -> u64 {
        self.lba_count
    }

    /// Where the backup header is: the device's last LBA.
    pub const fn backup_header_lba(&self) -> u64 {
        // `new` proved `lba_count` past the whole table.
        self.lba_count.saturating_sub(1)
    }

    pub const fn backup_array_lba(&self) -> u64 {
        self.backup_header_lba().saturating_sub(self.array_lbas)
    }

    /// The widest usable range: from just past the primary array to just
    /// before the backup's.
    pub const fn usable(&self) -> (u64, u64) {
        (self.array_lbas.saturating_add(2), self.backup_array_lba().saturating_sub(1))
    }
}

/// Why a table is not one this writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The disk GUID is zero.
    DiskGuid,
    /// The usable range is empty or reaches into either copy.
    UsableRange { first: u64, last: u64 },
    /// A used entry's range is reversed or leaves the usable range.
    OutsideUsable { index: u32 },
    /// An entry of zero type with any other field set: unused entries are
    /// all zero (§5.3.3).
    UnusedNotZero { index: u32 },
    ZeroUnique { index: u32 },
    DuplicateUnique { first: u32, second: u32 },
    Overlap { first: u32, second: u32 },
}

/// A table's contents, apart from where its two copies lie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub disk_guid: Guid,
    pub first_usable: u64,
    pub last_usable: u64,
    pub slots: [Slot; ENTRIES],
}

impl Table {
    /// An empty table across the whole of `geometry`.
    pub fn empty(geometry: &Geometry, disk_guid: Guid) -> Self {
        let (first_usable, last_usable) = geometry.usable();
        Self { disk_guid, first_usable, last_usable, slots: [Slot::UNUSED; ENTRIES] }
    }

    /// Each used slot with its index.
    pub fn used(&self) -> impl Iterator<Item = (u32, &Slot)> {
        (0u32..).zip(self.slots.iter()).filter(|(_, s)| s.is_used())
    }

    /// Everything §5.3 requires of a table on `geometry`.
    pub fn check(&self, geometry: &Geometry) -> Result<(), Invalid> {
        if self.disk_guid.is_zero() {
            return Err(Invalid::DiskGuid);
        }
        let (first, last) = geometry.usable();
        if self.first_usable < first || self.last_usable > last || self.first_usable > self.last_usable {
            return Err(Invalid::UsableRange { first: self.first_usable, last: self.last_usable });
        }
        for (index, slot) in (0u32..).zip(self.slots.iter()) {
            if !slot.is_used() {
                if *slot != Slot::UNUSED {
                    return Err(Invalid::UnusedNotZero { index });
                }
                continue;
            }
            if slot.unique_guid.is_zero() {
                return Err(Invalid::ZeroUnique { index });
            }
            if slot.first > slot.last || slot.first < self.first_usable || slot.last > self.last_usable {
                return Err(Invalid::OutsideUsable { index });
            }
        }
        for (first, a) in self.used() {
            for (second, b) in self.used().filter(|(i, _)| *i > first) {
                if a.unique_guid == b.unique_guid {
                    return Err(Invalid::DuplicateUnique { first, second });
                }
                if a.first <= b.last && b.first <= a.last {
                    return Err(Invalid::Overlap { first, second });
                }
            }
        }
        Ok(())
    }

    /// The bytes of both copies, once [`Self::check`] holds.
    pub fn encode(&self, geometry: &Geometry) -> Result<Encoded, Invalid> {
        self.check(geometry)?;
        let mut array = [0u8; ARRAY_BYTES];
        for (slot, raw) in self.slots.iter().zip(array.as_chunks_mut::<ENTRY_BYTES>().0) {
            slot.encode(raw);
        }
        let array_crc = crate::crc32(&array);
        let last = geometry.backup_header_lba();
        let primary = self.header(1, last, 2, array_crc);
        let backup = self.header(last, 1, geometry.backup_array_lba(), array_crc);
        Ok(Encoded { geometry: *geometry, array, primary, backup, mbr: protective_mbr(geometry) })
    }

    fn header(&self, my: u64, alternate: u64, array_lba: u64, array_crc: u32) -> Block {
        let mut block: Block = [0; 4096];
        put(&mut block, 0, crate::HEADER_SIGNATURE);
        put(&mut block, 8, &crate::HEADER_REVISION_1_0.to_le_bytes());
        put(&mut block, 12, &HEADER_BYTES.to_le_bytes());
        put(&mut block, 24, &my.to_le_bytes());
        put(&mut block, 32, &alternate.to_le_bytes());
        put(&mut block, 40, &self.first_usable.to_le_bytes());
        put(&mut block, 48, &self.last_usable.to_le_bytes());
        put(&mut block, 56, &self.disk_guid.0);
        put(&mut block, 72, &array_lba.to_le_bytes());
        put(&mut block, 80, &ENTRIES_U32.to_le_bytes());
        put(&mut block, 84, &ENTRY_BYTES_U32.to_le_bytes());
        put(&mut block, 88, &array_crc.to_le_bytes());
        let crc = block.first_chunk::<92>().and_then(|h| header_crc(h)).unwrap_or(0);
        put(&mut block, 16, &crc.to_le_bytes());
        block
    }
}

/// LBA 0 as §5.2.3 lays it for a GPT disk: one record of type 0xEE from LBA
/// 1 over the rest of the disk, saturating at what 32 bits count.
fn protective_mbr(geometry: &Geometry) -> Block {
    let mut block: Block = [0; 4096];
    put(&mut block, 446, &[0x00, 0x00, 0x02, 0x00, crate::MBR_TYPE_PROTECTIVE, 0xFF, 0xFF, 0xFF]);
    put(&mut block, 454, &1u32.to_le_bytes());
    let size = u32::try_from(geometry.backup_header_lba()).unwrap_or(u32::MAX);
    put(&mut block, 458, &size.to_le_bytes());
    put(&mut block, 510, &[0x55, 0xAA]);
    block
}

/// `bytes` into `buf` at `at`; the offsets here are constants inside the
/// buffers they name.
fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(dst) = buf.get_mut(at..).and_then(|rest| rest.get_mut(..bytes.len())) {
        dst.copy_from_slice(bytes);
    }
}

/// Both copies of a table, encoded.
pub struct Encoded {
    geometry: Geometry,
    array: [u8; ARRAY_BYTES],
    primary: Block,
    backup: Block,
    mbr: Block,
}

/// One thing a writer does, in [`Encoded::steps`]'s order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step<'a> {
    /// Write `bytes`, whole logical blocks, from LBA `lba`.
    Write { lba: u64, bytes: &'a [u8] },
    /// Every write before this is on the medium before any after it is
    /// issued: a device with a volatile cache is flushed.
    Flush,
}

impl Encoded {
    /// The writes and flushes that put this table on the disk, in the order
    /// they must reach it: the backup copy, a flush, then the protective MBR
    /// when `mbr` asks for it and the primary copy, and a flush.
    ///
    /// The backup goes first because §5.3.2 requires it. The flush between is
    /// what keeps a crash from tearing both copies at once — on a device that
    /// persists each sector whole or not at all and has every write before a
    /// completed flush on the medium: then the primary is not touched until
    /// the backup is whole, and one copy is whole at every instant, the old
    /// primary while the backup is written and the new backup while the
    /// primary is. A device that breaks either can leave neither copy valid.
    pub fn steps<'a>(&'a self, mbr: bool) -> impl Iterator<Item = Step<'a>> {
        let len = self.geometry.lba.bytes_usize();
        let block = |b: &'a Block| -> &'a [u8] { b.get(..len).unwrap_or_default() };
        let backup_array = Step::Write { lba: self.geometry.backup_array_lba(), bytes: &self.array };
        let backup_header = Step::Write { lba: self.geometry.backup_header_lba(), bytes: block(&self.backup) };
        let protective = mbr.then_some(Step::Write { lba: 0, bytes: block(&self.mbr) });
        let primary_array = Step::Write { lba: 2, bytes: &self.array };
        let primary_header = Step::Write { lba: 1, bytes: block(&self.primary) };
        [Some(backup_array), Some(backup_header), Some(Step::Flush), protective, Some(primary_array), Some(primary_header), Some(Step::Flush)]
            .into_iter()
            .flatten()
    }
}

/// What LBA 0 holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mbr {
    /// A GPT's protective MBR.
    Protective,
    /// An MBR's boot signature and records that are not one protective
    /// record: an MBR partition table, or a hybrid one.
    Other,
    /// No boot signature at all.
    Blank,
}

/// Which copy of a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Primary,
    Backup,
}

/// Why one copy is not a table this reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    Gpt(GptError),
    /// A valid header naming an array of another shape than the one this
    /// writes.
    Shape { entries: u32, entry_bytes: u32 },
}

impl From<GptError> for CopyError {
    fn from(e: GptError) -> Self {
        Self::Gpt(e)
    }
}

/// Everything a disk holds of a table: LBA 0 and each copy, read alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copies {
    pub geometry: Geometry,
    pub mbr: Mbr,
    pub primary: Result<Table, CopyError>,
    pub backup: Result<Table, CopyError>,
}

impl Copies {
    /// The table this disk carries, by §5.3.2's rule: the primary when it is
    /// valid, else the backup. `None` for a disk with no valid copy, or no
    /// protective MBR.
    pub fn in_force(&self) -> Option<(Which, &Table)> {
        if self.mbr != Mbr::Protective {
            return None;
        }
        match (&self.primary, &self.backup) {
            (Ok(t), _) => Some((Which::Primary, t)),
            (Err(_), Ok(t)) => Some((Which::Backup, t)),
            (Err(_), Err(_)) => None,
        }
    }

    /// Both copies valid and the same table.
    pub fn agree(&self) -> bool {
        matches!((&self.primary, &self.backup), (Ok(p), Ok(b)) if p == b)
    }
}

/// LBA 0 and both copies of `dev`'s table, each read and checked alone.
/// `None` only for a device [`Geometry::new`] refuses or whose LBA 0 does not
/// read.
pub fn read_copies(dev: &mut dyn Sectors) -> Option<Copies> {
    let geometry = Geometry::new(dev.lba_bytes(), dev.lba_count())?;
    let mut block: Block = [0; 4096];
    let lba0 = geometry.lba.of_block(&mut block);
    read(dev, 0, lba0).ok()?;
    let mbr = if check_protective_mbr(lba0).is_ok() {
        Mbr::Protective
    } else if lba0.get(510..512) == Some(&[0x55, 0xAA]) {
        Mbr::Other
    } else {
        Mbr::Blank
    };
    let disk = Disk { lba: geometry.lba, lba_count: geometry.lba_count, backup_header_lba: geometry.backup_header_lba() };
    let primary = read_copy(dev, &disk, 1);
    let backup = read_copy(dev, &disk, geometry.backup_header_lba());
    Some(Copies { geometry, mbr, primary, backup })
}

fn read_copy(dev: &mut dyn Sectors, disk: &Disk, header_lba: u64) -> Result<Table, CopyError> {
    let mut block: Block = [0; 4096];
    let lba = disk.lba.of_block(&mut block);
    read(dev, header_lba, lba)?;
    let header = parse_header(lba, disk, header_lba)?;
    if header.entry_count != ENTRIES_U32 || header.entry_bytes != ENTRY_BYTES_U32 {
        return Err(CopyError::Shape { entries: header.entry_count, entry_bytes: header.entry_bytes });
    }
    let mut array = [0u8; ARRAY_BYTES];
    for (at, chunk) in (header.entry_array_lba..header.entry_array_end).zip(array.chunks_mut(disk.lba.bytes_usize())) {
        read(dev, at, chunk)?;
    }
    let mut crc = Crc32::new();
    crc.update(&array);
    let computed = crc.finish();
    if computed != header.entry_array_crc {
        return Err(GptError::EntryArrayCrc { stored: header.entry_array_crc, computed }.into());
    }
    let mut slots = [Slot::UNUSED; ENTRIES];
    for (slot, raw) in slots.iter_mut().zip(array.as_chunks::<ENTRY_BYTES>().0) {
        *slot = Slot::decode(raw).ok_or(GptError::EntrySize(header.entry_bytes))?;
    }
    Ok(Table { disk_guid: header.disk_guid, first_usable: header.first_usable_lba, last_usable: header.last_usable_lba, slots })
}
