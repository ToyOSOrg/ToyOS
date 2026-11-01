use alloc::sync::Arc;

use crate::object::device::{DeviceClaim, DeviceInfo};
use toyos_abi::FramebufferInfo;
use crate::sync::Lock;
pub use toyos_abi::syscall::DeviceType;

// Tracks occupancy only, not an owner — every device syscall already carries the claim handle.
static TAKEN: [Lock<bool>; DeviceType::ALL.len()] =
    [const { Lock::new(false) }; DeviceType::ALL.len()];
static FB_INFO: Lock<Option<Screen>> = Lock::new(None);

/// The firmware's framebuffer, as a claim needs it.
///
/// Where the scanout is rather than a `SharedMemObject` over it, because each
/// claim mints its own object, and an aperture is taken back from every
/// process once its handle count reaches zero while the screen outlives any
/// single claim.
#[derive(Clone)]
pub struct Screen {
    pub info: FramebufferInfo,
    pub scanout: Scanout,
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
/// so what it gives back is its `pcidev` slot; an ISA function's and the ACPI
/// fixed hardware's exclusivity is its `isa` row's.
///
/// The slot and the row are held as what their modules' calls take, and the
/// drop of each is its release: a call borrows one from the claim's `Held`, so
/// none is made on a slot or a row its claim has let go.
enum Claimed {
    Class(DeviceType),
    PciFunction(crate::pcidev::Binding),
    Isa(crate::isa::Row),
    Acpi(crate::isa::Row),
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

    /// The claim on a function `pcidev` has bound.
    pub(crate) fn pci(binding: crate::pcidev::Binding) -> Self {
        Self { what: Claimed::PciFunction(binding) }
    }

    /// What `pcidev`'s calls on this claim are made with, or `None` for a
    /// claim on anything but a PCI function.
    pub(crate) fn binding(&self) -> Option<&crate::pcidev::Binding> {
        match &self.what {
            Claimed::PciFunction(binding) => Some(binding),
            Claimed::Class(_) | Claimed::Isa(_) | Claimed::Acpi(_) => None,
        }
    }

    /// What `isa`'s calls on this claim are made with, or `None` for a claim
    /// on anything but an ISA function or the ACPI fixed hardware.
    pub(crate) fn row(&self) -> Option<&crate::isa::Row> {
        match &self.what {
            Claimed::Isa(row) | Claimed::Acpi(row) => Some(row),
            Claimed::Class(_) | Claimed::PciFunction(_) => None,
        }
    }

    /// Register a poll on the watch of the function or row this claim holds.
    pub(crate) fn add_poll(&self, entry: crate::inbox::PollEntry) {
        match &self.what {
            Claimed::PciFunction(binding) => crate::pcidev::add_poll(binding, entry),
            Claimed::Isa(row) | Claimed::Acpi(row) => crate::isa::add_poll(row, entry),
            Claimed::Class(_) => unreachable!("a poll registered through a claim on no function or row"),
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        match &self.what {
            &Claimed::Class(class) => *taken(class).lock() = false,
            // Bus mastering off, then the domain, then the pages: the
            // binding's drop owns that order, and this is where a dying
            // process reaches it.
            Claimed::PciFunction(_) => {}
            // The row's drop is its release.
            Claimed::Isa(_) => {}
            // Back to the mode the firmware handed over, and then the row's drop.
            Claimed::Acpi(_) => crate::arch::acpi_mode::release(),
        }
    }
}

pub fn set_framebuffer_info(screen: Screen) {
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
    /// A driver in this kernel already binds the device; two drivers on one
    /// device is not something a claim may create.
    KernelDriven,
    /// This machine has no interrupt left to give the function: every vector,
    /// remapping entry or LPI a claim spends is held; or no memory is left for
    /// the claim.
    Exhausted,
    /// The function is there and could not be handed over — no MSI-X, no
    /// window to move a BAR into, an address it did not take. The kernel names
    /// which at the site.
    Unusable,
}

/// No memory for the claim's object: the device stays unclaimed.
impl From<crate::mm::charge::Exhausted> for ClaimError {
    fn from(crate::mm::charge::Exhausted: crate::mm::charge::Exhausted) -> Self {
        Self::Exhausted
    }
}

/// Try to claim exclusive access to a device.
///
/// `Claim` lives on this stack frame until the returned object takes it, so a
/// failure after `acquire` cannot leave a device held by nobody.
///
/// `selector` says *which* device where the class alone does not — a PCI
/// function's arrival in its first word, an ISA set's ports and lines — and the
/// syscall has refused a word the class does not read.
pub fn try_claim(class: DeviceType, selector: [u64; 2]) -> Result<Arc<DeviceClaim>, ClaimError> {
    // Availability is checked before acquiring, so an absent device reports `Absent`, not `Owned`.
    match class {
        DeviceType::Framebuffer => {
            let screen = (*FB_INFO.lock()).clone().ok_or(ClaimError::Absent)?;
            let claim = Claim::acquire(class)?;
            crate::drivers::panic_console::screen_claimed_by_userland();
            Ok(DeviceClaim::new(class, framebuffer_info(screen)?, claim)?)
        }
        DeviceType::PciFunction => {
            // A number past a `u32` is an arrival no function had.
            let arrival = u32::try_from(selector[0]).map_err(|_| ClaimError::Absent)?;
            // The function's own guard, taken inside: a PCI claim's exclusivity is
            // per function rather than per class, so there is no flag here to
            // acquire first.
            let windows = toyos_abi::pci::FirmwareWindows::from_mask(selector[1]).ok_or(ClaimError::Unusable)?;
            let (info, claim) = crate::pcidev::claim(toyos_abi::inventory::Arrival(arrival))?;
            // Granted before the claim exists for anyone: a window is the
            // mint's, and nothing after it can add one. A window this function
            // has none of refuses the mint, and the claim's drop releases it.
            for window in windows.iter() {
                claim.binding().map(|binding| crate::pcidev::grant_firmware(binding, window)).transpose().map_err(|_| ClaimError::Unusable)?;
            }
            Ok(DeviceClaim::new(class, DeviceInfo::PciFunction(info), claim)?)
        }
        DeviceType::Isa => {
            let set = toyos_abi::syscall::IsaId::from_wire(selector).ok_or(ClaimError::Absent)?;
            // The row's own guard, taken inside as a PCI function's is.
            let row = crate::isa::claim(set)?;
            let claim = Claim { what: Claimed::Isa(row) };
            Ok(DeviceClaim::new(class, DeviceInfo::Isa(set), claim)?)
        }
        DeviceType::Acpi => {
            // The row's own guard, taken inside, and the switch to ACPI mode.
            let (row, info) = crate::arch::acpi_mode::claim()?;
            let claim = Claim { what: Claimed::Acpi(row) };
            Ok(DeviceClaim::new(class, DeviceInfo::Acpi(info), claim)?)
        }
    }
}

/// The firmware's framebuffer: whole pages from `phys`, write-combining.
#[derive(Clone, Copy)]
pub struct Scanout {
    pub phys: u64,
    pub size: u64,
}

fn shm(scanout: Scanout) -> Result<Arc<crate::object::shm::SharedMemObject>, crate::mm::charge::Exhausted> {
    crate::object::shm::SharedMemObject::aperture(scanout.phys, scanout.size, crate::mm::policy::CachePolicy::WriteCombining)
}

/// The description a framebuffer claim answers with, over a freshly minted
/// scanout object.
fn framebuffer_info(screen: Screen) -> Result<DeviceInfo, crate::mm::charge::Exhausted> {
    Ok(DeviceInfo::Framebuffer(screen.info, shm(screen.scanout)?))
}
