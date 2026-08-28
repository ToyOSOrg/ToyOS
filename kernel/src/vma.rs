use alloc::sync::Arc;

use crate::file_backing::FileBacking;
use crate::mm::paging::Prot;
use crate::mm::PAGE_2M;

/// The stack extends upward to the PIE base, so no usable VA space exists above it.
pub const ALLOC_CEILING: u64 = STACK_BASE;

/// Guards against NULL-ish addresses below this floor.
pub fn alloc_floor() -> u64 {
    if crate::actuator::test_tiny_va() {
        ALLOC_CEILING - 256 * 1024 * 1024
    } else {
        0x0002_0000_0000 // 8 GB
    }
}

/// RSP starts at this address plus `USER_STACK_SIZE`.
pub const STACK_BASE: u64 = 0x00FF_FF80_0000;

/// Guard page between allocations.
pub const GUARD_SIZE: u64 = PAGE_2M;


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
