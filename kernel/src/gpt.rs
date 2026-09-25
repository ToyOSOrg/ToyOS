//! Resolves the partitions the bootloader handed off ([`KernelArgs`]) to
//! locations on a probed block device ([`probe`]), and collects every ROOT and
//! DATA candidate those devices carry.
//!
//! The boot partition's location must match firmware's account or is
//! refused; the log partition is trusted only on the device already found
//! to carry the boot partition, since its GUID names a file on that volume.
//! A ROOT or DATA candidate is selected by partition *type* and nothing more —
//! which of them is a role's filesystem is answered against each one's own
//! superblock, by `rootfs` and by `bcachefs_adapter::probe`. A partition a
//! process claims is found by [`claimable`], on the disks [`probe`] read, and
//! the inventory is answered from the tables [`probe`] listed ([`inventory`]).
//! Nothing here writes.

use alloc::vec::Vec;

use crate::block::{BlockDevice, DeviceId, Handle};
use crate::device::ClaimError;
use crate::sync::Lock;
use toyos_abi::boot::KernelArgs;
use toyos_abi::part::PartGuid;
use toyos_gpt::{GptError, Guid, Partition, Sectors};

/// The partition firmware loaded the bootloader from, in firmware's terms.
#[derive(Clone, Copy, Debug)]
pub struct BootPartition {
    pub guid: Guid,
    /// In the boot device's logical blocks; for cross-checking a GPT entry, never I/O.
    pub start_lba: u64,
    pub blocks: u64,
}

/// A partition this kernel was given, as a place on a device it can read.
#[derive(Clone, Copy, Debug)]
pub struct Volume {
    pub device: DeviceId,
    /// The device's logical block size; both LBAs below are in these units.
    pub lba_bytes: u32,
    pub start_lba: u64,
    pub blocks: u64,
}

/// What the kernel knows about where the given partitions live. `Ambiguous`
/// is permanent: two devices carrying one partition GUID means one is a
/// clone, and nothing here can tell which one firmware read.
enum Resolution {
    Unknown,
    Found { boot: Volume, log: Option<Volume> },
    Ambiguous,
}

/// One partition of a ToyOS type, on a device that answered for it.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub volume: Volume,
    pub guid: Guid,
}

static FIRMWARE: Lock<Option<BootPartition>> = Lock::new(None);
/// The log partition's identity; `None` only before [`init`] runs.
static LOG_GUID: Lock<Option<Guid>> = Lock::new(None);
static RESOLVED: Lock<Resolution> = Lock::new(Resolution::Unknown);
static ROOTS: Lock<Vec<Candidate>> = Lock::new(Vec::new());
static DATA: Lock<Vec<Candidate>> = Lock::new(Vec::new());
/// Every device [`probe`] read, with its logical block size: the disks a
/// partition claim is looked for on. Taken alone.
static DISKS: Lock<Vec<(Handle, u32)>> = Lock::new(Vec::new());

/// Every entry each disk's table stated when [`probe`] read it, for the
/// inventory: a table is outside every partition, so nothing a holder writes
/// changes it, and nothing here reads a disk again to answer.
static LISTED: Lock<Vec<Listed>> = Lock::new(Vec::new());

/// One disk's entries, as [`probe`] listed them.
struct Listed {
    handle: Handle,
    lba_bytes: u32,
    parts: Vec<Partition>,
}

/// The most entries the inventory lists for one disk: the 128 a GPT's entry
/// array holds by convention (UEFI 2.10 §5.3.2). A disk with more says so
/// when it is probed.
const MAX_LISTED: usize = 128;

/// Every GPT entry on every disk [`probe`] read, and who holds exactly its
/// span now, from the block layer's holds. A partition that is not whole
/// blocks is held by nothing, since no view can be made of it.
pub fn inventory() -> Vec<(DeviceId, Partition, Option<crate::block::Holder>)> {
    let unit = toyos_abi::part::BLOCK_BYTES as u64;
    let listed: Vec<(Handle, u32, Vec<Partition>)> = LISTED
        .lock()
        .iter()
        .map(|disk| (disk.handle.clone(), disk.lba_bytes, disk.parts.clone()))
        .collect();
    let mut out = Vec::new();
    for (handle, lba_bytes, parts) in listed {
        let lba = u64::from(lba_bytes);
        for part in parts {
            let span = part.first_lba.checked_mul(lba).zip(part.lba_count().checked_mul(lba));
            let holder = match span {
                Some((start, len)) if start % unit == 0 && len % unit == 0 => {
                    handle.holder(start / unit, start / unit + len / unit)
                }
                _ => None,
            };
            out.push((handle.device_id(), part, holder));
        }
    }
    out
}

