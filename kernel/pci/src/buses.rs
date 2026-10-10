//! Which buses of one ECAM window a walk of configuration space enters
//! (PCI-to-PCI Bridge Architecture Specification 1.2 §3.2.5.3 to §3.2.5.5).
//!
//! **A bus is entered only where something forwards configuration cycles to
//! it**: the window's first bus, which its host bridge decodes, and each
//! bridge's secondary bus, inside what the bus above that bridge is forwarded.
//! A bus number nothing forwards is one no cycle reaches, and what answers at
//! its address in the window is not a function.
//!
//! A bridge forwards a cycle for a bus from its secondary through its
//! subordinate, so a bus below it is forwarded what both bounds allow: the
//! narrower of the two subordinates. Every bus is entered at most once and
//! only above the bus whose bridge named it, so a walk ends however a machine
//! numbered its bridges, and it enters buses in ascending order.

use core::ops::RangeInclusive;

/// The dword of a Type 1 header holding its primary, secondary and
/// subordinate bus numbers, in that byte order from its low end.
pub const BUS_NUMBERS: u64 = 0x18;

/// Why a bridge's secondary bus is not entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unforwarded {
    /// Its secondary is not above the bus the bridge sits on: unconfigured
    /// (zero) or numbered against the tree.
    NotBelow { secondary: u8 },
    /// Its subordinate is below its secondary, so it forwards no bus.
    Inverted { secondary: u8, subordinate: u8 },
    /// Its secondary is past the last bus the bridge itself is forwarded.
    Outside { secondary: u8, limit: u8 },
    /// Another bridge already forwards its secondary.
    Taken { secondary: u8 },
}

impl core::fmt::Display for Unforwarded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::NotBelow { secondary } => write!(f, "its secondary bus {secondary:#04x} is not below it"),
            Self::Inverted { secondary, subordinate } => {
                write!(f, "its subordinate bus {subordinate:#04x} is below its secondary {secondary:#04x}")
            }
            Self::Outside { secondary, limit } => {
                write!(f, "its secondary bus {secondary:#04x} is past {limit:#04x}, the last bus it is forwarded")
            }
            Self::Taken { secondary } => write!(f, "another bridge already forwards bus {secondary:#04x}"),
        }
    }
}

/// The walk's state over one window.
pub struct Buses {
    /// Buses forwarded and not yet entered, one bit each.
    pending: [u64; 4],
    /// Buses forwarded at all, entered or not.
    forwarded: [u64; 4],
    /// For each forwarded bus, the last bus a cycle through it reaches.
    limit: [u8; 256],
}

fn bit(set: &[u64; 4], bus: u8) -> bool {
    set[usize::from(bus / 64)] & 1 << (bus % 64) != 0
}

fn set(set: &mut [u64; 4], bus: u8, on: bool) {
    let word = &mut set[usize::from(bus / 64)];
    *word = if on { *word | 1 << (bus % 64) } else { *word & !(1 << (bus % 64)) };
}

impl Buses {
    /// A walk of the window over `buses`, its host bridge forwarding the first.
    pub fn new(buses: RangeInclusive<u8>) -> Self {
        let mut walk = Self { pending: [0; 4], forwarded: [0; 4], limit: [0; 256] };
        let (first, last) = buses.into_inner();
        if first <= last {
            walk.forward(first, last);
        }
        walk
    }

    fn forward(&mut self, bus: u8, limit: u8) {
        set(&mut self.pending, bus, true);
        set(&mut self.forwarded, bus, true);
        self.limit[usize::from(bus)] = limit;
    }

    /// The lowest bus forwarded and not yet entered, now entered.
    pub fn enter(&mut self) -> Option<u8> {
        let (word, bits) = self.pending.iter().enumerate().find(|(_, bits)| **bits != 0)?;
        let bus = (word * 64) as u8 + bits.trailing_zeros() as u8;
        set(&mut self.pending, bus, false);
        Some(bus)
    }

