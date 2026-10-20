//! The scheduler at the shapes of real machines, and work conservation.
//!
//! Every scenario runs at every [`topology::shapes`] machine — the T14 and the
//! AMD bench as their scouts read them, and two permuted synthetic trees up to
//! 768 CPUs — with every invariant walked after every step, work conservation
//! (I15) among them. What each run did at each level of the tree is printed as
//! counts:
//!
//! ```text
//! cargo test -p kernel-sim --test topology -- --nocapture
//! ```

use kernel::sched::cpu::{Balance, PUSH_THRESHOLD};
use kernel::sched::fair::QUANTUM_NS;
use kernel_sim::choice::ChoiceStream;
use kernel_sim::explore::{run, Outcome};
use kernel_sim::scenarios;
use kernel_sim::topology::{self, CpuKeys, LevelCounts, Shape};
use kernel_sim::workload::{Scenario, SurplusShape};

const MS: u64 = 1_000_000;

fn stream(scenario: &Scenario, seed: u64) -> ChoiceStream {
    if seed.is_multiple_of(2) {
        ChoiceStream::from_seed(seed)
    } else {
        ChoiceStream::pct(seed, scenario.cpus(), 3)
    }
}

fn sweep(scenario: &Scenario, seeds: u64) -> Vec<Outcome> {
    (0..seeds).map(|seed| run(scenario.clone(), &mut stream(scenario, seed))).collect()
}

/// `eax:ebx:ecx:edx` quads in hex, from the first `[` after `label` in `line`
/// to its `]`.
fn quads(line: &str, label: &str) -> Vec<[u32; 4]> {
    let after = &line[line.find(label).expect("the label is on the line") + label.len()..];
    let inside = &after[after.find('[').expect("a list opens") + 1..after.find(']').expect("and closes")];
    inside
        .split_whitespace()
        .map(|quad| {
            let words: Vec<u32> = quad
                .split(':')
                .map(|word| u32::from_str_radix(word, 16).expect("a hex word"))
                .collect();
            [words[0], words[1], words[2], words[3]]
        })
        .collect()
}

/// What a bench's scout logged, decoded the way the SDM and the APM define the
/// fields: each listed CPU's x2APIC id from the MADT, and from every CPU that
/// came up its leaf 0xB SMT and package widths (the subleaves of level type 1
/// and 2, EAX bits 4:0) and how many ids share its highest data or unified
/// cache (the deterministic cache leaf, 4 on Intel and 0x8000001D on AMD, EAX
/// bits 25:14 plus one). Those three are identical on every CPU that ran, or
/// the shape would not be one tree.
fn scouted(log: &str) -> Vec<CpuKeys> {
    let madt: Vec<u32> = log
        .lines()
        .find_map(|line| line.split("MADT cpus=[").nth(1))
        .expect("the log lists the MADT")
        .trim_end_matches(']')
        .split(", ")
        .map(|id| id.parse().expect("a decimal id"))
        .collect();
    let mut widths = Vec::new();
    for line in log.lines().filter(|line| line.contains(" levels B=")) {
        let cpu: usize = line
            .split("scout-topo: cpu")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|cpu| cpu.parse().ok())
            .expect("the line names its cpu");
        let levels = quads(line, "B=");
        assert_eq!(levels[0][3], madt[cpu], "cpu{cpu}'s own x2APIC id is the MADT's");
        let shift = |kind: u32| {
            levels.iter().find(|q| (q[2] >> 8) & 0xff == kind).expect("the level is listed")[0] & 0x1f
        };
        widths.push((shift(1), shift(2)));
    }
    let mut sharing = Vec::new();
    for line in log.lines().filter(|line| line.contains(" caches ")) {
        let leaf = if line.contains("8000001D=") { "8000001D=" } else { "4=" };
        let (_, shared) = quads(line, leaf)
            .into_iter()
            .take_while(|q| q[0] & 0x1f != 0)
            .filter(|q| matches!(q[0] & 0x1f, 1 | 3))
            .map(|q| ((q[0] >> 5) & 7, ((q[0] >> 14) & 0xfff) + 1))
            .max()
            .expect("the CPU has a data cache");
        sharing.push(shared);
    }
    widths.dedup();
    sharing.dedup();
    let ([(smt, package)], [shared]) = (widths.as_slice(), sharing.as_slice()) else {
        panic!("the CPUs that ran disagree: widths {widths:?}, sharing {sharing:?}");
    };
    let llc = shared.next_power_of_two().trailing_zeros();
    madt.iter()
        .map(|&id| CpuKeys { core: id >> smt, llc: id >> llc, package: id >> package })
        .collect()
}

