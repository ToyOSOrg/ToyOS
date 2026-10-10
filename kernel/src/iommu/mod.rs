//! The unit that decides what a device may reach.
//!
//! Inventories the machine's IOMMU units, turns translation on, and hands a driver an address space of its own to put its DMA in; an unusable unit is logged and left off rather than halting boot. Names above the backend stay backend-neutral, and what every backend decides alike is declared here once: a domain's addresses ([`window`]) and what a fault record ends ([`fault`]).
//!
//! The refusal is deliberately not yet built for a driver in this kernel: landing it before any userspace driver exists would cost every machine and protect nothing. A function a *process* drives is the other case and is refused ([`OwnSpace`], [`remapping`]), because a descriptor it writes a physical address into is an arbitrary read and write over all of memory, and a message it sends unremapped is any vector at any CPU; its message is its claim slot's own entry ([`Remapped`]).
//!
//! `trait Iommu` is deliberately not added: with one backend it would have a single implementor.

// CI runs kernel clippy with `-D warnings`, so an undocumented `unsafe` block anywhere in this module tree fails the build.
#![warn(clippy::undocumented_unsafe_blocks)]

use crate::arch::iommu_unit as unit;

pub(crate) mod fault;
pub(crate) mod window;

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
    pub(crate) const fn pci(bus: u8, device: u8, function: u8) -> Self {
        Self(((bus as u32) << 8) | ((device as u32) << 3) | function as u32)
    }

    /// The bus half of the id.
    pub(crate) const fn bus(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub(crate) const fn devfn(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// The 16-bit requester id a source-id check compares against; `pci` is the only constructor, so it always fits.
    pub(crate) const fn requester(self) -> u16 {
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
    pub(crate) const fn identity(phys: u64) -> Self {
        Self(phys)
    }

    /// An address a domain's allocator handed out, which is nothing else's address.
    pub(crate) const fn translated(at: u64) -> Self {
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
    pub(crate) const fn new(id: u16) -> Self {
        assert!(id != 0);
        Self(id)
    }

    pub(crate) const fn raw(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for DomainId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "domain{}", self.0)
    }
}

/// Why a device got no address space of its own, or nothing put in one; carried
/// rather than collapsed, since one message for all of them sends whoever reads
/// it looking in the wrong place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IommuError {
    NoUnit,
    /// The units disagree on the depth a domain's tables would be built at.
    WidthsDisagree,
    DomainsExhausted(u32),
    /// Every address up to this ceiling is handed out.
    AddressesExhausted(u64),
    /// What this machine's units translate does not reach above its memory, so
    /// a device window has nowhere to sit that a stale descriptor would miss.
    WindowBelowMemory { translatable: u8, floor: u64, top: u64 },
    /// From the floor to the first root-bridge window or reserved region above
    /// it is less than a new domain was asked to hand out.
    NoRoom { floor: u64, ceiling: u64, room: u64 },
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
            Self::AddressesExhausted(ceiling) => {
                write!(f, "a domain's device addresses up to {ceiling:#x} are all handed out")
            }
            Self::WindowBelowMemory { translatable, floor, top } => write!(
                f,
                "this machine's units translate {translatable} bits, whose device window would \
                 start at {floor:#x}, at or below the {top:#x} its memory reaches"
            ),
            Self::NoRoom { floor, ceiling, room } => write!(
                f,
                "a domain's addresses from {floor:#x} end at {ceiling:#x}, where a root bridge's \
                 window or a reserved region begins, short of the {room:#x} bytes asked of it"
            ),
            Self::Unaligned(at) => write!(f, "{at:#x} is not a 2 MiB boundary"),
            Self::NotMapped(at) => write!(f, "{:#x} is not mapped in this domain", at.raw()),
        }
    }
}

/// Where a device's addresses come from. Not a bare `DomainId`: a machine with no
/// unit has to be something this type can say, or every driver grows the branch.
#[derive(Clone, Copy)]
pub enum DeviceSpace {
    /// Nothing translates here, so a device address is a physical address.
    Untranslated,
    /// The device reaches exactly what is mapped in this and nothing else.
    Own(DomainId),
}

