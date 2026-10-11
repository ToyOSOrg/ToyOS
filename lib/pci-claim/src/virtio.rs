//! A virtio function's claim, brought to the features its driver and device
//! agreed: the capability walk, the register window, and §3.1.1 up to
//! `FEATURES_OK`, in the order `toyos-virtio`'s types fix.
//!
//! What is the device type's own — its queues, their vectors, its
//! configuration — begins at the [`Setup`] this answers.

use toyos::shm::SharedMemory;
use toyos::PciDev;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_virtio::pci::{vendor_caps, ConfigSpace, Layout, Offer, Setup, WalkRefusal, VIRTIO_F_ACCESS_PLATFORM};

use crate::{Bar, KernelRefused};

/// Why the function was not brought up. Each keeps its own word: a machine
/// whose kernel refused a call and a device that refused a feature set ask
/// different things of a caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    Kernel(KernelRefused),
    /// The capability list, as the walk refused it.
    Walk(WalkRefusal<SyscallError>),
    /// What the device published or answered, as the transport refused it.
    Device(toyos_virtio::pci::Refusal),
}

impl From<toyos_virtio::pci::Refusal> for Refusal {
    fn from(why: toyos_virtio::pci::Refusal) -> Self {
        Self::Device(why)
    }
}

impl From<KernelRefused> for Refusal {
    fn from(why: KernelRefused) -> Self {
        Self::Kernel(why)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(why) => write!(f, "{why}"),
            Self::Walk(why) => write!(f, "{why}"),
            Self::Device(why) => write!(f, "{why}"),
        }
    }
}

/// The claim's configuration space, as the capability walk reads it: each
/// read one `config_read` of its width, and its refusal the kernel's word.
struct ClaimConfig<'a>(&'a PciDev);

impl ConfigSpace for ClaimConfig<'_> {
    type Refused = SyscallError;

    fn read8(&self, at: u16) -> Result<u8, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U8).map(|byte| byte as u8)
    }

    fn read32(&self, at: u16) -> Result<u32, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U32)
    }
}

/// A function past `FEATURES_OK`.
pub struct Negotiated {
    pub setup: Setup<Bar>,
    /// Held for the register window's life: `setup`'s window points into it.
    pub mapping: SharedMemory,
    /// The driver's feature line, for it to say under its own name.
    pub features: Features,
}

/// What was offered and agreed, in the shape `iommu_virtio_platform` reads
/// back for every virtio function a machine creates.
#[derive(Clone, Copy)]
pub struct Features {
    at: (u8, u8, u8),
    offered: u64,
    negotiated: u64,
}

impl std::fmt::Display for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (bus, dev, func) = self.at;
        write!(
            f,
            "VirtIO: PCI {bus:02x}:{dev:02x}.{func} features device={:#x} negotiated={:#x} \
             access_platform={}",
            self.offered,
            self.negotiated,
            if self.negotiated & VIRTIO_F_ACCESS_PLATFORM != 0 { 'y' } else { 'n' },
        )
    }
}

/// Walk the claim's capabilities, map the BAR they name, and negotiate
/// `wanted` beside what `toyos-virtio` accepts wherever it is offered.
///
/// Takes the claim's one description (`PciDev::describe`).
pub fn negotiate(dev: &PciDev, wanted: u64) -> Result<Negotiated, Refusal> {
    let info = dev.describe().map_err(KernelRefused::on("the claim's description"))?;
    let layout = Layout::of(&vendor_caps(&ClaimConfig(dev)).map_err(Refusal::Walk)?)?;
    // The kernel reports 0 bytes for a BAR it keeps back.
    let bar = layout.bar();
    let bar_bytes = *info
        .bar_bytes
        .get(bar as usize)
        .filter(|bytes| **bytes > 0)
        .ok_or(toyos_virtio::pci::Refusal::MissingCap("a BAR this claim may map"))?;
    let (window, mapping) = Bar::map(dev, bar as u32, bar_bytes)?;
    let offer = Offer::acknowledge(window, &layout)?;
    let offered = offer.features();
    let setup = offer.accept(wanted)?;
    let features = Features { at: (info.bus, info.dev, info.func), offered, negotiated: setup.features() };
    Ok(Negotiated { setup, mapping, features })
}
