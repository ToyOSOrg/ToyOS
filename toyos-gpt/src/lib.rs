//! GPT parsing, for the one question ToyOS asks a partition table: **where is
//! the partition firmware booted us from?**
//!
//! Not "which partition looks like an ESP". The bootloader takes the unique
//! partition GUID out of its own `LoadedImage` device path while Boot Services
//! are still alive and hands it to the kernel; this crate finds *that* GUID in
//! *that* device's table, or refuses. Searching for a type GUID, or for the
//! first FAT-looking thing, is how an operating system reformats a disk that
//! belongs to somebody else.
//!
//! Everything here treats the disk as hostile. A GPT is bytes an attacker (or
//! a dying flash controller) may have written: every length, count and LBA in
//! it is checked before it is used, nothing is indexed without a bound, and no
//! path panics. The kernel's fail-fast rule is for kernel bugs, never for
//! input that crossed a trust boundary.
//!
//! `no_std`, no allocation, no `unsafe`: the entry array is streamed a block
//! at a time through [`Sectors`], so nothing here is sized by a number the
//! disk chose.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

mod crc32;
mod guid;

use core::num::NonZeroU64;

pub use crc32::{crc32, Crc32};
pub use guid::Guid;

/// The largest partition entry array this crate will walk, in bytes.
///
/// Policy, not physics, and generous: UEFI requires the array to be at least
/// 16,384 bytes and every table in practice is exactly that — 128 entries of
/// 128 bytes — so this is eight times the mandated minimum. It bounds *work*
/// rather than memory, because the array is never held: a header claiming
/// four billion entries would otherwise be four billion entries' worth of
/// device reads before the CRC that proves it was a lie.
pub const MAX_ENTRY_ARRAY_BYTES: u64 = 128 * 1024;

/// The smallest a GPT header may claim to be, from the UEFI specification.
const MIN_HEADER_BYTES: u32 = 92;
/// The smallest a partition entry may be, from the UEFI specification.
const MIN_ENTRY_BYTES: u32 = 128;
/// UTF-16 units in an entry's name field, from byte 56 to the end of the
/// 128 bytes every entry has.
const NAME_UNITS: usize = 36;
/// An entry's name field whole, zero units included.
type Name = [u16; NAME_UNITS];
const HEADER_SIGNATURE: &[u8; 8] = b"EFI PART";
const HEADER_REVISION_1_0: u32 = 0x0001_0000;
/// The MBR partition type that says "this disk is GPT, keep out".
const MBR_TYPE_PROTECTIVE: u8 = 0xEE;

