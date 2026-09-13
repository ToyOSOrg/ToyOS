//! Where a BAR may be put, and which address the caller is handed next.
//!
//! A run is address space the caller found nothing to decode in — a
//! [`bridge::Window`], the same pair of addresses that crate already declares.
//! That the caller found nothing there is a statement about what it could
//! *read*, and none of its sources says whether an address reaches the bus at
//! all; what says that is
//! [`toyos_abi::boot::KernelArgs::root_bridge_windows`], and an address inside
//! none of those windows is never touched.
//!
//! **Selecting an address and taking it out of the runs is one operation.**
//! [`reserve`] is the only way to be offered one, and it holds the runs by
//! `&mut`, so a second caller cannot be handed an address the first is still
//! probing.

use toyos_abi::boot::RootBridgeWindow;

use crate::aperture::{self, Decode};
use crate::bridge::Window;

/// One address a `span`-byte window could take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
    pub at: u64,
    /// The base of the root bridge window holding the whole span, where one
    /// does. `None` is not "nothing decodes it": it is "nothing says", and the
    /// caller names such an address rather than reading it.
    pub inside: Option<u64>,
}

/// The one address `run` offers for a `span`-byte window, and where it stands
/// against `windows`.
///
/// Aligned to the span and not merely to a page: a BAR's low address bits are
/// hardwired to zero, so a window wider than one page has to start on its own
/// size or the function decodes somewhere else (PCIe base spec §7.5.1.2.1).
fn offered(run: &Window, windows: &[RootBridgeWindow], span: u64) -> Option<Candidate> {
    let at = run.start.checked_next_multiple_of(span)?;
    let end = at.checked_add(span)?;
    if end > run.end {
        return None;
    }
    let inside = match aperture::decode(windows, at, end) {
        Decode::Inside(base) => Some(base),
        Decode::Empty | Decode::Unrouted => None,
    };
    Some(Candidate { at, inside })
}

