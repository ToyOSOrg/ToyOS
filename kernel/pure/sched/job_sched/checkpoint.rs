//! The stage-1 checkpoint: does job-group EEVDF on per-CPU queues give
//! feasible weighted sharing on a multicore machine, and does the realtime
//! ceiling protect ordinary progress?
//!
//! Each test asserts and prints its table (`cargo test -p kernel --lib
//! --features sched-check -- checkpoint --nocapture`). "Bounded" means no
//! persistent drift: a job's error over the longest window is within its error
//! over a short one plus the EEVDF lag bound of the requests involved. The
//! tests before the cross-CPU ones run the plain candidate
//! (`Params::CANDIDATE`, per-CPU lag only); its `counterexample_` tests assert
//! the drift that makes cross-CPU fairness necessary. Every figure is the
//! model's, for the scenario that ran: not a hardware guarantee.

use super::sim::*;
use super::*;
use alloc::format;
use std::println;

const WINDOWS: [u64; 5] = [10 * MS, 100 * MS, 1000 * MS, 4000 * MS, 8000 * MS];
const SPAN: Nanos = Nanos(10_000 * MS);

/// Error over the longest window no larger than over 100 ms plus two
/// requests' lag (`−r < lag < r` at each end of a window).
fn assert_bounded(sim: &Sim, job: usize, what: &str) {
    let short = sim.window_error(job, 100 * MS, Nanos::ZERO);
    let long = sim.window_error(job, 8000 * MS, Nanos::ZERO);
    let allowance = 2.0 * Params::DEFAULT.slice as f64 / MS as f64;
    assert!(long <= short + allowance, "{what}: job {job} drifts: {short:.2} ms over 100 ms, {long:.2} ms over 8 s");
}

fn share(sim: &Sim, job: JobId) -> f64 {
    let (f, r) = sim.got[&job];
    (f + r) as f64 / sim.now.0 as f64
}

/// The plan's own reference case at three widths and both benches' shapes:
/// a build going 1 → 16 → 1 threads every half second beside a single-thread
/// job. The single job's fluid share is always one CPU; the build gets the rest.
#[test]
fn a_single_thread_job_keeps_one_cpu_beside_a_build_going_1_16_1() {
    for (name, topo) in [("flat4", flat(4)), ("t14", t14()), ("amd", amd()), ("flat16", flat(16))] {
        let m = topo.cpus() as u32;
        let mut sim = Sim::new(topo, Params::CANDIDATE);
        let single = sim.job(WEIGHT_UNIT);
        let build = sim.job(WEIGHT_UNIT);
        sim.thread(single, Work::Hog, CpuId(0), false);
        sim.thread(build, Work::Hog, CpuId(1 % m), false);
        for phase in 0..10u64 {
            sim.run_until(Nanos(phase * 1000 * MS + 500 * MS));
            let helpers: Vec<_> = (0..15).map(|i| sim.thread(build, Work::Hog, CpuId(i % m), false)).collect();
            sim.run_until(Nanos((phase + 1) * 1000 * MS));
            for h in helpers {
                sim.finish(h);
            }
        }
        println!("{}", sim.report(&format!("single + build 1-16-1, {name}"), Nanos::ZERO, &WINDOWS));
        assert!(share(&sim, single) > 0.99, "{name}: the single-thread job lost its CPU");
        assert_bounded(&sim, 0, name);
        assert_bounded(&sim, 1, name);
    }
}

#[test]
fn weighted_builds_split_by_weight() {
    let mut sim = Sim::new(flat(4), Params::CANDIDATE);
    let a = sim.job(2 * WEIGHT_UNIT);
    let b = sim.job(WEIGHT_UNIT);
    for i in 0..8 {
        sim.thread(a, Work::Hog, CpuId(i % 4), false);
        sim.thread(b, Work::Hog, CpuId(i % 4), false);
    }
    sim.run_until(SPAN);
    println!("{}", sim.report("A8 (weight 2) beside B8 (weight 1) on 4", Nanos::ZERO, &WINDOWS));
    assert_bounded(&sim, 0, "weighted");
    assert_bounded(&sim, 1, "weighted");
}

