//! The machine's cache tree: which core, last-level cache and package each
//! dense CPU id sits in, built only from what each CPU read of itself.
//!
//! A key names one domain at its level and is unique machine-wide (an x2APIC id
//! or packed MPIDR shifted right by the level's width). Two CPUs share a level
//! when their keys at it are equal. Dense ids carry no locality: firmware lists
//! SMT siblings four ids apart on one bench and adjacent on another.

use alloc::collections::BTreeMap;
use alloc::vec;
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

/// Positions `start..end` of [`Topology::order`].
#[derive(Clone, Copy, Debug)]
struct Run {
    start: usize,
    end: usize,
}

impl Run {
    fn contains(self, position: usize) -> bool {
        (self.start..self.end).contains(&position)
    }
}

/// Where one CPU sits in [`Topology::order`]: its own position and the runs
/// its core, LLC and package occupy, each inside the next.
#[derive(Clone, Copy, Debug)]
struct Place {
    position: usize,
    core: Run,
    llc: Run,
    package: Run,
}

#[derive(Clone, Debug)]
pub struct Topology {
    /// Indexed by dense CPU id.
    keys: Vec<CpuKeys>,
    /// Every CPU, ordered by package, LLC and core key, so that each domain at
    /// every level is one contiguous run.
    order: Vec<CpuId>,
    /// Indexed by dense CPU id.
    places: Vec<Place>,
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
        let mut order: Vec<CpuId> = (0..keys.len() as u32).map(CpuId).collect();
        order.sort_unstable_by_key(|cpu| {
            let k = keys[cpu.0 as usize];
            (k.package, k.llc, k.core, cpu.0)
        });
        // The checks above make each domain one run of `order`, ending where
        // the key at its level changes.
        let runs = |key: fn(&CpuKeys) -> u32| {
            let key_at = |position: usize| key(&keys[order[position].0 as usize]);
            let mut runs = vec![Run { start: 0, end: 0 }; order.len()];
            let mut start = 0;
            for end in 1..=order.len() {
                if end == order.len() || key_at(end) != key_at(start) {
                    runs[start..end].fill(Run { start, end });
                    start = end;
                }
            }
            runs
        };
        let (cores, llcs, packages) = (runs(|k| k.core), runs(|k| k.llc), runs(|k| k.package));
        let mut places = vec![None; keys.len()];
        for (position, cpu) in order.iter().enumerate() {
            places[cpu.0 as usize] = Some(Place {
                position,
                core: cores[position],
                llc: llcs[position],
                package: packages[position],
            });
        }
        let places = places.into_iter().map(|place| place.expect("`order` holds every CPU")).collect();
        Ok(Self { keys, order, places })
    }

    /// `cpus` cores of one thread each, under one LLC in one package.
    pub fn flat(cpus: u32) -> Self {
        Self::new((0..cpus).map(|core| CpuKeys { core, llc: 0, package: 0 }).collect())
            .expect("a flat machine of at least one CPU is a tree")
    }

    pub fn cpus(&self) -> usize {
        self.keys.len()
    }

    /// A number for the LLC `cpu` is in, the same for every CPU of that LLC
    /// and different for every other: the position of its first CPU in the
    /// topology's order.
    pub fn llc_index(&self, cpu: CpuId) -> u16 {
        u16::try_from(self.places[cpu.0 as usize].llc.start).expect("a CPU mask names at most 1024 CPUs")
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

    /// `cpu`'s position in the order [`Self::ring`] walks.
    pub fn position(&self, cpu: CpuId) -> usize {
        self.places[cpu.0 as usize].position
    }

    /// Every CPU of `cpu`'s core, `cpu` among them.
    pub fn core(&self, cpu: CpuId) -> &[CpuId] {
        let core = self.places[cpu.0 as usize].core;
        &self.order[core.start..core.end]
    }

    fn run(&self, cpu: CpuId, level: Option<Level>) -> Run {
        let place = self.places[cpu.0 as usize];
        match level {
            None => Run { start: place.position, end: place.position + 1 },
            Some(Level::Core) => place.core,
            Some(Level::Llc) => place.llc,
            Some(Level::Package) => place.package,
            Some(Level::Machine) => Run { start: 0, end: self.order.len() },
        }
    }

    /// The CPUs in `cpu`'s domain at `outer` and outside its domain at `inner`
    /// (`None` is `cpu` alone), each once, starting at the first whose
    /// [`Self::position`] is `from` or after and wrapping round.
    pub fn ring(
        &self,
        cpu: CpuId,
        inner: Option<Level>,
        outer: Level,
        from: usize,
    ) -> impl Iterator<Item = CpuId> + '_ {
        let (inner, outer) = (self.run(cpu, inner), self.run(cpu, Some(outer)));
        let span = outer.end - outer.start;
        let first = if outer.contains(from) { from - outer.start } else { 0 };
        (0..span)
            .map(move |step| outer.start + (first + step) % span)
            .filter(move |&position| !inner.contains(position))
            .map(|position| self.order[position])
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

    fn ids(cpus: impl Iterator<Item = CpuId>) -> Vec<u32> {
        cpus.map(|cpu| cpu.0).collect()
    }

    /// Dense ids listed against the tree, as firmware lists them: two packages
    /// of one LLC of two cores of two threads, each core's threads four ids
    /// apart and the packages interleaved.
    fn scattered() -> Topology {
        Topology::new(vec![
            k(0, 0, 0),
            k(2, 1, 1),
            k(1, 0, 0),
            k(3, 1, 1),
            k(0, 0, 0),
            k(2, 1, 1),
            k(1, 0, 0),
            k(3, 1, 1),
        ])
        .unwrap()
    }

    #[test]
    fn a_ring_is_its_level_and_nothing_nearer_whatever_the_ids() {
        let t = scattered();
        let me = CpuId(0);
        assert_eq!(ids(t.core(me).iter().copied()), [0, 4]);
        assert_eq!(ids(t.ring(me, None, Level::Core, 0)), [4]);
        assert_eq!(ids(t.ring(me, Some(Level::Core), Level::Llc, 0)), [2, 6]);
        assert_eq!(ids(t.ring(me, Some(Level::Llc), Level::Package, 0)), Vec::<u32>::new());
        assert_eq!(ids(t.ring(me, Some(Level::Package), Level::Machine, 0)), [1, 5, 3, 7]);
        assert_eq!(ids(t.ring(me, None, Level::Llc, 0)), [4, 2, 6]);
        let mut every = ids(t.ring(me, None, Level::Machine, 0));
        every.sort_unstable();
        assert_eq!(every, [1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn a_ring_starts_at_the_position_it_is_given_and_wraps() {
        let t = scattered();
        let me = CpuId(1);
        let other_package = ids(t.ring(me, Some(Level::Package), Level::Machine, 0));
        assert_eq!(other_package, [0, 4, 2, 6]);
        let after_four = t.position(CpuId(4)) + 1;
        assert_eq!(ids(t.ring(me, Some(Level::Package), Level::Machine, after_four)), [2, 6, 0, 4]);
        // A position inside the excluded run starts at the run's end, and one
        // outside the ring's own domain at the domain's start.
        assert_eq!(ids(t.ring(me, None, Level::Llc, t.position(me))), [5, 3, 7]);
        assert_eq!(ids(t.ring(me, None, Level::Llc, 0)), [5, 3, 7]);
    }
}
