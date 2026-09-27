//! The partitions of the disks this kernel drives — the USB disks, until usbd
//! drives the controller — and the ones the bootloader named.
//!
//! **The loader names three partitions** ([`KernelArgs`]): the ROOT it read,
//! the running slot's volume and the log's. This kernel mounts none of them
//! but ROOT, which is in memory (`rootfs`); it answers their GUIDs in the
//! inventory ([`loaded`]) so the file servers for those roles find them, on
//! whatever disk and whoever drives it.
//!
//! The boot partition's location must match firmware's account on a disk
//! [`probe`] read, and [`probe_usb_disks`] waits for the disk that carries it
//! to appear. A partition a process claims is found by [`claimable`], on the
//! disks [`probe`] read, and the inventory is answered from the tables
//! [`probe`] listed ([`inventory`]). Nothing here writes.

use alloc::vec::Vec;

use crate::block::{BlockDevice, DeviceId, Handle};
use crate::device::ClaimError;
use crate::drivers::{usb_storage, xhci};
use crate::sync::Lock;
use toyos_abi::boot::KernelArgs;
use toyos_abi::inventory::Role;
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

/// What the kernel knows about where the boot partition lives. `Ambiguous`
/// is permanent: two devices carrying one partition GUID means one is a
/// clone, and nothing here can tell which one firmware read.
enum Resolution {
    Unknown,
    Found { boot: Volume },
    Ambiguous,
}

static FIRMWARE: Lock<Option<BootPartition>> = Lock::new(None);
static RESOLVED: Lock<Resolution> = Lock::new(Resolution::Unknown);
/// The partitions the loader named, by role; empty before [`init`].
static LOADED: Lock<Vec<(Role, Guid)>> = Lock::new(Vec::new());
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
    let listed: Vec<(Handle, u32, Vec<Partition>)> = LISTED
        .lock()
        .iter()
        .map(|disk| (disk.handle.clone(), disk.lba_bytes, disk.parts.clone()))
        .collect();
    let mut out = Vec::new();
    for (handle, lba_bytes, parts) in listed {
        for part in parts {
            let holder = match crate::block::span_blocks(part.first_lba, part.lba_count(), lba_bytes) {
                Ok((first_block, blocks)) => handle.holder(first_block, first_block + blocks),
                Err(_) => None,
            };
            out.push((handle.device_id(), part, holder));
        }
    }
    out
}

/// The partitions the loader named, for the inventory.
pub fn loaded() -> Vec<(Role, Guid)> {
    LOADED.lock().clone()
}

/// Where the log partition the loader named is, as far as this kernel sees.
pub enum LogPlace {
    /// The loader named none.
    Unnamed,
    /// On a disk this kernel drives, where its file server claims it.
    Driven,
    /// Not on the disk this kernel booted from and drives, which is where an
    /// image carries it: this machine has no log partition.
    Absent,
    /// The boot disk is one this kernel does not drive, so whether the log
    /// partition is on it is its file server's to say.
    Undriven,
}

/// [`LogPlace`], once [`probe_usb_disks`] has read every disk it will.
pub fn log_place() -> LogPlace {
    let Some(guid) = LOADED.lock().iter().find(|(role, _)| *role == Role::Log).map(|(_, g)| *g) else {
        return LogPlace::Unnamed;
    };
    if LISTED.lock().iter().any(|disk| disk.parts.iter().any(|p| p.unique_guid == guid)) {
        return LogPlace::Driven;
    }
    match *RESOLVED.lock() {
        Resolution::Found { .. } => LogPlace::Absent,
        Resolution::Unknown | Resolution::Ambiguous => LogPlace::Undriven,
    }
}

