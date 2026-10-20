//! The machine's cache tree: which core, last-level cache and package each
//! dense CPU id sits in, built only from what each CPU read of itself.
//!
//! A key names one domain at its level and is unique machine-wide (an x2APIC id
//! or packed MPIDR shifted right by the level's width). Two CPUs share a level
//! when their keys at it are equal. Dense ids carry no locality: firmware lists
//! SMT siblings four ids apart on one bench and adjacent on another.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::sched::hw::CpuId;

/// One CPU's key at each level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CpuKeys {
    pub core: u32,
    pub llc: u32,
    pub package: u32,
}

/// The lowest domain two distinct CPUs share.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    /// SMT siblings: one core's hardware threads.
    Core,
    /// One last-level cache.
    Llc,
    /// One package.
    Package,
    /// Different packages.
    Machine,
}

/// Keys that do not describe one tree. Firmware and hypervisors report these;
/// the machine is refused by name rather than scheduled on a guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TopologyError {
    NoCpus,
    /// One core key under two LLC or package keys.
    CoreSpansDomains { core: u32 },
    /// One LLC key under two package keys.
    LlcSpansPackages { llc: u32 },
}

#[derive(Clone, Debug)]
pub struct Topology {
    /// Indexed by dense CPU id.
    keys: Vec<CpuKeys>,
}

impl Topology {
    pub fn new(keys: Vec<CpuKeys>) -> Result<Self, TopologyError> {
        if keys.is_empty() {
            return Err(TopologyError::NoCpus);
        }
        let mut cores = BTreeMap::new();
        let mut llcs = BTreeMap::new();
        for k in &keys {
            if *cores.entry(k.core).or_insert((k.llc, k.package)) != (k.llc, k.package) {
                return Err(TopologyError::CoreSpansDomains { core: k.core });
            }
            if *llcs.entry(k.llc).or_insert(k.package) != k.package {
                return Err(TopologyError::LlcSpansPackages { llc: k.llc });
            }
        }
        Ok(Self { keys })
    }

    pub fn cpus(&self) -> usize {
        self.keys.len()
    }

    pub fn keys(&self, cpu: CpuId) -> CpuKeys {
        self.keys[cpu.0 as usize]
    }

    /// The lowest level `a` and `b` share, or `None` when they are one CPU.
    pub fn level(&self, a: CpuId, b: CpuId) -> Option<Level> {
        if a == b {
            return None;
        }
        let (a, b) = (self.keys(a), self.keys(b));
        Some(if a.core == b.core {
            Level::Core
        } else if a.llc == b.llc {
            Level::Llc
        } else if a.package == b.package {
            Level::Package
        } else {
            Level::Machine
        })
    }

    /// How many domains the machine has at `level`.
    pub fn domains(&self, level: Level) -> usize {
        let mut keys: Vec<u32> = self
            .keys
            .iter()
            .map(|k| match level {
                Level::Core => k.core,
                Level::Llc => k.llc,
                Level::Package => k.package,
                Level::Machine => 0,
            })
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn k(core: u32, llc: u32, package: u32) -> CpuKeys {
        CpuKeys { core, llc, package }
    }

    #[test]
    fn a_tree_is_accepted_and_answers_levels() {
        // Two packages, each one LLC of two cores of two threads.
        let t = Topology::new(vec![
            k(0, 0, 0),
            k(0, 0, 0),
            k(1, 0, 0),
            k(1, 0, 0),
            k(2, 1, 1),
            k(2, 1, 1),
            k(3, 1, 1),
            k(3, 1, 1),
        ])
        .unwrap();
        assert_eq!(t.level(CpuId(0), CpuId(0)), None);
        assert_eq!(t.level(CpuId(0), CpuId(1)), Some(Level::Core));
        assert_eq!(t.level(CpuId(0), CpuId(2)), Some(Level::Llc));
        assert_eq!(t.level(CpuId(0), CpuId(4)), Some(Level::Machine));
        assert_eq!(t.domains(Level::Core), 4);
        assert_eq!(t.domains(Level::Llc), 2);
        assert_eq!(t.domains(Level::Package), 2);
    }

    #[test]
    fn keys_that_are_not_a_tree_are_refused_by_name() {
        assert_eq!(Topology::new(vec![]).unwrap_err(), TopologyError::NoCpus);
        assert_eq!(
            Topology::new(vec![k(0, 0, 0), k(0, 1, 0)]).unwrap_err(),
            TopologyError::CoreSpansDomains { core: 0 },
        );
        assert_eq!(
            Topology::new(vec![k(0, 0, 0), k(1, 0, 1)]).unwrap_err(),
            TopologyError::LlcSpansPackages { llc: 0 },
        );
    }
}
