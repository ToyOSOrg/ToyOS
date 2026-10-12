//! The machine's shape: which core, last-level cache and package each dense CPU
//! id sits in, so that a run can say which level of the cache tree each
//! migration, wake, failed steal and IPI crossed.
//!
//! A shape is what each CPU reads of itself: its x2APIC id, shifted right by the
//! width of each level — the SMT and package widths of CPUID leaf 0xB, and the
//! last-level cache's width, the base-2 log of how many ids share it, from the
//! deterministic cache leaf. Two CPUs share a level when their keys at it are
//! equal, and the lowest level they share is the one a move between them
//! crosses; a level whose domains are its parent's collapses into it, because no
//! pair of CPUs then has it as its lowest.
//!
//! Dense ids are the order the firmware listed the CPUs in, which is no topology
//! order: the T14's SMT siblings are four ids apart and the AMD bench's
//! adjacent, so a synthetic shape is enumerated under a seeded permutation and
//! nothing may read locality off an id's value.
//!
//! The scheduler core reads none of this. It is the simulator's instrument, and
//! it prices no level: there is no cache, frequency or SMT cost here.

use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

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

/// A count's column in [`LevelCounts`]: 0 is one CPU, then each [`Level`].
pub fn slot(level: Option<Level>) -> usize {
    level.map_or(0, |level| 1 + level as usize)
}

/// What one run did at each level, each array indexed by [`slot`]. Counts, never
/// times: whether a level is worth crossing is metal's verdict.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct LevelCounts {
    /// Tasks moved between CPUs, by steal or wake forward.
    pub migrations: [u64; 5],
    /// Wakes, by the level between the CPU a task parked on and the one it was
    /// placed on.
    pub wakes: [u64; 5],
    /// Steal probes their victim answered with no task.
    pub failed_steals: [u64; 5],
    /// Kicks, by the level between the CPU whose step sent one and its target.
    pub ipis: [u64; 5],
    /// Kicks no CPU's step sent: a device's.
    pub device_ipis: u64,
}

impl LevelCounts {
    pub fn add(&mut self, other: &Self) {
        for (sum, row) in [
            (&mut self.migrations, &other.migrations),
            (&mut self.wakes, &other.wakes),
            (&mut self.failed_steals, &other.failed_steals),
            (&mut self.ipis, &other.ipis),
        ] {
            for (a, b) in sum.iter_mut().zip(row) {
                *a += b;
            }
        }
        self.device_ipis += other.device_ipis;
    }

    /// One line per count, one column per level.
    pub fn table(&self) -> String {
        let mut out = String::from("                same    core     llc package machine\n");
        for (name, row) in [
            ("migrations", &self.migrations),
            ("wakes placed", &self.wakes),
            ("failed steals", &self.failed_steals),
            ("ipis", &self.ipis),
        ] {
            out.push_str(&format!("{name:<14}"));
            for count in row {
                out.push_str(&format!("{count:>8}"));
            }
            out.push('\n');
        }
        out.push_str(&format!("device ipis   {:>8}\n", self.device_ipis));
        out
    }
}

/// One CPU's key at each level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Keys {
    pub core: u32,
    pub llc: u32,
    pub package: u32,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub name: &'static str,
    /// Indexed by dense CPU id.
    keys: Vec<Keys>,
}

/// How many low bits of an id a level `count` wide takes.
fn width(count: u32) -> u32 {
    count.next_power_of_two().trailing_zeros()
}