impl DeviceSpace {
    /// One of a device's own, or the machine's own with the reason. For a
    /// driver **in this kernel**, whose addresses are the kernel's either way.
    pub fn create() -> Self {
        match unit::domain::create(0) {
            Ok((id, _)) => Self::Own(id),
            Err(why) => {
                log!("iommu: no domain of its own for a device: {why}");
                Self::Untranslated
            }
        }
    }

    /// Put `bytes` at `phys` in this space; returns what to program the device
    /// with.
    ///
    /// Read and write both, always: nothing here can give a permission set a
    /// second value. The only leaf is 2 MiB, coarser than any split a driver's
    /// pools offer, and QEMU drops an access its cached translation denies
    /// rather than recording a fault — unexpressible and unobservable both.
    pub fn map(self, phys: u64, bytes: u64) -> Result<u64, IommuError> {
        match self {
            Self::Untranslated => Ok(phys),
            Self::Own(id) => unit::domain::map(id, phys, bytes).map(Iova::raw),
        }
    }

    /// Take `bytes` at `at` back, so the pages behind them can be reused.
    pub fn unmap(self, at: u64, bytes: u64) -> Result<(), IommuError> {
        match self {
            Self::Untranslated => Ok(()),
            Self::Own(id) => unit::domain::unmap(id, Iova::translated(at), bytes),
        }
    }

    /// Move `bus:device.function` onto this space, every mapping it needs already
    /// in place: the device is translating the moment this returns.
    pub fn attach(self, bus: u8, device: u8, function: u8) {
        if let Self::Own(id) = self {
            OwnSpace(id).attach(bus, device, function);
        }
    }
}

/// A device's own address space, for a function a *process* drives: it has no
/// untranslated form, so nothing holding one can hand that process a physical
/// address to write into a descriptor.
#[derive(Clone, Copy)]
pub struct OwnSpace(DomainId);

impl OwnSpace {
    /// One with `room` bytes of it handed out at the address answered, or the
    /// reason there is none: a machine with no unit and a machine out of
    /// domains are both refusals.
    pub fn create(room: u64) -> Result<(Self, u64), IommuError> {
        unit::domain::create(room).map(|(id, at)| (Self(id), at.raw()))
    }

    /// [`DeviceSpace::map`].
    pub fn map(self, phys: u64, bytes: u64) -> Result<u64, IommuError> {
        unit::domain::map(self.0, phys, bytes).map(Iova::raw)
    }

    /// Put `bytes` at `phys` at `at` again, where a device may still be aimed
    /// from a mapping this space took back.
    pub fn map_at(self, at: u64, phys: u64, bytes: u64) -> Result<(), IommuError> {
        unit::domain::map_at(self.0, Iova::translated(at), phys, bytes)
    }

    /// Put `bytes` at `phys` at `at`, inside room [`Self::create`] handed out,
    /// and write no record of it: for a mapping its holder makes and takes back
    /// as often as it likes.
    pub fn place(self, at: u64, phys: u64, bytes: u64) -> Result<(), IommuError> {
        unit::domain::place(self.0, Iova::translated(at), phys, bytes).map(|_| ())
    }

    /// [`DeviceSpace::unmap`].
    pub fn unmap(self, at: u64, bytes: u64) -> Result<(), IommuError> {
        unit::domain::unmap(self.0, Iova::translated(at), bytes)
    }

