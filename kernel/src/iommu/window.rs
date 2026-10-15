//! A domain's device addresses — where they start, where they end and which
//! of them are handed out — for every backend.
//!
//! **A kernel driver's domain maps each pool at its own physical address**
//! ([`Addresses::Identity`]): a root bridge's window is MMIO and never RAM
//! (`toyos_pci::aperture::check` refuses a declaration saying otherwise), so a
//! device address equal to a RAM address is never routed peer-to-peer before
//! the unit sees it (PCIe Base §2.4), and no declaration is needed. The
//! function is never released to a claim, so no later holder inherits an
//! address it was aimed at.
//!
//! **A claimed function's domain hands its addresses out above all memory**
//! ([`Window`]), so a descriptor a holder left behind names nothing the next
//! holder's domain maps, and clear of every declared root-bridge window. Where
//! the unit translates past every address the CPU issues, they lie above every
//! such address. Where it translates no more bits than the CPU's physical
//! addresses, whatever the backend, they start a quarter of the way up and end
//! under the first declared window or region firmware reserved for a device
//! (an RMRR or IVMD) over that start. An address is handed out once, unmapped
//! or not: a device holding a stale one would reach whatever took its place.

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

/// Where a domain over `translatable` bits begins and ends on a CPU issuing
/// `cpu`-bit physical addresses, `reserved` the declared root-bridge windows
/// and firmware's reserved regions.
const fn bounds(translatable: u8, cpu: u8, reserved: &[(u64, u64)]) -> (u64, u64) {
    if translatable > cpu {
        return (1 << cpu, 1 << translatable);
    }
    let floor = first_address(translatable);
    (floor, ceiling(translatable, floor, reserved))
}

/// Where one domain's device addresses come from.
#[derive(Clone, Copy)]
pub enum Addresses {
    /// A kernel driver's: each mapping at its own physical address, below
    /// `1 << translatable`.
    Identity { translatable: u8 },
    /// A claimed function's.
    Window(Window),
}

impl Addresses {
    /// A kernel driver's, refused where memory reaches past what the unit
    /// translates: a pool there would have no address equal to its own.
    pub fn identity(translatable: u8) -> Result<Self, IommuError> {
        let top = crate::mm::pmm::top();
        if top > 1 << translatable {
            return Err(IommuError::MemoryPastTranslation { translatable, top });
        }
        Ok(Self::Identity { translatable })
    }

    /// Where `bytes` at `phys` goes in this domain: `phys` itself in a kernel
    /// driver's, and in a claimed function's room never handed out before.
    pub fn map(&mut self, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
        match self {
            Self::Identity { translatable } => match phys.checked_add(bytes.next_multiple_of(PAGE_2M)) {
                Some(end) if end <= 1 << *translatable => Ok(Iova::translated(phys)),
                _ => Err(IommuError::MemoryPastTranslation { translatable: *translatable, top: phys.saturating_add(bytes) }),
            },
            Self::Window(window) => window.reserve(bytes).ok_or(IommuError::AddressesExhausted(window.ceiling())),
        }
    }
}

impl Window {
    /// A new domain's window over `translatable` bits ([`bounds`]), with
    /// `room` bytes of it handed out first; refused where it would start at or
    /// below memory or hold less than that, and at least one leaf.
    pub fn new(translatable: u8, reserved: &[(u64, u64)], room: u64) -> Result<(Self, Iova), IommuError> {
        let (floor, ceiling) = bounds(translatable, crate::arch::cpu::physical_address_bits(), reserved);
        let top = crate::mm::pmm::top();
        if floor <= top {
            return Err(IommuError::WindowBelowMemory { translatable, floor, top });
        }
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

/// [`bounds`] on each kind of machine: a unit wider than the CPU's addresses
/// places above them, whatever is declared; a unit no wider places under its
/// first declared window.
const _: () = {
    const T14: [(u64, u64); 1] = [(0x40_0000_0000, 0x60_3DC0_0000)];
    assert!(matches!(bounds(48, 40, &[]), (0x100_0000_0000, 0x1_0000_0000_0000)));
    assert!(matches!(bounds(48, 40, &T14), (0x100_0000_0000, 0x1_0000_0000_0000)));
    assert!(matches!(bounds(39, 39, &T14), (0x20_0000_0000, 0x40_0000_0000)));
    // The AMD bench: 48-bit host tables on a CPU of 48, all its windows below
    // the quarter floor, so the whole of the upper quarter.
    assert!(matches!(bounds(48, 48, &[(0x4_5020_0000, 0xfc_9000_0000)]), (0x4000_0000_0000, 0x1_0000_0000_0000)));
};