/// **The oracle: each bench's shape is what its own CPUs reported.** The logs
/// are the scouts' boots as posted on the topology measurement
/// (https://github.com/ToyOSOrg/ToyOS/pull/865#issuecomment-6119427274), staged
/// from `042015035`. The AMD bench brought up 8 of its 16 CPUs, so the other
/// eight are read as its MADT lists them under the widths the eight that ran
/// reported — the MADT's ids being APIC ids, as they are for every CPU that
/// came up.
#[test]
fn a_benchs_shape_is_what_its_scout_read() {
    for (shape, log) in [
        (Shape::t14(), include_str!("scout/t14.log")),
        (Shape::amd_bench(), include_str!("scout/amd-bench.log")),
    ] {
        let read = scouted(log);
        assert_eq!(shape.cpus(), read.len(), "{}: the MADT lists every CPU", shape.name);
        for (cpu, keys) in read.into_iter().enumerate() {
            assert_eq!(shape.keys(cpu), keys, "{}: cpu{cpu}", shape.name);
        }
    }
}

/// Seeds per scenario at a shape: the in-test count for the benches' widths,
/// fewer as the machine widens and every step walks more CPUs.
fn seeds(shape: &Shape) -> u64 {
    match shape.cpus() {
        0..=16 => 100,
        17..=64 => 20,
        _ => 2,
    }
}