/// The next address `runs` offers for a `span`-byte window, taken out of `runs`
/// in the same call so nothing is offered it twice.
///
/// A run that answered is shortened to what lies above the reservation; the
/// piece below it is shorter than the span that was asked for. A run whose
/// address no window firmware declared holds is emptied instead of shortened:
/// the windows do not change, so nothing there can ever be read.
pub fn reserve(
    runs: &mut [Window],
    windows: &[RootBridgeWindow],
    span: u64,
) -> Option<Candidate> {
    for run in runs.iter_mut() {
        let Some(candidate) = offered(run, windows, span) else { continue };
        run.start = match candidate.inside {
            Some(_) => candidate.at + span,
            None => run.end,
        };
        return Some(candidate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ThinkPad T14's six free runs of 2 MiB or more below `0xfec00000`,
    /// as `pcidev`'s survey printed them, and the two windows its firmware
    /// answered for its one root bridge.
    const T14_RUNS: [Window; 6] = [
        Window { start: 0x9920_0000, end: 0x99a0_0000 },
        Window { start: 0xa080_0000, end: 0xa200_0000 },
        Window { start: 0xae20_0000, end: 0xb000_0000 },
        Window { start: 0xbcf2_0000, end: 0xc000_0000 },
        Window { start: 0xd000_0000, end: 0xfe01_0000 },
        Window { start: 0xfe01_1000, end: 0xfec0_0000 },
    ];
    const T14_WINDOWS: [RootBridgeWindow; 2] = [
        RootBridgeWindow { base: 0xa200_0000, length: 0x1b00_0000 },
        RootBridgeWindow { base: 0x40_0000_0000, length: 0x20_3dc0_0000 },
    ];

    const MIB: u64 = 1024 * 1024;

    /// Every address `runs` hands out before it is empty, in order.
    fn drain(runs: &mut [Window], windows: &[RootBridgeWindow], span: u64) -> std::vec::Vec<Candidate> {
        let mut out = std::vec::Vec::new();
        while let Some(candidate) = reserve(runs, windows, span) {
            out.push(candidate);
        }
        out
    }

    /// Exactly one of the T14's six runs offers an address inside the window
    /// its firmware named, and the 736 MiB run at `0xd0000000` is not it.
    #[test]
    fn one_of_the_t14s_runs_is_inside_the_window_its_firmware_named() {
        let mut runs = T14_RUNS;
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        let inside: std::vec::Vec<_> =
            got.iter().filter_map(|c| c.inside.map(|base| (c.at, base))).collect();
        assert_eq!(inside[0], (0xae20_0000, 0xa200_0000));
        assert!(inside.iter().all(|(at, base)| *base == 0xa200_0000
            && (0xae20_0000..0xb000_0000).contains(at)));
        assert!(got.iter().any(|c| c.at == 0xd000_0000 && c.inside.is_none()));
    }

    /// A reserved address is never offered again, and the walk ends.
    #[test]
    fn a_reserved_address_is_never_offered_twice() {
        let mut runs = T14_RUNS;
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        let mut seen: std::vec::Vec<u64> = got.iter().map(|c| c.at).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count);
        // The one run inside a window is 30 MiB, so it alone answers fifteen
        // times; the five outside answer once each and are then empty.
        assert_eq!(got.iter().filter(|c| c.inside.is_some()).count(), 15);
        assert_eq!(got.iter().filter(|c| c.inside.is_none()).count(), 5);
    }

    /// **The order a caller puts the runs in cannot reach an address the
    /// windows do not hold.** Largest run first is the rule this module
    /// refuses, and on the T14 it reaches the 736 MiB run at `0xd0000000`
    /// first — which still comes back named and never inside.
    #[test]
    fn largest_run_first_still_offers_no_address_outside_a_window() {
        let mut runs = T14_RUNS;
        runs.sort_unstable_by_key(|run| core::cmp::Reverse(run.end - run.start));
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        assert_eq!(got[0], Candidate { at: 0xd000_0000, inside: None });
        let inside: std::vec::Vec<_> =
            got.iter().filter_map(|c| c.inside.map(|base| (c.at, base))).collect();
        assert_eq!(inside[0], (0xae20_0000, 0xa200_0000));
        assert!(inside.iter().all(|(at, _)| (0xae20_0000..0xb000_0000).contains(at)));
    }

    /// A run is offered at its first `span`-aligned address, not at its start.
    ///
    /// `0xbcf20000..0xc0000000`'s first 2 MiB page is `0xbd000000`, one byte
    /// past the end of the window firmware named.
    #[test]
    fn a_run_is_offered_aligned_to_the_span_and_not_at_its_start() {
        let mut run = [Window { start: 0xbcf2_0000, end: 0xc000_0000 }];
        assert_eq!(reserve(&mut run, &T14_WINDOWS, 2 * MIB).unwrap().at, 0xbd00_0000);
        // 64 MiB does not fit between 0xbd000000 and 0xc0000000, and a
        // candidate that ignored the alignment would say it does.
        let mut run = [Window { start: 0xbcf2_0000, end: 0xc000_0000 }];
        assert_eq!(reserve(&mut run, &T14_WINDOWS, 64 * MIB), None);
        // The whole of the span has to be inside the run, not merely its base.
        assert!(reserve(&mut [Window { start: 0, end: 2 * MIB }], &[], 2 * MIB).is_some());
        assert_eq!(reserve(&mut [Window { start: 0, end: 2 * MIB - 1 }], &[], 2 * MIB), None);
    }

    /// The whole span has to be inside a firmware window, not merely its base.
    #[test]
    fn a_span_that_runs_past_a_window_is_not_inside_it() {
        let mut run = [Window { start: 0xbc00_0000, end: 0xc000_0000 }];
        assert_eq!(
            reserve(&mut run, &T14_WINDOWS, 32 * MIB),
            Some(Candidate { at: 0xbc00_0000, inside: None })
        );
        let mut run = [Window { start: 0xbc00_0000, end: 0xc000_0000 }];
        assert_eq!(
            reserve(&mut run, &T14_WINDOWS, 2 * MIB),
            Some(Candidate { at: 0xbc00_0000, inside: Some(0xa200_0000) })
        );
    }

    /// A machine whose firmware named nothing offers addresses no window holds,
    /// and they are answered rather than dropped: the caller names what it left
    /// alone.
    #[test]
    fn a_candidate_no_window_holds_is_answered_and_not_dropped() {
        let mut runs = [Window { start: 0xc020_0000, end: 0xfec0_0000 }];
        assert_eq!(
            reserve(&mut runs, &[], 2 * MIB),
            Some(Candidate { at: 0xc020_0000, inside: None })
        );
        assert_eq!(reserve(&mut runs, &[], 2 * MIB), None);
    }

    /// No run, no span, and a span no run can hold: each answers nothing rather
    /// than an address nothing checked.
    #[test]
    fn nothing_is_offered_where_nothing_fits() {
        let mut runs = T14_RUNS;
        assert_eq!(reserve(&mut [], &T14_WINDOWS, 2 * MIB), None);
        assert_eq!(reserve(&mut runs, &T14_WINDOWS, 0), None);
        assert_eq!(reserve(&mut runs, &T14_WINDOWS, 1 << 31), None);
        // A run at the very top: the aligned address fits and the span does not.
        let mut run = [Window { start: u64::MAX - 2 * MIB, end: u64::MAX }];
        assert_eq!(reserve(&mut run, &[], 2 * MIB), None);
    }
}
