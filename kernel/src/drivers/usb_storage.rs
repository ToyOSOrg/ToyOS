//! USB mass storage as a [`BlockDevice`]. Disk state lives in the xHCI
//! controller; this holds only an index and reported geometry, so the
//! controller lock is taken per operation and never held across one.
//! Each `read_blocks`/`write_blocks`/`flush` call opens its own operation
//! budget via [`crate::block::begin_operation`]; `xhci::wait/msc.rs` recovers
//! the deadline from the running context rather than a parameter, since it
//! crosses `BlockAccess`/`BlockDevice` frames that cannot carry it.
//! The guard must stay named `_op`; `let _` would drop it immediately, ending the operation before the call it bounds.

use crate::block::{self, BlockDevice, BlockError, BlockResult, DeviceId};
use crate::log;
use super::xhci;

/// Where USB disks start in the [`DeviceId`] space; must stay clear of NVMe's range since the page cache keys on this.
const USB_DEVICE_ID_BASE: DeviceId = 16;

/// Disk numbers issued this boot; `0..count()` names every bound disk, and a number never moves or is reissued.
pub fn count() -> usize {
    xhci::storage_count()
}

/// A handle to the `index`-th disk, or `None` if there is no such disk.
pub fn open(index: usize) -> Option<UsbBlockDevice> {
    let geometry = xhci::storage_geometry(index)?;
    Some(UsbBlockDevice {
        index,
        id: USB_DEVICE_ID_BASE + index as DeviceId,
        blocks: geometry.blocks,
        lba_bytes: geometry.logical_block_bytes,
    })
}

pub struct UsbBlockDevice {
    index: usize,
    id: DeviceId,
    blocks: u64,
    /// What the device addresses in; cached here because a second query can return `None`, which this `u32` can't hold.
    lba_bytes: u32,
}

impl UsbBlockDevice {
    /// The device's own logical block size, as used by a GPT — not the 4 KiB [`BlockDevice`] transfer size.
    pub fn logical_block_bytes(&self) -> u32 {
        self.lba_bytes
    }

    /// Every result must pass through here so budget refusals reach the census the slow-vs-failed policy reads.
    fn noted(&self, done: BlockResult) -> BlockResult {
        if done == Err(BlockError::BudgetExpired) {
            block::census::budget_expired(self.id);
        }
        done
    }

    /// Whether the controller will still speak to the disk under this index, distinct from a failed transfer — unlike geometry, which stays valid after recovery has already given up on it.
    #[cfg(feature = "boot-actuators")]
    pub fn healthy(&self) -> bool {
        xhci::storage_online(self.index) == Some(true)
    }
}

impl BlockDevice for UsbBlockDevice {
    fn device_id(&self) -> DeviceId {
        self.id
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
        let _op = block::begin_operation();
        let done = xhci::storage_read(self.index, lba, count, buf);
        if done.is_err() {
            log!("usb-storage: read of {count} blocks at {lba} {} on disk {}",
                gave_up(done), self.index);
        }
        self.noted(done)
    }

    fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
        let _op = block::begin_operation();
        let done = xhci::storage_write(self.index, lba, count, buf);
        if done.is_err() {
            log!("usb-storage: write of {count} blocks at {lba} {} on disk {}",
                gave_up(done), self.index);
        }
        self.noted(done)
    }

    fn flush(&mut self) -> BlockResult {
        let _op = block::begin_operation();
        let began = crate::clock::now();
        let done = xhci::storage_flush(self.index);
        block::census::flush_took(self.id, (crate::clock::now() - began).nanos());
        if done.is_err() {
            log!("usb-storage: cache flush {} on disk {}", gave_up(done), self.index);
        }
        self.noted(done)
    }
}

/// "failed" is a claim about the disk; a budget refusal is a claim about the caller's clock, so the two need different words.
/// Caller must have already matched `done.is_err()`; an `Ok` here would still print "failed".
fn gave_up(done: BlockResult) -> &'static str {
    match done {
        Err(BlockError::BudgetExpired) => "ran out of its operation budget",
        _ => "failed",
    }
}