/// A device that can be read one logical block at a time.
///
/// The unit is the device's own logical block, because that is the unit a GPT
/// is written in. A caller whose driver speaks a coarser block — the kernel's
/// `BlockDevice` is 4 KiB — adapts in its implementation of this trait, not
/// inside the parser: the parser must not have to know 4096-byte reads exist.
pub trait Sectors {
    fn lba_bytes(&self) -> u32;
    fn lba_count(&self) -> u64;
    /// Granularity of `lba_count()` in logical blocks. A count floored by a
    /// coarser reader can omit at most one less than this value.
    fn lba_count_granularity(&self) -> NonZeroU64;
    /// Fill `buf` — exactly `lba_bytes()` long — with logical block `lba`.
    /// `false` means the read did not happen and its contents are unknown.
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

/// Every way a table can fail to name a partition, and nothing that panics.
///
/// One variant per refusal rather than a single "malformed", because the
/// caller logs this on a machine whose only channel out may be a screen: what
/// a first bare-metal boot needs is which field was wrong and what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptError {
    /// The device's logical block size is not one this crate parses.
    UnsupportedLbaSize(u32),
    /// A read the parse needed did not happen.
    ReadFailed(u64),
    /// The device is too small to hold a GPT at all.
    DeviceTooSmall(u64),
    /// LBA 0 is not a GPT protective MBR. A disk with a real MBR partition
    /// table, or with a hybrid one, lands here and is refused rather than
    /// interpreted.
    NoProtectiveMbr,
    /// LBA 1 does not begin `EFI PART`.
    NoHeader,
    UnsupportedRevision(u32),
    /// `header_size` outside `92..=lba_bytes`.
    HeaderSize(u32),
    /// The header's reserved word is not zero.
    HeaderReserved(u32),
    /// The header does not claim to live at LBA 1, so it is not the primary
    /// header and this is not the disk it was written for.
    HeaderMisplaced(u64),
    HeaderCrc { stored: u32, computed: u32 },
    /// `first_usable_lba`/`last_usable_lba` are not a range inside the device.
    UsableRange { first: u64, last: u64 },
    /// `size_of_partition_entry` is not at least 128, a power of two, and a
    /// divisor of the logical block — the three together are what keep an
    /// entry from straddling two reads.
    EntrySize(u32),
    /// The array is larger than [`MAX_ENTRY_ARRAY_BYTES`].
    EntryArrayTooBig { entries: u32, entry_size: u32 },
    /// The array does not fit between the header and the first usable block,
    /// or runs off the end of the device.
    EntryArrayMisplaced { lba: u64, lbas: u64 },
    EntryArrayCrc { stored: u32, computed: u32 },
    /// The table is well-formed and does not contain the GUID asked for.
    /// Carries the partition count because "this disk has three partitions and
    /// none of them is ours" and "this disk has none" are different facts, and
    /// on a machine with no serial port the log line is the whole diagnostic.
    NotFound { used_entries: u32 },
    /// The matching entry's blocks are not inside the disk's usable range.
    PartitionRange { first: u64, last: u64 },
    /// Another entry claims blocks the matching entry also claims. Refused
    /// rather than resolved: the caller's next move is to write to those
    /// blocks, and there is no reading of this table under which that is safe.
    PartitionOverlap { index: u32 },
    /// The primary's `last_usable_lba` reaches into the backup GPT, whose
    /// blocks start no later than `backup_array_lba`. The mirror is not
    /// usable space: a partition allowed there is one whose writes destroy
    /// the recovery copy.
    UsableRangeCoversBackup { last: u64, backup_array_lba: u64 },
    /// Two entries carry the searched-for unique GUID — the one fact
    /// identifying the boot partition — so this table does not name one
    /// partition. Refused rather than resolved first-wins.
    DuplicateUniqueGuid { first: u32, second: u32 },
}

impl GptError {
    /// Whether this refusal means the primary never became a CRC-verified
    /// table at all, as opposed to becoming one and then answering "not
    /// found" or "not sane". Only the first kind is worth retrying against
    /// the backup: the other two already read the table successfully, and
    /// retrying them would be comparing two copies instead of refusing.
    fn primary_never_checked_out(self) -> bool {
        !matches!(
            self,
            GptError::NotFound { .. }
                | GptError::PartitionRange { .. }
                | GptError::PartitionOverlap { .. }
                | GptError::DuplicateUniqueGuid { .. }
        )
    }
}

/// One partition entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    index: u32,
    type_guid: Guid,
    unique_guid: Guid,
    first_lba: u64,
    last_lba: u64,
    lba_count: NonZeroU64,
    name: Name,
}

impl Partition {
    /// `stated`, if its blocks are a partition inside `header`'s usable range.
    fn place(stated: Stated, name: Name, header: &Header) -> Result<Self, Stated> {
        if stated.first < header.first_usable_lba || stated.last > header.last_usable_lba {
            return Err(stated);
        }
        let lba_count = stated
            .last
            .checked_sub(stated.first)
            .and_then(|span| span.checked_add(1))
            .and_then(NonZeroU64::new)
            .ok_or(stated)?;
        Ok(Self {
            index: stated.index,
            type_guid: stated.type_guid,
            unique_guid: stated.unique_guid,
            first_lba: stated.first,
            last_lba: stated.last,
            lba_count,
            name,
        })
    }

