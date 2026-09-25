//! A device claim, and the one console every kernel-spawned process starts on.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;
use toyos_abi::FramebufferInfo;

use crate::device::{Claim, DeviceType};

use super::handle::HandleTable;
use super::shm::SharedMemObject;
use super::{Held, KObjectRef, KObjectVariant, ObjectCore, ZeroHandles};

/// What the class answers when its holder reads it.
pub enum DeviceInfo {
    // Keyboard and mouse answer with events, not a description.
    Events,
    Framebuffer(FramebufferInfo, FramebufferBuffers),
    /// The function, and the `pcidev` slot every call on this claim reaches it
    /// through. No buffer handle beside it: a PCI claimant asks for its own
    /// memory, so this mint installs nothing.
    PciFunction(toyos_abi::pci::PciFunctionInfo, u8),
    Hda(toyos_abi::hda::HdaInfo, Arc<SharedMemObject>),
    VirtioSound(toyos_abi::virtio_sound::VirtioSoundInfo, Arc<SharedMemObject>),
    /// Which partition, how long, and both its GUIDs; the view it moves blocks
    /// through is the claim's own (`device::Claim::partition`).
    Partition(toyos_abi::part::PartitionInfo),
}

/// The two scanout buffers and the cursor plane.
pub struct FramebufferBuffers {
    pub scanout: [Arc<SharedMemObject>; 2],
    pub cursor: Arc<SharedMemObject>,
}

// MAP is required; DUP and TRANSFER let a daemon hand the buffer to another process.
const BUFFER_RIGHTS: Rights = Rights::MAP.union(Rights::DUP).union(Rights::TRANSFER);

fn install_buffers(
    table: &mut HandleTable,
    buffers: &[&Arc<SharedMemObject>],
) -> Result<alloc::vec::Vec<RawHandle>, SyscallError> {
    let objects = buffers
        .iter()
        .map(|b| (KObjectRef::SharedMem((*b).clone()), BUFFER_RIGHTS))
        .collect();
    table.install_all(objects).map_err(|_| SyscallError::ResourceExhausted)
}

impl DeviceInfo {
    /// The description as bytes, with a handle installed for every named buffer.
    // All-or-nothing: a table too full for the whole batch installs none and leaves
    // `described.bytes` unset, so the next read re-mints instead of binding stranded handles.
    fn mint(&self, table: &mut HandleTable) -> Result<Box<[u8]>, SyscallError> {
        Ok(match self {
            Self::Events => Box::new([]),
            Self::Framebuffer(info, buffers) => {
                let mut info = *info;
                let h = install_buffers(
                    table,
                    &[&buffers.scanout[0], &buffers.scanout[1], &buffers.cursor],
                )?;
                info.scanout = [h[0], h[1]];
                info.cursor = h[2];
                info.as_bytes().into()
            }
            // Nothing to install: every address in it is a size, and the memory
            // is what `SYS_DEVICE_DMA_ALLOC` answers later.
            Self::PciFunction(info, _) => info.as_bytes().into(),
            Self::Partition(info) => info.as_bytes().into(),
            Self::Hda(info, pcm) => {
                let mut info = *info;
                info.pcm = install_buffers(table, &[pcm])?[0];
                info.as_bytes().into()
            }
            Self::VirtioSound(info, dma) => {
                let mut info = *info;
                info.dma = install_buffers(table, &[dma])?[0];
                info.as_bytes().into()
            }
        })
    }
}

/// One process's exclusive hold on a device.
pub struct DeviceClaim {
    pub(super) core: ObjectCore,
    class: DeviceType,
    /// The `pcidev` slot for a claim on one, read without the `described` lock:
    /// a poll's readiness check and a `close` are both places that must not
    /// take it.
    pci_slot: Option<u8>,
    // No Rights::DUP: at most one handle exists, so info_read needs no per-handle state.
    info_read: AtomicBool,
    described: crate::sync::Lock<Described>,
    reference: Held<Claim>,
}

// Minted once: re-minting on a second read would leak a handle each time.
struct Described {
    info: DeviceInfo,
    // Shares this lock with `info`: SYS_GPU_SET_RESOLUTION must replace both together.
    bytes: Option<Box<[u8]>>,
}

impl DeviceClaim {
    pub fn new(class: DeviceType, info: DeviceInfo, claim: Claim) -> Arc<Self> {
        let pci_slot = match &info {
            DeviceInfo::PciFunction(_, slot) => Some(*slot),
            _ => None,
        };
        Arc::new(Self {
            core: Self::new_core(),
            class,
            pci_slot,
            info_read: AtomicBool::new(false),
            described: crate::sync::Lock::new(Described { info, bytes: None }),
            reference: Held::new(claim),
        })
    }

    pub fn class(&self) -> DeviceType {
        self.class
    }

