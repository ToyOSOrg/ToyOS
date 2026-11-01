//! The unit that decides what a device may reach.
//!
//! Inventories the machine's IOMMU units, turns translation on, and hands a driver an address space of its own to put its DMA in; an unusable unit is logged and left off rather than halting boot. Names above the backend stay backend-neutral, and what every backend decides alike is declared here once: a domain's addresses (`kernel::iova`, over what each backend reserves) and what a fault record ends ([`fault`]).
//!
//! The refusal is deliberately not yet built for a driver in this kernel: landing it before any userspace driver exists would cost every machine and protect nothing. A function a *process* drives is the other case and is refused ([`OwnSpace`], [`remapping`]), because a descriptor it writes a physical address into is an arbitrary read and write over all of memory, and a message nothing translates for it alone raises any claim's interrupt; its message raises its own interrupt, translated for it alone ([`Remapped`]).
//!
//! `trait Iommu` is deliberately not added: with one backend it would have a single implementor.

// CI runs kernel clippy with `-D warnings`, so an undocumented `unsafe` block anywhere in this module tree fails the build.
#![warn(clippy::undocumented_unsafe_blocks)]

use crate::arch::iommu_unit as unit;
use crate::arch::message_unit as messages;
pub use kernel::iova::Limit;

pub(crate) mod domains;
pub(crate) mod fault;
#[cfg(feature = "boot-actuators")]
pub(crate) mod testdev;

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
    /// Every allowed address below this limit is handed out.
    AddressesExhausted(Limit),
    /// No declaration of the root bridges' windows is held, so no address is
    /// known to be clear of them.
    Undeclared,
    /// Less than a new domain was asked to hand out is allowed below 4 GiB.
    NoRoom { room: u64 },
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
            Self::AddressesExhausted(limit) => {
                write!(f, "a domain's device addresses below {:#x} are all handed out", limit.end())
            }
            Self::Undeclared => write!(
                f,
                "no declaration of the root bridges' windows is held, so no device address is known \
                 to be clear of them"
            ),
            Self::NoRoom { room } => {
                write!(f, "less than the {room:#x} bytes a new domain hands out first is allowed below 4 GiB")
            }
            Self::Unaligned(at) => write!(f, "{at:#x} is not a 2 MiB boundary"),
            Self::NotMapped(at) => write!(f, "{:#x} is not mapped in this domain", at.raw()),
        }
    }
}

/// A device's own address space, for a function a *process* drives: it has no
/// untranslated form, so nothing holding one can hand that process a physical
/// address to write into a descriptor.
#[derive(Clone, Copy)]
pub struct OwnSpace(DomainId);

impl OwnSpace {
    /// One with `room` bytes of it below 4 GiB handed out at the address
    /// answered, or the reason there is none: a machine with no unit and a
    /// machine out of domains are both refusals.
    ///
    /// What firmware reserved for function `bus:device.function` is mapped in
    /// it at its own addresses, before the function can attach.
    pub fn create(room: u64, bus: u8, device: u8, function: u8) -> Result<(Self, u64), IommuError> {
        let (id, at) = unit::domain::create(room)?;
        unit::domain::keep_reserved(id, StreamId::pci(bus, device, function));
        Ok((Self(id), at.raw()))
    }

    /// Put `bytes` at `phys` in this space, ending at or below `limit`, the
    /// device's own address width; returns what to program the device with.
    ///
    /// Read and write both, always: nothing here can give a permission set a
    /// second value. The only leaf is 2 MiB, and QEMU drops an access its
    /// cached translation denies rather than recording a fault —
    /// unexpressible and unobservable both.
    pub fn map(self, phys: u64, bytes: u64, limit: Limit) -> Result<u64, IommuError> {
        unit::domain::map(self.0, phys, bytes, limit).map(Iova::raw)
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
        unit::domain::place(self.0, Iova::translated(at), phys, bytes)
    }

    /// Take `bytes` at `at` back, so the pages behind them can be reused.
    pub fn unmap(self, at: u64, bytes: u64) -> Result<(), IommuError> {
        unit::domain::unmap(self.0, Iova::translated(at), bytes)
    }