    /// [`DeviceSpace::attach`].
    pub fn attach(self, bus: u8, device: u8, function: u8) {
        unit::domain::attach(StreamId::pci(bus, device, function), self.0);
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
/// No domain's addresses reach into one of `windows`, the memory firmware says the root bridges decode.
///
/// Calls `unit::init` directly rather than through a dispatch, because x86-64 has one backend and the dispatch is not yet a real seam.
pub fn init(
    rsdp_addr: u64,
    devices: &[crate::drivers::pci::PciDevice],
    windows: &[toyos_abi::boot::RootBridgeWindow],
) {
    unit::init(rsdp_addr, devices, windows);
}

/// How a kernel driver's source must address its interrupt. Not a yes/no: a
/// caller that folded the third answer into [`Delivery::Direct`] would write a
/// message the unit blocks and lose the device in silence.
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

/// Every unit on this machine remaps interrupts, so a claimed function can be
/// given a message only its own entry delivers. Only [`remapping`] makes one.
#[derive(Clone, Copy)]
pub struct Remapping(());

/// No unit remaps this machine's interrupts, so a function a process drives
/// would carry a compatibility-format message, which names any vector at any
/// CPU.
pub struct NotRemapped;

/// Whether a claimed function's message can be remapped: a fact of the
/// machine, fixed before the first driver arms anything.
pub fn remapping() -> Result<Remapping, NotRemapped> {
    if unit::interrupt::is_armed() {
        Ok(Remapping(()))
    } else {
        Err(NotRemapped)
    }
}

/// A claimed function's message: claim slot `slot`'s own remapping entry,
/// written for [`Self::function`] alone.
///
/// The only message a function a process drives is armed with, since
/// [`claim_msi`] is the only thing that makes one; and dropping it puts the
/// entry back to not present, so no refusal after it is written leaves the
/// function an entry it can reach.
pub struct Remapped {
    slot: usize,
    function: crate::drivers::pci::PciDevice,
    address: u32,
    data: u32,
}

impl Remapped {
    pub fn function(&self) -> &crate::drivers::pci::PciDevice {
        &self.function
    }

    pub fn address(&self) -> u32 {
        self.address
    }

    pub fn data(&self) -> u32 {
        self.data
    }

    fn stream(&self) -> StreamId {
        StreamId::pci(self.function.bus, self.function.dev, self.function.func)
    }
}

impl Drop for Remapped {
    fn drop(&mut self) {
        unit::interrupt::release(self.slot, self.stream());
    }
}

/// Write claim slot `slot`'s entry for `function` at `vector`.
pub fn claim_msi(
    _: Remapping,
    slot: usize,
    function: &crate::drivers::pci::PciDevice,
    vector: u8,
) -> Remapped {
    let stream = StreamId::pci(function.bus, function.dev, function.func);
    let msi = unit::interrupt::claim(slot, stream, vector);
    Remapped { slot, function: *function, address: msi.address, data: msi.data }
}

/// Where a kernel driver's `bus:device.function`'s message-signalled interrupt
/// must point. Takes the triple, not a [`StreamId`]: what a requester id is
/// stays in this module.
pub fn remap_msi(
    bus: u8,
    device: u8,
    function: u8,
    vector: u8,
    dest: u32,
) -> Delivery<MsiMessage> {
    if !unit::interrupt::is_armed() {
        return Delivery::Direct;
    }
    match unit::interrupt::msi(StreamId::pci(bus, device, function), vector, dest) {
        Ok(msi) => Delivery::Remapped(MsiMessage { address: msi.address, data: msi.data }),
        Err(why) => Delivery::Refused(why),
    }
}

pub fn remap_pin(apic_id: u8, vector: u8, dest: u32, level: bool) -> Delivery<PinRedirect> {
    if !unit::interrupt::is_armed() {
        return Delivery::Direct;
    }
    match unit::interrupt::pin(apic_id, vector, dest, level) {
        Ok(pin) => Delivery::Remapped(PinRedirect { low: pin.low, high: pin.high }),
        Err(why) => Delivery::Refused(why),
    }
}

/// Record that `bus:device.function` is driven by a process on `slot`, or is no
/// longer driven by one.
///
/// What the fault handler needs it for is its *terminal* action. A stream every
/// driver of which is in this kernel has nothing to hand a fault to, so the
/// response is a halt; a stream a process drives has an owner to refuse, and
/// the machine keeps running. Takes the triple rather than a [`StreamId`], like
/// [`remap_msi`]: what a requester id is stays in this module.
pub fn note_user_owned(bus: u8, device: u8, function: u8, slot: Option<usize>) {
    fault::user_owned(StreamId::pci(bus, device, function), slot);
}

/// Reached from the IDT gate the unit's own `FEDATA` names.
///
/// Fires when a device has been told no, so what it reports is a bug in whoever owns that device, not in the IOMMU.
pub fn fault_interrupt() {
    unit::fault::service();
}