    /// Which `pcidev` slot this claim drives, for a claim on a PCI function.
    ///
    /// Every call the substrate answers goes through this: the handle is the
    /// authority and the slot is what it names.
    pub fn pci_slot(&self) -> Option<usize> {
        self.pci_slot.map(usize::from)
    }

    /// The view a partition claim transfers through: `None` for a claim on
    /// anything else, and once the last handle has let the partition go.
    ///
    /// A clone, and a clone holds the partition for as long as it lives — so
    /// one is kept across a device operation and never across a wait, where a
    /// killed thread's stack could strand it.
    pub fn partition_view(&self) -> Option<crate::block::Partition> {
        self.reference.with(|claim| claim.partition().cloned()).flatten()
    }

    /// The block device and unique GUID of the partition a partition claim is
    /// on; `None` for a claim on anything else, and once the last handle has
    /// let the partition go.
    pub fn partition_on(&self) -> Option<(crate::block::DeviceId, [u8; 16])> {
        let unique = match &self.described.lock().info {
            DeviceInfo::Partition(info) => info.unique_guid,
            _ => return None,
        };
        let device = self.reference.with(|claim| claim.partition().map(|view| view.device_id())).flatten()?;
        Some((device, unique))
    }

    pub fn info_read(&self) -> bool {
        self.info_read.load(Ordering::Relaxed)
    }

    /// Write this claim's description into `buf`, minting handles on first call only.
    pub fn describe(
        &self,
        table: &mut HandleTable,
        buf: &mut crate::user_ptr::UserBytesMut,
    ) -> u64 {
        let mut described = self.described.lock();
        if described.bytes.is_none() {
            match described.info.mint(table) {
                Ok(minted) => described.bytes = Some(minted),
                Err(e) => return e.to_u64(),
            }
        }
        let bytes = described.bytes.as_deref().expect("just minted");
        let count = buf.len().min(bytes.len());
        buf.write_at(0, &bytes[..count]);
        self.info_read.store(true, Ordering::Relaxed);
        count as u64
    }

    /// Replace the description, for a mode set that reallocated the buffers — old handles keep working; nothing is revoked.
    pub fn remint(
        &self,
        table: &mut HandleTable,
        info: DeviceInfo,
    ) -> Result<Box<[u8]>, SyscallError> {
        let mut described = self.described.lock();
        let minted = info.mint(table)?;
        described.info = info;
        described.bytes = Some(minted.clone());
        Ok(minted)
    }
}

/// Negative control for the mint's all-or-nothing rule: a three-handle
/// framebuffer batch minted into a table with room for two must install none.
#[cfg(feature = "boot-actuators")]
pub(crate) fn mint_rollback_selftest() {
    use crate::object::shm::{Region, SharedMemObject};
    let mut table = HandleTable::new();
    table.stage_room(2);
    let buffers = FramebufferBuffers {
        scanout: [SharedMemObject::over(Region::empty()), SharedMemObject::over(Region::empty())],
        cursor: SharedMemObject::over(Region::empty()),
    };
    let info = DeviceInfo::Framebuffer(
        FramebufferInfo {
            scanout: [toyos_abi::HANDLE_INVALID; 2],
            cursor: toyos_abi::HANDLE_INVALID,
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: 0,
            flags: 0,
        },
        buffers,
    );
    let before = table.iter().count();
    let refused = info.mint(&mut table).is_err();
    let installed = table.iter().count() - before;
    let verdict = if refused && installed == 0 { "PASS" } else { "FAIL" };
    crate::log!("leak-selftest: device-mint {verdict} (refused={refused} installed={installed})");
}

// Released on last handle, not last Arc: a parked daemon can strand an Arc without releasing.
impl ZeroHandles for DeviceClaim {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}

/// One holder's view of the machine's console — each holder gets its own, never a shared handle.
/// A line it writes reaches the serial console and is a record tagged with the holder's name.
pub struct ConsoleObject {
    pub(super) core: ObjectCore,
    // Lock order process_data -> line -> BackendGuard; taking them in reverse deadlocks.
    line: crate::sync::Lock<crate::drivers::serial::ConsoleLine>,
}

impl ConsoleObject {
    /// `name` is what the holder's records are tagged with: its process table name.
    pub fn new(name: [u8; crate::process::THREAD_NAME_LEN]) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            line: crate::sync::Lock::new(crate::drivers::serial::ConsoleLine::new(Some(
                crate::log::spoken::Speaker::new(name),
            ))),
        })
    }

    /// Take a userland write, emitting every whole line it completes.
    pub fn write(&self, buf: &crate::user_ptr::UserBytes) {
        // Negative control: bypasses buffering so console_line_atomicity reds if this breaks.
        if crate::actuator::console_unbuffered() {
            crate::drivers::serial::write_console(buf);
            return;
        }
        self.line.lock().write(buf);
    }
}

// Flushes the partial line on last handle instead of dropping it.
impl Drop for ConsoleObject {
    fn drop(&mut self) {
        self.line.lock().finish();
    }
}
