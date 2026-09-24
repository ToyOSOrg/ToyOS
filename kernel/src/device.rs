use alloc::sync::Arc;

use crate::object::device::{DeviceClaim, DeviceInfo, FramebufferBuffers};
use crate::object::shm::Region;
use crate::{keyboard, mouse};
use toyos_abi::FramebufferInfo;
use crate::sync::Lock;
pub use toyos_abi::syscall::DeviceType;

// Tracks occupancy only, not an owner — every device syscall already carries the claim handle.
static TAKEN: [Lock<bool>; DeviceType::ALL.len()] =
    [const { Lock::new(false) }; DeviceType::ALL.len()];
static FB_INFO: Lock<Option<Screen>> = Lock::new(None);

/// What the display driver published, as a claim needs it.
///
/// Screen stores `Region`s rather than `SharedMemObject`s because each claim
/// mints its own object, and a `SharedMemObject` is retired once its handle
/// count reaches zero while the screen itself outlives any single claim.
#[derive(Clone)]
pub struct Screen {
    pub info: FramebufferInfo,
    pub scanout: [Region; 2],
    pub cursor: Region,
}

fn taken(class: DeviceType) -> &'static Lock<bool> {
    &TAKEN[DeviceType::ALL
        .iter()
        .position(|c| *c == class)
        .expect("`DeviceType::ALL` names every class")]
}

/// A move-only proof that a device is claimed; at most one exists per device.
///
/// The exclusivity reaches userland because `DeviceClaim` is created without
/// `Rights::DUP`, so at most one handle to it can ever exist.
pub struct Claim {
    what: Claimed,
}

/// What one claim holds. A class is at most one device on this machine and a
/// per-class flag says whether it is taken; a PCI function is one of several,
/// so what it gives back is its `pcidev` slot; a partition's exclusivity is its
/// view's own hold on the blocks (`block::Partition::of`).
enum Claimed {
    Class(DeviceType),
    PciFunction(usize),
    Partition(crate::block::Partition),
}

impl Claim {
    fn acquire(class: DeviceType) -> Result<Self, ClaimError> {
        let mut held = taken(class).lock();
        if *held {
            return Err(ClaimError::Owned);
        }
        *held = true;
        Ok(Self { what: Claimed::Class(class) })
    }

    /// The guard for a `pcidev` slot the caller has already reserved.
    ///
    /// It exists from the moment the slot is taken, so a bring-up that refuses
    /// half way through frees the slot on the way out rather than stranding it.
    pub(crate) fn pci(slot: usize) -> Self {
        Self { what: Claimed::PciFunction(slot) }
    }

    /// The view a partition claim transfers through, or `None` for any other.
    pub(crate) fn partition(&self) -> Option<&crate::block::Partition> {
        match &self.what {
            Claimed::Partition(view) => Some(view),
            Claimed::Class(_) | Claimed::PciFunction(_) => None,
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        match self.what {
            Claimed::Class(class) => *taken(class).lock() = false,
            // Bus mastering off, then the domain, then the pages: `release`
            // owns that order, and this is where a dying process reaches it.
            Claimed::PciFunction(slot) => crate::pcidev::release(slot),
            // The view drops with this, and its hold with the last clone of it.
            Claimed::Partition(_) => {}
        }
    }
}

pub fn set_framebuffer_info(screen: Screen) {
    // Mouse motion maps into a fixed 0..32767 space scaled by this geometry, so it must track every mode change.
    crate::mouse::set_screen(screen.info.width, screen.info.height);
    *FB_INFO.lock() = Some(screen);
}

/// Why a claim did not succeed. Carried apart rather than collapsed: a caller
/// degrades on "no such device" and reports the rest, and one word for all of
/// them sends whoever reads the line looking in the wrong place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimError {
    /// Another process holds the claim.
    Owned,
    /// This machine has no such device — no driver ever registered one.
    Absent,
    /// The name matches more than one function on this machine. Refused rather
    /// than resolved to the first: a config asking for one of two identical
    /// cards cannot say which, and picking would be the kernel deciding.
    Ambiguous,
    /// A driver in this kernel already binds that function; two drivers on one
    /// device is not something a claim may create.
    KernelDriven,
    /// Every slot that can carry a driven function is taken.
    Exhausted,
    /// The function is there and could not be handed over — no MSI-X, no
    /// window to move a BAR into, an address it did not take. The kernel names
    /// which at the site.
    Unusable,
}

