//! Where a BAR may be put, and in what order the machine is asked about each
//! address.
//!
//! A [`Run`] is address space the caller found nothing to decode in. That is a
//! statement about what the caller could *read* — a firmware memory map, the
//! BARs a bus has assigned, the ranges its bridges forward — and none of those
//! says whether an address reaches the bus at all. What says that is the host
//! bridge's aperture, which is ACPI's `_CRS`, and the nearest thing in reach is
//! the current settings firmware answered for its root bridges
//! ([`toyos_abi::boot::KernelArgs::root_bridge_windows`]).
//!
//! **Those settings order the candidates and decide none of them.** A run
//! inside one is a run some bridge is forwarding today; a run outside it is not
//! thereby unreachable, because the answer describes what firmware *used* and
//! not what the platform decodes — QEMU's q35 routes everything above RAM to
//! PCI while its firmware answers one megabyte. So this yields every candidate,
//! firmware's own first, and the caller settles each one against the machine.

use toyos_abi::boot::RootBridgeWindow;

use crate::aperture::{self, Decode};

/// A run of address space the caller found nothing to decode in, `start..end`,
/// `end` exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    pub start: u64,
    pub end: u64,
}

/// One address a `span`-byte window could take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
    pub at: u64,
    /// The base of the root bridge window holding the whole span, where one
    /// does. `None` is not "nothing decodes it": it is "nothing says".
    pub inside: Option<u64>,
}

/// The first `span`-byte window `run` can hold, aligned to `span`.
///
/// Aligned to the span and not merely to a page: a BAR's low address bits are
/// hardwired to zero, so a window wider than one page has to start on its own
/// size or the function decodes somewhere else (PCIe base spec §7.5.1.2.1).
fn fitted(run: &Run, span: u64) -> Option<u64> {
    let at = run.start.checked_next_multiple_of(span)?;
    (at.checked_add(span)? <= run.end).then_some(at)
}

/// Every address `runs` offers for a `span`-byte window: the ones a window
/// firmware named holds first, then the rest, each set in `runs` order.
///
/// One candidate per run, because a run that has been taken from is a shorter
/// run — the caller cuts what it placed and asks again.
pub fn candidates<'a>(
    runs: &'a [Run],
    windows: &'a [RootBridgeWindow],
    span: u64,
) -> Candidates<'a> {
    Candidates { runs, windows, span, at: 0, rest: false }
}

/// [`candidates`]'s iterator: two passes over the runs, holding no allocation
/// because this crate has none to hold.
pub struct Candidates<'a> {
    runs: &'a [Run],
    windows: &'a [RootBridgeWindow],
    span: u64,
    at: usize,
    /// False while the runs firmware's own answer holds are being yielded.
    rest: bool,
}