    /// Position in the entry array, from 0.
    pub const fn index(&self) -> u32 {
        self.index
    }

    pub const fn type_guid(&self) -> Guid {
        self.type_guid
    }

    pub const fn unique_guid(&self) -> Guid {
        self.unique_guid
    }

    pub const fn first_lba(&self) -> u64 {
        self.first_lba
    }

    /// Inclusive, as GPT stores it.
    pub const fn last_lba(&self) -> u64 {
        self.last_lba
    }

    pub const fn lba_count(&self) -> NonZeroU64 {
        self.lba_count
    }

    /// The entry's name, UTF-16 as the table states it, up to the first zero
    /// unit: what it says is the caller's to decode.
    pub fn name(&self) -> &[u16] {
        self.name.split(|unit| *unit == 0).next().unwrap_or_default()
    }

    /// Whether this is an ESP *by type*. A sanity check for a log line and
    /// nothing more — the selection has already happened, by unique GUID.
    pub fn is_efi_system(&self) -> bool {
        self.type_guid == Guid::EFI_SYSTEM
    }
}

/// One used entry as the table states it, before anything is proven of its
/// blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stated {
    pub index: u32,
    pub type_guid: Guid,
    pub unique_guid: Guid,
    pub first: u64,
    pub last: u64,
}

/// One used entry a scan hands back: `Err` where its blocks are no partition
/// on its disk.
pub type Entry = Result<Partition, Stated>;

/// A located partition plus what the table around it looked like.
///
/// Made only by [`locate`]: no other entry of its table claims its blocks.
///
/// The extra fields are not decoration: a log line saying "this disk has three
/// partitions and none of them is ours" is a different diagnostic from "this
/// disk has no partition table", and on a machine with no serial port the
/// difference is the whole debugging session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    partition: Partition,
    disk_guid: Guid,
    used_entries: u32,
}

impl Located {
    pub const fn partition(&self) -> Partition {
        self.partition
    }

    pub const fn disk_guid(&self) -> Guid {
        self.disk_guid
    }

    /// Entries with a non-zero type GUID, i.e. partitions that exist.
    pub const fn used_entries(&self) -> u32 {
        self.used_entries
    }
}

/// The primary GPT header, once every field in it has been checked.
#[derive(Debug, Clone, Copy)]
struct Header {
    disk_guid: Guid,
    first_usable_lba: u64,
    last_usable_lba: u64,
    entry_array_lba: u64,
    /// One past the array's last block, which the header check proved
    /// representable.
    entry_array_end: u64,
    entry_count: u32,
    entry_bytes: u32,
    /// `entry_bytes`, which the header check proved at least 128 and a
    /// divisor of the block.
    entry_len: usize,
    entry_array_crc: u32,
}

/// Find the partition carrying `target` on `dev`.
///
/// Reads only. The order is not negotiable: the protective MBR, then the
/// header and its CRC, then the entry array and *its* CRC — and only then is
/// anything the array said allowed to mean something. A match found on the way
/// through is held back until the array's CRC proves the bytes it came from
/// were not garbage.
///
/// The primary copy at LBA 1 is tried first. UEFI puts a full second copy at
/// the end of the device precisely so a torn write to the front is
/// recoverable, so a primary that never became a checked table — a read that
/// failed, a header that did not parse, an entry array whose CRC did not
/// hold — is retried against the backup at `lba_count - 1` before this
/// refuses. A primary that *did* become a checked table is trusted alone: the
/// two copies are never compared, so [`GptError::NotFound`],
/// [`GptError::PartitionRange`] and [`GptError::PartitionOverlap`] — every
/// refusal that only exists once the array's CRC has already held — are never
/// retried against the backup. That is the answer to what a disagreement
/// between the two should do: refuse rather than pick, by construction,
/// because this never reads both and chooses.
pub fn locate(dev: &mut dyn Sectors, target: Guid) -> Result<Located, GptError> {
    let disk = open_disk(dev)?;
    match locate_at(dev, 1, target, &disk) {
        Ok(located) => Ok(located),
        Err(primary_err) if primary_err.primary_never_checked_out() => {
            locate_at(dev, disk.backup_header_lba, target, &disk).or(Err(primary_err))
        }
        Err(primary_err) => Err(primary_err),
    }
}

