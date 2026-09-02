//! The unit that decides what a device may reach.
//!
//! Inventories the machine's IOMMU units, gives every enumerated PCI function an identity-mapped context entry, and turns translation on; an unusable unit is logged and left off rather than halting boot, and interrupt remapping and per-driver domains are not yet built (`issues/kernel/the-iommu-stops-at-translation.md`). Names above `vtd/` stay backend-neutral so a second backend drops in without moving the seam.
//!
//! The refusal is deliberately not yet built: landing it before any userspace driver exists would cost every machine and protect nothing.
//!
//! `DomainId`, `DmaPerm`, `IommuError`, and `trait Iommu` are deliberately not added: with one domain and one backend, each would be a type with a single value and a single implementor.

// CI runs kernel clippy with `-D warnings`, so an undocumented `unsafe` block anywhere in this module tree fails the build.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod vtd;

/// The address width a device's translations cover.
///
/// `Bits57` is omitted even where a unit advertises it, because it needs a fifth page-table level no machine in reach uses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AddressWidth {
    Bits39,
    Bits48,
}

impl AddressWidth {
    pub const fn bits(self) -> u8 {
        match self {
            Self::Bits39 => 39,
            Self::Bits48 => 48,
        }
    }
}

/// The unit's name for whoever issued a request: VT-d's source-id, an SMMU StreamID.
///
/// `StreamId` is `u32`, wider than VT-d's 16-bit source-id, because an SMMU StreamID is 32 bits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct StreamId(u32);

impl StreamId {
    /// Named `pci`, not `new`: an SMMU StreamID is not always a bus/device/function triple.
    pub(in crate::iommu) const fn pci(bus: u8, device: u8, function: u8) -> Self {
        Self(((bus as u32) << 8) | ((device as u32) << 3) | function as u32)
    }

    /// The bus half of the id.
    pub(in crate::iommu) const fn bus(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub(in crate::iommu) const fn devfn(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// The 16-bit requester id a source-id check compares against; `pci` is the only constructor, so it always fits.
    pub(in crate::iommu) const fn requester(self) -> u16 {
        self.0 as u16
    }
}

/// An address a *device* uses. Never a physical address, never a virtual one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct Iova(u64);

impl Iova {
    /// This kernel always identity-maps: a device address equals its physical address, by policy, never because passthrough is unavailable.
    ///
    /// This constructor is the single site the identity policy is stated in, so the stage that ends identity-mapping deletes it and the compiler flags every site that assumed it.
    pub(in crate::iommu) const fn identity(phys: u64) -> Self {
        Self(phys)
    }

    pub(in crate::iommu) const fn raw(self) -> u64 {
        self.0
    }
}

/// Formats as `bb:dd.f`, the same form `pci::enumerate` prints, so a stream id can be matched against it.
impl core::fmt::Display for StreamId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.0 >> 8, (self.0 >> 3) & 0x1f, self.0 & 0x7)
    }
}

/// Inventories units and gives every enumerated function a context entry, before any driver `init` runs.
///
/// Must run before any driver `init`, because a device must not be able to DMA before its unit is programmed.
///
/// The device list must be the complete enumeration: enabling translation with an unenumerated device left off it can brick the machine's own boot disk.
///
/// Calls `vtd::init` directly rather than through a dispatch, because x86-64 has one backend and the dispatch is not yet a real seam.
pub fn init(rsdp_addr: u64, devices: &[crate::drivers::pci::PciDevice]) {
    vtd::init(rsdp_addr, devices);
}

/// How a source must address its interrupt, which is not a yes/no: a unit that
/// remaps and cannot give this source an entry is a third answer, and a caller
/// that folded it into [`Delivery::Direct`] would write a message the unit
/// blocks and lose the device in silence.
pub enum Delivery<T> {
    /// No unit remaps interrupts on this machine; write what has always been written.
    Direct,
    /// Write this instead — the interrupt now reaches its destination through the unit.
    Remapped(T),
    /// The unit remaps and this source has no entry; the caller refuses the device.
    Refused,
}

/// The address and data a PCI function writes into its MSI or MSI-X registers.
pub struct MsiMessage {
    pub address: u32,
    pub data: u32,
}

/// What a pin's redirection entry carries in place of a destination.
pub struct PinRedirect {
    pub low: u32,
    pub high: u32,
}

/// Where `bus:device.function`'s message-signalled interrupt must point.
///
/// Takes the triple rather than a [`StreamId`], which no driver can build: what
/// a requester id is stays inside this module.
pub fn remap_msi(
    bus: u8,
    device: u8,
    function: u8,
    vector: u8,
    dest: u32,
) -> Delivery<MsiMessage> {
    if !vtd::interrupt::is_armed() {
        return Delivery::Direct;
    }
    match vtd::interrupt::msi(StreamId::pci(bus, device, function), vector, dest) {
        Some(msi) => Delivery::Remapped(MsiMessage { address: msi.address, data: msi.data }),
        None => Delivery::Refused,
    }
}

/// Where a pin on the interrupt controller with `apic_id` must send `vector`.
pub fn remap_pin(apic_id: u8, vector: u8, dest: u32, level: bool) -> Delivery<PinRedirect> {
    if !vtd::interrupt::is_armed() {
        return Delivery::Direct;
    }
    match vtd::interrupt::pin(apic_id, vector, dest, level) {
        Some(pin) => Delivery::Remapped(PinRedirect { low: pin.low, high: pin.high }),
        None => Delivery::Refused,
    }
}

/// Reached from the IDT gate the unit's own `FEDATA` names.
///
/// Fires when a device has been told no, so what it reports is a bug in whoever owns that device, not in the IOMMU.
pub fn fault_interrupt() {
    vtd::fault::service();
}
