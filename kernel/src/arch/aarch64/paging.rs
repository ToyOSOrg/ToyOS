//! Page tables. The boot runs on the loader's: one L0 table that `TTBR0_EL1`
//! walks for the identity view and `TTBR1_EL1` for the view at `PHYS_OFFSET`,
//! 2 MiB blocks whose `AttrIndx` names [`super::control_regs`]'s `MAIR_EL1`.
//! The kernel's own tables — the direct map, MMIO windows, and a user
//! address space per process with an ASID — are the port's stage 4, so an
//! [`AddressSpace`] cannot exist yet: the type is uninhabited, and every
//! method on it is a match on nothing.

use core::convert::Infallible;

use crate::mm::UserAddr;
pub use crate::mm::policy::{CachePolicy, MmioPolicy, Prot, WindowProt};
use crate::sync::Lock;
use crate::vma::{Occupancy, Region, RegionKind};
use crate::MemoryMapEntry;

/// A translation table base: what `TTBR0_EL1` is loaded with for a space.
#[derive(Clone, Copy)]
pub struct Root(Infallible);

impl Root {
    pub fn phys(self) -> u64 {
        match self.0 {}
    }

    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn activate(self) {
        match self.0 {}
    }
}

/// A process's address space: its regions and the tables that map them.
pub struct AddressSpace {
    never: Infallible,
}

impl AddressSpace {
    pub fn new_user() -> Option<Self> {
        owed!("a user address space", "stage 4")
    }

    pub fn root(&self) -> Root {
        match self.never {}
    }

    pub fn map_range(&mut self, _vaddr: UserAddr, _phys: u64, _size: u64, _prot: Prot, _cache: CachePolicy) {
        match self.never {}
    }

    pub fn remap(&mut self, _vaddr: UserAddr, _phys: u64, _prot: Prot) {
        match self.never {}
    }

    pub fn map_window(&mut self, _vaddr: UserAddr, _phys: u64, _prot: &WindowProt) {
        match self.never {}
    }

    pub fn map_window_if_absent(&mut self, _vaddr: UserAddr, _phys: u64, _prot: &WindowProt) -> bool {
        match self.never {}
    }

    pub fn unmap(&mut self, _vaddr: UserAddr) {
        match self.never {}
    }

    pub fn translate(&self, _vaddr: UserAddr) -> Option<crate::mm::DirectMap> {
        match self.never {}
    }

    pub fn alloc_region(&mut self, _size: u64, _kind: RegionKind) -> Option<UserAddr> {
        match self.never {}
    }

    pub fn alloc_and_map(
        &mut self,
        _phys: u64,
        _size: u64,
        _prot: Prot,
        _cache: CachePolicy,
    ) -> Option<(UserAddr, u64)> {
        match self.never {}
    }

    pub fn free_and_unmap(&mut self, _addr: UserAddr) -> Option<u64> {
        match self.never {}
    }

    pub fn insert_region(&mut self, _addr: UserAddr, _region: Region) {
        match self.never {}
    }

    pub fn find_region(&self, _addr: UserAddr) -> Option<(UserAddr, &Region)> {
        match self.never {}
    }

    pub fn occupancy(&self, _addr: UserAddr, _size: u64) -> Occupancy {
        match self.never {}
    }

    pub fn overlapping_regions(
        &self,
        _start: UserAddr,
        _end: UserAddr,
    ) -> impl Iterator<Item = (&UserAddr, &Region)> {
        match self.never {}
        #[allow(unreachable_code)]
        core::iter::empty()
    }

    pub fn direct_map_policy(&self, _phys: u64) -> Option<CachePolicy> {
        match self.never {}
    }

    pub fn user_policy(&self, _addr: UserAddr) -> Option<CachePolicy> {
        match self.never {}
    }

    pub fn guard_4k(&mut self, _phys: u64) {
        match self.never {}
    }
}

pub fn kernel() -> &'static alloc::sync::Arc<Lock<AddressSpace>> {
    owed!("the kernel's page tables", "stage 4")
}

pub fn kernel_root() -> Root {
    owed!("the kernel's page tables", "stage 4")
}

pub fn activate_kernel() {
    owed!("the kernel's page tables", "stage 4")
}

pub fn map_mmio(_phys: u64, _size: u64, _policy: MmioPolicy) -> crate::mm::Mmio {
    owed!("the kernel's page tables", "stage 4")
}

pub(crate) fn init(_memory_map: &[MemoryMapEntry]) {
    owed!("the kernel's page tables", "stage 4")
}

/// `PAR_EL1` after the MMU translated `addr` for an EL1 read in the tables
/// this CPU runs on now (`AT S1E1R`; Arm ARM K.a, C6.2.x and D24.2.131):
/// bit 0 set is a fault, and otherwise bits 63:56 are the memory type.
fn translate_read(addr: u64) -> u64 {
    let par: u64;
    // SAFETY: `AT` translates without accessing memory and reports through
    // `PAR_EL1`; the `ISB` makes the result the one read back.
    unsafe {
        core::arch::asm!(
            "at s1e1r, {addr}",
            "isb",
            "mrs {par}, par_el1",
            addr = in(reg) addr,
            par = out(reg) par,
            options(nostack, preserves_flags),
        );
    }
    par
}

/// Whether `addr` translates for a read in the tables this CPU runs on now:
/// the MMU's own answer, so broken tables become "not present" and never a
/// fault here.
pub fn present_in_current_tables(addr: u64) -> bool {
    translate_read(addr) & 1 == 0
}

/// Whether the scanout is write-combining at both of its addresses already:
/// on AArch64 that is Normal non-cacheable, whose stores gather, and the
/// loader maps it so. Asked of the MMU page by page rather than assumed, and
/// nothing is retyped: the type the loader wrote is the final one.
pub fn boot_map_write_combining(phys: u64, size: u64) -> bool {
    [phys, crate::mm::PHYS_OFFSET + phys].iter().all(|&base| {
        (0..size.div_ceil(4096)).all(|page| {
            let par = translate_read(base + page * 4096);
            // Outer Normal non-cacheable, and an inner nibble that says the same:
            // 0b0100 as `MAIR_EL1` spells it, or 0b0000, which is how a CPU may
            // report an inner Non-cacheable attribute in `PAR_EL1` (HVF on Apple
            // silicon does), and which `MAIR_EL1` itself never holds for Normal.
            let (outer, inner) = ((par >> 60) & 0xF, (par >> 56) & 0xF);
            par & 1 == 0 && outer == 0b0100 && (inner == 0b0100 || inner == 0)
        })
    })
}

/// What memory type the scanout is mapped with, for the GPU's report.
pub fn scanout_memory_type(_addr: u64, _size: u64) -> impl core::fmt::Display {
    owed!("the kernel's page tables", "stage 4");
    #[allow(unreachable_code)]
    ""
}
