//! Where a BAR may be put, and which address the caller is handed next.
//!
//! A run is address space the caller found nothing to decode in — a
//! [`bridge::Window`], the same pair of addresses that crate already declares.
//! That the caller found nothing there is a statement about what it could
//! *read*, and none of its sources says whether an address reaches the bus at
//! all; what says that is
//! [`toyos_abi::boot::KernelArgs::root_bridge_windows`], and an address inside
//! none of those windows is never offered.
//!
//! **Selecting an address and taking it out of the runs is one operation.**
//! [`reserve`] is the only way to be offered one, and it holds the runs by
//! `&mut`, so a second caller cannot be handed an address the first is still
//! probing.

use toyos_abi::boot::RootBridgeWindow;

use crate::bridge::Window;

/// One address a `span`-byte window may take, and the base of the root bridge
/// window firmware declared it inside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reservation {
    pub at: u64,
    pub window: u64,
    /// Which run it came out of, so [`release`] gives it back to that one.
    run: usize,
}

/// The first `span`-aligned address in `run` whose whole span lies inside one
/// of `windows`, and that window's base.
///
/// Aligned to the span and not merely to a page: a BAR's low address bits are
/// hardwired to zero, so a window wider than one page has to start on its own
/// size or the function decodes somewhere else (PCIe base spec §7.5.1.2.1).
///
/// **A run that straddles a window's edge answers the part inside it**, which
/// is why this asks every window where it meets the run rather than asking
/// about the run's first aligned address alone.
fn offered(run: &Window, windows: &[RootBridgeWindow], span: u64) -> Option<(u64, u64)> {
    let mut first: Option<(u64, u64)> = None;
    for window in windows {
        let Some(at) = run.start.max(window.base).checked_next_multiple_of(span) else {
            continue;
        };
        let Some(end) = at.checked_add(span) else { continue };
        if end > run.end || !window.holds(at, span) {
            continue;
        }
        if first.is_none_or(|(held, _)| at < held) {
            first = Some((at, window.base));
        }
    }
    first
}

/// The next address `runs` offers for a `span`-byte window, taken out of `runs`
/// in the same call so nothing is offered it twice.
///
/// **Only an address inside a window firmware declared is ever answered**: a
/// run holding none is passed over untouched, and so is the part of a run below
/// the address this hands out — which is outside every window, or too low for
/// this span's alignment.
pub fn reserve(
    runs: &mut [Window],
    windows: &[RootBridgeWindow],
    span: u64,
) -> Option<Reservation> {
    for (index, run) in runs.iter_mut().enumerate() {
        let Some((at, window)) = offered(run, windows, span) else { continue };
        run.start = at + span;
        return Some(Reservation { at, window, run: index });
    }
    None
}

