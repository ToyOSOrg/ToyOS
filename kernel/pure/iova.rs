//! A domain's device addresses: which a unit may hand out, in what order, and
//! never at or above what the function they are handed to can issue.
//!
//! The allowed addresses are the unit's translated range less what the
//! backend reserves — the first leaf, the root bridges' windows, firmware's
//! reservations, the architecture's special ranges — each widened to whole
//! leaves. They are handed out lowest first and each exactly once, so under
//! hand-out-once no other order keeps a function of any width in range
//! longer.
//!
//! **Every grant ends at or below its holder's [`Limit`].** A function that
//! drops its upper address bits issues an address above its limit as one
//! below it, which can be another grant of the same domain: so refusing is
//! the only answer once nothing below the limit is left.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

/// The one leaf size a domain maps.
pub const LEAF: u64 = 2 << 20;

/// One past the highest device address a function issues, as its driver states
/// it in bits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Limit(u64);

impl Limit {
    /// The narrowest a driver may state: 32 bits.
    pub const NARROWEST: Self = Self(1 << 32);
    /// 64 bits.
    pub const WIDEST: Self = Self(u64::MAX);

    /// `bits` of address, from 32 to 64; `None` outside that.
    pub const fn bits(bits: u8) -> Option<Self> {
        match bits {
            32..=63 => Some(Self(1 << bits)),
            // Not representable one past the end; no unit translates the top leaf.
            64 => Some(Self::WIDEST),
            _ => None,
        }
    }

    pub const fn end(self) -> u64 {
        self.0
    }
}

/// One domain's allowed addresses and how far up they are handed out.
pub struct Window {
    /// Ascending, disjoint, whole leaves, none empty.
    allowed: Vec<(u64, u64)>,
    /// The first address not yet handed out.
    next: u64,
}

