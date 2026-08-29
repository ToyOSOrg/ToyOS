use core::ptr::{read_volatile, write_volatile};

use super::DirectMap;

/// Bounds-checked volatile window over device or kernel-owned memory. Copy, no ownership, no lifetime.
#[derive(Clone, Copy)]
pub struct Mmio {
    base: *mut u8,
    size: u64,
}

// SAFETY: the window's address is fixed for the machine's life and Mmio carries no lock, so Send costs nothing new.
unsafe impl Send for Mmio {}
// SAFETY: every access goes through read_volatile/write_volatile below, which order correctly regardless of which CPU issues them.
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
        unsafe { read_volatile(self.base.add(offset as usize) as *const u8) }
    }

    #[inline]
    pub fn write_u8(self, offset: u64, val: u8) {
        self.check(offset, 1);
        // SAFETY: check asserted the offset fits; write_volatile preserves ordering against other MMIO accesses.
        unsafe { write_volatile(self.base.add(offset as usize), val) }
    }

    #[inline]
    pub fn read_u16(self, offset: u64) -> u16 {
        self.check(offset, 2);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        unsafe { read_volatile(self.base.add(offset as usize) as *const u16) }
    }

    #[inline]
    pub fn write_u16(self, offset: u64, val: u16) {
        self.check(offset, 2);
        // SAFETY: check asserted the offset fits; write_volatile preserves ordering against other MMIO accesses.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u16, val) }
    }

    #[inline]
    pub fn read_u32(self, offset: u64) -> u32 {
        self.check(offset, 4);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        unsafe { read_volatile(self.base.add(offset as usize) as *const u32) }
    }

    #[inline]
    pub fn write_u32(self, offset: u64, val: u32) {
        self.check(offset, 4);
        // SAFETY: check asserted the offset fits; write_volatile preserves ordering against other MMIO accesses.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u32, val) }
    }

    #[inline]
    pub fn read_u64(self, offset: u64) -> u64 {
        self.check(offset, 8);
        // SAFETY: check asserted the offset fits; read_volatile preserves the register's read side effect.
        unsafe { read_volatile(self.base.add(offset as usize) as *const u64) }
    }

    #[inline]
    pub fn write_u64(self, offset: u64, val: u64) {
        self.check(offset, 8);
        // SAFETY: check asserted the offset fits; write_volatile preserves ordering against other MMIO accesses.
        unsafe { write_volatile(self.base.add(offset as usize) as *mut u64, val) }
    }
}