/// Every partition on `dev` whose *type* GUID is `target`, in entry order.
/// `out` is cleared, then filled from the front.
pub fn locate_type(
    dev: &mut dyn Sectors,
    target: Guid,
    out: &mut [Option<Entry>],
) -> Result<TypeScan, GptError> {
    scan(dev, &|type_guid| type_guid == target, out)
}

/// Every partition on `dev`, in entry order: [`locate_type`] with no type
/// asked for.
pub fn list(dev: &mut dyn Sectors, out: &mut [Option<Entry>]) -> Result<TypeScan, GptError> {
    scan(dev, &|_| true, out)
}

fn scan(
    dev: &mut dyn Sectors,
    keep: &dyn Fn(Guid) -> bool,
    out: &mut [Option<Entry>],
) -> Result<TypeScan, GptError> {
    let disk = open_disk(dev)?;
    match scan_type_at(dev, 1, keep, &disk, out) {
        Ok(scan) => Ok(scan),
        Err(primary_err) if primary_err.primary_never_checked_out() => {
            scan_type_at(dev, disk.backup_header_lba, keep, &disk, out).or(Err(primary_err))
        }
        Err(primary_err) => Err(primary_err),
    }
}

/// What [`locate_type`] found: `matched` is how many entries carried the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeScan {
    pub matched: u32,
    pub disk_guid: Guid,
    /// Entries with a non-zero type GUID, i.e. partitions that exist.
    pub used_entries: u32,
}

struct Disk {
    lba: LbaSize,
    lba_count: u64,
    /// `lba_count - 1`, where the backup header is: at least 2.
    backup_header_lba: u64,
    lba_count_slack: u64,
}

/// A logical block size this crate parses, so one block is a fixed prefix of
/// a [`Block`].
///
/// The floor is the smallest logical block any device has ever reported and
/// the value every GPT in the wild is laid out in; the ceiling is 4Kn, and is
/// also what the rest of this kernel is written in. It matches the NVMe
/// driver's own accepted range, which is not a coincidence: above 4096 the
/// block no longer divides the kernel's 4 KiB block, and below 512 the GPT
/// header does not fit in one.
#[derive(Clone, Copy)]
enum LbaSize {
    B512,
    B1024,
    B2048,
    B4096,
}

/// One logical block of the largest size this crate parses.
type Block = [u8; 4096];

impl LbaSize {
    fn of(bytes: u32) -> Option<Self> {
        match bytes {
            512 => Some(Self::B512),
            1024 => Some(Self::B1024),
            2048 => Some(Self::B2048),
            4096 => Some(Self::B4096),
            _ => None,
        }
    }

    const fn bytes(self) -> u32 {
        match self {
            Self::B512 => 512,
            Self::B1024 => 1024,
            Self::B2048 => 2048,
            Self::B4096 => 4096,
        }
    }

    fn of_block(self, block: &mut Block) -> &mut [u8] {
        match self {
            Self::B512 => &mut block[..512],
            Self::B1024 => &mut block[..1024],
            Self::B2048 => &mut block[..2048],
            Self::B4096 => &mut block[..],
        }
    }
}

/// The preamble both walks share: a block size this crate parses, a device big
/// enough to hold a table, and a protective MBR at LBA 0.
fn open_disk(dev: &mut dyn Sectors) -> Result<Disk, GptError> {
    let lba_bytes = dev.lba_bytes();
    let lba = LbaSize::of(lba_bytes).ok_or(GptError::UnsupportedLbaSize(lba_bytes))?;
    let lba_count = dev.lba_count();
    // LBA 0 protective MBR, LBA 1 header, at least one block of entries.
    let backup_header_lba = match lba_count.checked_sub(1) {
        Some(last) if last >= 2 => last,
        _ => return Err(GptError::DeviceTooSmall(lba_count)),
    };
    let lba_count_slack = dev.lba_count_granularity().get().saturating_sub(1);

    let mut block: Block = [0; 4096];
    let block = lba.of_block(&mut block);

    read(dev, 0, block)?;
    check_protective_mbr(block)?;

    Ok(Disk { lba, lba_count, backup_header_lba, lba_count_slack })
}