impl Iterator for Candidates<'_> {
    type Item = Candidate;

    fn next(&mut self) -> Option<Candidate> {
        loop {
            let Some(run) = self.runs.get(self.at) else {
                if self.rest {
                    return None;
                }
                self.rest = true;
                self.at = 0;
                continue;
            };
            self.at += 1;
            let Some(at) = fitted(run, self.span) else { continue };
            let inside = match aperture::decode(self.windows, at, at + self.span) {
                Decode::Inside(base) => Some(base),
                Decode::Empty | Decode::Unrouted => None,
            };
            if inside.is_some() != self.rest {
                return Some(Candidate { at, inside });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ThinkPad T14's six free runs of 2 MiB or more below `0xfec00000`,
    /// as `pcidev`'s survey printed them, and the two windows its firmware
    /// answered for its one root bridge.
    const T14_RUNS: [Run; 6] = [
        Run { start: 0x9920_0000, end: 0x99a0_0000 },
        Run { start: 0xa080_0000, end: 0xa200_0000 },
        Run { start: 0xae20_0000, end: 0xb000_0000 },
        Run { start: 0xbcf2_0000, end: 0xc000_0000 },
        Run { start: 0xd000_0000, end: 0xfe01_0000 },
        Run { start: 0xfe01_1000, end: 0xfec0_0000 },
    ];
    const T14_WINDOWS: [RootBridgeWindow; 2] = [
        RootBridgeWindow { base: 0xa200_0000, length: 0x1b00_0000 },
        RootBridgeWindow { base: 0x40_0000_0000, length: 0x20_3dc0_0000 },
    ];

    const MIB: u64 = 1024 * 1024;

    fn addresses(runs: &[Run], windows: &[RootBridgeWindow], span: u64) -> std::vec::Vec<u64> {
        candidates(runs, windows, span).map(|c| c.at).collect()
    }

    /// **Firmware's answer orders them, and the order is the whole claim.** One
    /// of that machine's six runs lies inside the window its firmware named,
    /// and it comes first; the 736 MiB run at `0xd0000000` — the one a search
    /// by size would have taken, and the one that reads all-ones on the
    /// machine — comes fourth of the five that follow.
    #[test]
    fn the_runs_firmware_named_come_first_and_the_rest_follow_by_address() {
        assert_eq!(
            addresses(&T14_RUNS, &T14_WINDOWS, 2 * MIB),
            [0xae20_0000, 0x9920_0000, 0xa080_0000, 0xbd00_0000, 0xd000_0000, 0xfe20_0000],
        );
        // And exactly one of them is inside, named by the window's base.
        let inside: std::vec::Vec<_> = candidates(&T14_RUNS, &T14_WINDOWS, 2 * MIB)
            .filter_map(|c| c.inside.map(|base| (c.at, base)))
            .collect();
        assert_eq!(inside, [(0xae20_0000, 0xa200_0000)]);
    }

    /// A run is offered at its first `span`-aligned address, not at its start.
    ///
    /// `0xbcf20000..0xc0000000` is 49 MiB of free space whose first 2 MiB page
    /// is `0xbd000000` — one byte past the end of the window firmware named, so
    /// the alignment is also what moves that run out of the first pass.
    #[test]
    fn a_run_is_offered_aligned_to_the_span_and_not_at_its_start() {
        let run = [Run { start: 0xbcf2_0000, end: 0xc000_0000 }];
        assert_eq!(addresses(&run, &T14_WINDOWS, 2 * MIB), [0xbd00_0000]);
        assert_eq!(addresses(&run, &T14_WINDOWS, 16 * MIB), [0xbd00_0000]);
        // 64 MiB does not fit between 0xbd000000 and 0xc0000000, and a
        // candidate that ignored the alignment would say it does.
        assert_eq!(addresses(&run, &T14_WINDOWS, 64 * MIB), []);
        // The whole of the span has to be inside the run, not merely its base.
        assert_eq!(addresses(&[Run { start: 0, end: 2 * MIB }], &[], 2 * MIB), [0]);
        assert_eq!(addresses(&[Run { start: 0, end: 2 * MIB - 1 }], &[], 2 * MIB), []);
    }

    /// The whole span has to be inside a firmware window for the window to
    /// order it. A span that starts inside and runs past is in the second pass,
    /// where the machine is still asked about it.
    #[test]
    fn a_span_that_runs_past_a_window_is_not_inside_it() {
        // 0xbc000000 is inside 0xa2000000..0xbd000000; 0xbc000000 + 32 MiB is
        // not, so this run's candidate falls to the second pass.
        let run = [Run { start: 0xbc00_0000, end: 0xc000_0000 }];
        let got: std::vec::Vec<_> = candidates(&run, &T14_WINDOWS, 32 * MIB).collect();
        assert_eq!(got, [Candidate { at: 0xbc00_0000, inside: None }]);
        // The same run at 2 MiB is wholly inside and comes back named.
        let got: std::vec::Vec<_> = candidates(&run, &T14_WINDOWS, 2 * MIB).collect();
        assert_eq!(got, [Candidate { at: 0xbc00_0000, inside: Some(0xa200_0000) }]);
    }

    /// QEMU's q35 under this repository's OVMF: the firmware answer is one
    /// megabyte, so no run is inside it and every candidate is in the second
    /// pass — which is the machine this ordering may not strand.
    #[test]
    fn a_machine_whose_answer_holds_no_run_still_offers_every_run() {
        let windows = [
            RootBridgeWindow { base: 0xc000_0000, length: 0x10_0000 },
            RootBridgeWindow { base: 0x8_0000_0000, length: 0x10_0000 },
        ];
        let runs = [Run { start: 0xc020_0000, end: 0xfec0_0000 }];
        let got: std::vec::Vec<_> = candidates(&runs, &windows, 2 * MIB).collect();
        assert_eq!(got, [Candidate { at: 0xc020_0000, inside: None }]);
        // And a machine whose firmware named nothing at all is the same case,
        // not an empty one.
        let got: std::vec::Vec<_> = candidates(&runs, &[], 2 * MIB).collect();
        assert_eq!(got, [Candidate { at: 0xc020_0000, inside: None }]);
    }

    /// No run, no span, and a span no run can hold: each answers nothing rather
    /// than an address nothing checked.
    #[test]
    fn nothing_is_offered_where_nothing_fits() {
        assert_eq!(addresses(&[], &T14_WINDOWS, 2 * MIB), []);
        assert_eq!(addresses(&T14_RUNS, &T14_WINDOWS, 0), []);
        assert_eq!(addresses(&T14_RUNS, &T14_WINDOWS, 1 << 31), []);
        // A run at the very top: the aligned address fits and the span does not.
        let run = [Run { start: u64::MAX - 2 * MIB, end: u64::MAX }];
        assert_eq!(addresses(&run, &[], 2 * MIB), []);
    }
}