/// List `handle`'s table into [`LISTED`], once per disk.
fn list(sectors: &mut DeviceSectors<'_>, handle: &Handle, lba_bytes: u32) {
    let id = handle.device_id();
    let mut found = alloc::vec![BLANK; MAX_LISTED];
    // A disk with no table this kernel parses carries no partition, and
    // `collect` says so, naming the refusal.
    let Ok(scan) = toyos_gpt::list(sectors, &mut found) else { return };
    if scan.matched as usize > scan.listed {
        log!(
            "gpt: device {id} carries {} partitions and the inventory lists {}",
            scan.matched,
            scan.listed
        );
    }
    found.truncate(scan.listed);
    LISTED.lock().push(Listed { handle: handle.clone(), lba_bytes, parts: found });
}

/// How many partitions of one ToyOS type one device may offer this kernel.
///
/// A bound rather than a `Vec` because [`toyos_gpt::locate_type`] fills a
/// caller's slice; a device carrying more says so in the log, and a boot that
/// then finds no match panics naming what it did see.
const MAX_PER_DEVICE: usize = 4;

/// Take both partitions' identities out of the bootloader's handoff.
pub fn init(args: &KernelArgs) {
    let log_guid = Guid(args.log_partition_guid);
    log!("gpt: the boot volume names {log_guid} as the log partition");
    *LOG_GUID.lock() = Some(log_guid);

    if args.boot_partition_present == 0 {
        log!("gpt: firmware named no boot partition — this machine has none");
        return;
    }
    let part = BootPartition {
        guid: Guid(args.boot_partition_guid),
        start_lba: args.boot_partition_start_lba,
        blocks: args.boot_partition_blocks,
    };
    log!(
        "gpt: firmware booted us from partition {} at LBA {}+{}",
        part.guid, part.start_lba, part.blocks
    );
    *FIRMWARE.lock() = Some(part);
}

pub fn boot_partition() -> Option<BootPartition> {
    *FIRMWARE.lock()
}

/// Where the boot partition is, if a device has been found to carry it.
pub fn boot_volume() -> Option<Volume> {
    match *RESOLVED.lock() {
        Resolution::Found { boot, .. } => Some(boot),
        Resolution::Unknown | Resolution::Ambiguous => None,
    }
}

/// True only while resolution is still `Unknown`; `Ambiguous` is permanent.
pub fn boot_volume_still_possible() -> bool {
    matches!(*RESOLVED.lock(), Resolution::Unknown)
}

/// Where the log partition is, on the device that carries the boot partition.
pub fn log_volume() -> Option<Volume> {
    match *RESOLVED.lock() {
        Resolution::Found { log, .. } => log,
        Resolution::Unknown | Resolution::Ambiguous => None,
    }
}

/// Every ROOT candidate seen so far, across every device probed.
pub fn root_candidates() -> Vec<Candidate> {
    ROOTS.lock().clone()
}

/// Every DATA candidate seen so far, across every device probed.
pub fn data_candidates() -> Vec<Candidate> {
    DATA.lock().clone()
}

/// Ask one registered block device what it carries: ROOT and DATA candidates
/// always, and the boot partition when firmware named one.
pub fn probe(handle: &Handle, lba_bytes: u32) {
    let id = handle.device_id();
    let first = {
        let mut disks = DISKS.lock();
        let first = !disks.iter().any(|(held, _)| held.device_id() == id);
        if first {
            disks.push((handle.clone(), lba_bytes));
        }
        first
    };
    let mut sectors = DeviceSectors::new(handle, lba_bytes);
    if first {
        list(&mut sectors, handle, lba_bytes);
    }
    collect(&mut sectors, id, lba_bytes, "ROOT", Guid::TOYOS_ROOT, &ROOTS);
    collect(&mut sectors, id, lba_bytes, "DATA", Guid::TOYOS_DATA, &DATA);

    let Some(firmware) = boot_partition() else {
        return;
    };

    let found = match toyos_gpt::locate(&mut sectors, firmware.guid) {
        Ok(found) => found,
        Err(GptError::NotFound { used_entries }) => {
            log!("gpt: device {id} has {used_entries} partitions and none of them is ours");
            return;
        }
        Err(e) => {
            log!("gpt: device {id} has no partition table we can use: {e:?}");
            return;
        }
    };

    // Firmware's and the table's accounts must agree; a mismatch refuses, never repairs.
    let part = found.partition;
    if part.first_lba != firmware.start_lba || part.lba_count() != firmware.blocks {
        log!(
            "gpt: device {id} puts {} at LBA {}+{} but firmware said {}+{} — not treating it as \
             the boot volume",
            part.unique_guid,
            part.first_lba,
            part.lba_count(),
            firmware.start_lba,
            firmware.blocks
        );
        return;
    }

    let volume = Volume {
        device: id,
        lba_bytes,
        start_lba: part.first_lba,
        blocks: part.lba_count(),
    };

    let mut resolved = RESOLVED.lock();
    match *resolved {
        Resolution::Unknown => {
            // Only here: the log GUID names a file on this volume, not on any other disk.
            let log = locate_log(&mut sectors, id, lba_bytes);
            log!(
                "gpt: device {id} carries the boot partition at LBA {}+{} ({}-byte blocks), \
                 entry {} of {} on disk {}{}",
                volume.start_lba,
                volume.blocks,
                lba_bytes,
                part.index,
                found.used_entries,
                found.disk_guid,
                if part.is_efi_system() { "" } else { " — and its type is not ESP" }
            );
            *resolved = Resolution::Found { boot: volume, log };
        }
        Resolution::Found { boot: first, .. } => {
            log!(
                "gpt: device {id} carries the same partition GUID as device {} — one of them is \
                 a copy and nothing here can say which one we booted from, so this machine now \
                 has no boot volume",
                first.device
            );
            *resolved = Resolution::Ambiguous;
        }
        Resolution::Ambiguous => {
            log!("gpt: device {id} also carries the boot partition GUID");
        }
    }
}