/// [`locate_type`]'s work against one header, primary or backup.
fn scan_type_at(
    dev: &mut dyn Sectors,
    header_lba: u64,
    keep: &dyn Fn(Guid) -> bool,
    disk: &Disk,
    out: &mut [Option<Entry>],
) -> Result<TypeScan, GptError> {
    out.fill(None);
    let mut block: Block = [0; 4096];
    let block = disk.lba.of_block(&mut block);

    read(dev, header_lba, block)?;
    let header = parse_header(block, disk, header_lba)?;

    // At most `entry_count`, a `u32`: never saturates.
    let mut matched = 0u32;
    let mut slots = out.iter_mut();
    // Meaningless unless `walk_entries` returns `Ok`: the array's CRC is
    // checked at the end of the walk, and an `Err` hands the caller nothing.
    let used_entries = walk_entries(dev, &header, disk.lba, &mut |stated, name| {
        if !keep(stated.type_guid) {
            return;
        }
        matched = matched.saturating_add(1);
        if let Some(slot) = slots.next() {
            *slot = Some(Partition::place(stated, name, &header));
        }
    })?;

    Ok(TypeScan { matched, disk_guid: header.disk_guid, used_entries })
}

/// `locate`'s work against one header, primary or backup — read it, check it,
/// walk the array it names, and match `target` against what CRC-verified.
fn locate_at(
    dev: &mut dyn Sectors,
    header_lba: u64,
    target: Guid,
    disk: &Disk,
) -> Result<Located, GptError> {
    let mut block: Block = [0; 4096];
    let block = disk.lba.of_block(&mut block);

    read(dev, header_lba, block)?;
    let header = parse_header(block, disk, header_lba)?;

    let (found, used_entries) = scan_entries(dev, &header, target, disk.lba)?;
    let Some((stated, name)) = found else {
        return Err(GptError::NotFound { used_entries });
    };
    let partition = Partition::place(stated, name, &header)
        .map_err(|stated| GptError::PartitionRange { first: stated.first, last: stated.last })?;
    check_no_overlap(dev, &header, &partition, disk.lba)?;

    Ok(Located { partition, disk_guid: header.disk_guid, used_entries })
}

fn read(dev: &mut dyn Sectors, lba: u64, buf: &mut [u8]) -> Result<(), GptError> {
    if lba >= dev.lba_count() || !dev.read_lba(lba, buf) {
        return Err(GptError::ReadFailed(lba));
    }
    Ok(())
}

/// LBA 0 must be a protective MBR and nothing else.
///
/// One 0xEE record covering the disk, the other three empty, and the boot
/// signature. A hybrid MBR — a protective record next to real ones, which is
/// what a Mac installer leaves behind — is refused here rather than ignored:
/// it means two tables describe this disk and they can disagree, and picking
/// one of them is guessing.
fn check_protective_mbr(lba0: &[u8]) -> Result<(), GptError> {
    // The MBR is 512 bytes at the front of LBA 0 whatever the block size is.
    let Some(mbr) = lba0.first_chunk::<512>() else {
        return Err(GptError::NoProtectiveMbr);
    };
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return Err(GptError::NoProtectiveMbr);
    }
    // The type byte of each of the four 16-byte records from byte 446.
    let types = [mbr[450], mbr[466], mbr[482], mbr[498]];
    let protective = types.iter().filter(|&&ty| ty == MBR_TYPE_PROTECTIVE).count();
    if protective != 1 || types.iter().any(|&ty| ty != 0 && ty != MBR_TYPE_PROTECTIVE) {
        return Err(GptError::NoProtectiveMbr);
    }
    Ok(())
}

