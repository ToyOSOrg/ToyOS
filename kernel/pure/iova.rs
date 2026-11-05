//! A domain's device addresses: which a unit may hand out, and in what order.
//!
//! The allowed addresses are the unit's translated range below 4 GiB less
//! what the backend reserves — the first leaf, the root bridges' windows,
//! firmware's reservations, the architecture's special ranges — each widened
//! to whole leaves. They are handed out lowest first and each exactly once.
//!
//! **Nothing is handed out at or above 4 GiB**, which every PCI function
//! issues whole: a function that drops its upper address bits cannot issue
//! one of these addresses as another, so no driver states a width.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

/// The one leaf size a domain maps.
pub const LEAF: u64 = 2 << 20;

/// Every device address a domain hands out ends at or below this.
pub const FOUR_GIB: u64 = 1 << 32;

/// One domain's allowed addresses and how far up they are handed out.
pub struct Window {
    /// Ascending, disjoint, whole leaves, none empty.
    allowed: Vec<(u64, u64)>,
    /// The first address not yet handed out.
    next: u64,
}

impl Window {
    /// `[LEAF, 1 << translatable)` below [`FOUR_GIB`] less every `(start,
    /// end)` of `reserved`, in any order, each widened out to whole leaves.
    ///
    /// # Panics
    /// If `translatable` is past 63: no unit translates 64 bits.
    pub fn new(translatable: u8, reserved: &[(u64, u64)]) -> Self {
        assert!(translatable < 64, "iova: a {translatable}-bit unit");
        let below = if cfg!(feature = "iova-past-four-gib") { u64::MAX } else { FOUR_GIB };
        let mut allowed = alloc::vec![(LEAF, (1u64 << translatable).min(below))];
        for &(start, end) in reserved.iter().filter(|(start, end)| end > start) {
            let start = start & !(LEAF - 1);
            let end = end.checked_next_multiple_of(LEAF).unwrap_or(!(LEAF - 1));
            allowed = allowed
                .into_iter()
                .flat_map(|(from, to)| [(from, to.min(start)), (from.max(end), to)])
                .filter(|(from, to)| to > from)
                .collect();
        }
        Self { allowed, next: 0 }
    }

    pub fn allowed(&self) -> &[(u64, u64)] {
        &self.allowed
    }

    /// The lowest run of `bytes`, rounded up to whole leaves, never handed
    /// out; `None` where none is left, which hands nothing out.
    pub fn reserve(&mut self, bytes: u64) -> Option<u64> {
        let span = bytes.checked_next_multiple_of(LEAF).filter(|span| *span != 0)?;
        for &(start, end) in &self.allowed {
            let at = start.max(self.next);
            let stop = at.checked_add(span)?;
            if stop > end {
                continue;
            }
            self.next = stop;
            return Some(at);
        }
        None
    }

