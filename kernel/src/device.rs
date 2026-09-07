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
/// so what it gives back is its `pcidev` slot.
enum Claimed {
    Class(DeviceType),
    PciFunction(usize),
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
}

impl Drop for Claim {
    fn drop(&mut self) {
        match self.what {
            Claimed::Class(class) => *taken(class).lock() = false,
            // Bus mastering off, then the domain, then the pages: `release`
            // owns that order, and this is where a dying process reaches it.
            Claimed::PciFunction(slot) => crate::pcidev::release(slot),
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
/// `selector` says *which* device where the class alone does not — today that
/// is a PCI function's vendor and device id, and every other class ignores it.
pub fn try_claim(class: DeviceType, selector: u64) -> Result<Arc<DeviceClaim>, ClaimError> {
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
            let id = toyos_abi::syscall::PciId::from_wire(selector)
                .ok_or(ClaimError::Absent)?;
            // The slot's own guard, taken inside: a PCI claim's exclusivity is
            // per function rather than per class, so there is no flag here to
            // acquire first.
            let (info, slot, claim) = crate::pcidev::claim(id)?;
            Ok(DeviceClaim::new(class, DeviceInfo::PciFunction(info, slot), claim))
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