    /// A bridge on entered bus `bus`, its [`BUS_NUMBERS`] dword `numbers`:
    /// the buses it forwards, its secondary to be entered, or why not.
    pub fn bridge(&mut self, bus: u8, numbers: u32) -> Result<RangeInclusive<u8>, Unforwarded> {
        let [_, secondary, subordinate, _] = numbers.to_le_bytes();
        let limit = self.limit[usize::from(bus)];
        if secondary <= bus {
            return Err(Unforwarded::NotBelow { secondary });
        }
        if subordinate < secondary {
            return Err(Unforwarded::Inverted { secondary, subordinate });
        }
        if secondary > limit {
            return Err(Unforwarded::Outside { secondary, limit });
        }
        if bit(&self.forwarded, secondary) {
            return Err(Unforwarded::Taken { secondary });
        }
        let reached = subordinate.min(limit);
        self.forward(secondary, reached);
        Ok(secondary..=reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// A bridge's bus-number dword: primary, secondary, subordinate.
    fn numbers(primary: u8, secondary: u8, subordinate: u8) -> u32 {
        u32::from_le_bytes([primary, secondary, subordinate, 0x00])
    }

    /// A machine as `(bus, bridges on it as (secondary, subordinate))`, walked
    /// as the kernel walks it: every bus entered, in the order entered.
    fn entered(window: RangeInclusive<u8>, machine: &[(u8, &[(u8, u8)])]) -> Vec<u8> {
        let mut walk = Buses::new(window);
        let mut out = Vec::new();
        while let Some(bus) = walk.enter() {
            out.push(bus);
            for (_, bridges) in machine.iter().filter(|(on, _)| *on == bus) {
                for &(secondary, subordinate) in *bridges {
                    let _ = walk.bridge(bus, numbers(bus, secondary, subordinate));
                }
            }
        }
        out
    }

    /// Root ports on bus 0 forwarding 4, 9, 0x0a and a hot-plug range from
    /// 0x20, each secondary entered once and in ascending order whatever
    /// order the ports were read in.
    #[test]
    fn the_buses_bridges_forward_are_entered_and_no_other() {
        let machine: &[(u8, &[(u8, u8)])] = &[(0, &[(4, 4), (0x20, 0x49), (9, 9), (0x0a, 0x0a)])];
        assert_eq!(entered(0..=0xff, machine), [0, 4, 9, 0x0a, 0x20]);
    }

    /// The defect's shape: a window of 0..=0x3f with one bridge forwarding 1.
    /// No bus past it is entered — the 0x40 onwards a 256-bus walk read as
    /// functions — and none between that nothing forwards.
    #[test]
    fn no_bus_past_the_window_and_none_nothing_forwards_is_entered() {
        assert_eq!(entered(0..=0x3f, &[(0, &[(1, 1)])]), [0, 1]);
        assert_eq!(entered(0..=0x3f, &[]), [0]);
    }

    /// A window that starts above bus 0 enters its own first bus.
    #[test]
    fn a_window_is_entered_at_its_first_bus() {
        assert_eq!(entered(0x10..=0x1f, &[(0x10, &[(0x11, 0x12)]), (0x11, &[(0x12, 0x12)])]), [0x10, 0x11, 0x12]);
    }

    /// A nested bridge is forwarded only what the bridge above it is: a
    /// subordinate past its parent's is narrowed to it, and a secondary past
    /// it is not entered at all.
    #[test]
    fn a_bridge_below_another_is_forwarded_only_what_that_one_is() {
        let mut walk = Buses::new(0..=0xff);
        assert_eq!(walk.enter(), Some(0));
        assert_eq!(walk.bridge(0, numbers(0, 1, 3)), Ok(1..=3));
        assert_eq!(walk.enter(), Some(1));
        assert_eq!(walk.bridge(1, numbers(1, 2, 9)), Ok(2..=3));
        assert_eq!(walk.bridge(1, numbers(1, 4, 4)), Err(Unforwarded::Outside { secondary: 4, limit: 3 }));
        assert_eq!(walk.enter(), Some(2));
        assert_eq!(walk.bridge(2, numbers(2, 3, 3)), Ok(3..=3));
        assert_eq!(walk.enter(), Some(3));
        assert_eq!(walk.enter(), None);
    }

    /// The window's own last bus bounds a bridge on its first.
    #[test]
    fn a_bridge_is_not_forwarded_past_the_window() {
        let mut walk = Buses::new(0..=0x3f);
        walk.enter();
        assert_eq!(walk.bridge(0, numbers(0, 0x40, 0x40)), Err(Unforwarded::Outside { secondary: 0x40, limit: 0x3f }));
        assert_eq!(walk.bridge(0, numbers(0, 0x3f, 0xff)), Ok(0x3f..=0x3f));
    }

    /// Every bridge that could make the walk go round, or enter one bus
    /// twice, is refused: an unconfigured one, one pointing at its own bus or
    /// above it in the tree, one forwarding nothing, and a second claiming a
    /// bus a first already forwards.
    #[test]
    fn a_bridge_that_would_loop_or_repeat_a_bus_is_refused() {
        let mut walk = Buses::new(0..=0xff);
        assert_eq!(walk.enter(), Some(0));
        assert_eq!(walk.bridge(0, numbers(0, 0, 0)), Err(Unforwarded::NotBelow { secondary: 0 }));
        assert_eq!(walk.bridge(0, numbers(0, 5, 4)), Err(Unforwarded::Inverted { secondary: 5, subordinate: 4 }));
        assert_eq!(walk.bridge(0, numbers(0, 2, 8)), Ok(2..=8));
        assert_eq!(walk.bridge(0, numbers(0, 2, 2)), Err(Unforwarded::Taken { secondary: 2 }));
        assert_eq!(walk.enter(), Some(2));
        assert_eq!(walk.bridge(2, numbers(2, 2, 2)), Err(Unforwarded::NotBelow { secondary: 2 }));
        assert_eq!(walk.bridge(2, numbers(2, 1, 8)), Err(Unforwarded::NotBelow { secondary: 1 }));
        assert_eq!(walk.enter(), None);
    }

    /// Every possible bus-number dword, on every bus of a full window: the
    /// walk ends, and enters no bus twice and none outside the window.
    #[test]
    fn no_bridge_numbering_makes_the_walk_repeat_or_leave_the_window() {
        for first in [0u8, 0x10] {
            let mut walk = Buses::new(first..=0x3f);
            let mut seen = [false; 256];
            let mut steps = 0u32;
            while let Some(bus) = walk.enter() {
                assert!((first..=0x3f).contains(&bus), "{bus:#x} is outside the window");
                assert!(!seen[usize::from(bus)], "{bus:#x} entered twice");
                seen[usize::from(bus)] = true;
                steps += 1;
                assert!(steps <= 256);
                for secondary in 0..=0xffu8 {
                    for subordinate in [0u8, secondary, secondary.saturating_add(3), 0xff] {
                        let _ = walk.bridge(bus, numbers(bus, secondary, subordinate));
                    }
                }
            }
            assert_eq!(steps, 0x40 - u32::from(first));
        }
    }
}
