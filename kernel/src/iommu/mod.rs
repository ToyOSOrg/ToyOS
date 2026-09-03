//! The unit that decides what a device may reach.
//!
//! Inventories the machine's IOMMU units, gives every enumerated PCI function an identity-mapped context entry, turns translation on, remaps every interrupt source through a source-id-verified table entry, and hands a driver an address space of its own to put its DMA in; an unusable unit is logged and left off rather than halting boot. Names above `vtd/` stay backend-neutral so a second backend drops in without moving the seam.
//!
//! The refusal is deliberately not yet built: landing it before any userspace driver exists would cost every machine and protect nothing.
//!
//! `trait Iommu` is deliberately not added: with one backend it would have a single implementor.

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
    /// The domain every kernel driver that has not moved is still on maps a device address to the physical address it equals.
    ///
    /// The single site that policy is stated in, so the stage that moves the last driver deletes it and the compiler flags every site that assumed it.
    pub(in crate::iommu) const fn identity(phys: u64) -> Self {
        Self(phys)
    }

    /// An address a domain's allocator handed out, which is nothing else's address.
    pub(in crate::iommu) const fn translated(at: u64) -> Self {
        Self(at)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// A device address space. Never 0, which an all-zero context entry also names.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DomainId(u16);

impl DomainId {
    pub(in crate::iommu) const fn new(id: u16) -> Self {
        assert!(id != 0);
        Self(id)
    }

    pub(in crate::iommu) const fn raw(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for DomainId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "domain{}", self.0)
    }
}

/// Why a device got no address space of its own, or nothing put in one. Carried
/// rather than collapsed: one message for all of them sends whoever reads it
/// looking in the wrong place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IommuError {
    NoUnit,
    /// The units disagree on the depth a domain's tables would be built at.
    WidthsDisagree,
    DomainsExhausted(u32),
    AddressesExhausted(u8),
    /// Not a whole number of the 2 MiB leaves this kernel writes.
    Unaligned(u64),
    NotMapped(Iova),
}

impl core::fmt::Display for IommuError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoUnit => write!(f, "no unit on this machine translates"),
            Self::WidthsDisagree => {
                write!(f, "this machine's units disagree on the address width a domain covers")
            }
            Self::DomainsExhausted(ceiling) => {
                write!(f, "every one of this machine's {ceiling} domains is taken")
            }
            Self::AddressesExhausted(bits) => {
                write!(f, "a domain's {bits} bits of device address are all handed out")
            }
            Self::Unaligned(at) => write!(f, "{at:#x} is not a 2 MiB boundary"),
            Self::NotMapped(at) => write!(f, "{:#x} is not mapped in this domain", at.raw()),
        }
    }
}

/// Where a device's addresses come from. Not a bare `DomainId`: a machine with
/// no unit has to be something this type can say, or every driver grows the
/// same branch.
#[derive(Clone, Copy)]
pub enum DeviceSpace {
    /// Nothing translates here, so a device address is a physical address.
    Untranslated,
    /// The device reaches exactly what is mapped in this and nothing else.
    Own(DomainId),
}

impl DeviceSpace {
    /// One of a device's own, or the machine's own with the reason — the same
    /// policy an unusable unit gets.
    pub fn create() -> Self {
        match vtd::domain::create() {
            Ok(id) => Self::Own(id),
            Err(why) => {
                log!("iommu: no domain of its own for a device: {why}");
                Self::Untranslated
            }
        }
    }

    /// Put `bytes` of physical memory at `phys` in this space and return the
    /// address the device must be programmed with.
    ///
    /// Read and write both, always: nothing here can give a permission set a
    /// second value. The only leaf is 2 MiB, coarser than any split a driver's
    /// pools offer, and QEMU drops an access its cached translation denies
    /// rather than recording a fault — unexpressible and unobservable both.
    pub fn map(self, phys: u64, bytes: u64) -> Result<u64, IommuError> {
        match self {
            Self::Untranslated => Ok(phys),
            Self::Own(id) => vtd::domain::map(id, phys, bytes).map(Iova::raw),
        }
    }

    /// Take `bytes` at `at` back, so the pages behind them can be reused.
    pub fn unmap(self, at: u64, bytes: u64) -> Result<(), IommuError> {
        match self {
            Self::Untranslated => Ok(()),
            Self::Own(id) => vtd::domain::unmap(id, Iova::translated(at), bytes),
        }
    }

    /// Move `bus:device.function` onto this space; every mapping it needs is in
    /// place first, since the device is translating the moment this returns.
    pub fn attach(self, bus: u8, device: u8, function: u8) {
        if let Self::Own(id) = self {
            vtd::domain::attach(StreamId::pci(bus, device, function), id);
        }
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

/// How a source must address its interrupt. Not a yes/no: a caller that folded
/// the third answer into [`Delivery::Direct`] would write a message the unit
/// blocks and lose the device in silence.
pub enum Delivery<T> {
    /// No unit remaps interrupts on this machine; write what has always been written.
    Direct,
    /// Write this instead — the interrupt now reaches its destination through the unit.
    Remapped(T),
    /// The unit remaps and this source has no entry; the caller refuses the device.
    Refused(Refused),
}

/// Why a source could not be given an entry. Carried rather than collapsed:
/// one message for all three sends whoever reads it looking in the wrong place.
#[derive(Clone, Copy)]
pub enum Refused {
    /// Wider than the destination an entry holds without extended interrupt mode.
    DestinationTooWide(u32),
    /// Every entry in the table is already spoken for.
    TableFull,
    /// Firmware's device scopes named no requester id for this interrupt controller.
    ControllerUnnamed(u8),
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DestinationTooWide(id) => {
                write!(f, "apic id {id:#x} does not fit a remapping entry's destination")
            }
            Self::TableFull => write!(f, "the interrupt remapping table is full"),
            Self::ControllerUnnamed(id) => {
                write!(f, "firmware named no requester id for interrupt controller {id}")
            }
        }
    }
}

pub struct MsiMessage {
    pub address: u32,
    pub data: u32,
}

pub struct PinRedirect {
    pub low: u32,
    pub high: u32,
}

/// Where `bus:device.function`'s message-signalled interrupt must point. Takes
/// the triple, not a [`StreamId`]: what a requester id is stays in this module.
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
        Ok(msi) => Delivery::Remapped(MsiMessage { address: msi.address, data: msi.data }),
        Err(why) => Delivery::Refused(why),
    }
}

pub fn remap_pin(apic_id: u8, vector: u8, dest: u32, level: bool) -> Delivery<PinRedirect> {
    if !vtd::interrupt::is_armed() {
        return Delivery::Direct;
    }
    match vtd::interrupt::pin(apic_id, vector, dest, level) {
        Ok(pin) => Delivery::Remapped(PinRedirect { low: pin.low, high: pin.high }),
        Err(why) => Delivery::Refused(why),
    }
}

/// Reached from the IDT gate the unit's own `FEDATA` names.
///
/// Fires when a device has been told no, so what it reports is a bug in whoever owns that device, not in the IOMMU.
pub fn fault_interrupt() {
    vtd::fault::service();
}