/// List `handle`'s table into [`LISTED`], once per disk.
fn list(sectors: &mut DeviceSectors<'_>, handle: &Handle, lba_bytes: u32) {
    let id = handle.device_id();
    let mut found = alloc::vec![BLANK; MAX_LISTED];
    // A disk with no table this kernel parses carries no partition.
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

/// Take the partitions the loader named out of its handoff.
pub fn init(args: &KernelArgs) {
    let mut loaded = LOADED.lock();
    if args.root_partition_guid != [0; 16] {
        loaded.push((Role::Root, Guid(args.root_partition_guid)));
    }
    if args.log_partition_guid != [0; 16] {
        let log_guid = Guid(args.log_partition_guid);
        log!("gpt: the boot volume names {log_guid} as the log partition");
        loaded.push((Role::Log, log_guid));
    }
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
    loaded.push((Role::Boot, part.guid));
    *FIRMWARE.lock() = Some(part);
}

pub fn boot_partition() -> Option<BootPartition> {
    *FIRMWARE.lock()
}

/// True only while resolution is still `Unknown`; `Ambiguous` is permanent.
pub fn boot_volume_still_possible() -> bool {
    matches!(*RESOLVED.lock(), Resolution::Unknown)
}

/// Ask every USB disk for its table, retrying while the boot partition is
/// still not found: a USB-booted disk appears only once the controller binds
/// it, and a claim of one of its partitions is found on the disks read here.
pub fn probe_usb_disks() {
    let deadline = crate::clock::nanos_since_boot() + xhci::PORT_SETTLE_CEILING.nanos();
    let mut probed = 0;
    loop {
        probed = probe_announced(probed);
        // Nothing further can change: no partition named, resolved, or
        // ambiguity no device can repair.
        if boot_partition().is_none() || !boot_volume_still_possible() {
            return;
        }
        if crate::clock::nanos_since_boot() >= deadline {
            log!(
                "usb-storage: {probed} disk(s) on this machine and none carries the boot \
                 partition after {} ms of looking",
                xhci::PORT_SETTLE_CEILING.duration().millis()
            );
            return;
        }
        // Paced, not spun: one MMIO read per port under the controller lock,
        // on physical hardware.
        let next = crate::clock::nanos_since_boot() + xhci::PORT_POLL.nanos();
        while crate::clock::nanos_since_boot() < next {
            core::hint::spin_loop();
        }
        xhci::recheck_ports();
    }
}

/// Probe every disk announced since the last call; indices are stable and
/// dense, so a disk is never probed twice.
fn probe_announced(mut probed: usize) -> usize {
    let count = usb_storage::count();
    while probed < count {
        let index = probed;
        probed += 1;
        // One call for both the handle and the block size; the disk carries its
        // own geometry, and registering it is what makes it openable by number.
        let Some((handle, lba_bytes)) = usb_storage::handle(index) else {
            log!(
                "usb-storage: disk {index} was announced and was gone again before its partition \
                 table could be read — not probed"
            );
            continue;
        };
        probe(&handle, lba_bytes);
    }
    probed
}

/// Ask one registered block device what it carries: its table for the
/// inventory, and the boot partition when firmware named one.
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
            *resolved = Resolution::Found { boot: volume };
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

/// One partition a claim may hold: where it is, and its unique GUID.
#[derive(Clone, Copy, Debug)]
pub struct Claimable {
    pub volume: Volume,
    pub unique: Guid,
}

/// Partitions no claim may take whatever a table says: ROOT's source, when
/// the boot could not hold its span (`rootfs::hold_source`).
static WITHHELD: Lock<Vec<Guid>> = Lock::new(Vec::new());

/// Refuse every claim of `guid` for the machine's life, as the kernel's.
pub fn withhold(guid: PartGuid) {
    WITHHELD.lock().push(Guid(guid.0));
}

/// Where one partition is on the disks that answered, and which did not.
pub struct Sought {
    /// The one partition carrying the GUID, `None` where no table that
    /// answered carries it, or why the tables that answered name no one.
    pub found: Result<Option<Claimable>, Unnamed>,
    /// The disks that did not answer a read of their table, of which neither
    /// "none" nor "one" is known.
    pub silent: Vec<DeviceId>,
}

/// Why the tables that answered name no one partition for a GUID.
#[derive(Clone, Copy, Debug)]
pub enum Unnamed {
    /// Carried twice, on one disk or across two.
    Ambiguous,
    /// Named by a table that refuses it.
    Unusable,
}

impl From<Unnamed> for ClaimError {
    fn from(unnamed: Unnamed) -> Self {
        match unnamed {
            Unnamed::Ambiguous => ClaimError::Ambiguous,
            Unnamed::Unusable => ClaimError::Unusable,
        }
    }
}

/// The one partition on this machine whose unique GUID is `guid`, past the
/// range and overlap checks `toyos_gpt::locate` makes (UEFI 2.10 §5.3.3).
///
/// Read off the disks when asked and never cached: a table is outside every
/// partition, so no claim can write one. `Absent` for a GUID no table carries
/// and for the zero GUID, which GPT gives every unused entry; `Ambiguous` for
/// one carried twice, on one disk or across two; `Unusable` for a disk that
/// did not answer, since then neither "none" nor "one" is known;
/// `KernelDriven` for a GUID [`withhold`] named.
pub fn claimable(guid: PartGuid) -> Result<Claimable, ClaimError> {
    let target = Guid(guid.0);
    if target.is_zero() {
        return Err(ClaimError::Absent);
    }
    if WITHHELD.lock().contains(&target) {
        log!("partclaim: {target} is where ROOT was read from, and the kernel withholds it");
        return Err(ClaimError::KernelDriven);
    }
    let sought = seek(guid);
    let found = sought.found?;
    if !sought.silent.is_empty() {
        return Err(ClaimError::Unusable);
    }
    found.ok_or(ClaimError::Absent)
}

/// Look for `guid` on every disk [`probe`] read, reading past a disk that does
/// not answer and naming it.
pub fn seek(guid: PartGuid) -> Sought {
    let target = Guid(guid.0);
    let mut silent = Vec::new();
    if target.is_zero() {
        return Sought { found: Ok(None), silent };
    }
    let disks = DISKS.lock().clone();
    let mut found: Option<Claimable> = None;
    for (handle, lba_bytes) in &disks {
        let id = handle.device_id();
        let part = match toyos_gpt::locate(&mut DeviceSectors::new(handle, *lba_bytes), target) {
            Ok(located) => located.partition,
            Err(e) => {
                match table_refused(id, target, e) {
                    Ok(Unread::Lacks) => {}
                    Ok(Unread::Silent) => silent.push(id),
                    Err(refused) => return Sought { found: Err(refused), silent },
                }
                continue;
            }
        };
        if let Some(first) = found {
            log!(
                "partclaim: {target} is on device {} and on device {id}, so it names no one \
                 partition",
                first.volume.device
            );
            return Sought { found: Err(Unnamed::Ambiguous), silent };
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
    Sought { found: Ok(found), silent }
}

/// A disk whose table gave no partition for a GUID, and no refusal.
enum Unread {
    /// Its table does not carry the GUID.
    Lacks,
    /// It did not answer a read of its table.
    Silent,
}

/// What a table's refusal means for a claim: a disk that does not carry the
/// partition, one that did not answer, or the claim's refusal.
fn table_refused(id: DeviceId, target: Guid, e: GptError) -> Result<Unread, Unnamed> {
    match e {
        GptError::NotFound { .. } => Ok(Unread::Lacks),
        GptError::ReadFailed(lba) => {
            log!("partclaim: device {id} did not answer a read of LBA {lba} while looking for {target}");
            Ok(Unread::Silent)
        }
        GptError::DuplicateUniqueGuid { first, second } => {
            log!("partclaim: device {id} carries {target} in entries {first} and {second}");
            Err(Unnamed::Ambiguous)
        }
        GptError::PartitionRange { .. } | GptError::PartitionOverlap { .. } => {
            log!("partclaim: device {id} names {target} and its own table refuses it: {e:?}");
            Err(Unnamed::Unusable)
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
        | GptError::UsableRangeCoversBackup { .. } => Ok(Unread::Lacks),
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
