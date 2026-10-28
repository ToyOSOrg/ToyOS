//! What a remapping unit's protected memory regions cover while every unit is
//! handed over, and the order the hand-over takes.
//!
//! A unit has one low region, below 4 GiB, and one high region. Each is the
//! largest stretch of memory the firmware map calls RAM, with no reserved
//! region in it and no gap, since a gap may decode as MMIO, above a floor the
//! caller's platform sets. Each is shrunk to
//! the units' alignment, so what is protected is never more than that.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

pub const FOUR_GIB: u64 = 1 << 32;

/// `[base, end)`, never empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub base: u64,
    pub end: u64,
}

/// The largest part of `ram` inside `[lo, hi)` that touches no `avoid` range
/// and no gap between `ram`'s ranges, with its base rounded up and its end
/// rounded down to `align` (a power of two). `None` if nothing is left.
pub fn largest(ram: &[(u64, u64)], lo: u64, hi: u64, avoid: &[(u64, u64)], align: u64) -> Option<Span> {
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
                if base < end && best.is_none_or(|b| end - base > b.end - b.base) {
                    best = Some(Span { base, end });
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

/// One step of handing every unit over, units named by index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Every unit's protected regions on.
    Arm,
    /// Translation off, this kernel's root pointed and invalidated through the
    /// unit's registers, translation on.
    Translate(usize),
    /// Every unit's protected regions off.
    Release,
    /// The invalidation queue and interrupt remapping, which need memory the
    /// regions may cover.
    Finish(usize),
}

/// The hand-over of `units` units, between `Arm` and `Release` where `guarded`.
pub fn order(units: usize, guarded: bool) -> Vec<Step> {
    let mut steps = Vec::with_capacity(2 * units + 2);
    if guarded {
        steps.push(Step::Arm);
    }
    steps.extend((0..units).map(Step::Translate));
    if guarded {
        steps.push(Step::Release);
    }
    steps.extend((0..units).map(Step::Finish));
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const MIB: u64 = 1 << 20;

    #[test]
    fn every_translation_happens_inside_the_guard_and_nothing_needs_memory_until_it_is_gone() {
        for units in 0..5 {
            let steps = order(units, true);
            let at = |s: Step| steps.iter().position(|&t| t == s).unwrap();
            let arm = at(Step::Arm);
            let release = at(Step::Release);
            for unit in 0..units {
                assert!(arm < at(Step::Translate(unit)), "{steps:?}");
                assert!(at(Step::Translate(unit)) < release, "{steps:?}");
                assert!(release < at(Step::Finish(unit)), "{steps:?}");
            }
            assert_eq!(steps.len(), 2 * units + 2);
        }
    }

    #[test]
    fn an_unguarded_hand_over_still_translates_every_unit_before_finishing_any() {
        let steps = order(3, false);
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Arm | Step::Release)).count(), 0);
        let last_translate = steps.iter().rposition(|s| matches!(s, Step::Translate(_))).unwrap();
        let first_finish = steps.iter().position(|s| matches!(s, Step::Finish(_))).unwrap();
        assert!(last_translate < first_finish);
    }

    /// The T14's shape: RAM up to its graphics region, which an RMRR names,
    /// and RAM again above 4 GiB.
    #[test]
    fn a_region_stops_at_the_reserved_region_and_at_4_gib() {
        let ram = [(0x10_0000, 0x9c00_0000), (0x1_0000_0000, 0x4_5f80_0000)];
        let avoid = [(0x9c00_0000, 0xa080_0000)];
        assert_eq!(
            largest(&ram, 16 * MIB, FOUR_GIB, &avoid, MIB),
            Some(Span { base: 16 * MIB, end: 0x9c00_0000 })
        );
        assert_eq!(
            largest(&ram, FOUR_GIB, u64::MAX, &avoid, MIB),
            Some(Span { base: FOUR_GIB, end: 0x4_5f80_0000 })
        );
    }

    #[test]
    fn a_gap_in_the_map_ends_a_stretch_and_adjacent_ranges_join() {
        let ram = [(0x0100_0000, 0x2000_0000), (0x2000_0000, 0x3000_0000), (0x3010_0000, 0x3800_0000)];
        assert_eq!(largest(&ram, 0, FOUR_GIB, &[], MIB), Some(Span { base: 0x0100_0000, end: 0x3000_0000 }));
    }

    #[test]
    fn a_reserved_region_inside_a_stretch_leaves_its_larger_side() {
        let ram = [(0x0100_0000, 0x8000_0000)];
        let avoid = [(0x2000_0000, 0x2010_0000)];
        assert_eq!(largest(&ram, 0, FOUR_GIB, &avoid, MIB), Some(Span { base: 0x2010_0000, end: 0x8000_0000 }));
    }

    #[test]
    fn alignment_only_ever_shrinks_a_region() {
        let ram = [(0x0123_4000, 0x0987_6000)];
        let span = largest(&ram, 0, FOUR_GIB, &[], 16 * MIB).unwrap();
        assert_eq!(span, Span { base: 0x0200_0000, end: 0x0900_0000 });
        assert!(largest(&ram, 0, FOUR_GIB, &[], 1 << 30).is_none());
    }

    #[test]
    fn nothing_is_protected_where_nothing_is_ram() {
        assert!(largest(&[], 0, FOUR_GIB, &[], MIB).is_none());
        let ram = vec![(0x1000_0000, 0x2000_0000)];
        assert!(largest(&ram, 0, FOUR_GIB, &[(0, u64::MAX)], MIB).is_none());
        assert!(largest(&ram, FOUR_GIB, u64::MAX, &[], MIB).is_none());
    }
}
