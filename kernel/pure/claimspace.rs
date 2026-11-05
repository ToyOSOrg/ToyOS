//! Where a claimed function's device addresses go: one window its domain
//! reserves once, grants in its first part and lent regions in the rest, so
//! claiming, granting, lending and ending as often as holders like spends no
//! address outside it.
//!
//! **A function no reset quiets may still be aimed at what its last holder
//! was given**, its residue. A lent region is never placed over residue: a
//! stale transfer must land in the new holder's fresh memory, never in a
//! region another process may still read. A grant is fresh memory, so it may
//! be, and the residue it covers is the grant's from then on.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

use crate::iova::LEAF;

/// `bytes` of device addresses from `at`, whole leaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub at: u64,
    pub bytes: u64,
}

impl Span {
    pub fn end(self) -> u64 {
        self.at + self.bytes
    }

    fn touches(self, other: Self) -> bool {
        self.at < other.end() && other.at < self.end()
    }
}

/// One function's window: `grants` bytes for its grants from `at`, then
/// `lends` bytes for the regions its holders lend it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    at: u64,
    grants: u64,
    lends: u64,
}

impl Window {
    /// # Panics
    /// If any of the three is not a whole number of leaves.
    pub const fn new(at: u64, grants: u64, lends: u64) -> Self {
        assert!(at.is_multiple_of(LEAF) && grants.is_multiple_of(LEAF) && lends.is_multiple_of(LEAF), "claimspace: a window of part leaves");
        Self { at, grants, lends }
    }

    pub const fn end(self) -> u64 {
        self.at + self.grants + self.lends
    }

    /// The lowest run of `bytes` in the grant part that touches none of
    /// `live`, what the claim holds now, taken out of `residue`; and whether
    /// any of `residue` was there.
    pub fn grant(self, bytes: u64, live: impl Iterator<Item = Span> + Clone, residue: &mut Vec<Span>) -> Option<(Span, bool)> {
        let span = first_free(self.at, self.at + self.grants, bytes, live)?;
        Some((span, carve(residue, span)))
    }

    /// The lowest run of `bytes` in the lend part that touches none of
    /// `live` and none of `residue`.
    pub fn lend_room(self, bytes: u64, live: impl Iterator<Item = Span> + Clone, residue: &[Span]) -> Option<Span> {
        first_free(self.at + self.grants, self.end(), bytes, live.chain(residue.iter().copied()))
    }
}

/// The lowest leaf-aligned run of `bytes` in `[start, end)` that touches none
/// of `taken`; `None` for an empty or part-leaf `bytes`, or where none is left.
fn first_free(start: u64, end: u64, bytes: u64, taken: impl Iterator<Item = Span> + Clone) -> Option<Span> {
    if bytes == 0 || !bytes.is_multiple_of(LEAF) {
        return None;
    }
    (start..end)
        .step_by(LEAF as usize)
        .map(|at| Span { at, bytes })
        .take_while(|span| span.end() <= end)
        .find(|span| !taken.clone().any(|other| other.touches(*span)))
}

