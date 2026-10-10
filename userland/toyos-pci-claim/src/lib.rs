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
use toyos_abi::syscall::SyscallError;
use toyos_device_memory::{DmaBuffers, Registers};

pub mod virtio;

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
        Ok((Self::over(window, region.device_addr), region))
    }

    /// The grant over memory the caller already holds: a host test's plain
    /// allocation, with the address a device would be told.
    pub fn over(window: Window, device_base: u64) -> Self {
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

/// The kernel refused a call a bring-up cannot go on without, and the word is
/// the kernel's own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KernelRefused {
    pub call: &'static str,
    pub why: SyscallError,
}

impl KernelRefused {
    pub fn on(call: &'static str) -> impl Fn(SyscallError) -> Self {
        move |why| Self { call, why }
    }
}

impl std::fmt::Display for KernelRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the kernel refused {}: {:?}", self.call, self.why)
    }
}
