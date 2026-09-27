//! Where a range of user address space goes, and whether it can go anywhere.
//!
//! **A length that crossed the trust boundary becomes a [`PageSpan`] or a
//! refusal, and placement takes nothing else.** `sys_mmap` takes its length
//! from a register; one near `u64::MAX` must come back as an error and not as
//! an overflow trap taken while the address-space lock is held, so the only
//! sum on the raw length is the checked one in [`Window::span`], and
//! [`Window::gap`] cannot be handed a length that did not pass it.
//!
//! Two answers, in the order the kernel asks them. [`Window::span`] is the
//! syscall boundary's: `None` when no placement in the window could ever hold
//! the length — the caller's argument is wrong, which is `InvalidArgument`.
//! [`Window::gap`] is the placement: `None` when a span that could fit does not
//! fit the space as it stands — the machine's answer, which is
//! `ResourceExhausted`.
//!
//! What a region already registered says about itself is the kernel's own
//! ledger and is not re-checked: a wrap there is a kernel bug and traps.

use crate::span::{align_2m_checked, PAGE_2M, USER_TOP};

/// The part of the user half anonymous ranges are placed in, and the guard
/// left above each. Built in a `const`, so a window that breaks
/// [`new`](Self::new)'s asserts does not compile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    floor: u64,
    ceiling: u64,
    guard: u64,
}

/// A length the window's [`span`](Window::span) accepted: whole 2 MiB pages,
/// nonzero, and no more than a window less one guard, so every sum placement
/// takes on it stays inside the user half.
///
/// No operators and no other constructor: the proof travels with the value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PageSpan(u64);

impl PageSpan {
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

impl Window {
    /// Every bound 2 MiB-aligned, inside the user half, with room for at least
    /// the guard: the kernel's own layout, so a window that breaks any of them
    /// is a kernel bug.
    pub const fn new(floor: u64, ceiling: u64, guard: u64) -> Self {
        assert!(
            floor.is_multiple_of(PAGE_2M) && ceiling.is_multiple_of(PAGE_2M) && guard.is_multiple_of(PAGE_2M),
            "a placement window's bounds and guard are whole 2 MiB pages"
        );
        assert!(ceiling <= USER_TOP, "a placement window is inside the user half");
        assert!(floor < ceiling && ceiling - floor >= guard, "a placement window holds its guard");
        Self { floor, ceiling, guard }
    }

    /// Nothing is placed below this.
    pub const fn floor(&self) -> u64 {
        self.floor
    }

    /// The whole-page span a request of `size` bytes occupies, or `None` when
    /// nothing in this window could hold it: zero, or more than the window
    /// less one guard.
    pub fn span(&self, size: u64) -> Option<PageSpan> {
        if size == 0 {
            return None;
        }
        let span = align_2m_checked(size)?;
        (span <= self.ceiling - self.floor - self.guard).then_some(PageSpan(span))
    }