/// Try to claim exclusive access to a device.
///
/// `Claim` lives on this stack frame until the returned object takes it, so a
/// failure after `acquire` cannot leave a device held by nobody.
///
/// `selector` says *which* device where the class alone does not — a PCI
/// function's vendor and device id in its first word, a partition's GUID in
/// both — and every other class ignores it.
pub fn try_claim(class: DeviceType, selector: [u64; 2]) -> Result<Arc<DeviceClaim>, ClaimError> {
    // Availability is checked before acquiring, so an absent device reports `Absent`, not `Owned`.
    match class {
        DeviceType::Keyboard => {
            // A claim is evidence: with no i8042 keyboard armed and no xHCI
            // controller bound, nothing can ever feed the stream it names.
            if !keyboard::source_exists() {
                return Err(ClaimError::Absent);
            }
            let claim = Claim::acquire(class)?;
            // Keystrokes queued before this claim belong to no one; delivering them would leak them to the new owner.
            keyboard::discard_queued();
            Ok(DeviceClaim::new(class, DeviceInfo::Events, claim))
        }
        DeviceType::Mouse => {
            if !mouse::source_exists() {
                return Err(ClaimError::Absent);
            }
            let claim = Claim::acquire(class)?;
            mouse::discard_queued();
            Ok(DeviceClaim::new(class, DeviceInfo::Events, claim))
        }
        DeviceType::Framebuffer => {
            let screen = (*FB_INFO.lock()).clone().ok_or(ClaimError::Absent)?;
            let claim = Claim::acquire(class)?;
            crate::drivers::panic_console::screen_claimed_by_userland();
            Ok(DeviceClaim::new(class, framebuffer_info(screen), claim))
        }
        DeviceType::PciFunction => {
            let id = toyos_abi::syscall::PciId::from_wire(selector[0])
                .ok_or(ClaimError::Absent)?;
            // The slot's own guard, taken inside: a PCI claim's exclusivity is
            // per function rather than per class, so there is no flag here to
            // acquire first.
            let (info, slot, claim) = crate::pcidev::claim(id)?;
            Ok(DeviceClaim::new(class, DeviceInfo::PciFunction(info, slot), claim))
        }
        DeviceType::Partition | DeviceType::PartitionOfType => {
            let guid = toyos_abi::part::PartGuid::from_wire(selector);
            let name = match class {
                DeviceType::Partition => toyos_abi::part::PartitionName::Unique(guid),
                _ => toyos_abi::part::PartitionName::OfType(guid),
            };
            let found = crate::gpt::claimable(name)?;
            let view = partition_view(&found)?;
            let info = toyos_abi::part::PartitionInfo {
                blocks: view.block_count(),
                unique_guid: found.unique.0,
                type_guid: found.ty.0,
            };
            let claim = Claim { what: Claimed::Partition(view) };
            Ok(DeviceClaim::new(class, DeviceInfo::Partition(info), claim))
        }
        DeviceType::HdaAudio => {
            let (info, pcm) = crate::drivers::hda::info().ok_or(ClaimError::Absent)?;
            let claim = Claim::acquire(class)?;
            Ok(DeviceClaim::new(class, DeviceInfo::Hda(info, shm(pcm)), claim))
        }
        DeviceType::VirtioSound => {
            let (info, dma) = crate::drivers::virtio_sound::info().ok_or(ClaimError::Absent)?;
            let claim = Claim::acquire(class)?;
            Ok(DeviceClaim::new(class, DeviceInfo::VirtioSound(info, shm(dma)), claim))
        }
    }
}

/// The unit a claim transfers in is the block layer's, so a partition that is
/// whole blocks of one is whole blocks of the other.
const _: () = assert!(toyos_abi::part::BLOCK_BYTES as u64 == crate::mm::PAGE_SIZE);

/// The claim's view of `found`, held against every other holder of any of its
/// blocks. A partition the kernel mounted is the kernel's, one another claim
/// holds is that claim's, and one that begins or ends inside a block would
/// share that block with its neighbour, so it is not claimable at all.
fn partition_view(found: &crate::gpt::Claimable) -> Result<crate::block::Partition, ClaimError> {
    use crate::block::{Holder, ViewRefused};
    let volume = found.volume;
    let guid = found.unique;
    let handle = crate::block::open(volume.device).ok_or(ClaimError::Absent)?;
    let unit = toyos_abi::part::BLOCK_BYTES as u64;
    let lba = volume.lba_bytes as u64;
    let (Some(start), Some(len)) =
        (volume.start_lba.checked_mul(lba), volume.blocks.checked_mul(lba))
    else {
        log!("partclaim: {guid} claims LBA {}+{} of {lba} bytes, which is no byte range",
            volume.start_lba, volume.blocks);
        return Err(ClaimError::Unusable);
    };
    if start % unit != 0 || len % unit != 0 {
        log!(
            "partclaim: {guid} is at {start}+{len} bytes on device {}, which is not whole \
             {unit}-byte blocks — a transfer would share one with its neighbour",
            volume.device
        );
        return Err(ClaimError::Unusable);
    }
    match crate::block::Partition::of(handle, start / unit, len / unit, Holder::Claim) {
        Ok(view) => Ok(view),
        Err(ViewRefused::Held(Holder::Kernel(what))) => {
            log!("partclaim: {guid} is held by the kernel ({what}) and cannot be claimed");
            Err(ClaimError::KernelDriven)
        }
        Err(ViewRefused::Held(Holder::Claim)) => Err(ClaimError::Owned),
        Err(ViewRefused::OffDevice) => {
            log!("partclaim: {guid} is at {start}+{len} bytes, off device {}", volume.device);
            Err(ClaimError::Unusable)
        }
    }
}

fn shm(region: Region) -> Arc<crate::object::shm::SharedMemObject> {
    crate::object::shm::SharedMemObject::over(region)
}

/// The description a framebuffer claim answers with, over freshly minted buffer objects.
pub fn framebuffer_info(screen: Screen) -> DeviceInfo {
    let Screen { info, scanout: [front, back], cursor } = screen;
    DeviceInfo::Framebuffer(
        info,
        FramebufferBuffers { scanout: [shm(front), shm(back)], cursor: shm(cursor) },
    )
}
