//! A PCI claim, as a userland driver reaches the function behind it.
//!
//! What the kernel keeps is the claim: config space, the vector it programmed
//! into the function's MSI-X table, and the address space the function
//! translates through. **This crate is the one implementation of
//! `toyos-device-memory`'s boundary over the SDK's [`Window`]** — [`Bar`] and
//! [`Grant`], an instruction deep, every offset that reaches them bounded by
//! the driver crate above — and, for a virtio function, the bring-up every
//! virtio driver makes before its device type begins ([`virtio`]).
//!
//! Neither [`Bar`] nor [`Grant`] owns its mapping: whoever holds the driver
//! keeps the `SharedMemory` and the `DmaRegion` for as long as it keeps the
//! driver, and the constructors here hand both back together.

use std::sync::atomic::{fence, Ordering};

use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};
use toyos_abi::pci::{DmaMapping, PciFunctionInfo};
use toyos_abi::syscall::SyscallError;
use toyos_device_memory::{DmaBuffers, Registers};

pub mod virtio;

/// The claim's description, read once per claim (`PciDev::describe`).
pub fn describe(dev: &PciDev) -> Result<PciFunctionInfo, KernelRefused> {
    dev.describe().map_err(KernelRefused::on("the claim's description"))
}

/// The claim's function's vendor and device id, config space's first dword.
pub fn id(dev: &PciDev) -> Result<(u16, u16), KernelRefused> {
    let word = dev
        .config_read(0, toyos_abi::syscall::RegWidth::U32)
        .map_err(KernelRefused::on("the vendor and device id"))?;
    Ok((word as u16, (word >> 16) as u16))
}

/// State the address width the claim's function issues (`PciDev::dma_limit`),
/// before its first grant or lend.
pub fn limit(dev: &PciDev, bits: u8) -> Result<(), KernelRefused> {
    dev.dma_limit(bits).map_err(KernelRefused::on("the address limit"))
}

/// Let the claim's function master the bus (`PciDev::dma_start`), once its
/// driver has stopped and reset it.
pub fn start(dev: &PciDev) -> Result<(), KernelRefused> {
    dev.dma_start().map_err(KernelRefused::on("the bus-master start"))
}

/// `region`, which a peer lent, put into the claim's function's domain
/// (`PciDev::dma_map`).
pub fn lend(dev: &PciDev, region: toyos::BorrowedHandle<'_>) -> Result<DmaMapping, KernelRefused> {
    let call = "the lent region's mapping";
    dev.dma_map(region).map_err(|e| KernelRefused { call, why: Why::of_lent(call, e) })
}

/// A mapped BAR, as a driver reaches its device's registers.
#[derive(Clone, Copy)]
pub struct Bar(Window);

impl Bar {
    /// BAR `index` of the claim, `bytes` long, and the mapping the window
    /// points into.
    pub fn map(dev: &PciDev, index: u32, bytes: u64) -> Result<(Self, SharedMemory), KernelRefused> {
        let mapped = dev.map_bar(index, bytes).map_err(KernelRefused::on("the register window"))?;
        // SAFETY: `map_bar` answered `bytes` bytes of live mapping, and
        // `mapped` goes to the caller with the window, which keeps both for as
        // long as it keeps either.
        let window = unsafe { Window::new(mapped.as_ptr(), bytes as usize) };
        Ok((Self(window), mapped))
    }
}

impl Registers for Bar {
    fn bytes(&self) -> usize {
        self.0.bytes()
    }

    fn read8(&self, at: usize) -> u8 {
        self.0.read(at)
    }

    fn read16(&self, at: usize) -> u16 {
        self.0.read(at)
    }

    fn read32(&self, at: usize) -> u32 {
        self.0.read(at)
    }

    fn write8(&self, at: usize, value: u8) {
        self.0.write(at, value);
    }

    fn write16(&self, at: usize, value: u16) {
        self.0.write(at, value);
    }

    fn write32(&self, at: usize, value: u32) {
        self.0.write(at, value);
    }
}

/// A DMA grant, as a driver reaches its rings and descriptors.
#[derive(Clone, Copy)]
pub struct Grant {
    window: Window,
    /// Where the device reaches the grant's first byte. Not a physical address:
    /// what the unit translates for this function and for nothing else.
    device_base: u64,
}

impl Grant {
    /// A fresh grant of at least `bytes` for the claim's function, and the
    /// region the window points into.
    pub fn alloc(dev: &PciDev, bytes: u64) -> Result<(Self, DmaRegion), KernelRefused> {
        let region = dev.dma_alloc(bytes).map_err(KernelRefused::on("a DMA grant"))?;
        // SAFETY: the kernel rounds the request up to whole pages, never down,
        // and `region` goes to the caller with the grant, which keeps both for
        // as long as it keeps either.
        let window = unsafe { Window::new(region.memory.as_ptr(), bytes as usize) };
        Ok((Self { window, device_base: region.device_addr }, region))
    }

