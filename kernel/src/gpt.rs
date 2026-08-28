//! Resolves the partitions the bootloader handed off ([`KernelArgs`]) to
//! locations on a probed block device ([`probe`]).
//!
//! The boot partition's location must match firmware's account or is
//! refused; the log partition is trusted only on the device already found
//! to carry the boot partition, since its GUID names a file on that volume.
//! Nothing here writes.

use crate::block::{BlockDevice, DeviceId};
use crate::sync::Lock;
use toyos_abi::boot::KernelArgs;
use toyos_gpt::{GptError, Guid, Sectors};

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

static FIRMWARE: Lock<Option<BootPartition>> = Lock::new(None);
/// The log partition's identity; `None` only before [`init`] runs.
static LOG_GUID: Lock<Option<Guid>> = Lock::new(None);
static RESOLVED: Lock<Resolution> = Lock::new(Resolution::Unknown);

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

/// Ask one block device whether it carries the boot partition.
pub fn probe(dev: &mut dyn BlockDevice, lba_bytes: u32) {
    let id = dev.device_id();
    let Some(firmware) = boot_partition() else {
        return;
    };

    let mut sectors = DeviceSectors::new(dev, lba_bytes);
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
    dev: &'a mut dyn BlockDevice,
    lba_bytes: u32,
    lbas_per_block: u64,
    cached: Option<u64>,
    buf: [u8; 4096],
}

impl<'a> DeviceSectors<'a> {
    fn new(dev: &'a mut dyn BlockDevice, lba_bytes: u32) -> Self {
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
            if self.dev.read_blocks(block, 1, &mut self.buf).is_err() {
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
