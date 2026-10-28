//! Where a remapping unit's protected memory regions go while it is handed over.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

pub const FOUR_GIB: u64 = 1 << 32;

/// `[base, end)`, never empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub base: u64,
    pub end: u64,
}

impl Span {
    pub fn holds(self, (start, end): (u64, u64)) -> bool {
        self.base <= start && end <= self.end
    }
}

/// The largest part of `ram` inside `[lo, hi)` that touches no `avoid` range
/// and spans no gap between `ram`'s ranges, with its base rounded up and its
/// end rounded down to `align` (a power of two), among the parts holding every
/// `within` range that starts inside `[lo, hi)`. `None` if no part does.
pub fn largest(
    ram: &[(u64, u64)],
    lo: u64,
    hi: u64,
    avoid: &[(u64, u64)],
    align: u64,
    within: &[(u64, u64)],
) -> Option<Span> {
    assert!(align.is_power_of_two(), "a protected region's alignment {align:#x} is not a power of two");
    let mut ram: Vec<(u64, u64)> = ram.iter().copied().filter(|&(s, e)| s < e).collect();
    ram.sort_unstable();
    // Adjacent ranges are one stretch; a gap ends it.
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ram {
        match runs.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => runs.push((start, end)),
        }
    }
    let mut avoid: Vec<(u64, u64)> = avoid.iter().copied().filter(|&(s, e)| s < e).collect();
    avoid.sort_unstable();
    let needed: Vec<(u64, u64)> = within.iter().copied().filter(|&(s, _)| lo <= s && s < hi).collect();
    let mut best: Option<Span> = None;
    for (start, end) in runs {
        let mut piece = start.max(lo);
        let stop = end.min(hi);
        for &(a, b) in avoid.iter().chain(core::iter::once(&(stop, u64::MAX))) {
            if b <= piece {
                continue;
            }
            let cut = a.min(stop);
            if cut > piece {
                let base = piece.div_ceil(align).saturating_mul(align);
                let end = cut / align * align;
                let span = Span { base, end };
                if base < end
                    && needed.iter().all(|&r| span.holds(r))
                    && best.is_none_or(|b| end - base > b.end - b.base)
                {
                    best = Some(span);
                }
            }
            if b >= stop {
                break;
            }
            piece = piece.max(b);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const MIB: u64 = 1 << 20;

    /// RAM up to a graphics region an RMRR names, and RAM again above 4 GiB.
    #[test]
    fn a_region_stops_at_the_reserved_region_and_at_4_gib() {
        let ram = [(0x10_0000, 0x9c00_0000), (0x1_0000_0000, 0x4_5f80_0000)];
        let avoid = [(0x9c00_0000, 0xa080_0000)];
        assert_eq!(
            largest(&ram, 0, FOUR_GIB, &avoid, MIB, &[]),
            Some(Span { base: 0x10_0000, end: 0x9c00_0000 })
        );
        assert_eq!(
            largest(&ram, FOUR_GIB, u64::MAX, &avoid, MIB, &[]),
            Some(Span { base: FOUR_GIB, end: 0x4_5f80_0000 })
        );
    }

    #[test]
    fn a_gap_in_the_map_ends_a_stretch_and_adjacent_ranges_join() {
        let ram = [(0x0100_0000, 0x2000_0000), (0x2000_0000, 0x3000_0000), (0x3010_0000, 0x3800_0000)];
        assert_eq!(largest(&ram, 0, FOUR_GIB, &[], MIB, &[]), Some(Span { base: 0x0100_0000, end: 0x3000_0000 }));
    }

    #[test]
    fn a_reserved_region_inside_a_stretch_leaves_its_larger_side() {
        let ram = [(0x0100_0000, 0x8000_0000)];
        let avoid = [(0x2000_0000, 0x2010_0000)];
        assert_eq!(largest(&ram, 0, FOUR_GIB, &avoid, MIB, &[]), Some(Span { base: 0x2010_0000, end: 0x8000_0000 }));
    }

    #[test]
    fn a_region_is_the_part_that_holds_what_it_must_cover_even_when_smaller() {
        let ram = [(0x0100_0000, 0x2000_0000), (0x2010_0000, 0x8000_0000)];
        let tables = [(0x0160_0000, 0x0180_0000)];
        assert_eq!(largest(&ram, 0, FOUR_GIB, &[], MIB, &tables), Some(Span { base: 0x0100_0000, end: 0x2000_0000 }));
        // Ranges that start in the other window bind nothing here.
        assert_eq!(
            largest(&ram, 0, FOUR_GIB, &[], MIB, &[(0x1_0000_0000, 0x1_0020_0000)]),
            Some(Span { base: 0x2010_0000, end: 0x8000_0000 })
        );
    }

    #[test]
    fn what_no_one_part_holds_leaves_no_region() {
        let ram = [(0x0100_0000, 0x2000_0000), (0x2010_0000, 0x8000_0000)];
        let split = [(0x0160_0000, 0x0180_0000), (0x5000_0000, 0x5020_0000)];
        assert!(largest(&ram, 0, FOUR_GIB, &[], MIB, &split).is_none());
        let under_an_rmrr = [(0x3000_0000, 0x3020_0000)];
        assert!(largest(&ram, 0, FOUR_GIB, &[(0x3000_0000, 0x3100_0000)], MIB, &under_an_rmrr).is_none());
    }

    #[test]
    fn alignment_only_ever_shrinks_a_region() {
        let ram = [(0x0123_4000, 0x0987_6000)];
        let span = largest(&ram, 0, FOUR_GIB, &[], 16 * MIB, &[]).unwrap();
        assert_eq!(span, Span { base: 0x0200_0000, end: 0x0900_0000 });
        assert!(largest(&ram, 0, FOUR_GIB, &[], 1 << 30, &[]).is_none());
    }

    #[test]
    fn nothing_is_protected_where_nothing_is_ram() {
        assert!(largest(&[], 0, FOUR_GIB, &[], MIB, &[]).is_none());
        let ram = vec![(0x1000_0000, 0x2000_0000)];
        assert!(largest(&ram, 0, FOUR_GIB, &[(0, u64::MAX)], MIB, &[]).is_none());
        assert!(largest(&ram, FOUR_GIB, u64::MAX, &[], MIB, &[]).is_none());
    }
}
