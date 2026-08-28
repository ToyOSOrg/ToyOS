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

/// A move-only proof that a device class is claimed; at most one exists per class.
///
/// The exclusivity reaches userland because `DeviceClaim` is created without
/// `Rights::DUP`, so at most one handle to it can ever exist.
pub struct Claim {
    class: DeviceType,
}

impl Claim {
    fn acquire(class: DeviceType) -> Result<Self, ClaimError> {
        let mut held = taken(class).lock();
        if *held {
            return Err(ClaimError::Owned);
        }
        *held = true;
        Ok(Self { class })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        *taken(self.class).lock() = false;
    }
}

pub fn set_framebuffer_info(screen: Screen) {
    // Mouse motion maps into a fixed 0..32767 space scaled by this geometry, so it must track every mode change.
    crate::mouse::set_screen(screen.info.width, screen.info.height);
    *FB_INFO.lock() = Some(screen);
}

/// Why a claim did not succeed — distinguishes "no such device" from "already held" so callers can degrade correctly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimError {
    /// Another process holds the claim.
    Owned,
    /// This machine has no such device — no driver ever registered one.
    Absent,
}

/// Try to claim exclusive access to a device.
///
/// `Claim` lives on this stack frame until the returned object takes it, so a
/// failure after `acquire` cannot leave a class held by nobody.
pub fn try_claim(class: DeviceType) -> Result<Arc<DeviceClaim>, ClaimError> {
    // Availability is checked before acquiring, so an absent device reports `Absent`, not `Owned`.
    match class {
        DeviceType::Keyboard => {
            let claim = Claim::acquire(class)?;
            // Keystrokes queued before this claim belong to no one; delivering them would leak them to the new owner.
            keyboard::discard_queued();
            Ok(DeviceClaim::new(class, DeviceInfo::Events, claim))
        }
        DeviceType::Mouse => {
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
        DeviceType::Nic => {
            let (info, dma) = crate::net::nic_info().ok_or(ClaimError::Absent)?;
            let claim = Claim::acquire(class)?;
            Ok(DeviceClaim::new(class, DeviceInfo::Nic(info, shm(dma)), claim))
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