    /// Whether `bytes` at `at` is room [`Self::reserve`] already handed out:
    /// the only room a mapping may be placed in by address. `at` must be a
    /// leaf boundary and the run inside one allowed range.
    pub fn handed_out(&self, at: u64, bytes: u64) -> bool {
        let Some(span) = bytes.checked_next_multiple_of(LEAF).filter(|span| *span != 0) else { return false };
        let Some(stop) = at.checked_add(span) else { return false };
        at.is_multiple_of(LEAF) && stop <= self.next && self.allowed.iter().any(|&(start, end)| start <= at && stop <= end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use std::collections::BTreeSet;

    /// Every run of `bytes` until `window` refuses one.
    fn drain(window: &mut Window, bytes: u64) -> Vec<(u64, u64)> {
        core::iter::from_fn(|| window.reserve(bytes).map(|at| (at, at + bytes.next_multiple_of(LEAF))))
            .take(1 << 14)
            .collect()
    }

    #[test]
    fn addresses_go_lowest_first_from_the_first_leaf() {
        let mut window = Window::new(48, &[]);
        assert_eq!(window.reserve(1), Some(LEAF));
        assert_eq!(window.reserve(LEAF + 1), Some(2 * LEAF));
        assert_eq!(window.reserve(LEAF), Some(4 * LEAF));
        assert_eq!(window.reserve(0), None);
    }

    #[test]
    fn a_reserved_range_is_skipped_by_whole_leaves() {
        // Unaligned at both ends, and a run too long for the gap before it.
        let reserved = [(3 * LEAF + 1, 5 * LEAF - 1)];
        let mut window = Window::new(48, &reserved);
        assert_eq!(window.allowed()[..2], [(LEAF, 3 * LEAF), (5 * LEAF, FOUR_GIB)]);
        assert_eq!(window.reserve(LEAF), Some(LEAF));
        assert_eq!(window.reserve(2 * LEAF), Some(5 * LEAF));
        // The leaf the long run could not use is never handed out after it.
        assert_eq!(window.reserve(LEAF), Some(7 * LEAF));
    }

    /// The defect the bound exists for: a function that drops its upper
    /// address bits issues an address at or above 4 GiB as one below it,
    /// which can be another address of the same domain. A unit that
    /// translates 48 bits still hands out nothing there, to the last leaf
    /// before the refusal.
    #[test]
    fn nothing_is_handed_out_at_or_above_four_gib() {
        let mut window = Window::new(48, &[(8 * LEAF, FOUR_GIB - 4 * LEAF)]);
        let granted = drain(&mut window, LEAF);
        assert!(granted.iter().all(|&(_, end)| end <= FOUR_GIB), "{granted:x?}");
        assert_eq!(granted.len(), 7 + 4, "every leaf below 4 GiB, and none above");
    }

    /// Against a straightforward model, over windows a fixed seed varies:
    /// what `reserve` hands out is the lowest allowed run at or above the
    /// cursor, never reserved, and `None` exactly where no such run is left.
    #[test]
    fn every_reservation_is_the_lowest_allowed_run() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut draw = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..200 {
            let reserved: Vec<(u64, u64)> = (0..draw(6))
                .map(|_| {
                    let start = draw(2048) * LEAF + draw(LEAF);
                    (start, start + draw(512) * LEAF + draw(LEAF))
                })
                .collect();
            let mut window = Window::new(33, &reserved);
            let leaves: BTreeSet<u64> =
                (1..FOUR_GIB / LEAF).map(|leaf| leaf * LEAF).filter(|at| window.allowed().iter().any(|&(s, e)| s <= *at && at + LEAF <= e)).collect();
            let mut next = 0;
            for _ in 0..60 {
                let span = (1 + draw(64)) * LEAF;
                let fits = |at: &u64| *at >= next && (0..span / LEAF).all(|i| leaves.contains(&(at + i * LEAF)));
                let lowest = leaves.iter().copied().find(fits);
                let got = window.reserve(span);
                assert_eq!(got, lowest, "reserve({span:#x}) with {reserved:x?}");
                if let Some(at) = got {
                    assert!(reserved.iter().all(|&(s, e)| at + span <= s || e <= at), "{at:#x} inside {reserved:x?}");
                    assert!(at + span <= FOUR_GIB);
                    assert!(window.handed_out(at, span));
                    next = at + span;
                }
            }
        }
    }

    #[test]
    fn only_what_was_handed_out_whole_is_room() {
        let mut window = Window::new(48, &[(4 * LEAF, 5 * LEAF)]);
        let first = window.reserve(3 * LEAF).unwrap();
        let after_the_hole = window.reserve(LEAF).unwrap();
        assert_eq!((first, after_the_hole), (LEAF, 5 * LEAF));
        assert!(window.handed_out(first, 3 * LEAF));
        assert!(window.handed_out(2 * LEAF, 1));
        assert!(window.handed_out(after_the_hole, LEAF));
        // Across the reserved leaf, off a boundary, past the cursor, empty, and
        // past the end of the address space: none of it.
        assert!(!window.handed_out(3 * LEAF, 2 * LEAF));
        assert!(!window.handed_out(LEAF + 1, LEAF));
        assert!(!window.handed_out(6 * LEAF, LEAF));
        assert!(!window.handed_out(LEAF, 0));
        assert!(!window.handed_out(!(LEAF - 1), LEAF));
    }

    /// The AMD bench as its firmware declares it (`pcidev: firmware declared
    /// root bridge memory`), with the x86 interrupt range and AMD-Vi's special
    /// range reserved: a function's window, then every leaf left below 4 GiB.
    #[test]
    fn the_amd_benchs_layout() {
        let reserved = vec![
            (0xd000_0000, 0xd070_0000),
            (0xfc_9000_0000, 0xfc_a020_0000),
            (0xd070_0000, 0xf800_0000),
            (0xfc00_0000, 0xfdc0_0000),
            (0xff00_0000, 0xffea_b000),
            (0x4_5020_0000, 0xfc_9000_0000),
            (0xfc_a020_0000, 0xfc_b000_0000),
            (0xfee0_0000, 0xfef0_0000),
            (0xfd_0000_0000, 0x100_0000_0000),
        ];
        let mut window = Window::new(48, &reserved);
        assert_eq!(window.reserve(288 << 20), Some(LEAF));
        // 1663 leaves below the first window, 32 over ECAM, 9 below the
        // interrupt range, less the window's 144.
        assert_eq!(drain(&mut window, LEAF).len(), 1663 + 32 + 9 - 144);
        assert_eq!(window.reserve(LEAF), None);
    }
}
