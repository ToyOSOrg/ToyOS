//! A bounds-checked window over a device's registers, and the ordering every
//! access through it carries.
//!
//! **The contract, which is what a driver may rely on:** a `write_*` is ordered
//! after every store this CPU made to memory before it, as a device's DMA reads
//! observe them — the descriptor before the doorbell — and a `read_*` is
//! ordered before every load this CPU makes after it — the status before the
//! entry it reports. It is Linux's `writel`/`readl` and not their `_relaxed`
//! forms: `arch::barrier::before_mmio_write` and `after_mmio_read` supply it,
//! which is a compiler barrier on x86-64's TSO and a `dmb` on AArch64, where a
//! plain `fence` is inner-shareable and orders nothing a device sees.
//!
//! A store to device memory is not a store to plain memory, and `volatile`
//! alone promises nothing about the two against each other: without the
//! barrier the compiler may move a descriptor write past the doorbell that
//! publishes it on either architecture.

use core::ptr::{read_volatile, write_volatile};

use super::DirectMap;
use crate::arch::barrier;

/// Bounds-checked volatile window over device or kernel-owned memory. Copy, no ownership, no lifetime.
#[derive(Clone, Copy)]
pub struct Mmio {
    base: *mut u8,
    size: u64,
}

// SAFETY: the window's address is fixed for the machine's life and Mmio carries no lock, so Send costs nothing new.
unsafe impl Send for Mmio {}
// SAFETY: every access is one volatile load or store carrying the module's ordering
// contract on whichever CPU issues it; nothing here is shared state of its own.
unsafe impl Sync for Mmio {}

impl Mmio {
    pub(super) fn new(base: DirectMap, size: u64) -> Self {
        Self { base: base.as_mut_ptr(), size }
    }

    /// The same bounded volatile window, over physical memory this kernel owns instead of a device's registers.
    ///
    /// # Safety
    /// `base` must name `size` bytes of memory this kernel owns for the machine's life, valid for volatile access.
    pub unsafe fn over_phys(base: DirectMap, size: u64) -> Self {
        Self::new(base, size)
    }

    /// The window's base as an integer, for callers that cannot use this type directly.
    pub fn addr(self) -> u64 {
        self.base as u64
    }

    /// The window's byte size — the bound every access here is checked against.
    pub fn size(self) -> u64 {
        self.size
    }

    /// Rebuild a window from an [`addr`](Self::addr) and a [`size`](Self::size)
    /// taken off a live one.
    ///
    /// For the one caller that has to keep the *numbers* and not the object: the
    /// reset-time xHCI stop reads its controllers out of atomics, because a
    /// panicked CPU may take no lock.
    ///
    /// # Safety
    /// `addr` and `size` must be one live `Mmio`'s own `addr()` and `size()`,
    /// over a window mapped for the machine's life.
    pub unsafe fn from_addr(addr: u64, size: u64) -> Self {
        Self { base: addr as *mut u8, size }
    }

    pub fn subregion(self, offset: u64, size: u64) -> Mmio {
        assert!(offset + size <= self.size,
            "Mmio subregion OOB: offset={:#x} size={:#x} total={:#x}", offset, size, self.size);
        Mmio {
            // SAFETY: offset + size <= self.size was just asserted, so the result stays inside self's window.
            base: unsafe { self.base.add(offset as usize) },
            size,
        }
    }

    fn check(&self, offset: u64, len: u64) {
        assert!(offset + len <= self.size,
            "Mmio OOB: offset={:#x} len={} size={:#x}", offset, len, self.size);
    }

    #[inline]
    pub fn read_u8(self, offset: u64) -> u8 {
        self.check(offset, 1);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        let value = unsafe { read_volatile(self.base.add(offset as usize) as *const u8) };
        barrier::after_mmio_read();
        value
    }

    #[inline]
    pub fn write_u8(self, offset: u64, val: u8) {
        self.check(offset, 1);
        barrier::before_mmio_write();
        // SAFETY: check asserted the offset fits; write_volatile keeps the store, and the
        // barrier above orders it after this CPU's earlier stores.
        unsafe { write_volatile(self.base.add(offset as usize), val) }
    }

    #[inline]
    pub fn read_u16(self, offset: u64) -> u16 {
        self.check(offset, 2);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        let value = unsafe { read_volatile(self.base.add(offset as usize) as *const u16) };
        barrier::after_mmio_read();
        value
    }

    #[inline]
    pub fn write_u16(self, offset: u64, val: u16) {
        self.check(offset, 2);
        barrier::before_mmio_write();
        // SAFETY: check asserted the offset fits; write_volatile keeps the store, and the
        // barrier above orders it after this CPU's earlier stores.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u16, val) }
    }

    #[inline]
    pub fn read_u32(self, offset: u64) -> u32 {
        self.check(offset, 4);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        let value = unsafe { read_volatile(self.base.add(offset as usize) as *const u32) };
        barrier::after_mmio_read();
        value
    }

    #[inline]
    pub fn write_u32(self, offset: u64, val: u32) {
        self.check(offset, 4);
        barrier::before_mmio_write();
        // SAFETY: check asserted the offset fits; write_volatile keeps the store, and the
        // barrier above orders it after this CPU's earlier stores.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u32, val) }
    }

    #[inline]
    pub fn read_u64(self, offset: u64) -> u64 {
        self.check(offset, 8);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        let value = unsafe { read_volatile(self.base.add(offset as usize) as *const u64) };
        barrier::after_mmio_read();
        value
    }

    #[inline]
    pub fn write_u64(self, offset: u64, val: u64) {
        self.check(offset, 8);
        barrier::before_mmio_write();
        // SAFETY: check asserted the offset fits; write_volatile keeps the store, and the
        // barrier above orders it after this CPU's earlier stores.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u64, val) }
    }
}