    /// Move `bus:device.function` onto this space, every mapping it needs already
    /// in place: the device is translating the moment this returns.
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
/// No domain's addresses reach into one of `windows`, the root bridges'
/// declared memory, or `None` where no declaration is held ([`window`]).
/// `kernel` is this kernel's image, `[start, end)` physical: what a unit's
/// hand-over keeps out of every device's reach where it can.
pub fn init(
    rsdp_addr: u64,
    devices: &[crate::drivers::pci::PciDevice],
    windows: Option<&toyos_pci::aperture::Declaration<alloc::vec::Vec<toyos_abi::boot::RootBridgeWindow>>>,
    kernel: (u64, u64),
) {
    unit::init(rsdp_addr, devices, windows.map(|declared| declared.windows()), kernel);
}

/// Once, after every kernel driver has taken its functions from firmware and
/// before userland runs: AMD-Vi turns its SMI filters on.
pub fn drivers_settled() {
    unit::drivers_settled();
}

/// Why a source could not be given a message: the architecture's own reasons.
pub use messages::Refused;

/// Every claimed function's message is translated to its own interrupt, and
/// no other's. Only [`remapping`] makes one.
#[derive(Clone, Copy)]
pub struct Remapping(());

/// Why no claimed function's message would reach only its claim's interrupt.
#[derive(Clone, Copy)]
pub enum NotRemapped {
    /// Nothing translates its message, so the message names any interrupt.
    Unremapped,
    /// Remapped, and a write its holder aims at the interrupt range could
    /// still be an SMI no unit filters.
    SmiUnfiltered,
}

/// Whether every message a claimed function sends, whatever its holder
/// writes into its MSI-X table or aims a DMA at, reaches only its claim's
/// interrupt: a fact of the machine, fixed before the first claim. Where it
/// is, the table's BAR is the holder's like any other.
pub fn remapping() -> Result<Remapping, NotRemapped> {
    messages::fences_claims().map(|()| Remapping(()))
}

/// Whether `function` can be given a domain of its own and a remapping entry
/// no other requester reaches, or why not: a function no programmed unit
/// serves, one firmware reserves memory for, or one whose requester id the
/// platform speaks as.
pub fn isolates(function: &crate::drivers::pci::PciDevice) -> Result<(), &'static str> {
    match unit::domain::unclaimable(StreamId::pci(function.bus, function.dev, function.func)) {
        None => Ok(()),
        Some(why) => Err(why),
    }
}

/// A claimed function's message, raising an interrupt of its own and
/// translated for [`Self::function`] alone.
///
/// The only message a function a process drives is armed with, since
/// [`claim_msi`] is the only thing that makes one; and dropping it takes the
/// translation and the interrupt back, so no refusal after it is written
/// leaves the function a message that reaches anything.
pub struct Remapped {
    claimed: messages::Claimed,
    function: crate::drivers::pci::PciDevice,
}

impl Remapped {
    pub fn function(&self) -> &crate::drivers::pci::PciDevice {
        &self.function
    }

    pub fn address(&self) -> u32 {
        self.claimed.msi.address
    }

    pub fn data(&self) -> u32 {
        self.claimed.msi.data
    }

    /// What the message raises, for the record.
    pub fn interrupt(&self) -> impl core::fmt::Display + '_ {
        self.claimed.interrupt()
    }

    fn stream(&self) -> StreamId {
        StreamId::pci(self.function.bus, self.function.dev, self.function.func)
    }
}

impl Drop for Remapped {
    fn drop(&mut self) {
        messages::release(&self.claimed, self.stream());
    }
}

/// A message for `pcidev` function `number`, raising an interrupt taken for
/// it and translated for `function` alone.
pub fn claim_msi(
    _: Remapping,
    number: usize,
    function: &crate::drivers::pci::PciDevice,
) -> Result<Remapped, Refused> {
    let stream = StreamId::pci(function.bus, function.dev, function.func);
    let claimed = messages::claim(number, stream)?;
    Ok(Remapped { claimed, function: *function })
}

/// Reached from the IDT gate the unit's own `FEDATA` names.
///
/// Fires when a device has been told no, so what it reports is a bug in whoever owns that device, not in the IOMMU.
pub fn fault_interrupt() {
    unit::fault::service();
}
