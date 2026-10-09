//! What both NIC drivers need from the substrate: `toyos-device-memory`'s
//! boundary over the SDK's `toyos::volatile::Window`, the claim's
//! configuration space as `toyos-virtio` walks it, the kernel's word for a
//! call a bring-up cannot go on without, and the latch a diagnostic is printed
//! on.
//!
//! **[`Bar`] and [`Grant`] are the one implementation of the boundary both
//! driver crates are written against**, an instruction deep: every offset
//! that reaches them a driver crate has bounded, and the two barriers are here
//! and nowhere else in this program. Neither owns its mapping: a driver's
//! holder keeps the `SharedMemory` and the `DmaRegion` for as long as it keeps
//! the driver.

use std::cell::Cell;
use std::sync::atomic::{fence, Ordering};

use toyos::volatile::Window;
use toyos::PciDev;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_device_memory::{DmaBuffers, Registers};
use toyos_virtio::pci::ConfigSpace;

/// A mapped BAR, as a driver reaches its device's registers.
#[derive(Clone, Copy)]
pub struct Bar(Window);

impl Bar {
    pub fn over(window: Window) -> Self {
        Self(window)
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
    pub fn over(window: Window, device_base: u64) -> Self {
        Self { window, device_base }
    }

    /// The same bytes, for the holder's own view of them: the frames, which no
    /// driver crate reaches.
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

/// A claim's configuration space, as `toyos-virtio`'s capability walk reads
/// it: each read one `config_read` of its width, and its refusal the kernel's
/// word.
pub struct ClaimConfig<'a>(pub &'a PciDev);

impl ConfigSpace for ClaimConfig<'_> {
    type Refused = SyscallError;

    fn read8(&self, at: u16) -> Result<u8, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U8).map(|byte| byte as u8)
    }

    fn read32(&self, at: u16) -> Result<u32, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U32)
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

/// What a diagnostic last said, so it says it again only on a change.
///
/// **On change, not per element**: a device flooding a ring with descriptors a
/// driver will not act on costs one line, not one per descriptor, which is the
/// difference between a diagnostic and a way to drown the console from the
/// other side of the boundary.
pub struct Latch<T: Copy + PartialEq>(Cell<T>);

impl<T: Copy + PartialEq + Default> Default for Latch<T> {
    fn default() -> Self {
        Self(Cell::new(T::default()))
    }
}

impl<T: Copy + PartialEq> Latch<T> {
    /// The previous value if `now` is not it, and `None` if nothing has moved.
    pub fn moved(&self, now: T) -> Option<T> {
        let was = self.0.replace(now);
        (was != now).then_some(was)
    }
}