/// Record every partition on this device whose type is `ty`.
///
/// Each type match is then located again by its own *unique* GUID, because
/// that road is the one carrying the range and overlap checks: a candidate this
/// records has passed everything `toyos-gpt` refuses a partition for.
fn collect(
    sectors: &mut DeviceSectors<'_>,
    id: DeviceId,
    lba_bytes: u32,
    what: &str,
    ty: Guid,
    into: &Lock<Vec<Candidate>>,
) {
    let mut found = [BLANK; MAX_PER_DEVICE];
    let scan = match toyos_gpt::locate_type(sectors, ty, &mut found) {
        Ok(scan) => scan,
        Err(e) => {
            log!("gpt: device {id} carries no {what} this kernel can read: {e:?}");
            return;
        }
    };
    if scan.matched as usize > scan.listed {
        log!(
            "gpt: device {id} carries {} {what} partitions and this kernel looks at {}",
            scan.matched,
            scan.listed
        );
    }
    for candidate in &found[..scan.listed] {
        let checked = match toyos_gpt::locate(sectors, candidate.unique_guid) {
            Ok(located) => located.partition,
            Err(e) => {
                log!(
                    "gpt: device {id} names a {what} {} its own table then refuses: {e:?}",
                    candidate.unique_guid
                );
                continue;
            }
        };
        log!(
            "gpt: device {id} carries the {what} candidate {} at LBA {}+{}",
            checked.unique_guid,
            checked.first_lba,
            checked.lba_count()
        );
        into.lock().push(Candidate {
            volume: Volume {
                device: id,
                lba_bytes,
                start_lba: checked.first_lba,
                blocks: checked.lba_count(),
            },
            guid: checked.unique_guid,
        });
    }
}

/// One partition a claim may hold: where it is, and its unique GUID.
#[derive(Clone, Copy, Debug)]
pub struct Claimable {
    pub volume: Volume,
    pub unique: Guid,
}

/// The one partition on this machine whose unique GUID is `guid`, past the
/// range and overlap checks `toyos_gpt::locate` makes (UEFI 2.10 §5.3.3).
///
/// Read off the disks when asked and never cached: a table is outside every
/// partition, so no claim can write one. `Absent` for a GUID no table carries
/// and for the zero GUID, which GPT gives every unused entry; `Ambiguous` for
/// one carried twice, on one disk or across two; `Unusable` for a disk that
/// did not answer, since then neither "none" nor "one" is known.
pub fn claimable(guid: PartGuid) -> Result<Claimable, ClaimError> {
    let target = Guid(guid.0);
    if target.is_zero() {
        return Err(ClaimError::Absent);
    }
    let disks = DISKS.lock().clone();
    let mut found: Option<Claimable> = None;
    for (handle, lba_bytes) in &disks {
        let id = handle.device_id();
        let part = match toyos_gpt::locate(&mut DeviceSectors::new(handle, *lba_bytes), target) {
            Ok(located) => located.partition,
            Err(e) => {
                table_refused(id, target, e)?;
                continue;
            }
        };
        if let Some(first) = found {
            log!(
                "partclaim: {target} is on device {} and on device {id}, so it names no one \
                 partition",
                first.volume.device
            );
            return Err(ClaimError::Ambiguous);
        }
        found = Some(Claimable {
            volume: Volume {
                device: id,
                lba_bytes: *lba_bytes,
                start_lba: part.first_lba,
                blocks: part.lba_count(),
            },
            unique: part.unique_guid,
        });
    }
    found.ok_or(ClaimError::Absent)
}