/// Every header field this crate reads, as the disk states it.
struct StatedHeader {
    signature: [u8; 8],
    revision: u32,
    header_bytes: u32,
    crc: u32,
    reserved: u32,
    my_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: Guid,
    entry_array_lba: u64,
    entry_count: u32,
    entry_bytes: u32,
    entry_array_crc: u32,
}

impl StatedHeader {
    /// `None` only where `block` is shorter than a header.
    fn decode(block: &[u8]) -> Option<Self> {
        Some(Self {
            signature: bytes(block, 0)?,
            revision: le_u32(block, 8)?,
            header_bytes: le_u32(block, 12)?,
            crc: le_u32(block, 16)?,
            reserved: le_u32(block, 20)?,
            my_lba: le_u64(block, 24)?,
            first_usable_lba: le_u64(block, 40)?,
            last_usable_lba: le_u64(block, 48)?,
            disk_guid: bytes(block, 56).map(Guid)?,
            entry_array_lba: le_u64(block, 72)?,
            entry_count: le_u32(block, 80)?,
            entry_bytes: le_u32(block, 84)?,
            entry_array_crc: le_u32(block, 88)?,
        })
    }
}

fn parse_header(lba1: &[u8], disk: &Disk, header_lba: u64) -> Result<Header, GptError> {
    let Disk { lba, lba_count, backup_header_lba, lba_count_slack } = *disk;
    let lba_bytes = lba.bytes();
    let Some(stated) = StatedHeader::decode(lba1) else {
        return Err(GptError::NoHeader);
    };
    if &stated.signature != HEADER_SIGNATURE {
        return Err(GptError::NoHeader);
    }
    if stated.revision != HEADER_REVISION_1_0 {
        return Err(GptError::UnsupportedRevision(stated.revision));
    }
    let header_bytes = stated.header_bytes;
    if header_bytes < MIN_HEADER_BYTES || header_bytes > lba_bytes {
        return Err(GptError::HeaderSize(header_bytes));
    }
    if stated.reserved != 0 {
        return Err(GptError::HeaderReserved(stated.reserved));
    }
    if stated.my_lba != header_lba {
        return Err(GptError::HeaderMisplaced(stated.my_lba));
    }

    let covered = usize::try_from(header_bytes).ok().and_then(|len| lba1.get(..len));
    let Some(computed) = covered.and_then(header_crc) else {
        return Err(GptError::HeaderSize(header_bytes));
    };
    if stated.crc != computed {
        return Err(GptError::HeaderCrc { stored: stated.crc, computed });
    }

    let (first_usable_lba, last_usable_lba) = (stated.first_usable_lba, stated.last_usable_lba);
    if first_usable_lba < 2 || last_usable_lba < first_usable_lba || last_usable_lba >= lba_count {
        return Err(GptError::UsableRange { first: first_usable_lba, last: last_usable_lba });
    }

    let entry_bytes = stated.entry_bytes;
    let entry_len = usize::try_from(entry_bytes).map_err(|_| GptError::EntrySize(entry_bytes))?;
    if entry_bytes < MIN_ENTRY_BYTES
        || !entry_bytes.is_power_of_two()
        || entry_bytes > lba_bytes
        || !lba_bytes.is_multiple_of(entry_bytes)
    {
        return Err(GptError::EntrySize(entry_bytes));
    }
    let entry_count = stated.entry_count;
    let too_big = GptError::EntryArrayTooBig { entries: entry_count, entry_size: entry_bytes };
    let array_bytes = u64::from(entry_count).checked_mul(u64::from(entry_bytes)).ok_or(too_big)?;
    if array_bytes == 0 || array_bytes > MAX_ENTRY_ARRAY_BYTES {
        return Err(too_big);
    }

    let entry_array_lba = stated.entry_array_lba;
    let array_lbas = array_bytes.div_ceil(u64::from(lba_bytes));
    let entry_array_end = entry_array_lba
        .checked_add(array_lbas)
        .ok_or(GptError::EntryArrayMisplaced { lba: entry_array_lba, lbas: array_lbas })?;
    // The primary's array sits between the header block and the first usable
    // block; the backup's sits between the last usable block and its own
    // header, at the top of the device — the mirror image, because the
    // backup header is the *last* LBA rather than the second. Anywhere else
    // is data somebody may be using, or off the device, and a table that says
    // otherwise describes a different disk.
    let misplaced = if header_lba == 1 {
        entry_array_lba < 2 || entry_array_end > first_usable_lba
    } else {
        entry_array_lba <= last_usable_lba || entry_array_end > header_lba
    };
    if misplaced {
        return Err(GptError::EntryArrayMisplaced { lba: entry_array_lba, lbas: array_lbas });
    }
    // UEFI 2.11 §5.3.2: “The backup GPT Partition Entry Array must be located
    // after the Last Usable LBA and end before the backup GPT Header.” A
    // coarser [`Sectors`] reader concedes only its declared count-floor sliver;
    // the clamp keeps the backup header itself unconcedable.
    let backup_array_lba = lba_count
        .saturating_add(lba_count_slack)
        .saturating_sub(array_lbas.saturating_add(1))
        .min(backup_header_lba);
    if header_lba == 1 && last_usable_lba >= backup_array_lba {
        return Err(GptError::UsableRangeCoversBackup { last: last_usable_lba, backup_array_lba });
    }

    Ok(Header {
        disk_guid: stated.disk_guid,
        first_usable_lba,
        last_usable_lba,
        entry_array_lba,
        entry_array_end,
        entry_count,
        entry_bytes,
        entry_len,
        entry_array_crc: stated.entry_array_crc,
    })
}