/// **Counterexample.** Three single-thread jobs of equal weight on two CPUs.
/// Each is entitled to 2/3 of a CPU; per-CPU queues give one of them a CPU of
/// its own for as long as no CPU idles, and no load-driven move can help: any
/// placement has loads 2 and 1. The error grows by 1/3 CPU-second per second.
#[test]
fn counterexample_three_single_thread_jobs_on_two_cpus() {
    let mut sim = Sim::new(flat(2), Params::CANDIDATE);
    for c in [0, 0, 1] {
        let j = sim.job(WEIGHT_UNIT);
        sim.thread(j, Work::Hog, CpuId(c), false);
    }
    sim.run_until(SPAN);
    println!("{}", sim.report("3 single-thread jobs on 2 CPUs", Nanos::ZERO, &WINDOWS));
    let drift = sim.window_error(0, 8000 * MS, Nanos::ZERO).max(sim.window_error(1, 8000 * MS, Nanos::ZERO));
    assert!(drift >= 0.9 * 8000.0 / 3.0, "the error no longer grows with the window: {drift:.1} ms");
}

/// **Counterexample.** Two jobs of three threads on four CPUs. Placement
/// {A,A} {A,B} {B} {B} has the same per-CPU loads (2/3, 2/3, 1/3, 1/3 of a
/// job weight) as the fair {A,B} {A,B} {A} {B}, so balancing by load leaves
/// it: A receives 1.5 CPUs and B 2.5 where each is entitled to 2.
#[test]
fn counterexample_a_fair_and_an_unfair_placement_have_the_same_loads() {
    let mut sim = Sim::new(flat(4), Params::CANDIDATE);
    let a = sim.job(WEIGHT_UNIT);
    let b = sim.job(WEIGHT_UNIT);
    let threads: Vec<_> =
        [(a, 0), (a, 0), (a, 1), (b, 1), (b, 2), (b, 3)].iter().map(|&(j, c)| sim.thread(j, Work::Hog, CpuId(c), true)).collect();
    // Placed by hand, then free once every CPU's weights have settled.
    sim.run_until(Nanos(20 * MS));
    for t in threads {
        sim.unpin(t);
    }
    sim.run_until(SPAN);
    println!("{}", sim.report("A3 B3 on 4 CPUs from {A,A} {A,B} {B} {B}", Nanos::ZERO, &WINDOWS));
    assert!(share(&sim, a) < 1.6 && share(&sim, b) > 2.4, "balancing found the fair placement");
}