/// `residue` less `span`, splitting a range it covers part of; whether it
/// covered any.
fn carve(residue: &mut Vec<Span>, span: Span) -> bool {
    let before = residue.len();
    let mut carved = false;
    let mut kept = Vec::with_capacity(before + 1);
    for aimed in residue.drain(..) {
        if !aimed.touches(span) {
            kept.push(aimed);
            continue;
        }
        carved = true;
        if aimed.at < span.at {
            kept.push(Span { at: aimed.at, bytes: span.at - aimed.at });
        }
        if span.end() < aimed.end() {
            kept.push(Span { at: span.end(), bytes: aimed.end() - span.end() });
        }
    }
    *residue = kept;
    carved
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const MIB: u64 = 1 << 20;
    /// The shapes `pcidev` gives a window: 32 MiB of grants of up to 8 MiB
    /// each, then 256 MiB to lend in, below 4 GiB.
    const WINDOW: Window = Window::new(0x8000_0000, 32 * MIB, 256 * MIB);
    const MAX_GRANT: u64 = 8 * MIB;

    fn draw(seed: &mut u64, n: u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed % n
    }

    /// Grants of random sizes until the next would pass the grant part: what
    /// one claim holds.
    fn layout(seed: &mut u64) -> Vec<u64> {
        let mut held = 0;
        let mut sizes = Vec::new();
        loop {
            let bytes = (1 + draw(seed, MAX_GRANT / LEAF)) * LEAF;
            if held + bytes > 32 * MIB || draw(seed, 8) == 0 {
                return sizes;
            }
            held += bytes;
            sizes.push(bytes);
        }
    }

    fn inside(span: Span, from: u64, to: u64) -> bool {
        from <= span.at && span.end() <= to
    }

    #[test]
    fn grants_go_lowest_first_in_the_grant_part_and_lends_after_it() {
        let live = [Span { at: 0x8000_0000, bytes: 2 * LEAF }];
        let none = &mut Vec::new();
        assert_eq!(WINDOW.grant(LEAF, live.iter().copied(), none), Some((Span { at: 0x8000_0000 + 2 * LEAF, bytes: LEAF }, false)));
        assert_eq!(WINDOW.lend_room(LEAF, live.iter().copied(), &[]), Some(Span { at: 0x8000_0000 + 32 * MIB, bytes: LEAF }));
        assert_eq!(WINDOW.grant(33 * MIB, core::iter::empty(), none), None, "a grant past the grant part");
        assert_eq!(WINDOW.grant(LEAF + 1, core::iter::empty(), none), None, "a part leaf");
        assert_eq!(WINDOW.grant(0, core::iter::empty(), none), None);
    }

    #[test]
    fn a_lent_region_is_never_placed_over_residue_and_a_grant_may_be() {
        let lends = 0x8000_0000 + 32 * MIB;
        let mut residue = vec![Span { at: 0x8000_0000, bytes: 2 * LEAF }, Span { at: lends, bytes: LEAF }];
        assert_eq!(WINDOW.lend_room(LEAF, core::iter::empty(), &residue), Some(Span { at: lends + LEAF, bytes: LEAF }));
        assert_eq!(WINDOW.grant(LEAF, core::iter::empty(), &mut residue), Some((Span { at: 0x8000_0000, bytes: LEAF }, true)));
        assert_eq!(residue, [Span { at: 0x8000_0000 + LEAF, bytes: LEAF }, Span { at: lends, bytes: LEAF }]);
    }

    #[test]
    fn carving_keeps_what_a_grant_does_not_cover() {
        let mut residue = vec![Span { at: 0, bytes: 4 * LEAF }, Span { at: 8 * LEAF, bytes: LEAF }];
        assert!(carve(&mut residue, Span { at: LEAF, bytes: LEAF }));
        assert_eq!(residue, [Span { at: 0, bytes: LEAF }, Span { at: 2 * LEAF, bytes: 2 * LEAF }, Span { at: 8 * LEAF, bytes: LEAF }]);
        assert!(!carve(&mut residue, Span { at: 5 * LEAF, bytes: 2 * LEAF }), "nothing aimed there");
        assert!(carve(&mut residue, Span { at: 0, bytes: 9 * LEAF }));
        assert_eq!(residue, []);
    }

    /// A function no reset quiets ends each claim aimed at all it was given and
    /// all it never covered of what it was aimed at before. Whatever layout
    /// each holder chooses, every grant its claim may hold is served, never
    /// outside the window, a lent region never over residue — and the residue
    /// never leaves the window either, so nothing a function was ever aimed
    /// at is an address its window does not keep, and the residue's ranges
    /// never overlap, so what it holds is bounded by the window too.
    #[test]
    fn a_function_no_reset_quiets_serves_every_holder_whatever_the_last_one_laid_out() {
        let mut seed = 0x0123_4567_89AB_CDEFu64;
        let mut residue: Vec<Span> = Vec::new();
        for claim in 0..2000 {
            let mut live: Vec<Span> = Vec::new();
            for bytes in layout(&mut seed) {
                let (span, _) = WINDOW
                    .grant(bytes, live.iter().copied(), &mut residue)
                    .unwrap_or_else(|| panic!("claim {claim}: a {bytes:#x} grant was refused beside {live:x?}"));
                assert!(inside(span, 0x8000_0000, 0x8000_0000 + 32 * MIB));
                live.push(span);
                if draw(&mut seed, 3) == 0 {
                    let lent = (1 + draw(&mut seed, 4)) * LEAF;
                    if let Some(span) = WINDOW.lend_room(lent, live.iter().copied(), &residue) {
                        assert!(inside(span, 0x8000_0000 + 32 * MIB, WINDOW.end()));
                        assert!(!residue.iter().any(|aimed| aimed.touches(span)), "lent over residue");
                        live.push(span);
                    }
                }
            }
            residue.extend(live);
            assert!(residue.iter().all(|aimed| inside(*aimed, 0x8000_0000, WINDOW.end())));
            for (i, a) in residue.iter().enumerate() {
                assert!(!residue[i + 1..].iter().any(|b| a.touches(*b)), "claim {claim}: residue {a:x?} overlaps another range");
            }
        }
    }
}
