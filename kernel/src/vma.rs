use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::file_backing::FileBacking;
use crate::mm::policy::Prot;
use crate::mm::{UserAddr, PAGE_2M};
use toyos_userbound::{PageSpan, Window};

/// The stack extends upward to the PIE base, so no usable VA space exists above it.
pub const ALLOC_CEILING: u64 = STACK_BASE;

/// RSP starts at this address plus `USER_STACK_SIZE`.
pub const STACK_BASE: u64 = 0x00FF_FF80_0000;

/// Guard page between allocations.
const GUARD_SIZE: u64 = PAGE_2M;

/// The floor at 8 GB.
const WINDOW: Window = Window::new(0x0002_0000_0000, ALLOC_CEILING, GUARD_SIZE);

/// Where `find_gap` places, and the bound every length from userland is
/// refused against before any sum is taken on it.
pub fn window() -> Window {
    WINDOW
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

/// Every region an address space registers, keyed by start: where a new one
/// goes and what an address falls in. The page tables are the architecture's
/// (`mm::paging::AddressSpace`), and this is the half both share.
#[derive(Default)]
pub struct Regions(BTreeMap<UserAddr, Region>);

impl Regions {
    /// Where `span` goes, top-down and never below the floor: a region the
    /// kernel placed under it, the clock page, bounds no gap.
    fn find_gap(&self, span: PageSpan) -> Option<UserAddr> {
        let taken = self.0.iter().rev().map(|(start, region)| (start.raw(), region.size));
        window().gap(span, taken).map(UserAddr::new)
    }

    /// Allocate a virtual address range and register the region. `size` is
    /// made a [`PageSpan`] before anything is summed on it.
    pub fn alloc(&mut self, size: u64, kind: RegionKind) -> Option<UserAddr> {
        let span = window().span(size)?;
        let addr = self.find_gap(span)?;
        self.0.insert(addr, Region { size: span.bytes(), kind });
        Some(addr)
    }

    /// A [`RegionKind::Mapped`] region for `size` bytes: its address and its
    /// size in whole pages, which the caller maps.
    pub fn alloc_mapped(&mut self, size: u64) -> Option<(UserAddr, u64)> {
        let span = window().span(size)?;
        let addr = self.find_gap(span)?;
        let aligned = span.bytes();
        self.0.insert(addr, Region { size: aligned, kind: RegionKind::Mapped });
        Some((addr, aligned))
    }

    /// Unregister the region at `addr`, answering its size.
    pub fn remove(&mut self, addr: UserAddr) -> Option<u64> {
        Some(self.0.remove(&addr)?.size)
    }

    /// Insert a region at a specific address (for ELF segments, stack, etc.)
    pub fn insert(&mut self, addr: UserAddr, region: Region) {
        assert!(self.find(addr).is_none(), "insert_region: address {:#x} already occupied", addr.raw());
        self.0.insert(addr, region);
    }

    /// Find the region containing `addr`. Returns (start_addr, region).
    pub fn find(&self, addr: UserAddr) -> Option<(UserAddr, &Region)> {
        let (&start, region) = self.0.range(..=addr).next_back()?;
        if addr.raw() < start.raw() + region.size {
            Some((start, region))
        } else {
            None
        }
    }

    /// The end is saturating so a caller's arithmetic cannot wrap into a smaller range.
    pub fn occupancy(&self, addr: UserAddr, size: u64) -> Occupancy {
        let end = UserAddr::new(addr.raw().saturating_add(size));
        let mut over = self.overlapping(addr, end);
        let Some((&start, region)) = over.next() else {
            return Occupancy::Free;
        };
        if over.next().is_none() && start == addr && region.size == size {
            Occupancy::Whole
        } else {
            Occupancy::Partial
        }
    }

    /// Iterate all regions that overlap the range [start, end).
    pub fn overlapping(&self, start: UserAddr, end: UserAddr) -> impl Iterator<Item = (&UserAddr, &Region)> {
        // Overlaps [start, end) iff s < end && s+n > start; `range(..end)` prunes the first half.
        self.0.range(..end).filter(move |(&s, r)| s.raw() + r.size > start.raw())
    }
}