/// Put a reservation back.
///
/// A run hands its addresses out upward and holds one extent, so the only one
/// it can take back is the one it handed out last: a reservation another has
/// since been taken above stays spent rather than punching a hole this cannot
/// represent.
pub fn release(runs: &mut [Window], reservation: Reservation, span: u64) {
    let Some(run) = runs.get_mut(reservation.run) else { return };
    if reservation.at.checked_add(span) == Some(run.start) {
        run.start = reservation.at;
    }
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
    fn drain(
        runs: &mut [Window],
        windows: &[RootBridgeWindow],
        span: u64,
    ) -> std::vec::Vec<Reservation> {
        let mut out = std::vec::Vec::new();
        while let Some(reservation) = reserve(runs, windows, span) {
            out.push(reservation);
        }
        out
    }

    /// Every address the T14 offers is inside the window its firmware named,
    /// and the 736 MiB run at `0xd0000000` — a read of which took the machine
    /// down — is offered by nothing.
    #[test]
    fn every_address_the_t14_offers_is_inside_the_window_its_firmware_named() {
        let mut runs = T14_RUNS;
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        assert_eq!(got[0].at, 0xae20_0000);
        assert!(got
            .iter()
            .all(|r| r.window == 0xa200_0000 && (0xae20_0000..0xb000_0000).contains(&r.at)));
    }

    /// **A run that straddles a window's edge answers its inside part.** This
    /// 48 MiB run reaches 32 MiB past the base of the window at `0xa2000000`,
    /// and every one of those sixteen pages is one a BAR may be put at; a run
    /// emptied over its first aligned address being outside loses all of them.
    #[test]
    fn a_run_that_straddles_a_windows_edge_offers_the_part_inside_it() {
        let mut runs = [Window { start: 0xa100_0000, end: 0xa400_0000 }];
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        assert_eq!(got[0], Reservation { at: 0xa200_0000, window: 0xa200_0000, run: 0 });
        assert_eq!(got.len(), 16);
        assert!(got.iter().all(|r| (0xa200_0000..0xa400_0000).contains(&r.at)));
    }

    /// A reserved address is never offered again, and the walk ends.
    #[test]
    fn a_reserved_address_is_never_offered_twice() {
        let mut runs = T14_RUNS;
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        let mut seen: std::vec::Vec<u64> = got.iter().map(|r| r.at).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), got.len());
        // The one run inside the window is 30 MiB, and it alone answers.
        assert_eq!(got.len(), 15);
    }

    /// **The order a caller puts the runs in cannot reach an address the
    /// windows do not hold.** Largest run first is the rule this module
    /// refuses, and on the T14 it puts the 736 MiB run at `0xd0000000` first —
    /// which answers nothing whatever its position.
    #[test]
    fn largest_run_first_still_offers_no_address_outside_a_window() {
        let mut runs = T14_RUNS;
        runs.sort_unstable_by_key(|run| core::cmp::Reverse(run.end - run.start));
        let got = drain(&mut runs, &T14_WINDOWS, 2 * MIB);
        assert_eq!(got[0].at, 0xae20_0000);
        assert!(got.iter().all(|r| (0xae20_0000..0xb000_0000).contains(&r.at)));
    }

    /// A run is offered at a `span`-aligned address, not at its start.
    ///
    /// `0xbcf20000..0xc0000000`'s first 2 MiB page is `0xbd000000`, one byte
    /// past the end of the window firmware named, so that run answers nothing
    /// at all.
    #[test]
    fn a_run_is_offered_aligned_to_the_span_and_not_at_its_start() {
        let mut run = [Window { start: 0xbcf2_0000, end: 0xc000_0000 }];
        assert_eq!(reserve(&mut run, &T14_WINDOWS, 2 * MIB), None);
        let mut run = [Window { start: 0xbc0f_0000, end: 0xc000_0000 }];
        assert_eq!(reserve(&mut run, &T14_WINDOWS, 2 * MIB).unwrap().at, 0xbc20_0000);
        // The whole of the span has to be inside the run, not merely its base.
        let all = [RootBridgeWindow { base: 0, length: u64::MAX }];
        assert!(reserve(&mut [Window { start: 0, end: 2 * MIB }], &all, 2 * MIB).is_some());
        assert_eq!(reserve(&mut [Window { start: 0, end: 2 * MIB - 1 }], &all, 2 * MIB), None);
    }

    /// The whole span has to be inside a firmware window, not merely its base:
    /// 32 MiB at `0xbc000000` would reach past `0xbd000000`, where the window
    /// ends.
    #[test]
    fn a_span_that_runs_past_a_window_is_not_inside_it() {
        let mut run = [Window { start: 0xbc00_0000, end: 0xc000_0000 }];
        assert_eq!(reserve(&mut run, &T14_WINDOWS, 32 * MIB), None);
        let mut run = [Window { start: 0xbc00_0000, end: 0xc000_0000 }];
        assert_eq!(
            reserve(&mut run, &T14_WINDOWS, 2 * MIB),
            Some(Reservation { at: 0xbc00_0000, window: 0xa200_0000, run: 0 })
        );
    }

    /// A machine whose firmware named nothing is offered nothing, whatever its
    /// runs hold, and its runs are left as they were.
    #[test]
    fn nothing_is_offered_where_firmware_named_no_window() {
        let mut runs = [Window { start: 0xc020_0000, end: 0xfec0_0000 }];
        assert_eq!(reserve(&mut runs, &[], 2 * MIB), None);
        assert_eq!(runs[0], Window { start: 0xc020_0000, end: 0xfec0_0000 });
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
        let all = [RootBridgeWindow { base: 0, length: u64::MAX }];
        let mut run = [Window { start: u64::MAX - 2 * MIB, end: u64::MAX }];
        assert_eq!(reserve(&mut run, &all, 2 * MIB), None);
    }

    /// An address a caller could not use goes back to the run it came from and
    /// is offered again; one another reservation has been taken above stays
    /// spent.
    #[test]
    fn a_released_address_is_offered_again() {
        let mut runs = T14_RUNS;
        let first = reserve(&mut runs, &T14_WINDOWS, 2 * MIB).expect("an address");
        release(&mut runs, first, 2 * MIB);
        assert_eq!(reserve(&mut runs, &T14_WINDOWS, 2 * MIB), Some(first));
        let second = reserve(&mut runs, &T14_WINDOWS, 2 * MIB).expect("a second address");
        release(&mut runs, first, 2 * MIB);
        release(&mut runs, second, 2 * MIB);
        assert_eq!(reserve(&mut runs, &T14_WINDOWS, 2 * MIB), Some(second));
    }
}