    /// The lowest address of the highest free run that holds `span` plus the
    /// guard above it, or `None` when no run is long enough.
    ///
    /// `taken` is every registered region, as `(start, size)`, highest start
    /// first; one at or above the ceiling is skipped. Nothing is placed below
    /// the floor, and nothing registered below it bounds a run.
    pub fn gap(&self, span: PageSpan, taken: impl IntoIterator<Item = (u64, u64)>) -> Option<u64> {
        // A span is at most a window and every window is inside the user half,
        // so neither this nor `floor + total` below can wrap.
        let total = span.0 + self.guard;
        let mut top = self.ceiling;
        for (start, len) in taken {
            if start >= self.ceiling {
                continue;
            }
            let end = (start + len).next_multiple_of(PAGE_2M);
            if end > top {
                top = start;
                continue;
            }
            if top.saturating_sub(end.max(self.floor)) >= total {
                return Some(top - total);
            }
            top = start;
            if top <= self.floor {
                return None;
            }
        }
        (top >= self.floor + total).then(|| top - total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's own window, by value: `vma::WINDOW`.
    const FLOOR: u64 = 0x0002_0000_0000;
    const CEILING: u64 = 0x00FF_FF80_0000;
    const WINDOW: Window = Window::new(FLOOR, CEILING, PAGE_2M);
    const ROOM: u64 = CEILING - FLOOR - PAGE_2M;

    /// The largest length whose rounding fits `u64`: it rounds to the last
    /// whole page below 2^64, and the guard added to that does not fit.
    const LAST_FIT: u64 = u64::MAX - (PAGE_2M - 1);
    /// One past it, where the rounding itself wraps.
    const FIRST_WRAP: u64 = LAST_FIT + 1;

    fn span(size: u64) -> PageSpan {
        WINDOW.span(size).unwrap_or_else(|| panic!("{size:#x} is placeable"))
    }

    #[test]
    fn a_length_near_the_top_of_u64_is_refused_and_not_summed() {
        for size in [u64::MAX - PAGE_2M, LAST_FIT, FIRST_WRAP, u64::MAX, 1 << 63, USER_TOP] {
            assert_eq!(WINDOW.span(size), None, "{size:#x}");
        }
    }

    #[test]
    fn a_zero_length_is_refused() {
        assert_eq!(WINDOW.span(0), None);
    }

    /// The boundary is the window less its guard, and the whole of it can be
    /// placed in an empty space: a refusal above it and none at it.
    #[test]
    fn the_largest_span_is_the_window_less_one_guard() {
        assert_eq!(span(ROOM).bytes(), ROOM);
        assert_eq!(span(ROOM - PAGE_2M + 1).bytes(), ROOM);
        assert_eq!(WINDOW.span(ROOM + 1), None);
        assert_eq!(WINDOW.gap(span(ROOM), []), Some(FLOOR));
    }

    /// The largest span one window mints, placed in the smallest window the
    /// kernel builds, is a refusal and not a wrap.
    #[test]
    fn a_span_from_a_wider_window_is_refused_by_a_narrower_one() {
        let tiny = Window::new(CEILING - 256 * 1024 * 1024, CEILING, PAGE_2M);
        assert_eq!(tiny.gap(span(ROOM), []), None);
    }

    #[test]
    fn a_span_is_whole_pages() {
        assert_eq!(span(1).bytes(), PAGE_2M);
        assert_eq!(span(PAGE_2M).bytes(), PAGE_2M);
        assert_eq!(span(PAGE_2M + 1).bytes(), 2 * PAGE_2M);
    }

    /// Top down, the guard above the placement, and the next placement below
    /// the last one's start.
    #[test]
    fn placement_is_top_down_with_the_guard_above() {
        let first = WINDOW.gap(span(4096), []).expect("an empty window places");
        assert_eq!(first, CEILING - 2 * PAGE_2M);
        let second = WINDOW.gap(span(4096), [(first, PAGE_2M)]).expect("room below the first");
        assert_eq!(second, first - 2 * PAGE_2M);
    }

    #[test]
    fn a_full_window_is_a_refusal_for_a_span_that_could_fit() {
        assert_eq!(WINDOW.gap(span(PAGE_2M), [(FLOOR, CEILING - FLOOR)]), None);
    }

    /// A run exactly one span and one guard long is used; one page shorter is not.
    #[test]
    fn a_hole_between_regions_takes_exactly_span_and_guard() {
        let low = FLOOR + 8 * PAGE_2M;
        let high = low + PAGE_2M + 3 * PAGE_2M;
        let taken = [(high, CEILING - high), (FLOOR, low - FLOOR + PAGE_2M)];
        assert_eq!(WINDOW.gap(span(2 * PAGE_2M), taken), Some(low + PAGE_2M));
        assert_eq!(WINDOW.gap(span(3 * PAGE_2M), taken), None);
    }

    /// A region the kernel placed below the floor bounds nothing.
    #[test]
    fn nothing_is_placed_below_the_floor() {
        let taken = [(FLOOR + 4 * PAGE_2M, CEILING - FLOOR - 4 * PAGE_2M), (0, PAGE_2M)];
        assert_eq!(WINDOW.gap(span(PAGE_2M), taken), Some(FLOOR + 2 * PAGE_2M));
        assert_eq!(WINDOW.gap(span(2 * PAGE_2M), taken), Some(FLOOR + PAGE_2M));
        assert_eq!(WINDOW.gap(span(3 * PAGE_2M), taken), Some(FLOOR));
        assert_eq!(WINDOW.gap(span(4 * PAGE_2M), taken), None);
    }

    #[test]
    #[should_panic(expected = "whole 2 MiB pages")]
    fn a_window_not_2_mib_aligned_is_a_kernel_bug() {
        let _ = Window::new(FLOOR + 4096, CEILING, PAGE_2M);
    }

    #[test]
    #[should_panic(expected = "holds its guard")]
    fn a_window_without_room_for_its_guard_is_a_kernel_bug() {
        let _ = Window::new(FLOOR, FLOOR + PAGE_2M, 2 * PAGE_2M);
    }

    #[test]
    #[should_panic(expected = "inside the user half")]
    fn a_window_past_the_user_half_is_a_kernel_bug() {
        let _ = Window::new(FLOOR, USER_TOP + PAGE_2M, PAGE_2M);
    }

    /// `find_gap` no longer pre-filters by the ceiling; `gap` is the one
    /// reader of it, so a region registered at or above it must bound nothing.
    #[test]
    fn a_region_at_or_above_the_ceiling_bounds_nothing() {
        let taken = [(CEILING, PAGE_2M), (CEILING + PAGE_2M, PAGE_2M)];
        assert_eq!(WINDOW.gap(span(PAGE_2M), taken), Some(CEILING - 2 * PAGE_2M));
    }
}
