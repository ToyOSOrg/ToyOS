use alloc::sync::Arc;

use crate::file_backing::FileBacking;
use crate::mm::policy::Prot;
use crate::mm::PAGE_2M;
use toyos_userbound::Window;

/// The stack extends upward to the PIE base, so no usable VA space exists above it.
pub const ALLOC_CEILING: u64 = STACK_BASE;

/// RSP starts at this address plus `USER_STACK_SIZE`.
pub const STACK_BASE: u64 = 0x00FF_FF80_0000;

/// Guard page between allocations.
const GUARD_SIZE: u64 = PAGE_2M;

/// The floor at 8 GB.
const WINDOW: Window = Window::new(0x0002_0000_0000, ALLOC_CEILING, GUARD_SIZE);
/// The `test-tiny-va` actuator's: 256 MiB under the ceiling, so a process can
/// run out of address space before it runs out of memory.
const TINY_WINDOW: Window = Window::new(ALLOC_CEILING - 256 * 1024 * 1024, ALLOC_CEILING, GUARD_SIZE);

/// Where `find_gap` places, and the bound every length from userland is
/// refused against before any sum is taken on it.
pub fn window() -> Window {
    if crate::actuator::test_tiny_va() { TINY_WINDOW } else { WINDOW }
}


/// `Mapped` has no `prot`: its pages are already installed, so nothing reads one.
pub enum RegionKind {
    /// On fault, reads the backing store and maps `prot`.
    FileBacked {
        backing: Arc<dyn FileBacking>,
        file_offset: u64,
        file_size: u64,
        prot: Prot,
    },
    /// On fault, maps a zeroed page as `prot`.
    Anonymous { prot: Prot },
    /// A fault here is refused — physical backing is already assigned.
    Mapped,
}

/// A contiguous region of virtual address space.
pub struct Region {
    /// 2 MiB-aligned for allocated regions, 4 KiB-aligned for VMAs.
    pub size: u64,
    /// For the demand-paged kinds, what a fault in this region installs.
    pub kind: RegionKind,
}

/// How a range meets the regions an address space registers. Needed because a
/// *placed* mapping (`sys_mmap`'s FIXED arm) skips `find_gap`'s implicit check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Occupancy {
    /// Nothing is registered over any part of it.
    Free,
    /// One region covers it end for end, and that region is all it runs into.
    Whole,
    /// Part of a region, several regions, or one that merely starts here.
    Partial,
}