/// The header's CRC is taken over itself with its own CRC field zeroed, so it
/// is computed in three pieces rather than by copying the block to patch it.
/// `None` only where `covered` is shorter than the CRC field's end.
fn header_crc(covered: &[u8]) -> Option<u32> {
    let (before, rest) = covered.split_first_chunk::<16>()?;
    let (_stored, after) = rest.split_first_chunk::<4>()?;
    let mut crc = Crc32::new();
    crc.update(before);
    crc.update(&[0; 4]);
    crc.update(after);
    Some(crc.finish())
}

/// Walk the entry array once, checking its CRC as we go, and show `visit`
/// every entry that exists. Returns how many those were.
///
/// **`Ok` is the only thing that licenses acting on what `visit` collected**:
/// the array is streamed, so the CRC is not known until the last block has been
/// through it, and a caller using its own state after an `Err` would make the
/// checksum decorative.
fn walk_entries(
    dev: &mut dyn Sectors,
    header: &Header,
    lba: LbaSize,
    visit: &mut dyn FnMut(Stated, Name),
) -> Result<u32, GptError> {
    let mut block: Block = [0; 4096];
    let block = lba.of_block(&mut block);

    let mut crc = Crc32::new();
    let mut indices = 0..header.entry_count;
    // At most `entry_count`, a `u32`: never saturates.
    let mut used = 0u32;

    for at in header.entry_array_lba..header.entry_array_end {
        read(dev, at, block)?;
        // The array is `entry_count` whole entries and nothing after them, so
        // the entries walked are exactly the bytes its CRC covers.
        for entry in block.chunks_exact(header.entry_len) {
            let Some(index) = indices.next() else { break };
            crc.update(entry);
            let (Some(stated), Some(name)) = (Stated::decode(index, entry), name(entry)) else {
                return Err(GptError::EntrySize(header.entry_bytes));
            };
            if !stated.type_guid.is_zero() {
                used = used.saturating_add(1);
                visit(stated, name);
            }
        }
    }

    let computed = crc.finish();
    if computed != header.entry_array_crc {
        return Err(GptError::EntryArrayCrc { stored: header.entry_array_crc, computed });
    }
    Ok(used)
}

