//! Which PCI functions a `drives` row has a process on.
//!
//! **One process per function a `drives` matcher names and the kernel does not
//! drive**, keyed by the function's arrival. The supervisor reads the kernel's
//! device set whenever it changes and after every end of a process it started,
//! hands each reading to [`Drives::decide`], and performs what it answers.
//!
//! **A reading that changed nothing starts nothing.** A start that was refused
//! — the claim, the hand-over or the spawn — and an end owed no restart leave
//! the function alone until its record differs from the first one read after.
//!
//! **A function that leaves the set is never started again.** Its process, if
//! one runs, is left to end on its claim's `Gone`, and its arrival is forgotten
//! once nothing runs on it; a device that returns is a new arrival.
//!
//! **The restart budget is the address's, not the arrival's**: every start at
//! an address after its first spends that address's [`Budget`], so a device
//! that keeps leaving and returning is given up on like one that keeps
//! crashing. Giving up is the arrival's: a later arrival there is tried again
//! once the budget has room.

use std::collections::BTreeMap;
use std::time::Instant;

use toyos_abi::inventory::{Arrival, Driven, Pci, PciAddr};
use toyos_manifest::device::PciMatch;
use toyos_manifest::{Manifest, Program};

use crate::restart::Budget;

/// Every `drives` matcher in `manifest`, with its row.
///
/// Panics on one that is no matcher: the build refuses to render such a
/// manifest, so it is an image whose halves disagree.
pub fn matchers(manifest: &Manifest) -> Vec<(&Program, PciMatch)> {
    let parse = |row: &Program, entry: &String| {
        PciMatch::parse(entry).unwrap_or_else(|| panic!("supervisor: {} drives `{entry}`, which is no matcher", row.name))
    };
    manifest.programs.iter().flat_map(|row| row.drives.iter().map(move |entry| (row, parse(row, entry)))).collect()
}