    /// A grant over a leaked host allocation of `bytes`, zeroed, with the
    /// address a device would be told: a driver crate's host test's.
    #[cfg(feature = "host-grant")]
    pub fn leaked(bytes: usize, device_base: u64) -> Self {
        let backing = vec![0u64; bytes.div_ceil(8)].leak();
        // SAFETY: `leak` gives the allocation the `'static` lifetime the window
        // needs, and it is at least `bytes` long.
        let window = unsafe { Window::new(backing.as_mut_ptr().cast(), bytes) };
        Self { window, device_base }
    }

    /// The same bytes, for the holder's own view of them: what no driver crate
    /// reaches — frames, PCM.
    pub fn window(&self) -> Window {
        self.window
    }
}

impl DmaBuffers for Grant {
    fn bytes(&self) -> usize {
        self.window.bytes()
    }

    fn device_addr(&self, at: usize) -> u64 {
        self.device_base + at as u64
    }

    fn read16(&self, at: usize) -> u16 {
        self.window.read(at)
    }

    fn read32(&self, at: usize) -> u32 {
        self.window.read(at)
    }

    fn read64(&self, at: usize) -> u64 {
        self.window.read(at)
    }

    fn write16(&self, at: usize, value: u16) {
        self.window.write(at, value);
    }

    fn write32(&self, at: usize, value: u32) {
        self.window.write(at, value);
    }

    fn write64(&self, at: usize, value: u64) {
        self.window.write(at, value);
    }

    fn publish(&self) {
        fence(Ordering::Release);
    }

    fn observe(&self) {
        fence(Ordering::Acquire);
    }
}

/// The kernel refused a call a bring-up cannot go on without.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KernelRefused {
    pub call: &'static str,
    pub why: Why,
}

/// What the kernel refused a claim's call for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The claim ended under the call: another thread closed its last handle.
    Gone,
    /// No room: memory, a handle, the function's grant quota or its domain's
    /// device addresses.
    Exhausted,
    /// The function has no such thing to give: a BAR it lacks or the kernel
    /// keeps, a grant of no bytes or past the largest, a register past its
    /// configuration space.
    NotOffered,
    /// The IOMMU refused the function an access, and the claim answers nothing
    /// more.
    Faulted,
    /// The region a peer lent cannot be lent on to the function: it is not
    /// ordinary memory, the function reaches it already, or it came without
    /// the right to map it.
    Unlendable,
}

impl Why {
    /// The kernel's refusal of `call` on a claim this process holds as the
    /// supervisor minted it. The calls here answer nothing else, so any other
    /// word is a broken invariant.
    fn of(call: &str, e: SyscallError) -> Self {
        match e {
            SyscallError::Gone => Self::Gone,
            SyscallError::ResourceExhausted => Self::Exhausted,
            SyscallError::InvalidArgument => Self::NotOffered,
            SyscallError::Io => Self::Faulted,
            other => panic!("toyos-pci-claim: the kernel refused {call} {other:?}, which it never answers it"),
        }
    }

    /// [`Self::of`] for a region a peer lent, whose words about the region are
    /// the peer's and not this claim's.
    fn of_lent(call: &str, e: SyscallError) -> Self {
        match e {
            SyscallError::InvalidArgument | SyscallError::PermissionDenied => Self::Unlendable,
            other => Self::of(call, other),
        }
    }
}

impl KernelRefused {
    fn on(call: &'static str) -> impl Fn(SyscallError) -> Self {
        move |e| Self { call, why: Why::of(call, e) }
    }
}

impl std::fmt::Display for KernelRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the kernel refused {}: {:?}", self.call, self.why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each word the claim's calls can be answered is one named reason; the
    /// rest are words they never get, and fail loudly.
    #[test]
    fn every_kernel_refusal_a_claim_call_can_see_is_one_named_reason() {
        assert_eq!(Why::of("a call", SyscallError::Gone), Why::Gone);
        assert_eq!(Why::of("a call", SyscallError::ResourceExhausted), Why::Exhausted);
        assert_eq!(Why::of("a call", SyscallError::InvalidArgument), Why::NotOffered);
        assert_eq!(Why::of("a call", SyscallError::Io), Why::Faulted);
        for never in [
            SyscallError::Unknown,
            SyscallError::NotFound,
            SyscallError::PermissionDenied,
            SyscallError::AlreadyExists,
            SyscallError::BadAddress,
            SyscallError::WouldBlock,
            SyscallError::NotSupported,
        ] {
            assert!(std::panic::catch_unwind(|| Why::of("a call", never)).is_err(), "{never:?}");
        }
    }

    /// A lent region's own refusals are the peer's; the claim's are as above.
    #[test]
    fn a_lent_region_the_function_cannot_reach_is_unlendable() {
        assert_eq!(Why::of_lent("a lend", SyscallError::InvalidArgument), Why::Unlendable);
        assert_eq!(Why::of_lent("a lend", SyscallError::PermissionDenied), Why::Unlendable);
        assert_eq!(Why::of_lent("a lend", SyscallError::Gone), Why::Gone);
        assert_eq!(Why::of_lent("a lend", SyscallError::ResourceExhausted), Why::Exhausted);
        assert_eq!(Why::of_lent("a lend", SyscallError::Io), Why::Faulted);
        assert!(std::panic::catch_unwind(|| Why::of_lent("a lend", SyscallError::NotFound)).is_err());
    }
}