impl Window {
    /// `[LEAF, 1 << translatable)` less every `(start, end)` of `reserved`, in
    /// any order, each widened out to whole leaves.
    ///
    /// # Panics
    /// If `translatable` is past 63: no unit translates 64 bits.
    pub fn new(translatable: u8, reserved: &[(u64, u64)]) -> Self {
        assert!(translatable < 64, "iova: a {translatable}-bit unit");
        let mut allowed = alloc::vec![(LEAF, 1u64 << translatable)];
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

    /// The lowest run of `bytes`, rounded up to whole leaves, never handed out
    /// and ending at or below `limit`; `None` where none is left. A refusal
    /// hands nothing out, so a holder with a wider limit is still served.
    pub fn reserve(&mut self, bytes: u64, limit: Limit) -> Option<u64> {
        let span = bytes.checked_next_multiple_of(LEAF).filter(|span| *span != 0)?;
        for &(start, end) in &self.allowed {
            let at = start.max(self.next);
            let stop = at.checked_add(span)?;
            if stop > end {
                continue;
            }
            if stop > limit.end() && !cfg!(feature = "iova-ignores-limit") {
                return None;
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
    use std::collections::{BTreeMap, BTreeSet};

    const GIB: u64 = 1 << 30;

    fn bits(bits: u8) -> Limit {
        Limit::bits(bits).unwrap()
    }

    /// Every grant until `window` refuses one of `bytes` under `limit`.
    fn drain(window: &mut Window, bytes: u64, limit: Limit) -> Vec<(u64, u64)> {
        core::iter::from_fn(|| window.reserve(bytes, limit).map(|at| (at, at + bytes.next_multiple_of(LEAF))))
            .take(1 << 14)
            .collect()
    }

    #[test]
    fn a_limit_is_32_to_64_bits() {
        assert_eq!(Limit::bits(31), None);
        assert_eq!(Limit::bits(65), None);
        assert_eq!(Limit::bits(32), Some(Limit::NARROWEST));
        assert_eq!(bits(40).end(), 1 << 40);
        assert_eq!(bits(64).end(), u64::MAX);
    }

    #[test]
    fn addresses_go_lowest_first_from_the_first_leaf() {
        let mut window = Window::new(48, &[]);
        assert_eq!(window.reserve(1, Limit::NARROWEST), Some(LEAF));
        assert_eq!(window.reserve(LEAF + 1, Limit::NARROWEST), Some(2 * LEAF));
        assert_eq!(window.reserve(LEAF, Limit::NARROWEST), Some(4 * LEAF));
        assert_eq!(window.reserve(0, Limit::NARROWEST), None);
    }

    #[test]
    fn a_reserved_range_is_skipped_by_whole_leaves() {
        // Unaligned at both ends, and a run too long for the gap before it.
        let reserved = [(3 * LEAF + 1, 5 * LEAF - 1)];
        let mut window = Window::new(48, &reserved);
        assert_eq!(window.allowed()[..2], [(LEAF, 3 * LEAF), (5 * LEAF, 1 << 48)]);
        assert_eq!(window.reserve(LEAF, Limit::NARROWEST), Some(LEAF));
        assert_eq!(window.reserve(2 * LEAF, Limit::NARROWEST), Some(5 * LEAF));
        // The leaf the long run could not use is never handed out after it.
        assert_eq!(window.reserve(LEAF, Limit::NARROWEST), Some(7 * LEAF));
    }

    /// The defect a limit exists for: a 40-bit function told an address at or
    /// above `1 << 40` issues it with those bits dropped, and every grant the
    /// window hands out under its limit must stay distinct under that
    /// truncation, to the last one before the refusal.
    #[test]
    fn a_function_that_drops_its_upper_bits_reaches_only_its_own_grant() {
        const WIDTH: u8 = 40;
        // A few leaves below the limit, the rest of the unit above it.
        let mut window = Window::new(44, &[(8 * LEAF, (1 << WIDTH) - 4 * LEAF)]);
        let granted = drain(&mut window, LEAF, bits(WIDTH));
        let mut issued = BTreeMap::new();
        for &(at, end) in &granted {
            let truncated = at & ((1 << WIDTH) - 1);
            if let Some(other) = issued.insert(truncated, at) {
                panic!("the grant at {at:#x} is issued as {truncated:#x}, which is the grant at {other:#x}");
            }
            assert!(end <= 1 << WIDTH, "the grant {at:#x}..{end:#x} ends above the function's limit");
        }
        assert_eq!(granted.len(), 7 + 4, "every leaf below the limit, and none above");
        // The room above stays for a holder that states the wider width.
        assert_eq!(window.reserve(LEAF, bits(44)), Some(1 << WIDTH));
    }

    /// Against a straightforward model, over windows and limits a fixed seed
    /// varies: what `reserve` hands out is the lowest allowed run at or above
    /// the cursor, never reserved, never past its limit, and `None` exactly
    /// where no such run is left.
    #[test]
    fn every_grant_is_the_lowest_allowed_run_below_its_limit() {
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
                    let start = draw(4096) * LEAF + draw(LEAF);
                    (start, start + draw(512) * LEAF + draw(LEAF))
                })
                .collect();
            let mut window = Window::new(33, &reserved);
            let leaves: BTreeSet<u64> =
                (1..(1u64 << 33) / LEAF).map(|leaf| leaf * LEAF).filter(|at| window.allowed().iter().any(|&(s, e)| s <= *at && at + LEAF <= e)).collect();
            let mut next = 0;
            for _ in 0..60 {
                let span = (1 + draw(64)) * LEAF;
                let limit = if draw(2) == 0 { Limit::NARROWEST } else { bits(33) };
                let fits = |at: &u64| *at >= next && (0..span / LEAF).all(|i| leaves.contains(&(at + i * LEAF)));
                let lowest = leaves.iter().copied().find(fits).filter(|at| at + span <= limit.end());
                let got = window.reserve(span, limit);
                assert_eq!(got, lowest, "reserve({span:#x}) under {limit:?} with {reserved:x?}");
                if let Some(at) = got {
                    assert!(reserved.iter().all(|&(s, e)| at + span <= s || e <= at), "{at:#x} inside {reserved:x?}");
                    assert!(window.handed_out(at, span));
                    next = at + span;
                }
            }
        }
    }

    #[test]
    fn only_what_was_handed_out_whole_is_room() {
        let mut window = Window::new(48, &[(4 * LEAF, 5 * LEAF)]);
        let first = window.reserve(3 * LEAF, Limit::NARROWEST).unwrap();
        let after_the_hole = window.reserve(LEAF, Limit::NARROWEST).unwrap();
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
    /// range reserved: the lend window first, then what a 32-bit and a 40-bit
    /// function each get before they are refused.
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
        assert_eq!(window.reserve(256 << 20, Limit::NARROWEST), Some(LEAF));
        let narrow = drain(&mut window, LEAF, Limit::NARROWEST).len();
        let wide = drain(&mut window, LEAF, bits(40)).len();
        // 1663 leaves below the first window, 32 over ECAM, 9 below the
        // interrupt range, less the lend window's 128.
        assert_eq!(narrow, 1663 + 32 + 9 - 128);
        // Then 6785 up to the 64-bit window and 640 between it and the special range.
        assert_eq!(wide, 6785 + 640);
        assert!(narrow as u64 * LEAF > 3 * GIB);
        assert_eq!(window.reserve(LEAF, bits(41)), Some(1 << 40));
    }
}