/// What to do about one function.
#[derive(Debug, PartialEq, Eq)]
pub enum Act<'m> {
    /// Start a process of `row` holding `function`'s claim, endowed under
    /// `entry`'s spelling.
    Start { row: &'m Program, entry: PciMatch, function: Pci },
    /// `function`'s address has spent its budget: this arrival gets no process
    /// again.
    GiveUp { row: &'m Program, function: Pci },
}

#[derive(Default)]
pub struct Drives {
    functions: BTreeMap<Arrival, Phase>,
    /// Every address a process was started at, and what its starts spent.
    addresses: BTreeMap<PciAddr, Budget>,
}

#[derive(Default)]
enum Phase {
    /// A process is owed whenever the function is free.
    #[default]
    Owed,
    /// A process holds it, or is being started on it.
    Running,
    /// Left alone until its record differs from this one: the first read after.
    Held(Option<Pci>),
    GivenUp,
}

impl Drives {
    /// What to do about the functions `records` lists, which `rows` match on
    /// a machine whose boot controller is `boot`. `records` is the whole set:
    /// an arrival it does not list has left.
    pub fn decide<'m>(
        &mut self,
        rows: &[(&'m Program, PciMatch)],
        records: &[Pci],
        boot: Option<PciAddr>,
        now: Instant,
    ) -> Vec<Act<'m>> {
        self.functions.retain(|arrival, phase| {
            matches!(phase, Phase::Running) || records.iter().any(|pci| pci.arrival == *arrival)
        });
        let mut acts = Vec::new();
        for pci in records.iter().filter(|pci| pci.driven != Driven::Kernel) {
            let Some(&(row, entry)) = rows.iter().find(|(_, matcher)| matcher.matches(pci, boot)) else { continue };
            let phase = self.functions.entry(pci.arrival).or_default();
            if let Phase::Held(seen) = phase {
                match seen {
                    None => *seen = Some(*pci),
                    Some(seen) if seen != pci => *phase = Phase::Owed,
                    Some(_) => {}
                }
            }
            if !matches!(phase, Phase::Owed) || pci.driven != Driven::Free {
                continue;
            }
            let spent = match self.addresses.get_mut(&pci.at) {
                Some(budget) => budget.spend(now),
                None => {
                    self.addresses.insert(pci.at, Budget::default());
                    true
                }
            };
            if !spent {
                *phase = Phase::GivenUp;
                acts.push(Act::GiveUp { row, function: *pci });
                continue;
            }
            *phase = Phase::Running;
            acts.push(Act::Start { row, entry, function: *pci });
        }
        acts
    }

    /// The start [`Self::decide`] asked for on `arrival` did not happen.
    pub fn refused(&mut self, arrival: Arrival) {
        self.leave(arrival);
    }

    /// The process on `arrival` ended; `owed` is whether its row owes it
    /// another ([`crate::restart::owed`]). One whose function has left is
    /// owed nothing whatever its row says: the next [`Self::decide`] forgets
    /// the arrival.
    pub fn ended(&mut self, arrival: Arrival, owed: bool) {
        match owed {
            true => self.set(arrival, Phase::Owed),
            false => self.leave(arrival),
        }
    }

    fn leave(&mut self, arrival: Arrival) {
        self.set(arrival, Phase::Held(None));
    }

    fn set(&mut self, arrival: Arrival, phase: Phase) {
        let held = self.functions.get_mut(&arrival).expect("supervisor: an arrival it decided nothing about");
        *held = phase;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart::{RESTARTS, WINDOW};
    use toyos_abi::inventory::PciAddr;
    use toyos_abi::syscall::PciId;

    const EDU: PciMatch = PciMatch::new([0xff, 0x00, 0x00], Some(PciId { vendor: 0x1234, device: 0x11e8 }));

    fn row() -> Program {
        Program { name: "holder".into(), drives: vec![EDU.to_string()], ..Program::default() }
    }

    fn edu(arrival: u32, driven: Driven) -> Pci {
        Pci {
            arrival: Arrival(arrival),
            at: PciAddr { segment: 0, bus: 0, dev: arrival as u8, func: 0 },
            vendor: 0x1234,
            device: 0x11e8,
            class: 0xff,
            subclass: 0,
            prog_if: 0,
            driven,
        }
    }

    fn started(acts: &[Act<'_>]) -> Vec<u32> {
        acts.iter()
            .filter_map(|act| match act {
                Act::Start { function, .. } => Some(function.arrival.0),
                Act::GiveUp { .. } => None,
            })
            .collect()
    }

    /// A process per function the matcher names, none on the kernel's, none
    /// on another's, and one only however often the same reading is decided.
    #[test]
    fn each_free_function_a_matcher_names_gets_one_process() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let mut nic = edu(9, Driven::Free);
        nic.vendor = 0x1af4;
        let machine = [edu(3, Driven::Free), edu(4, Driven::Free), edu(5, Driven::Kernel), edu(6, Driven::Claimed), nic];
        let mut drives = Drives::default();
        let acts = drives.decide(&rows, &machine, None, now);
        assert_eq!(started(&acts), [3, 4]);
        assert!(matches!(acts[0], Act::Start { row: r, entry, .. } if r.name == "holder" && entry == EDU));
        assert!(drives.decide(&rows, &machine, None, now).is_empty());
        // Read again once each process holds its function.
        assert!(drives.decide(&rows, &[edu(3, Driven::Claimed), edu(4, Driven::Claimed)], None, now).is_empty());
    }

    /// **A refused start is not asked for again until its record changes**:
    /// the same reading decided again starts nothing, however often.
    #[test]
    fn a_refused_start_is_not_retried_until_its_record_changes() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let machine = [edu(3, Driven::Free)];
        let mut drives = Drives::default();
        assert_eq!(started(&drives.decide(&rows, &machine, None, now)), [3]);
        drives.refused(Arrival(3));
        for round in 0..2 * RESTARTS {
            assert!(drives.decide(&rows, &machine, None, now).is_empty(), "round {round} started it again");
        }
        // Another holder took it and gave it back: a change.
        drives.decide(&rows, &[edu(3, Driven::Claimed)], None, now);
        assert_eq!(started(&drives.decide(&rows, &machine, None, now)), [3]);
    }

    /// Every start after a function's first spends its budget, and a function
    /// that keeps refusing is given up on once, and never started again.
    #[test]
    fn a_function_that_keeps_refusing_is_given_up_on() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let (free, taken) = ([edu(3, Driven::Free)], [edu(3, Driven::Claimed)]);
        let mut drives = Drives::default();
        let mut starts = 0;
        let mut given_up = 0;
        for _ in 0..4 * RESTARTS {
            for act in drives.decide(&rows, &free, None, now) {
                match act {
                    Act::Start { .. } => {
                        starts += 1;
                        drives.refused(Arrival(3));
                    }
                    Act::GiveUp { function, .. } => {
                        assert_eq!(function.arrival, Arrival(3));
                        given_up += 1;
                    }
                }
            }
            drives.decide(&rows, &taken, None, now);
        }
        assert_eq!((starts, given_up), (RESTARTS + 1, 1));
        assert!(drives.decide(&rows, &free, None, now + 2 * WINDOW).is_empty());
    }

    /// An end owed a restart starts another at once, on the budget; one owed
    /// nothing leaves the function alone like a refusal.
    #[test]
    fn an_ended_process_is_started_again_only_when_owed() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let machine = [edu(3, Driven::Free), edu(4, Driven::Free)];
        let mut drives = Drives::default();
        assert_eq!(started(&drives.decide(&rows, &machine, None, now)), [3, 4]);
        drives.ended(Arrival(3), true);
        drives.ended(Arrival(4), false);
        assert_eq!(started(&drives.decide(&rows, &machine, None, now)), [3]);
        assert!(drives.decide(&rows, &machine, None, now).is_empty());
    }

    /// `edu(arrival, ..)` at the address of another arrival: the same device
    /// returning.
    fn at(pci: Pci, dev: u8) -> Pci {
        Pci { at: PciAddr { segment: 0, bus: 1, dev, func: 0 }, ..pci }
    }

    /// A function that arrives gets a process; one that leaves gets none
    /// again, its process is left to end, and its arrival is forgotten; the
    /// device returning is a new arrival with a process of its own.
    #[test]
    fn an_arrival_starts_a_departure_retires_and_a_return_is_new() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let mut drives = Drives::default();
        assert!(drives.decide(&rows, &[], None, now).is_empty());
        let first = at(edu(7, Driven::Free), 0);
        assert_eq!(started(&drives.decide(&rows, &[first], None, now)), [7]);
        // Gone from the set while its process still runs: nothing is started.
        assert!(drives.decide(&rows, &[], None, now).is_empty());
        // Its process ends on `Gone` with a crash its row would restart.
        drives.ended(Arrival(7), true);
        assert!(drives.decide(&rows, &[], None, now).is_empty());
        assert!(!drives.functions.contains_key(&Arrival(7)), "a departed arrival is forgotten");
        // The device back at the same address, after its budget's window.
        let back = at(edu(8, Driven::Free), 0);
        assert_eq!(started(&drives.decide(&rows, &[back], None, now + WINDOW + WINDOW)), [8]);
    }

    /// A function that leaves between the decision to start on it and the
    /// start's refusal is never started again, and a refusal of a start whose
    /// function has left is no error.
    #[test]
    fn a_departure_while_a_start_is_pending_starts_nothing() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let mut drives = Drives::default();
        assert_eq!(started(&drives.decide(&rows, &[edu(3, Driven::Free)], None, now)), [3]);
        // The set changed before the start was performed: it is still the
        // supervisor's to answer for.
        assert!(drives.decide(&rows, &[], None, now).is_empty());
        // Its claim answered `Gone`.
        drives.refused(Arrival(3));
        for _ in 0..3 {
            assert!(drives.decide(&rows, &[], None, now).is_empty());
        }
        assert!(drives.functions.is_empty());
    }

    /// Every start at an address after its first spends that address's
    /// budget, so a device that keeps returning is given up on, and an
    /// arrival there after the window is started again.
    #[test]
    fn a_returning_device_spends_its_addresss_budget() {
        let (row, now) = (row(), Instant::now());
        let rows = [(&row, EDU)];
        let mut drives = Drives::default();
        let mut starts = 0;
        let mut arrival = 10;
        for _ in 0..=RESTARTS {
            let acts = drives.decide(&rows, &[at(edu(arrival, Driven::Free), 0)], None, now);
            assert_eq!(started(&acts), [arrival], "arrival {arrival} at the same address");
            starts += 1;
            drives.decide(&rows, &[], None, now);
            drives.ended(Arrival(arrival), false);
            arrival += 1;
        }
        let acts = drives.decide(&rows, &[at(edu(arrival, Driven::Free), 0)], None, now);
        assert!(matches!(acts[..], [Act::GiveUp { function, .. }] if function.arrival == Arrival(arrival)), "{acts:?}");
        assert_eq!(starts, RESTARTS + 1);
        // Given up on is this arrival's: it is never started again.
        assert!(drives.decide(&rows, &[at(edu(arrival, Driven::Free), 0)], None, now + 2 * WINDOW).is_empty());
        // Another address has a budget of its own.
        assert_eq!(started(&drives.decide(&rows, &[at(edu(arrival + 1, Driven::Free), 1)], None, now)), [arrival + 1]);
        // And the next arrival at the first address, after the window, starts.
        drives.decide(&rows, &[], None, now);
        let later = at(edu(arrival + 2, Driven::Free), 0);
        assert_eq!(started(&drives.decide(&rows, &[later], None, now + 2 * WINDOW)), [arrival + 2]);
    }

    #[test]
    fn every_drives_entry_is_a_matcher_with_its_row() {
        let manifest = Manifest { programs: vec![row(), Program { name: "other".into(), ..Program::default() }], ..Manifest::default() };
        let found = matchers(&manifest);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0.name.as_str(), found[0].1), ("holder", EDU));
    }
}