impl Stated {
    /// `None` only where `entry` is shorter than the fields read.
    fn decode(index: u32, entry: &[u8]) -> Option<Self> {
        Some(Self {
            index,
            type_guid: bytes(entry, 0).map(Guid)?,
            unique_guid: bytes(entry, 16).map(Guid)?,
            first: le_u64(entry, 32)?,
            last: le_u64(entry, 40)?,
        })
    }
}

/// The entry carrying the unique GUID `target`, out of a walk whose CRC held.
fn scan_entries(
    dev: &mut dyn Sectors,
    header: &Header,
    target: Guid,
    lba: LbaSize,
) -> Result<(Option<(Stated, Name)>, u32), GptError> {
    let mut found: Option<(Stated, Name)> = None;
    let mut duplicate: Option<(u32, u32)> = None;

    let used = walk_entries(dev, header, lba, &mut |stated, name| {
        if stated.unique_guid != target {
            return;
        }
        match &found {
            None => found = Some((stated, name)),
            Some((first, _)) if duplicate.is_none() => duplicate = Some((first.index, stated.index)),
            Some(_) => {}
        }
    })?;

    // Held back until the CRC held, like the match itself.
    if let Some((first, second)) = duplicate {
        return Err(GptError::DuplicateUniqueGuid { first, second });
    }
    Ok((found, used))
}

/// Nothing else in the table may claim a block the matched partition claims.
///
/// A second pass rather than bookkeeping in the first, because the entry that
/// overlaps may have been read before the one that matched, and the array is
/// streamed. Thirty-two block reads on a 512-byte device, once per boot.
fn check_no_overlap(
    dev: &mut dyn Sectors,
    header: &Header,
    matched: &Partition,
    lba: LbaSize,
) -> Result<(), GptError> {
    let mut overlap = None;
    walk_entries(dev, header, lba, &mut |other, _| {
        if overlap.is_none()
            && other.index != matched.index
            && other.first <= other.last
            && other.first <= matched.last_lba
            && matched.first_lba <= other.last
        {
            overlap = Some(other.index);
        }
    })?;
    match overlap {
        Some(index) => Err(GptError::PartitionOverlap { index }),
        None => Ok(()),
    }
}

/// `N` bytes of `buf` from `at`, or `None` where `buf` ends first.
fn bytes<const N: usize>(buf: &[u8], at: usize) -> Option<[u8; N]> {
    buf.get(at..)?.first_chunk::<N>().copied()
}

/// An entry's name field as UTF-16 units, `None` only where `entry` ends first.
fn name(entry: &[u8]) -> Option<Name> {
    let field: [u8; 72] = bytes(entry, 56)?;
    let mut units = [0; NAME_UNITS];
    for (unit, pair) in units.iter_mut().zip(field.as_chunks::<2>().0) {
        *unit = u16::from_le_bytes(*pair);
    }
    Some(units)
}

fn le_u32(buf: &[u8], at: usize) -> Option<u32> {
    bytes(buf, at).map(u32::from_le_bytes)
}

fn le_u64(buf: &[u8], at: usize) -> Option<u64> {
    bytes(buf, at).map(u64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizes [`LbaSize`] admits are one range.
    #[test]
    fn the_block_sizes_parsed_are_the_declared_range() {
        let parsed: [u32; 4] = [512, 1024, 2048, 4096];
        for bytes in 0..=8192 {
            let admitted = LbaSize::of(bytes).map(LbaSize::bytes);
            assert_eq!(admitted.is_some(), parsed.contains(&bytes), "{bytes}");
            assert_eq!(admitted.unwrap_or(bytes), bytes);
        }
        let mut block: Block = [0; 4096];
        for lba in [LbaSize::B512, LbaSize::B1024, LbaSize::B2048, LbaSize::B4096] {
            assert_eq!(lba.of_block(&mut block).len() as u32, lba.bytes());
        }
    }
}