impl Shape {
    /// Each dense CPU's x2APIC id, in the order the firmware listed them, and
    /// each level's width in bits.
    pub fn from_apic_ids(
        name: &'static str,
        ids: &[u32],
        smt_shift: u32,
        llc_shift: u32,
        package_shift: u32,
    ) -> Self {
        assert!(
            smt_shift <= llc_shift && llc_shift <= package_shift,
            "{name}: a core must sit inside its LLC and an LLC inside its package",
        );
        let mut seen = ids.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ids.len(), "{name}: two CPUs carry one x2APIC id");
        let keys = ids
            .iter()
            .map(|&id| Keys {
                core: id >> smt_shift,
                llc: id >> llc_shift,
                package: id >> package_shift,
            })
            .collect();
        Self { name, keys }
    }

    /// `cpus` cores of one thread each, sharing one LLC in one package: the
    /// machine of a scenario that names no shape.
    pub fn flat(cpus: usize) -> Self {
        let cpus = u32::try_from(cpus).expect("a CPU count fits an x2APIC id");
        let ids: Vec<u32> = (0..cpus).collect();
        Self::from_apic_ids("flat", &ids, 0, width(cpus), width(cpus))
    }

    /// `packages × llcs × cores × threads`, the ids laid out as x2APIC ids are,
    /// listed under the permutation `seed` draws.
    pub fn tree(
        name: &'static str,
        packages: u32,
        llcs: u32,
        cores: u32,
        threads: u32,
        seed: u64,
    ) -> Self {
        let (t, c, l) = (width(threads), width(cores), width(llcs));
        let mut ids = Vec::new();
        for package in 0..packages {
            for llc in 0..llcs {
                for core in 0..cores {
                    for thread in 0..threads {
                        ids.push(package << (l + c + t) | llc << (c + t) | core << t | thread);
                    }
                }
            }
        }
        ids.shuffle(&mut SmallRng::seed_from_u64(seed));
        Self::from_apic_ids(name, &ids, t, c + t, l + c + t)
    }

    /// The T14 as its scout read it: four cores of two threads under one 8 MiB
    /// L3, listed first threads first, so that SMT siblings are four ids apart.
    pub fn t14() -> Self {
        Self::from_apic_ids("t14", &[0, 2, 4, 6, 1, 3, 5, 7], 1, 4, 4)
    }

    /// The AMD bench as its scout read it: two CCXs of four cores of two
    /// threads, each CCX under its own 4 MiB L3, listed in APIC order so that
    /// SMT siblings are adjacent.
    pub fn amd_bench() -> Self {
        let ids: Vec<u32> = (0..16).collect();
        Self::from_apic_ids("amd-bench", &ids, 1, 3, 7)
    }

    pub fn cpus(&self) -> usize {
        self.keys.len()
    }

    pub fn keys(&self, cpu: usize) -> Keys {
        self.keys[cpu]
    }

    /// The lowest level `a` and `b` share, or `None` when they are one CPU.
    pub fn level(&self, a: usize, b: usize) -> Option<Level> {
        // By id: SMT siblings share every key.
        if a == b {
            return None;
        }
        let (a, b) = (self.keys[a], self.keys[b]);
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

    /// `cpus=… cores=… llcs=… packages=…`, the shape as counts per level.
    pub fn describe(&self) -> String {
        format!(
            "{}: cpus={} cores={} llcs={} packages={}",
            self.name,
            self.cpus(),
            self.domains(Level::Core),
            self.domains(Level::Llc),
            self.domains(Level::Package),
        )
    }
}

/// Every shape the topology sweep runs every scenario at: the two benches as
/// their scouts read them, and two synthetic trees under a permuted listing —
/// the second 768 CPUs wide, two packages of 24 eight-core LLCs.
pub fn shapes() -> Vec<Shape> {
    vec![
        Shape::t14(),
        Shape::amd_bench(),
        Shape::tree("4x8x2", 1, 4, 8, 2, 1),
        Shape::tree("2x24x8x2", 2, 24, 8, 2, 2),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_listed_under_a_permutation_keeps_its_counts_per_level() {
        let shape = Shape::tree("2x24x8x2", 2, 24, 8, 2, 2);
        assert_eq!(shape.cpus(), 768);
        assert_eq!(shape.domains(Level::Core), 384);
        assert_eq!(shape.domains(Level::Llc), 48);
        assert_eq!(shape.domains(Level::Package), 2);
    }

    #[test]
    fn a_pair_is_at_the_lowest_level_it_shares() {
        let t14 = Shape::t14();
        assert_eq!(t14.level(0, 0), None);
        assert_eq!(t14.level(0, 4), Some(Level::Core));
        assert_eq!(t14.level(0, 1), Some(Level::Llc));
        let amd = Shape::amd_bench();
        assert_eq!(amd.level(0, 1), Some(Level::Core));
        assert_eq!(amd.level(0, 2), Some(Level::Llc));
        assert_eq!(amd.level(0, 8), Some(Level::Package));
        let wide = Shape::tree("2x24x8x2", 2, 24, 8, 2, 2);
        let other_package = (1..wide.cpus()).find(|&cpu| wide.keys(cpu).package != wide.keys(0).package);
        assert_eq!(other_package.map(|cpu| wide.level(0, cpu)), Some(Some(Level::Machine)));
    }

    #[test]
    fn the_permutation_moves_siblings_apart() {
        let shape = Shape::tree("4x8x2", 1, 4, 8, 2, 1);
        let adjacent_siblings = (0..shape.cpus() / 2)
            .filter(|&pair| shape.level(2 * pair, 2 * pair + 1) == Some(Level::Core))
            .count();
        assert!(
            adjacent_siblings < shape.cpus() / 2,
            "every even id's sibling is the id after it: the listing was not permuted",
        );
    }
}