/// The counterexamples' scenarios and the others above, scaled to `m` CPUs.
type Scenario = (&'static str, fn(&mut Sim, u32));
type Shape = (&'static str, fn() -> crate::sched::topology::Topology);

fn scaled_scenarios() -> [Scenario; 8] {
    [
        ("m+1 single-thread jobs", |sim, m| {
            for c in 0..=m {
                let j = sim.job(WEIGHT_UNIT);
                sim.thread(j, Work::Hog, CpuId(c % m), false);
            }
        }),
        ("{A,A}{A,B}{B}{B} per 4 CPUs", |sim, m| {
            let (a, b) = (sim.job(WEIGHT_UNIT), sim.job(WEIGHT_UNIT));
            let mut placed = Vec::new();
            for q in 0..m / 4 {
                for (j, c) in [(a, 0), (a, 0), (a, 1), (b, 1), (b, 2), (b, 3)] {
                    placed.push(sim.thread(j, Work::Hog, CpuId(4 * q + c), true));
                }
            }
            sim.run_until(Nanos(20 * MS));
            for t in placed {
                sim.unpin(t);
            }
        }),
        ("2m, m/2 and 1 threads from CPU 0", |sim, m| {
            for n in [2 * m, m / 2, 1] {
                let j = sim.job(WEIGHT_UNIT);
                for _ in 0..n {
                    sim.thread(j, Work::Hog, CpuId(0), false);
                }
            }
        }),
        ("single + build 1-16-1", |sim, m| {
            let (single, build) = (sim.job(WEIGHT_UNIT), sim.job(WEIGHT_UNIT));
            sim.thread(single, Work::Hog, CpuId(0), false);
            sim.thread(build, Work::Hog, CpuId(1), false);
            for phase in 0..9u64 {
                sim.run_until(Nanos(phase * 1000 * MS + 500 * MS));
                let helpers: Vec<_> = (0..15).map(|i| sim.thread(build, Work::Hog, CpuId(i % m), false)).collect();
                sim.run_until(Nanos((phase + 1) * 1000 * MS));
                for h in helpers {
                    sim.finish(h);
                }
            }
        }),
        ("2m + m hogs, churn every 10 ms", |sim, m| two_jobs(sim, m, Work::Hog, 10 * MS)),
        ("2m hogs + m duty 2/0.3 ms, churn every 10 ms", |sim, m| two_jobs(sim, m, DUTY, 10 * MS)),
        ("2m hogs + m duty 2/0.3 ms, churn every 1 ms", |sim, m| two_jobs(sim, m, DUTY, MS)),
        ("2m (weight 2) + 2m (weight 1)", |sim, m| {
            let (a, b) = (sim.job(2 * WEIGHT_UNIT), sim.job(WEIGHT_UNIT));
            for i in 0..2 * m {
                sim.thread(a, Work::Hog, CpuId(i % m), false);
                sim.thread(b, Work::Hog, CpuId(i % m), false);
            }
        }),
    ]
}

const DUTY: Work = Work::Duty { run: 2 * MS, sleep: 300 * US };

fn two_jobs(sim: &mut Sim, m: u32, work: Work, churn: u64) {
    let (a, b) = (sim.job(WEIGHT_UNIT), sim.job(WEIGHT_UNIT));
    for i in 0..2 * m {
        sim.thread(a, Work::Hog, CpuId(i % m), false);
    }
    for i in 0..m {
        sim.thread(b, work, CpuId(i % m), false);
    }
    sim.churn(churn, 1);
}

/// **Cross-CPU fairness** (`Params::cross_cpu_lag`), on every scenario above
/// at four CPUs and both benches' shapes, with the rotating partner policy
/// (each CPU compares itself with one other CPU per `fairness_period`; all
/// partners are run beside it for comparison).
///
/// **The ceiling is empirical.** 120 ms per unit of weight, at every window
/// from 100 ms to 8 s, for these scenarios. It is not a derived bound, and it
/// depends on what they share:
/// - widths of 4, 8 and 16 CPUs;
/// - weights within 2:1;
/// - the partner policy;
/// - the simulator's stand-in balancer, which keeps loads roughly even;
/// - leads that grow by a few ms per ms at most. Two jobs of many threads on
///   disjoint CPUs can grow theirs faster, and a job spread over `k` CPUs can
///   hold `k·lag_bound`.
///
/// The cost is printed: placement changes and migrations per second, beside
/// the scenarios' own random migrations.
#[test]
fn cross_cpu_lag_stays_within_its_ceiling_on_every_scenario_and_shape() {
    let p = Params::DEFAULT;
    let ceiling = 120.0;
    let shapes: [Shape; 3] = [("flat4", || flat(4)), ("t14", t14), ("amd", amd)];
    let mut worst: f64 = 0.0;
    let mut over = Vec::new();
    for partners in [Partners::Rotating, Partners::All] {
        for (shape, topo) in shapes {
            for (name, setup) in scaled_scenarios() {
                let mut sim = Sim::new(topo(), p);
                sim.partners = partners;
                let m = sim.topo.cpus() as u32;
                setup(&mut sim, m);
                sim.run_until(SPAN);
                let jobs = sim.samples[0].1.len();
                let mut line = format!("{partners:?} {shape:<6} {name:<46}");
                for w in [100 * MS, 1000 * MS, 8000 * MS] {
                    let e = (0..jobs).map(|j| sim.window_error(j, w, Nanos::ZERO)).fold(0.0, f64::max);
                    line.push_str(&format!(" err@{}ms {e:>6.1}", w / MS));
                }
                let secs = SPAN.0 as f64 / 1e9;
                line.push_str(&format!(
                    "  changes/s {:>6.1} migrations/s {:>7.1}",
                    sim.fairness_moves as f64 / secs,
                    sim.migrations as f64 / secs
                ));
                println!("{line}");
                if partners != Partners::Rotating {
                    continue;
                }
                for (j, job) in sim.jobs().iter().enumerate() {
                    let scale = (sim.weight(*job) as f64 / WEIGHT_UNIT as f64).max(1.0);
                    for w in [100 * MS, 1000 * MS, 4000 * MS, 8000 * MS] {
                        let e = sim.window_error(j, w, Nanos::ZERO);
                        worst = worst.max(e / scale);
                        if e > ceiling * scale {
                            over.push(format!("{shape} {name}: job {j} {e:.1} ms over {} ms", w / MS));
                        }
                    }
                }
            }
        }
    }
    println!("rotating: worst error per unit of weight {worst:.1} ms against a ceiling of {ceiling:.0} ms");
    assert!(over.is_empty(), "{}", over.join("\n"));
}

/// **Counterexample for cross-CPU fairness.** Four threads that run 1 ms and
/// sleep 0.1 ms, beside eight hogs, on four CPUs. The sleepers fall into step,
/// so the job's runnable count often dips below the two CPUs it is entitled to
/// without reaching zero. The fluid share gives the hogs the difference; the
/// per-job counter cannot tell a demand dip from a placement deficit, so it
/// repays the dip afterwards, and the job is over-served by about 1.25% for as
/// long as it runs. Telling the two apart needs the job's entitlement, which
/// depends on every job's demand: a global quantity. A job whose whole demand
/// stops (every thread asleep) is placed at its standing on return and is
/// not repaid.
#[test]
fn counterexample_a_dip_below_the_share_is_repaid_later() {
    let mut sim = Sim::new(flat(4), Params::DEFAULT);
    let s = sim.job(WEIGHT_UNIT);
    let h = sim.job(WEIGHT_UNIT);
    for i in 0..4 {
        sim.thread(s, Work::Duty { run: MS, sleep: 100 * US }, CpuId(i), false);
    }
    for i in 0..8 {
        sim.thread(h, Work::Hog, CpuId(i % 4), false);
    }
    sim.run_until(SPAN);
    println!("{}", sim.report("S4 duty 1 ms / 0.1 ms beside H8 hogs on 4, cross-CPU", Nanos::ZERO, &WINDOWS));
    let over_served = share(&sim, s) - sim.ideal[&s] / sim.now.0 as f64;
    assert!(over_served > 0.01, "the dip is no longer repaid: {over_served:.4} CPUs");
}

fn rt_machine(
    params: Params,
    workers_per_cpu: usize,
    work: Work,
    hop: bool,
    overrun: u64,
) -> (Sim, [JobId; 2], Vec<JobId>, Vec<ThreadId>) {
    let mut sim = Sim::new(flat(4), params);
    sim.rt_overrun = overrun;
    let o = sim.job(WEIGHT_UNIT);
    let p = sim.job(WEIGHT_UNIT);
    let mut owners = Vec::new();
    let mut workers = Vec::new();
    for c in 0..4 {
        for _ in 0..workers_per_cpu {
            let m = sim.job(WEIGHT_UNIT);
            let t = sim.thread(m, work, CpuId(c), !hop);
            sim.grant(t);
            owners.push(m);
            workers.push(t);
        }
    }
    for i in 0..8 {
        sim.thread(o, Work::Hog, CpuId(i % 4), false);
    }
    for i in 0..4 {
        sim.thread(p, Work::Hog, CpuId(i % 4), false);
    }
    if hop {
        sim.churn(500 * US, 3);
    }
    sim.run_until(Nanos(2000 * MS));
    (sim, [o, p], owners, workers)
}

fn rt_report(title: &str, sim: &Sim, ordinary: [JobId; 2], owners: &[JobId]) {
    let k = [1, 2, 4, 20];
    let floors: Vec<_> = k.iter().map(|&k| sim.ordinary_floor(k) / US).collect();
    let peaks: Vec<_> = k.iter().map(|&k| sim.rt_peak(k) / US).collect();
    let fallback: f64 = owners.iter().map(|m| sim.got[m].0 as f64).sum::<f64>() / sim.now.0 as f64;
    let spare = sim.spare_on.iter().sum::<u64>() as f64 / sim.now.0 as f64;
    let streak = sim.rt_streak_max.iter().max().copied().unwrap_or(0) / US;
    println!("{title}");
    println!("  over 1/2/4/20 intervals: ordinary floor {floors:?} us, realtime peak {peaks:?} us; longest realtime run {streak} us");
    println!(
        "  CPUs: ordinary jobs {:.3} {:.3}, workers realtime {:.3}, ordinary {fallback:.3}, spare {spare:.3}; owners revoked {}/{}",
        sim.got[&ordinary[0]].0 as f64 / sim.now.0 as f64,
        sim.got[&ordinary[1]].0 as f64 / sim.now.0 as f64,
        sim.rt_on.iter().sum::<u64>() as f64 / sim.now.0 as f64,
        owners.iter().filter(|&&m| sim.s.rt_owner(m).revoked).count(),
        owners.len(),
    );
}

/// No CPU runs more realtime than its allowance in any run of intervals,
/// however the workers behave: spinning in place, moved every 0.5 ms to
/// another CPU (any queued thread, of any band), or overrunning every
/// exhaustion by 200 µs (a non-preemptible section), whose excess is repaid in
/// the next interval.
#[test]
fn the_ceiling_bounds_realtime_on_every_cpu() {
    let p = Params { strikes_to_revoke: u32::MAX, ..Params::DEFAULT };
    for (title, hop, overrun) in [("spinners", false, 0), ("hopping spinners", true, 0), ("200 us overruns", false, 200 * US)] {
        let (sim, ordinary, owners, _) = rt_machine(p, 1, Work::Hog, hop, overrun);
        rt_report(&format!("one never-revoked realtime spinner per CPU: {title}"), &sim, ordinary, &owners);
        for k in [1, 2, 4, 20] {
            assert!(sim.rt_peak(k) <= k as u64 * p.rt_budget + overrun, "{title}: {k} intervals");
        }
        let intervals = sim.now.0 / p.rt_interval;
        for (c, &rt) in sim.rt_on.iter().enumerate() {
            assert!(rt <= intervals * p.rt_budget + overrun, "{title}: cpu{c} ran {rt} ns realtime");
        }
    }
}

/// Owner decision J2: once a CPU's allowance is exhausted its workers run
/// only on spare capacity, so in every interval with ordinary work runnable
/// throughout, that work gets all of the interval the allowance leaves, less
/// any overrun; and realtime never runs continuously longer than two budgets
/// (one interval's end and the next one's start) plus the overrun. Holds for
/// never-revoked spinners in place, moved every 0.5 ms, overrunning by 200 µs,
/// and for legitimate overload.
#[test]
fn ordinary_work_gets_every_intervals_remainder() {
    let p = Params { strikes_to_revoke: u32::MAX, ..Params::DEFAULT };
    let periodic = Work::Periodic { period: 5_333 * US, work: 1_200 * US };
    for (title, per_cpu, work, hop, overrun) in [
        ("spinners", 1, Work::Hog, false, 0),
        ("moved spinners", 1, Work::Hog, true, 0),
        ("spinners overrunning 200 us", 1, Work::Hog, false, 200 * US),
        ("four 1.2 ms / 5.333 ms callbacks per CPU", 4, periodic, false, 0),
    ] {
        let (sim, ordinary, owners, _) = rt_machine(p, per_cpu, work, hop, overrun);
        rt_report(&format!("J2: {title}"), &sim, ordinary, &owners);
        assert!(sim.ordinary_floor(1) >= p.rt_interval - p.rt_budget - overrun, "{title}: interval floor");
        let streak = sim.rt_streak_max.iter().max().copied().unwrap_or(0);
        assert!(streak <= 2 * p.rt_budget + overrun, "{title}: realtime ran {streak} ns without a break");
    }
}

/// Legitimate audio below the ceiling: two callbacks per CPU, each 1.2 ms in
/// every 5.333 ms period (256 frames at 48 kHz). Every period is met, ordinary
/// work gets every interval's remainder, and nobody is called a runaway.
#[test]
fn audio_below_the_ceiling_meets_every_period() {
    let periodic = Work::Periodic { period: 5_333 * US, work: 1_200 * US };
    let (sim, ordinary, owners, workers) = rt_machine(Params::DEFAULT, 2, periodic, false, 0);
    rt_report("two 1.2 ms / 5.333 ms callbacks per CPU", &sim, ordinary, &owners);
    for &w in &workers {
        let (missed, worst) = sim.missed(w);
        assert_eq!(missed, 0);
        // Behind at most the other callback on its CPU.
        assert!(worst <= 2 * 1_200 * US, "response {worst}");
    }
    assert!(owners.iter().all(|&m| !sim.s.rt_owner(m).revoked));
    // In any interval two releases can land: at most 2 × 2 × 1.2 ms realtime.
    assert!(sim.ordinary_floor(1) >= Params::DEFAULT.rt_interval - 4 * 1_200 * US);
}

/// Aggregate overload is not abuse: four such callbacks per CPU want 90% of
/// it; the ceiling holds, callbacks miss periods, and no owner is revoked.
#[test]
fn aggregate_overload_drops_audio_and_revokes_nobody() {
    let periodic = Work::Periodic { period: 5_333 * US, work: 1_200 * US };
    let (sim, ordinary, owners, workers) = rt_machine(Params::DEFAULT, 4, periodic, false, 0);
    rt_report("four 1.2 ms / 5.333 ms callbacks per CPU", &sim, ordinary, &owners);
    assert!(owners.iter().all(|&m| !sim.s.rt_owner(m).revoked));
    assert!(workers.iter().any(|&w| sim.missed(w).0 > 0));
    assert!(sim.rt_peak(1) <= Params::DEFAULT.rt_budget);
}

/// A spinning worker is a runaway once per 50 ms of realtime without
/// blocking; its owner is revoked at the third, after 150 ms of realtime —
/// about 215 ms at the 70% ceiling — and its job then shares as any other.
#[test]
fn a_runaway_owner_is_revoked() {
    let (sim, ordinary, owners, _) = rt_machine(Params::DEFAULT, 1, Work::Hog, false, 0);
    rt_report("one realtime spinner per CPU, revocable", &sim, ordinary, &owners);
    assert!(owners.iter().all(|&m| sim.s.rt_owner(m).revoked));
    let p = Params::DEFAULT;
    for (c, &rt) in sim.rt_on.iter().enumerate() {
        let strikes = p.strikes_to_revoke as u64 * p.runaway;
        assert!(rt <= strikes + p.rt_budget, "cpu{c}: {rt} ns realtime before revocation");
    }
}

/// **Counterexample for the owner-level abuse rule.** The runaway record
/// counts realtime execution per thread since it last blocked, so a spinner
/// that blocks for 1 µs every 40 ms, or a client whose worker is replaced by
/// a fresh granted thread every 40 ms (a stream reopened on a new thread), is
/// never a runaway: each takes the whole allowance of its CPU indefinitely.
/// The per-CPU ceiling still bounds it; the owner-level contract does not.
#[test]
fn counterexample_runaway_detection_misses_brief_blocks_and_replaced_threads() {
    let p = Params::DEFAULT;
    for replace in [false, true] {
        let mut sim = Sim::new(flat(1), p);
        let o = sim.job(WEIGHT_UNIT);
        sim.thread(o, Work::Hog, CpuId(0), true);
        let m = sim.job(WEIGHT_UNIT);
        let work = if replace { Work::Hog } else { Work::Duty { run: 40 * MS, sleep: US } };
        let mut w = sim.thread(m, work, CpuId(0), true);
        sim.grant(w);
        for step in 1..=50u64 {
            sim.run_until(Nanos(step * 40 * MS));
            if replace {
                sim.finish(w);
                w = sim.thread(m, Work::Hog, CpuId(0), true);
                sim.grant(w);
            }
        }
        let rt = sim.rt_on[0] as f64 / sim.now.0 as f64;
        let what = if replace { "worker replaced every 40 ms" } else { "1 µs block every 40 ms of work" };
        println!("{what}: realtime share {rt:.3}, strikes {}, revoked {}", sim.s.rt_owner(m).strikes, sim.s.rt_owner(m).revoked);
        assert!(!sim.s.rt_owner(m).revoked && sim.s.rt_owner(m).strikes == 0, "{what}: now caught");
        assert!(rt > 0.6, "{what}: realtime share {rt:.3}");
    }
}