#[test]
fn every_scenario_holds_at_every_shape() {
    let mut failures = Vec::new();
    for shape in topology::shapes() {
        let mut levels = LevelCounts::default();
        let (mut wc_rounds, mut rounds_to_run, mut runs) = (0, 0, 0);
        for scenario in scenarios::all() {
            let scenario = scenario.with_topology(shape.clone());
            for outcome in sweep(&scenario, seeds(&shape)) {
                if !outcome.passed() {
                    failures.push(format!("{}: {}", shape.name, outcome.report()));
                }
                levels.add(&outcome.levels);
                wc_rounds = wc_rounds.max(outcome.wc_rounds);
                rounds_to_run = rounds_to_run.max(outcome.rounds_to_run);
                runs += 1;
            }
        }
        println!(
            "{} over {runs} runs: worst idle beside stealable work {wc_rounds} rounds, worst \
             wait to run {rounds_to_run} rounds\n{}",
            shape.describe(),
            levels.table(),
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Seeds for the work-conservation cases.
const WC_SEEDS: u64 = 20;

/// Every thread of a machine-filling workload spawned onto cpu0 of a bench:
/// one victim, every other CPU idle, and only the balance path to fix it.
fn lopsided(shape: Shape, threads: usize) -> Scenario {
    scenarios::lopsided_placement(shape.cpus(), threads, 60 * MS).with_topology(shape)
}

/// **The shipped push keeps every idle CPU inside I15's bound** on the
/// lopsided machine, where the last CPU its cursor reaches waits out the
/// cursor's walk.
#[test]
fn the_push_keeps_an_idle_cpu_beside_work_inside_its_bound() {
    for shape in [Shape::t14(), Shape::amd_bench()] {
        let bound = shape.cpus() as u64 + 2;
        let scenario = lopsided(shape.clone(), 2 * shape.cpus());
        let mut worst = 0;
        for outcome in sweep(&scenario, WC_SEEDS) {
            assert!(outcome.passed(), "{}: {}", shape.name, outcome.report());
            worst = worst.max(outcome.wc_rounds);
        }
        println!("{}: worst idle beside stealable work {worst} rounds, bound {bound}", shape.name);
    }
}

/// **An idle CPU whose whole CCX looks busy is still reached**: the machine
/// [`scenarios::group_imbalance`] stages, under the shipped balance path. It
/// must hold, and the idle CPU must have been seen beside the work, or the
/// staging put nobody there.
#[test]
fn an_idle_cpu_whose_ccx_looks_busy_is_reached() {
    let scenario = scenarios::group_imbalance();
    let mut worst = 0;
    for outcome in sweep(&scenario, WC_SEEDS) {
        assert!(outcome.passed(), "{}", outcome.report());
        worst = worst.max(outcome.wc_rounds);
    }
    assert!(worst > 0, "no CPU of the staged machine was ever idle beside stealable work");
}

/// **The negative gate: a push that waits for more surplus than a probe takes.**
/// `PUSH_THRESHOLD`'s own doc names the drift: one above the probe's threshold
/// leaves a victim holding one stealable task unannounced, so a CPU that halted
/// before the task was queued is never rung. One thread on cpu0 above
/// `PUSH_THRESHOLD` is exactly that victim, and each runs `cpus + 3` quanta, so
/// the one it queues outlasts I15's `cpus + 2` rounds. It must fail, on I15
/// alone; the shipped threshold on the same machine is the control and must
/// hold.
#[test]
fn a_push_that_waits_for_more_than_a_probe_takes_is_caught() {
    for shape in [Shape::t14(), Shape::amd_bench()] {
        let threads = PUSH_THRESHOLD as usize + 1;
        let work = (shape.cpus() as u64 + 3) * QUANTUM_NS;
        let shipped =
            scenarios::lopsided_placement(shape.cpus(), threads, work).with_topology(shape.clone());
        let late = shipped.clone().with_balance(Balance::PushOnSurplus {
            threshold: PUSH_THRESHOLD + 1,
        });
        let caught: Vec<Outcome> = sweep(&late, WC_SEEDS).into_iter().filter(|o| !o.passed()).collect();
        assert!(
            !caught.is_empty(),
            "{}: a push one above the probe's threshold went undetected in {WC_SEEDS} schedules",
            shape.name,
        );
        for outcome in &caught {
            assert!(
                outcome.violations.iter().all(|v| v.starts_with("I15")),
                "{}: caught by something other than I15: {:?}",
                shape.name,
                outcome.violations,
            );
        }
        println!("{}: caught in {} of {WC_SEEDS} schedules", shape.name, caught.len());
        for outcome in sweep(&shipped, WC_SEEDS) {
            assert!(outcome.passed(), "{}: {}", shape.name, outcome.report());
        }
    }
}

/// **The negative gate for a CPU that is rung and declines to probe**, on every
/// machine down to two CPUs, where the rung CPU is the only sleeper and the
/// victim the only CPU a round waits on, ringing it in the very pass that
/// completes the round. Every pass's surplus is withheld from the probe after
/// its push rang on it ([`SurplusShape::Withheld`]), so an idle CPU wakes, reads
/// nothing worth a probe and sleeps again for as long as the work lasts. Every
/// schedule must fail, on I15 alone; the published surplus on the same machine
/// is the control and must hold.
#[test]
fn a_cpu_that_is_rung_and_declines_to_probe_is_caught() {
    for shape in [Shape::flat(2), Shape::t14(), Shape::amd_bench()] {
        let shipped = lopsided(shape.clone(), 3);
        let declined = shipped.clone().with_surplus(SurplusShape::Withheld);
        for outcome in sweep(&declined, WC_SEEDS) {
            assert!(!outcome.passed(), "{}: a declined probe went undetected", shape.name);
            assert!(
                outcome.violations.iter().all(|v| v.starts_with("I15")),
                "{}: caught by something other than I15: {:?}",
                shape.name,
                outcome.violations,
            );
        }
        for outcome in sweep(&shipped, WC_SEEDS) {
            assert!(outcome.passed(), "{}: {}", shape.name, outcome.report());
        }
    }
}
