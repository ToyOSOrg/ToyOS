//! A domain's device addresses — where they start, where they end and which
//! of them are handed out — for every backend.
//!
//! They start a quarter of the way up what the unit translates and above all
//! memory, so a descriptor still carrying one names nothing a domain maps and
//! faults rather than landing on a page. They end under the first root-bridge
//! window or reserved region over that start: a bridge may route a window
//! peer-to-peer before the unit sees the request (PCIe Base §2.4), and a
//! region firmware reserved is not a domain's (VT-d §3.16). An address is
//! handed out once, unmapped or not: a device holding a stale one would reach
//! whatever took its place.

use crate::iommu::{IommuError, Iova};
use crate::mm::PAGE_2M;

/// One domain's device addresses.
#[derive(Clone, Copy)]
pub struct Window {
    floor: u64,
    ceiling: u64,
    /// The first not yet handed out.
    next: u64,
}

/// A quarter of the way up `translatable` bits of device address.
pub const fn first_address(translatable: u8) -> u64 {
    1 << (translatable - 2)
}

/// Where a window over `translatable` bits starting at `floor` ends: under the
/// first of `reserved` that reaches above `floor`; at or below `floor` where
/// one of them covers it.
const fn ceiling(translatable: u8, floor: u64, reserved: &[(u64, u64)]) -> u64 {
    let mut ceiling = 1u64 << translatable;
    let mut i = 0;
    while i < reserved.len() {
        let (start, end) = reserved[i];
        if end > floor && start < ceiling {
            ceiling = start;
        }
        i += 1;
    }
    ceiling
}

impl Window {
    /// A new domain's window over `translatable` bits, clear of `reserved`,
    /// with `room` bytes of it handed out first; refused where it would start
    /// at or below memory or hold less than that, and at least one leaf.
    pub fn new(translatable: u8, reserved: &[(u64, u64)], room: u64) -> Result<(Self, Iova), IommuError> {
        let floor = first_address(translatable);
        let top = crate::mm::pmm::top();
        if floor <= top {
            return Err(IommuError::WindowBelowMemory { translatable, floor, top });
        }
        let ceiling = ceiling(translatable, floor, reserved);
        let mut window = Self::starting_at(floor, ceiling);
        match window.reserve(room) {
            Some(first) if ceiling.saturating_sub(floor) >= PAGE_2M => Ok((window, first)),
            _ => Err(IommuError::NoRoom { floor, ceiling, room }),
        }
    }

    const fn starting_at(floor: u64, ceiling: u64) -> Self {
        Self { floor, ceiling, next: floor }
    }

    pub const fn floor(&self) -> u64 {
        self.floor
    }

    pub const fn ceiling(&self) -> u64 {
        self.ceiling
    }

    /// `bytes`, rounded up to whole 2 MiB leaves, of addresses never handed out.
    pub const fn reserve(&mut self, bytes: u64) -> Option<Iova> {
        let Some(span) = bytes.checked_next_multiple_of(PAGE_2M) else { return None };
        let Some(end) = self.next.checked_add(span) else { return None };
        if end > self.ceiling {
            return None;
        }
        let at = Iova::translated(self.next);
        self.next = end;
        Some(at)
    }

    /// Whether `bytes` at `at` is room [`Self::reserve`] already handed out:
    /// the only room a mapping may be placed in by address.
    ///
    /// `at` must itself be a leaf `reserve` could have returned, not merely
    /// inside a handed-out span: a leaf's index floors to the enclosing leaf,
    /// so an unaligned `at` this let through would place a mapping short of,
    /// or overlapping, where the caller named.
    pub const fn handed_out(&self, at: Iova, bytes: u64) -> bool {
        if !at.raw().is_multiple_of(PAGE_2M) || at.raw() < self.floor {
            return false;
        }
        let Some(span) = bytes.checked_next_multiple_of(PAGE_2M) else { return false };
        match at.raw().checked_add(span) {
            Some(end) => end <= self.next,
            None => false,
        }
    }
}

/// [`Window`] and [`ceiling`] at their boundaries, at compile time: the
/// kernel binary has no test harness.
const _: () = {
    const FLOOR: u64 = first_address(48);
    let mut one = Window::starting_at(FLOOR, FLOOR + 2 * PAGE_2M);
    assert!(matches!(one.reserve(1), Some(at) if at.raw() == FLOOR));
    // Exactly what one `reserve` handed out.
    assert!(one.handed_out(Iova::translated(FLOOR), PAGE_2M));
    // Short of the floor, past what has been reserved, rounded up past it,
    // off a leaf boundary, and past the end of the address space: nothing.
    assert!(!one.handed_out(Iova::translated(FLOOR - PAGE_2M), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR + PAGE_2M), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR), PAGE_2M + 1));
    assert!(!one.handed_out(Iova::translated(FLOOR + 1), 0));
    assert!(!one.handed_out(Iova::translated(u64::MAX - PAGE_2M + 1), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR), u64::MAX));
    // Past the ceiling, and past the end of the address space, nothing.
    assert!(one.reserve(PAGE_2M + 1).is_none());
    assert!(one.reserve(u64::MAX).is_none());
    assert!(matches!(one.reserve(PAGE_2M), Some(at) if at.raw() == FLOOR + PAGE_2M));
    // Two leaves handed out; the second is room in its own right, and a
    // mid-span address off a leaf boundary is not.
    assert!(one.handed_out(Iova::translated(FLOOR + PAGE_2M), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR + 1), PAGE_2M));
    assert!(one.reserve(1).is_none());
};

/// [`ceiling`] over the windows the T14's firmware declares, unsorted as it
/// declares them, and over `virt`'s: a 39-bit unit's window ends where the
/// first root-bridge window above its floor begins, one reaching over the
/// floor leaves it nothing, and windows all below the floor leave the whole
/// input.
const _: () = {
    const FLOOR: u64 = first_address(39);
    const T14: [(u64, u64); 7] = [
        (0xA200_0000, 0xBD00_0000),
        (0x40_0000_0000, 0x60_3DC0_0000),
        (0xA080_0000, 0xA200_0000),
        (0xBD00_0000, 0xC000_0000),
        (0xFF00_0000, 0xFFB8_0000),
        (0xFFD3_A070, 0x1_0000_0000),
        (0x60_3DC0_0000, 0x80_0000_0000),
    ];
    assert!(FLOOR == 0x20_0000_0000);
    assert!(ceiling(39, FLOOR, &T14) == 0x40_0000_0000);
    assert!(ceiling(39, FLOOR, &[]) == 1 << 39);
    assert!(ceiling(39, FLOOR, &[(0x10_0000_0000, FLOOR + 1)]) < FLOOR);
    const VIRT: [(u64, u64); 2] = [(0x1000_0000, 0x3f00_0000), (0x80_0000_0000, 0x100_0000_0000)];
    assert!(ceiling(48, first_address(48), &VIRT) == 1 << 48);
};
