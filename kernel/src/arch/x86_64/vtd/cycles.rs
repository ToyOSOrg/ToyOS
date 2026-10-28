//! [`crate::actuator::iommu_handover_cycles`]: every translating unit moved
//! between its root and a copy of it, by each of the two ways a hand-over can
//! take, while whatever DMA firmware left running goes on through it: every
//! `switch` first, so nothing in that phase follows translation going off,
//! then every `off-on`.
//!
//! `off-on` is what a hand-over did when translation had to go off: `TE` off,
//! the other root pointed, the global invalidation, `TE` on. `switch` is §6.6's
//! root switch under translation: the other root pointed and the global
//! invalidation, `TE` left on. The two roots translate alike, so a fault
//! either way is the unit's disagreement with tables it was given, and each is
//! printed with the walk those tables give from memory.
//!
//! Fault events are held for the whole run and the records read here, so a
//! fault is counted against the cycle it followed and the machine goes on.

use alloc::vec::Vec;

use crate::drivers::pci::PciDevice;
use crate::iommu::fault::Who;
use crate::log;

use super::fault::{self, Held};
use super::table::{self, Table};
use super::{command, RTADDR_REG, SET_ROOT_TABLE_POINTER, TABLES, TRANSLATION_ENABLE, UNITS};

/// Cycles of each kind. Each moves every unit onto its other root, and the
/// count is even, so each phase ends with every unit on its own root.
const CYCLES: usize = 200;
const _: () = assert!(CYCLES.is_multiple_of(2));

/// How long each cycle's result is watched: at least one refresh of a panel
/// scanning out at 50 Hz or faster, so every address a scanout reads is read
/// once after each transition.
const WATCH_NS: u64 = 20_000_000;

/// Fault lines printed in full; every later one is only counted.
const LINES: usize = 16;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    OffOn,
    Switch,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::OffOn => "off-on",
            Self::Switch => "switch",
        }
    }
}

#[derive(Default)]
struct Tally {
    /// Cycles after which at least one record was read.
    faulted: usize,
    records: usize,
    overflowed: usize,
    /// The fewest nanoseconds from a transition's end to a record being read.
    soonest: Option<u64>,
}

/// One live unit: the index its fault records are armed under, its own root
/// and the copy.
struct Roots {
    index: usize,
    own: Table,
    copy: Table,
}

pub(super) fn run(devices: &[PciDevice]) {
    host_bridge(devices);
    let roots: Vec<Roots> = {
        let units = UNITS.lock();
        let mut tables = TABLES.lock();
        units
            .iter()
            .map(|unit| Roots {
                index: fault::index_of(unit.regs).expect("iommu-handover-cycles: a live unit with no fault records"),
                own: unit.root,
                copy: table::copy_root(&mut tables, unit.root),
            })
            .collect()
    };
    log!("iommu-handover-cycles: {} unit(s), {CYCLES} cycles of each kind, each watched {WATCH_NS} ns", roots.len());

    fault::hold(true);
    let mut tallies = [Tally::default(), Tally::default()];
    let mut printed = 0usize;
    for cycle in 0..2 * CYCLES {
        let kind = if cycle < CYCLES { Kind::Switch } else { Kind::OffOn };
        let onto = |roots: &Roots| if cycle % 2 == 0 { roots.copy } else { roots.own };
        {
            let mut units = UNITS.lock();
            for (unit, roots) in units.iter_mut().zip(&roots) {
                if kind == Kind::OffOn {
                    command(roots.index, unit.regs, TRANSLATION_ENABLE, false, "translation");
                }
                unit.regs.write_u64(RTADDR_REG, onto(roots).phys());
                command(roots.index, unit.regs, SET_ROOT_TABLE_POINTER, true, "the root table pointer");
                unit.queue.invalidate_all(unit.regs);
                if kind == Kind::OffOn {
                    command(roots.index, unit.regs, TRANSLATION_ENABLE, true, "translation");
                }
            }
        }
        let tally = &mut tallies[kind as usize];
        let start = crate::clock::nanos_since_boot();
        let mut seen = false;
        while crate::clock::nanos_since_boot() - start < WATCH_NS {
            fault::take_held(|Held { unit, fault, overflowed }| {
                let after = crate::clock::nanos_since_boot() - start;
                seen = true;
                tally.records += 1;
                tally.overflowed += usize::from(overflowed);
                tally.soonest = Some(tally.soonest.map_or(after, |s| s.min(after)));
                let Who::Function(stream) = fault.who else { return };
                if printed == LINES {
                    return;
                }
                printed += 1;
                let walk = roots.iter().find(|r| r.index == unit).map(|r| table::walk(onto(r), stream, fault.address));
                let walk: &dyn core::fmt::Display = match &walk {
                    Some(walk) => walk,
                    None => &"none",
                };
                log!(
                    "iommu-handover-cycles: cycle {cycle} {} unit{unit} stream={stream} addr={:#018x} access={} \
                     reason={:#04x} {} overflowed={} after={after}ns walk={walk}",
                    kind.name(),
                    fault.address,
                    fault.access,
                    fault.reason,
                    fault.name,
                    if overflowed { 'y' } else { 'n' },
                );
            });
            core::hint::spin_loop();
        }
        tally.faulted += usize::from(seen);
    }
    fault::hold(false);

    for kind in [Kind::Switch, Kind::OffOn] {
        let tally = &tallies[kind as usize];
        log!(
            "iommu-handover-cycles: {} {CYCLES} cycles, {} with a fault, {} records, {} overflowed, soonest {}",
            kind.name(),
            tally.faulted,
            tally.records,
            tally.overflowed,
            Soonest(tally.soonest),
        );
    }
}

struct Soonest(Option<u64>);

impl core::fmt::Display for Soonest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ns) => write!(f, "{ns}ns"),
            None => f.write_str("none"),
        }
    }
}

/// Where an Intel host bridge put graphics stolen memory, so a record's address
/// can be placed in it: `BDSM` (data) at B0h and `BGSM` (GTT) at B4h, bits
/// 31:20 each, and `TOLUD` at BCh (11th Generation Intel Core Datasheet Vol.
/// 2a, 631122, §3.1.31-§3.1.32). The values as read; what they mean is
/// Intel's host bridge alone.
fn host_bridge(devices: &[PciDevice]) {
    let Some(bridge) = devices.iter().find(|d| (d.bus, d.dev, d.func) == (0, 0, 0) && d.vendor_id() == 0x8086) else {
        return;
    };
    log!(
        "iommu-handover-cycles: host bridge {:04x}:{:04x} BDSM={:#010x} BGSM={:#010x} TOLUD={:#010x}",
        bridge.vendor_id(),
        bridge.device_id(),
        bridge.read_config_u32(0xB0),
        bridge.read_config_u32(0xB4),
        bridge.read_config_u32(0xBC),
    );
}