/// What a table's refusal means for a claim: `Ok` for a disk that does not
/// carry the partition, or the claim's refusal.
fn table_refused(id: DeviceId, target: Guid, e: GptError) -> Result<(), ClaimError> {
    match e {
        GptError::NotFound { .. } => Ok(()),
        GptError::ReadFailed(lba) => {
            log!("partclaim: device {id} did not answer a read of LBA {lba} while looking for {target}");
            Err(ClaimError::Unusable)
        }
        GptError::DuplicateUniqueGuid { first, second } => {
            log!("partclaim: device {id} carries {target} in entries {first} and {second}");
            Err(ClaimError::Ambiguous)
        }
        GptError::PartitionRange { .. } | GptError::PartitionOverlap { .. } => {
            log!("partclaim: device {id} names {target} and its own table refuses it: {e:?}");
            Err(ClaimError::Unusable)
        }
        // No table this kernel parses: a disk that carries no partition, which
        // is what `probe` concluded of it too.
        GptError::UnsupportedLbaSize(_)
        | GptError::DeviceTooSmall(_)
        | GptError::NoProtectiveMbr
        | GptError::NoHeader
        | GptError::UnsupportedRevision(_)
        | GptError::HeaderSize(_)
        | GptError::HeaderReserved(_)
        | GptError::HeaderMisplaced(_)
        | GptError::HeaderCrc { .. }
        | GptError::UsableRange { .. }
        | GptError::EntrySize(_)
        | GptError::EntryArrayTooBig { .. }
        | GptError::EntryArrayMisplaced { .. }
        | GptError::EntryArrayCrc { .. }
        | GptError::UsableRangeCoversBackup { .. } => Ok(()),
    }
}

/// A slot [`toyos_gpt::locate_type`] has not filled in.
const BLANK: Partition = Partition {
    index: 0,
    type_guid: Guid::ZERO,
    unique_guid: Guid::ZERO,
    first_lba: 0,
    last_lba: 0,
};

/// The log partition on the device already proven to carry the boot partition, or `None`.
fn locate_log(sectors: &mut DeviceSectors<'_>, id: DeviceId, lba_bytes: u32) -> Option<Volume> {
    let target = LOG_GUID.lock().expect("gpt::init runs before any device is probed");
    match toyos_gpt::locate(sectors, target) {
        Ok(found) => {
            let part = found.partition;
            log!(
                "gpt: device {id} carries the log partition {target} at LBA {}+{}, entry {} of {}",
                part.first_lba,
                part.lba_count(),
                part.index,
                found.used_entries
            );
            Some(Volume {
                device: id,
                lba_bytes,
                start_lba: part.first_lba,
                blocks: part.lba_count(),
            })
        }
        Err(e) => {
            log!(
                "gpt: device {id} carries the boot partition but nothing with the log partition's \
                 GUID {target}: {e:?} — this stick has no log partition and the kernel's log \
                 stays in memory"
            );
            None
        }
    }
}

/// The kernel's 4 KiB `BlockDevice`, seen in the device's own logical blocks; caches one block.
struct DeviceSectors<'a> {
    dev: &'a Handle,
    lba_bytes: u32,
    lbas_per_block: u64,
    cached: Option<u64>,
    buf: [u8; 4096],
}

impl<'a> DeviceSectors<'a> {
    fn new(dev: &'a Handle, lba_bytes: u32) -> Self {
        // Zero when lba_bytes doesn't divide 4096; that fails every read cleanly.
        let lbas_per_block = if lba_bytes != 0 && 4096 % lba_bytes == 0 {
            (4096 / lba_bytes) as u64
        } else {
            0
        };
        Self { dev, lba_bytes, lbas_per_block, cached: None, buf: [0; 4096] }
    }
}

impl Sectors for DeviceSectors<'_> {
    fn lba_bytes(&self) -> u32 {
        self.lba_bytes
    }

    fn lba_count(&self) -> u64 {
        self.dev.block_count().saturating_mul(self.lbas_per_block)
    }

    fn lba_count_granularity(&self) -> core::num::NonZeroU64 {
        core::num::NonZeroU64::new(self.lbas_per_block)
            .expect("a supported GPT LBA size divides 4096")
    }

    fn read_lba(&mut self, lba: u64, out: &mut [u8]) -> bool {
        if self.lbas_per_block == 0 || out.len() != self.lba_bytes as usize {
            return false;
        }
        let block = lba / self.lbas_per_block;
        if block >= self.dev.block_count() {
            return false;
        }
        if self.cached != Some(block) {
            // Must clear `cached` on a failed read too, or the buffer's previous block
            // looks valid for the next LBA in this block.
            if self.dev.lock().read_blocks(block, 1, &mut self.buf).is_err() {
                self.cached = None;
                return false;
            }
            self.cached = Some(block);
        }
        let at = (lba % self.lbas_per_block) as usize * self.lba_bytes as usize;
        out.copy_from_slice(&self.buf[at..at + out.len()]);
        true
    }
}
