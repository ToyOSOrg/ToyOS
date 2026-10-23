//! Which PCI functions a `drives` row has a process on.
//!
//! **One process per function a `drives` matcher names and the kernel does not
//! drive**, keyed by the function's arrival. The supervisor reads the inventory
//! at boot and after every end of a process it started, hands each reading to
//! [`Drives::decide`], and performs what it answers.
//!
//! **A reading that changed nothing starts nothing.** A start that was refused
//! — the claim, the hand-over or the spawn — and an end owed no restart leave
//! the function alone until its inventory record differs from the first one
//! read after. Every start of a function after its first spends its
//! [`Budget`], and a function whose budget is spent is given up on.

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
    /// `function`'s budget is spent: it gets no process again.
    GiveUp { row: &'m Program, function: Pci },
}

#[derive(Default)]
pub struct Drives {
    functions: BTreeMap<Arrival, Function>,
}

#[derive(Default)]
struct Function {
    budget: Budget,
    started: bool,
    phase: Phase,
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
    /// a machine whose boot controller is `boot`.
    pub fn decide<'m>(
        &mut self,
        rows: &[(&'m Program, PciMatch)],
        records: &[Pci],
        boot: Option<PciAddr>,
        now: Instant,
    ) -> Vec<Act<'m>> {
        self.functions.retain(|arrival, function| {
            matches!(function.phase, Phase::Running) || records.iter().any(|pci| pci.arrival == *arrival)
        });
        let mut acts = Vec::new();
        for pci in records.iter().filter(|pci| pci.driven != Driven::Kernel) {
            let Some(&(row, entry)) = rows.iter().find(|(_, matcher)| matcher.matches(pci, boot)) else { continue };
            let function = self.functions.entry(pci.arrival).or_default();
            if let Phase::Held(seen) = &mut function.phase {
                match seen {
                    None => *seen = Some(*pci),
                    Some(seen) if seen != pci => function.phase = Phase::Owed,
                    Some(_) => {}
                }
            }
            if !matches!(function.phase, Phase::Owed) || pci.driven != Driven::Free {
                continue;
            }
            if function.started && !function.budget.spend(now) {
                function.phase = Phase::GivenUp;
                acts.push(Act::GiveUp { row, function: *pci });
                continue;
            }
            function.started = true;
            function.phase = Phase::Running;
            acts.push(Act::Start { row, entry, function: *pci });
        }
        acts
    }

    /// The start [`Self::decide`] asked for on `arrival` did not happen.
    pub fn refused(&mut self, arrival: Arrival) {
        self.leave(arrival);
    }

    /// The process on `arrival` ended; `owed` is whether its row owes it
    /// another ([`crate::restart::owed`]).
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
        let function = self.functions.get_mut(&arrival).expect("supervisor: an arrival it decided nothing about");
        function.phase = phase;
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

    #[test]
    fn every_drives_entry_is_a_matcher_with_its_row() {
        let manifest = Manifest { programs: vec![row(), Program { name: "other".into(), ..Program::default() }], ..Manifest::default() };
        let found = matchers(&manifest);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0.name.as_str(), found[0].1), ("holder", EDU));
    }
}
