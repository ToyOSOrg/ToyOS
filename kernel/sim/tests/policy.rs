//! **The measured policy suite**: what the scheduler's policy actually delivers,
//! as numbers, against bounds derived from its own constants.
//!
//! `scenarios.rs` next door is the exit criterion — every negative gate fails,
//! every scenario passes. That is a suite of *verdicts*. The policy this
//! scheduler states — a job owns its share and its threads divide it, and each
//! CPU runs EEVDF over the job groups it holds (owner decision J1) — is about
//! quantities: how much share a process got, how long an interactive wake
//! waited, how long a runnable task was passed over. Each one below is
//! measured and then asserted.
//!
//! # The constant every bound here is made of
//!
//! ```text
//! one request = SLICE_NS + 2 × RUN_CHUNK_NS
//! ```
//!
//! EEVDF holds each entity's lag inside one request plus the granularity a
//! decision is taken late by (Stoica and Abdel-Wahab, Theorem 3), and the model
//! observes a decision up to two chunks late. So a group's service stays within
//! a request of its share, a thread whose group is due waits out at most the
//! request running ahead of it, and a thread in a group of `n` waits out its
//! `n − 1` siblings' requests at the rate its group is served.
//!
//! # What each case measures, and what it found
//!
//! | case | measured | derived bound | measured/bound |
//! |---|---|---|---|
//! | thread-count asymmetry | `solo` finishes one slice late at every `N` ≥ 2 | `2 × WORK + 2` requests | 0.94 |
//! | the deficit over time | 4 ms at N=4 and N=16, unchanged as the window doubles twice | an offset, not a drift | — |
//! | interactive wake | 5 ms at worst, 2 ms for every wake but one per run | one request; the minimum run and two chunks | 0.83 / 0.89 |
//! | starvation | 20 ms at 4 runnable threads, 120 ms at 17 | groups × the largest group × one request | 0.55–0.63 |
//! | a bounded re-arm | every CPU is reached at every width; 10 ms of probe gap | `every_ns + 3 × RUN_CHUNK` = 13 ms | 0.77 |
//!
//! The adversarial-placement case is the only one whose bound is a *count*
//! rather than a duration, and that is the model's limit rather than a choice:
//! the clock advances on the executing CPU's step, so a run's makespan is what
//! one CPU would take at every width and no wall-clock statement about a wide
//! machine can be read off it. What survives is the protocol — how many tasks
//! the balance path moved, and which CPUs ever got one — and that is enough to
//! separate a machine that recovers from one that does not. The two cure cases
//! below it inherit the same limit and answer it the same way: recovery is
//! counted in CPUs, and timed in **probe gaps** — how long a halted CPU sat
//! beside a published surplus with no probe of its own outstanding, which is a
//! quantity the protocol owns end to end.
//!
//! Every number came from this file at 16–40 seeds per point:
//! `cargo test --release -p kernel-sim --test policy -- --nocapture`, on
//! `e74dbf1` with the job-group scheduler applied. The tables in each test
//! carry the full sweeps.
//!
//! # What the numbers say, in one paragraph
//!
//! **A process cannot buy CPU by forking.** `swarm` with N threads delays a
//! single-threaded rival by one slice — the EEVDF lag of one request — at every
//! N from 2 to 64, once, and never again however long the run goes on. The
//! wait a runnable thread can see grows with its own group's size, not with
//! the machine's thread count: a thread among 16 siblings waits out 15 of their
//! requests at the half of the CPU its job is owed.
//!
//! **The pull half is one-shot, and the shipped push is what closes it.** An
//! idle CPU posts one probe on its way to `hlt` and nothing re-posts it, so
//! under plain `Balance::Pull` a CPU that halted before any neighbour published
//! surplus was never probed again. A bounded re-arm of the probe and a push on
//! surplus each reach every CPU at every width, and they cost opposite things —
//! the timer ticks whether or not there is anything to come for, the push fires
//! only where there is. That is why `Balance::PushOnSurplus` ships (owner
//! decision 2026-08-23) and the re-arm was declined; these three cases are the
//! numbers the decision was made on. Busy CPUs also balance their job groups'
//! summed weight between themselves (`SchedPass::balance_busy`), at most one
//! request per CPU per `BALANCE_NS`.
//!
//! # Determinism, seeds, and what varies
//!
//! The simulator is deterministic in its decision stream: a seed is a schedule.
//! Every case sweeps seeds 0..N alternating the uniform driver with PCT, which is
//! the idiom `scenarios.rs` uses and the only source of variation the tree has —
//! there is no wall clock and no host randomness anywhere in a run. Two of the
//! workloads sit at one CPU, where the enabled-step set is nearly a singleton and
//! every seed produces the *same* run: `share_gain`'s worst and best across 16
//! seeds are the identical nanosecond at every width, and that is reported rather
//! than hidden, because a worst-of-N over N identical runs is a worst-of-one and
//! a reader is owed that. The multi-CPU cases do vary, and there the sweep is
//! doing what a sweep is for.

use kernel::sched::cpu::Balance;
use kernel::sched::cpu::STALE_PASS_NS;
use kernel::sched::queue::{MIN_SLICE_NS, SLICE_NS};
use kernel::sched::task::QUANTUM_NS;
use kernel_sim::choice::ChoiceStream;
use kernel_sim::explore::{run, Outcome};
use kernel_sim::latency::{Latency, ReadyCause};
use kernel_sim::scenarios;
use kernel_sim::vm::{IPI_LATENCY_NS, RUN_CHUNK_NS};
use kernel_sim::workload::{JobShape, Scenario};

const MS: u64 = 1_000_000;

/// One thread's work in the share-gain cases, the figure `scenarios::WORK`
/// gives a `fairness_storm` thread and for the same reason — a window many
/// requests wide is what a granularity bound needs before it can separate a
/// fair split from a broken one.
const WORK: u64 = 60 * MS;

/// Seeds per point for the one-CPU cases. They are deterministic (see the module
/// header), so this buys reproducibility of the *reported* number rather than
/// search; the cases that actually explore say so where they set their own.
const SEEDS: u64 = 16;

/// The fair band's granularity: one request, observed up to two chunks late.
/// Every bound in this file is made of it.
///
/// `max KernelSection` is zero in all three policy workloads — none of them runs
/// an `Op::KernelSection` — so the term is dropped here rather than carried as a
/// zero, and any workload that grows one has to add it back.
const DISPATCH_NS: u64 = SLICE_NS + 2 * RUN_CHUNK_NS;

fn stream(scenario: &Scenario, seed: u64) -> ChoiceStream {
    if seed.is_multiple_of(2) {
        ChoiceStream::from_seed(seed)
    } else {
        ChoiceStream::pct(seed, scenario.cpus(), 3)
    }
}

/// Run one scenario over `seeds` schedules, asserting every invariant walk stays
/// clean, and hand each outcome to `fold`. Every case here is a measurement over
/// runs that also had to be *correct*, and a policy number taken off a run that
/// violated I1 would be a number about a broken machine.
fn sweep(scenario: &Scenario, seeds: u64, mut fold: impl FnMut(&Outcome)) {
    for seed in 0..seeds {
        let mut choices = stream(scenario, seed);
        let outcome = run(scenario.clone(), &mut choices);
        assert!(outcome.passed(), "{}", outcome.report());
        fold(&outcome);
    }
}

/// **The share-gain attack, measured**: what a process buys by having N runnable
/// threads instead of one.
///
/// `solo` has one thread and `swarm` has N, both pure CPU on one CPU, both
/// entitled to half of it. `solo`'s work is a fixed 60 ms, so the instant it
/// finishes *is* the share it was served at: 120 ms means half the CPU, 240 ms
/// means a quarter.
///
/// **What it found.** `solo` finishes one slice late at every width above one,
/// and the seeds do not disagree by a nanosecond
/// (`cargo test --release -p kernel-sim --test policy -- --nocapture`):
///
/// | N | T_solo | deficit | `solo`'s share over that window |
/// |---|---|---|---|
/// | 1  | 120 ms | 0 ms | 500‰ |
/// | 2–64 | 124 ms | 4 ms | 483‰ |
///
/// On one CPU the two jobs' groups weigh the same whatever N is — a group
/// weighs its job's weight times its share of the job's runnable threads, all
/// of which are here — so the swarm's threads divide its half among themselves.
///
/// **The bound is derived and it is not moved.** `solo`'s group's lag stays
/// within one request of zero, so it has had at least half the CPU less one
/// request at every instant, and spends its work at most two requests late.
#[test]
fn a_process_cannot_buy_cpu_by_forking() {
    let mut table = Vec::new();
    for threads in [1usize, 2, 4, 8, 16, 32, 64] {
        let scenario = scenarios::share_gain(threads, WORK);
        let solo = scenario
            .process_index("solo")
            .expect("share_gain has a solo");
        let bound = 2 * WORK + 2 * DISPATCH_NS;
        let (mut worst, mut best) = (0, u64::MAX);
        sweep(&scenario, SEEDS, |outcome| {
            let finish = outcome.process_finish_ns[solo]
                .expect("solo's 60 ms of work must complete inside the run");
            worst = worst.max(finish);
            best = best.min(finish);
        });
        assert!(
            worst <= bound,
            "at {threads} rival thread(s) a single-threaded process took {worst} ns to spend \
             {WORK} ns of CPU, against a derived {bound} ns — two of its own shares plus two \
             requests at {DISPATCH_NS} ns. Past this, forking buys more CPU than the policy \
             says it can.",
        );
        table.push((threads, worst, worst - 2 * WORK, WORK * 1000 / worst, best));
    }

    for &(threads, finish, deficit, permille, best) in &table {
        println!(
            "share_gain N={threads}: T_solo={finish} ns deficit={deficit} ns \
             share={permille}permille (best seed {best})",
        );
    }

    // The shape of the finding, asserted rather than only tabulated: the
    // deficit does not grow with the rival count, which is what "a helper buys
    // no entitlement" means for one CPU.
    let deficits: Vec<u64> = table.iter().map(|&(_, _, deficit, _, _)| deficit).collect();
    let (two, many) = (deficits[1], deficits[deficits.len() - 1]);
    assert!(
        many <= two,
        "the deficit went from {two} ns at two rival threads to {many} ns at {} — a deficit \
         that grows with the swarm is a share bought by forking",
        table[table.len() - 1].0,
    );
    // And the measurement has to be a comparison and not an accident of a
    // bound nothing could reach.
    assert!(
        many * 4 > 2 * DISPATCH_NS,
        "at {} rivals the deficit is {many} ns against a {} ns bound — more than a factor of \
         four of slack means the bound has stopped constraining anything",
        table[table.len() - 1].0,
        2 * DISPATCH_NS,
    );
}

/// **The deficit is an offset, not a drift** — which is the whole difference
/// between a granularity and a process that permanently owns more of the machine
/// than it is entitled to.
///
/// The case above measures `solo` finishing one slice late over a 120 ms
/// window. That number is only tolerable if it is paid *once*: if it were paid
/// per unit of work, `swarm` would be taking a fixed fraction of the machine for
/// ever and the policy would simply be false. So the identical scenario is run
/// with `solo`'s work doubled and doubled again, and the deficit has to stay
/// where it is.
///
/// **What it found**, at 16 seeds per point:
///
/// | N | work | T_solo | deficit | share |
/// |---|---|---|---|---|
/// | 4, 16 |  60 ms | 124 ms | 4 ms | 483‰ |
/// | 4, 16 | 120 ms | 244 ms | 4 ms | 491‰ |
/// | 4, 16 | 240 ms | 484 ms | 4 ms | 495‰ |
///
/// The deficit is the same nanosecond at every window length and every swarm,
/// so the share climbs toward 500‰ as the window grows.
#[test]
fn the_share_deficit_is_an_offset_and_not_a_drift() {
    for threads in [4usize, 16] {
        let mut deficits = Vec::new();
        for scale in [1u64, 2, 4] {
            let work = WORK * scale;
            let scenario = scenarios::share_gain(threads, work);
            let solo = scenario
                .process_index("solo")
                .expect("share_gain has a solo");
            let mut worst = 0;
            sweep(&scenario, SEEDS, |outcome| {
                worst =
                    worst.max(outcome.process_finish_ns[solo].expect("solo's work must complete"));
            });
            let deficit = worst - 2 * work;
            println!(
                "share_gain N={threads} work={work}: T_solo={worst} ns deficit={deficit} ns \
                 share={}permille",
                work * 1000 / worst,
            );
            deficits.push((work, worst, deficit));
        }
        let (_, _, first) = deficits[0];
        for &(work, finish, deficit) in &deficits[1..] {
            assert!(
                deficit <= first,
                "at {threads} rivals the deficit grew from {first} ns to {deficit} ns when the \
                 window went to {work} ns (T_solo {finish} ns). A deficit that scales with the \
                 work is not a granularity — it is a process permanently holding more of the \
                 machine than its share.",
            );
        }
        // And the corollary a reader actually wants: the share recovers toward
        // an even split as the window grows. Asserted, because "the deficit is
        // bounded" and "the split is fair over a long enough window" are the
        // same statement and only the second one is what a user feels.
        let share = |(work, finish, _): &(u64, u64, u64)| work * 1000 / finish;
        assert!(
            share(&deficits[2]) > share(&deficits[0]),
            "at {threads} rivals the share over a four-times-longer window is {}permille \
             against {}permille over the short one — it must climb, or the deficit is being \
             paid again",
            share(&deficits[2]),
            share(&deficits[0]),
        );
    }
}

/// The negative control for both cases above: the **rejected** policy, one job
/// per thread instead of one per process.
///
/// Without it, "a process cannot buy CPU by forking" is a number with nothing to
/// compare it to. Under a job per thread `swarm` is entitled to N times what
/// `solo` is, and the instrument has to see that — not as a slightly worse
/// deficit, but as a different machine. Measured: `solo` takes **1,020 ms** to
/// spend the same 60 ms of CPU, against 124 ms under the shipped policy and a
/// 132 ms derived bound, so the case above would red by 7.7×.
///
/// **Invariant I5's ceiling is lifted for this run, and only for it.** A job per
/// thread fails I5 within a few hundred microseconds — that verdict is
/// `scenarios::fair_job_per_thread`'s and is asserted next door — and the
/// explorer stops at the first violation, so `solo` would never finish and the
/// quantity this file measures could not be read at all. The lifted ceiling
/// suppresses the *verdict* and changes no scheduling decision:
/// `JobShape::PerThread` is still the whole of what differs from the control.
#[test]
fn a_job_per_thread_loses_the_floor() {
    const THREADS: usize = 16;
    let mut broken = scenarios::share_gain(THREADS, WORK).with_job(JobShape::PerThread);
    // Past anything this run can produce, so I5 records the spread and reports
    // no violation; `Vm::fair_over_bound` still counts every crossing.
    broken = broken.with_fair_allowance(u64::MAX);
    let solo = broken.process_index("solo").expect("share_gain has a solo");
    let bound = 2 * WORK + 2 * DISPATCH_NS;

    let mut broken_finish = 0;
    sweep(&broken, SEEDS, |outcome| {
        broken_finish =
            broken_finish.max(outcome.process_finish_ns[solo].expect("solo's work must complete"));
    });

    let shipped = scenarios::share_gain(THREADS, WORK);
    let mut shipped_finish = 0;
    sweep(&shipped, SEEDS, |outcome| {
        shipped_finish =
            shipped_finish.max(outcome.process_finish_ns[solo].expect("solo's work must complete"));
    });

    println!(
        "per-thread control at {THREADS} rivals: T_solo={broken_finish} ns against \
         {shipped_finish} ns shipped and a {bound} ns bound",
    );
    assert!(
        broken_finish > bound,
        "under one job per *thread* a single-threaded process against {THREADS} rival \
         threads finished its 60 ms of work at {broken_finish} ns — inside the {bound} ns the \
         case above allows. A policy that hands `swarm` sixteen times `solo`'s entitlement has \
         to break that bound, or the bound is not measuring the policy.",
    );
    assert!(
        shipped_finish * 2 < broken_finish,
        "the shipped policy finished at {shipped_finish} ns and the rejected one at \
         {broken_finish} ns. Two shares that close together mean this control is detecting \
         the workload rather than the policy.",
    );
}

/// **Mixed interactive and background**: how long a thread that sleeps, wakes and
/// runs briefly waits for a CPU held by threads that never yield.
///
/// The sleeper uses a quarter of a millisecond every three, so it is far under
/// its share and carries that back in with it: its group joins with no debt
/// and outweighs the hogs' group there, so its request is due first and it
/// takes the CPU once the running hog has had its minimum run.
///
/// **What it found**, 40 seeds per point: the worst wake is 5 ms at every hog
/// count from 1 to 64 and at both widths — at most the hog request running
/// ahead of it — and every wake but one per run at one CPU waits 2 ms at most,
/// the minimum run and the model's two chunks.
///
/// So the bound asserted on every wake is one request, and the sharper claim
/// is asserted at one CPU on the runner-up.
#[test]
fn an_interactive_wake_waits_out_at_most_one_request() {
    /// Enough runs that the distribution has a few hundred wakes in it; the
    /// device interrupt's position among the enabled steps is the freedom the
    /// seeds explore.
    const WAKE_SEEDS: u64 = 40;
    for (cpus, hogs) in [(1usize, 1usize), (1, 4), (1, 16), (1, 64), (2, 4), (2, 16)] {
        let scenario = scenarios::interactive_mix(cpus, hogs);
        let sleeper = scenario
            .process_index("sleeper")
            .expect("interactive_mix has a sleeper");
        let bound = DISPATCH_NS;

        let mut merged = Latency::default();
        let mut worst_runner_up = 0;
        sweep(&scenario, WAKE_SEEDS, |outcome| {
            let woken = outcome.wait(sleeper, ReadyCause::Woken);
            // Per *run*, because the claim is "every wake but one in a run" and
            // the union's second-largest across forty runs would be about all of
            // them together — see `Latency::merge`.
            worst_runner_up = worst_runner_up.max(woken.runner_up_ns());
            merged.merge(woken);
        });

        println!(
            "interactive cpus={cpus} hogs={hogs}: [{}] worst-run-2nd={worst_runner_up} ns \
             bound={bound} ns",
            merged.summary(),
        );
        assert!(
            merged.count() >= WAKE_SEEDS,
            "at {cpus} cpu(s) and {hogs} hog(s) the sleeper was woken {} time(s) over \
             {WAKE_SEEDS} runs — the distribution below is a measurement of nothing",
            merged.count(),
        );
        assert!(
            merged.max_ns() <= bound,
            "at {cpus} cpu(s) and {hogs} hog(s) an interactive wake waited {} ns against one \
             request, {bound} ns. Distribution: {}",
            merged.max_ns(),
            merged.summary(),
        );
        if cpus == 1 {
            let contract = MIN_SLICE_NS + 2 * RUN_CHUNK_NS;
            assert!(
                worst_runner_up <= contract,
                "at {hogs} hog(s) on one CPU, more than one wake per run waited out more \
                 than the minimum run: the second-worst wake of some run took \
                 {worst_runner_up} ns against {contract} ns. A wake due first is waiting for \
                 the running request instead of taking the CPU after the minimum run.",
            );
        }
    }
}

/// One kick and the pass it buys, as the model times it: [`IPI_LATENCY_NS`],
/// one chunk of grace for the pass a delivered kick owes, and the chunk that
/// carries the clock over it — the push case's own per-hop term.
const HOP_NS: u64 = IPI_LATENCY_NS + 2 * RUN_CHUNK_NS;

/// **A wake whose home CPU is busy runs on an idle sibling within two hops**:
/// the kick that makes the home CPU drain it, and the adopt that hands it to
/// the sibling.
///
/// `scenarios::busy_home` puts a sleeper and a hog of one process on one CPU
/// beside an idle one, with the balance path off — so the woken thread joins
/// the hog's group, no cut can serve it, and nothing but the forward can run it
/// before the hog's request ends.
///
/// **Only a run that forwarded is measured**, and most must. The model does
/// not oblige a CPU with nothing to do to take its first pass, so in some
/// schedules cpu1 has never published SLEEPING when the first wake lands —
/// there is no idle CPU to forward to, and the wake waits its turn, as it
/// should. With the balance path off the forward is the only thing that
/// migrates, so a run's migration count says which kind of run it was.
#[test]
fn a_wake_to_a_busy_cpu_beside_an_idle_one_runs_within_two_hops() {
    const BUSY_SEEDS: u64 = 40;
    let scenario = scenarios::busy_home();
    let pair = scenario.process_index("pair").expect("busy_home has its pair");
    let bound = 2 * HOP_NS;
    let mut merged = Latency::default();
    let mut forwarded = 0;
    sweep(&scenario, BUSY_SEEDS, |outcome| {
        if outcome.migrations == 0 {
            return;
        }
        forwarded += 1;
        merged.merge(outcome.wait(pair, ReadyCause::Woken));
    });
    println!("busy home: {forwarded}/{BUSY_SEEDS} runs forwarded: [{}] bound={bound} ns", merged.summary());
    assert!(
        forwarded * 2 >= BUSY_SEEDS,
        "only {forwarded} of {BUSY_SEEDS} runs handed the sleeper to the idle CPU — the \
         forward is not what serves a wake whose home is busy",
    );
    assert!(
        merged.count() >= forwarded,
        "{} wake(s) over {forwarded} runs — a measurement of nothing",
        merged.count(),
    );
    assert!(
        merged.max_ns() <= bound,
        "a wake waited {} ns against two hops, {bound} ns: the busy home's kick and the \
         idle CPU's adopt. Past this the wake is waiting for the hog's request. \
         Distribution: {}",
        merged.max_ns(),
        merged.summary(),
    );
}

/// **A pair that takes turns on one CPU stays there**: a wake whose poster
/// parks in the pass that drains it runs where it was posted, so an idle CPU
/// beside the pair is never handed a turn.
///
/// `scenarios::turn_taking_pair` starts both threads on cpu0 with the balance
/// path off, so the forward is the only thing that could move either.
#[test]
fn a_turn_taking_pair_is_never_forwarded_off_its_cpu() {
    const SEEDS: u64 = 40;
    let scenario = scenarios::turn_taking_pair();
    sweep(&scenario, SEEDS, |outcome| {
        assert_eq!(outcome.migrations, 0, "a turn was handed to the idle CPU: {}", outcome.report());
    });
}

/// **A pair split across LLCs is brought into one**: on
/// `scenarios::split_pair`, with balancing off, the one move is a wake
/// following its waker's LLC, and after it every wake stays inside that LLC.
#[test]
fn a_split_pair_moves_once_into_one_llc() {
    sweep(&scenarios::split_pair(), 40, |outcome| {
        assert_eq!(outcome.migrations, 1, "the pair did not settle into one LLC with one move: {}", outcome.report());
    });
}

/// **A server woken from two LLCs in turn mostly stays put**: on
/// `scenarios::server_of_two_llcs` the server moves only when two consecutive
/// wakes name the same other LLC, which the order the clients' requests
/// land in makes rare. Over 40 seeds it averages at most one move in a run
/// of 24 wakes (measured: 36 moves in all, at most 3 in one run). Following
/// every wake's LLC moves it about 220 times over the same seeds.
#[test]
fn a_server_woken_from_two_llcs_in_turn_does_not_follow_them() {
    const SEEDS: u64 = 40;
    let mut moves = 0;
    sweep(&scenarios::server_of_two_llcs(), SEEDS, |outcome| moves += outcome.migrations);
    assert!(moves <= SEEDS, "the server followed its clients' LLCs: {moves} moves over {SEEDS} runs");
}

/// **A wake due ahead of the running task waits out [`MIN_SLICE_NS`] and a
/// hop, not a request** — on one CPU, against one rival process, every wake
/// but one per run.
///
/// `interactive_mix(1, 1)`'s sleeper runs a quarter of a millisecond every
/// three, so its request is due first at nearly every wake. The exception is
/// the start, where the sleeper's first wake can find the hog's group still
/// ahead; the per-run runner-up is the claim about every other, exactly as the
/// case above reads it.
#[test]
fn a_wake_due_first_waits_out_the_minimum_run_and_no_more() {
    const OWED_SEEDS: u64 = 40;
    let scenario = scenarios::interactive_mix(1, 1);
    let sleeper = scenario.process_index("sleeper").expect("interactive_mix has a sleeper");
    let bound = MIN_SLICE_NS + HOP_NS;
    let mut merged = Latency::default();
    let mut worst_runner_up = 0;
    sweep(&scenario, OWED_SEEDS, |outcome| {
        let woken = outcome.wait(sleeper, ReadyCause::Woken);
        worst_runner_up = worst_runner_up.max(woken.runner_up_ns());
        merged.merge(woken);
    });
    println!(
        "owed wake: [{}] worst-run-2nd={worst_runner_up} ns bound={bound} ns",
        merged.summary(),
    );
    assert!(
        merged.count() >= OWED_SEEDS,
        "{} wake(s) over {OWED_SEEDS} runs — a measurement of nothing",
        merged.count(),
    );
    assert!(
        worst_runner_up <= bound,
        "a second wake in one run waited {worst_runner_up} ns against {bound} ns — the \
         minimum run and one hop. Past this a wake due first is waiting for the running \
         task's request.",
    );
}

/// **A thread that naps does not preempt the sibling that spins**: two threads
/// of one job are one group, and inside a group the CPU decides only at a
/// request's end.
///
/// `scenarios::sleep_spin_siblings` puts them on one CPU. The spinner runs a
/// whole request per turn and the napper one burst between turns, so the
/// spinner's [`scenarios::SPINNER_WORK`] is done within
/// `SPINNER_WORK / SLICE` turns of `SLICE + burst + 2 × RUN_CHUNK`. A napper
/// whose every wake cut the spinner's request would hold it to
/// [`MIN_SLICE_NS`] a burst, and double the time it takes. Measured: 148 ms
/// against 175.
#[test]
fn a_sibling_that_naps_does_not_preempt_the_one_that_spins() {
    let scenario = scenarios::sleep_spin_siblings();
    let spinner = kernel::sched::task::TaskKey(1);
    let turns = scenarios::SPINNER_WORK.div_ceil(SLICE_NS);
    let bound = turns * (SLICE_NS + scenarios::NAP_BURST_NS + 2 * RUN_CHUNK_NS);
    let mut worst = 0;
    sweep(&scenario, SEEDS, |outcome| {
        let finish = *outcome
            .thread_finish_ns
            .get(&spinner)
            .expect("the spinner's work completes inside the run");
        worst = worst.max(finish);
    });
    println!("sleep-spin siblings: spinner done at {worst} ns, bound {bound} ns");
    assert!(
        worst <= bound,
        "the spinner took {worst} ns for {} ns of work against {bound} ns — its napping \
         sibling is taking the CPU off it inside its requests",
        scenarios::SPINNER_WORK,
    );
}

/// **Wakeup storms and the balance path**: many waiters made runnable at once,
/// over and over, on machines from one CPU to eight.
///
/// The drain — how long the last waiter waits for a CPU — is what a storm costs,
/// and the failure everyone fears is that it is *serialized*: a post claims
/// every waiter in one loop, and each claim posts a `Msg::Wake` to that waiter's
/// home CPU.
///
/// **No pathological serialization** is the claim: quadrupling the storm on one
/// CPU may cost four times the drain, linear in the waiters a single run queue
/// holds, and every drain stays inside its derived bound. The balance path has
/// to have moved something at every width above one, which is what makes this a
/// statement about a machine that was rebalancing under the load rather than
/// about independent queues.
#[test]
fn a_wakeup_storm_drains_without_serializing() {
    const STORM_SEEDS: u64 = 20;
    let mut drains = Vec::new();
    for (cpus, waiters) in [
        (1usize, 16usize),
        (2, 16),
        (4, 16),
        (1, 64),
        (4, 64),
        (8, 64),
    ] {
        let scenario = scenarios::wakeup_storm(cpus, waiters);
        let index = scenario
            .process_index("waiters")
            .expect("wakeup_storm has waiters");
        let per_queue = waiters.div_ceil(cpus) as u64;
        let bound = (per_queue + 1) * DISPATCH_NS;

        let mut merged = Latency::default();
        let mut migrations = 0;
        sweep(&scenario, STORM_SEEDS, |outcome| {
            merged.merge(outcome.wait(index, ReadyCause::Woken));
            migrations = migrations.max(outcome.migrations);
        });

        println!(
            "storm cpus={cpus} waiters={waiters}: [{}] migrations={migrations} bound={bound} ns",
            merged.summary(),
        );
        assert!(
            merged.count() >= waiters as u64 * STORM_SEEDS,
            "a storm of {waiters} waiters over {STORM_SEEDS} runs produced only {} wake \
             latencies — most of the storm is not reaching the instrument",
            merged.count(),
        );
        assert!(
            merged.max_ns() <= bound,
            "at {cpus} cpu(s) the last of {waiters} woken waiters waited {} ns against a \
             derived {bound} ns — {per_queue} waiter(s) per run queue plus the leader, at \
             {DISPATCH_NS} ns a dispatch. Distribution: {}",
            merged.max_ns(),
            merged.summary(),
        );
        if cpus > 1 {
            assert!(
                migrations > 0,
                "at {cpus} cpus nothing was ever migrated, so this drain says nothing about \
                 a machine under a balance path — it is {cpus} independent queues",
            );
        }
        drains.push((cpus, waiters, merged.max_ns()));
    }

    // Linear in the waiters one queue holds, not quadratic in the storm. Both
    // points are at one CPU, so the whole storm lands in one run queue and the
    // comparison is about the drain and not about placement.
    let one_cpu = |waiters: usize| {
        drains
            .iter()
            .find(|&&(cpus, count, _)| cpus == 1 && count == waiters)
            .map(|&(_, _, drain)| drain)
            .expect("both one-CPU points were swept")
    };
    let (small, large) = (one_cpu(16), one_cpu(64));
    assert!(
        large <= small * 5,
        "quadrupling the storm from 16 to 64 waiters on one CPU took the drain from {small} \
         ns to {large} ns. Four times the waiters may cost four times the drain — that is what \
         one run queue serving them one at a time *is* — and this allows a quarter more on top; \
         past it the cost is growing faster than the queue and something outside the run queue \
         is serializing the storm.",
    );
    println!("storm drain 16→64 waiters on one cpu: {small} ns → {large} ns");
}

/// Seeds per point for the two adversarial-placement cases.
const LOPSIDED_SEEDS: u64 = 20;

/// The widths the adversarial machine is measured at: CPUs, and threads all
/// spawned onto cpu0 of them.
const LOPSIDED: [(usize, usize); 4] = [(2, 8), (4, 16), (4, 64), (8, 64)];

/// How far the balance path can drain the CPU an adversary loaded, derived.
///
/// Both halves of the pull path stop at the same inequality:
/// `answer_steal_requests` hands over only a stealable task and
/// `post_steal_probe` will not probe a victim publishing no queued one — and
/// both are sampled after the pick, so they exclude the task the CPU is about
/// to run. So the floor a loaded CPU can be drained to is the one task
/// it runs, and everything above that floor is stealable, one task per
/// answered probe.
///
fn stealable(threads: usize) -> u64 {
    threads as u64 - 1
}

/// **Every move a lopsided run makes has a cause, and none is a bounce.**
///
/// Before any thread exits, the counts only change by moves. cpu0 gives up
/// [`even_drain`] tasks net, and each other CPU sources at most two moves of
/// its own:
/// - one at start-up, when an idle CPU's nearest neighbour holding surplus is
///   one busy balancing has just given a second thread, rather than cpu0;
/// - one at the end of the drain, when cpu0 answers a burst of requests posted
///   in the same instant, drops below a CPU that took threads earlier, and that
///   CPU hands one on.
///
/// After the first exit, a move needs a pair whose weights differ by more than
/// one thread (`pop_surplus`'s quarter-thread margin), and only an exit opens
/// one, so the moves are at most one per exit. Measured over 20 seeds: at most
/// even + 8 before an exit (8 CPUs, 64 threads) and 53 moves after 64 exits
/// (4 CPUs, 64 threads). A pair that bounces a thread with no exit between
/// exceeds one bound or the other.
fn assert_moves_have_causes(outcome: &Outcome, cpus: usize, threads: usize, label: &str) {
    let before = even_drain(threads, cpus) + 2 * (cpus as u64 - 1);
    assert!(
        outcome.migrations_before_exit <= before,
        "{label} at {cpus} cpus and {threads} threads: {} moves before any thread exited, past \
         the {before} that an even split, one start-up chain and one end-of-drain correction \
         per other CPU account for",
        outcome.migrations_before_exit,
    );
    let after = outcome.migrations - outcome.migrations_before_exit;
    assert!(
        after <= outcome.exits,
        "{label} at {cpus} cpus and {threads} threads: {after} moves after the first exit, \
         against {} exits that could each open one imbalance",
        outcome.exits,
    );
}

/// The fewest migrations that leave cpu0 holding no more than an even split of
/// the job, which every CPU's threads weigh the same in.
fn even_drain(threads: usize, cpus: usize) -> u64 {
    (threads - threads.div_ceil(cpus)) as u64
}

/// **Adversarial placement**: every thread of a saturating workload spawned onto
/// one CPU, and what the balance path does with the machine that leaves.
///
/// Spawn placement is least-loaded-with-rotation, so this state arises from no
/// workload at all — which is why the simulator grew a placement knob for it
/// (`workload::PlacementShape`), and why until it did, the only balance path
/// anything had measured was the handful of probes a wakeup storm happens to
/// provoke. What this stages is the machine the steal request's pull half exists
/// for: every runnable thread on cpu0, every other CPU with nothing, and no
/// wake, no block and no second placement anywhere in the run.
///
/// Run under the scenario default, which is the shipped policy —
/// [`Balance::PushOnSurplus`] since the owner's 2026-08-23 decision.
///
/// **Two findings.** Where the balance path runs at all it drains the loaded
/// CPU to an even split: idle CPUs each take a thread, and then busy CPUs ask
/// for threads by their summed group weight, which for one job is their thread
/// count, until no move narrows it. The best seed at every width moves at least
/// [`even_drain`] tasks.
///
/// And the pull alone **only ever reaches the CPUs that are still awake.** The
/// probe is posted from an idle pass, one per idle trip, and only against a
/// victim that has already *published* a surplus; a CPU whose idle pass ran
/// before the loaded one published halted with no probe outstanding, and
/// without the push nothing in this protocol woke it. Under the shipped push every width recovers in
/// full (20/20, the tables in
/// [`a_push_on_surplus_reaches_every_cpu_the_pull_path_left_asleep`]), and the
/// assertion below holds the shipped default to it: dropping the producer-side
/// poke reds it at the first width (verified 2026-08-23, 9 against 20). The
/// idler's re-read behind the fence is *not* held here — this model runs a
/// pass atomically, so nothing can publish a surplus inside the window that
/// re-read closes — it is held by `loom/tests/loom_push.rs`, whose
/// `push-fence-relaxed` control reds.
#[test]
fn the_balance_path_drains_the_cpu_an_adversary_loaded() {
    let mut table = Vec::new();
    for (cpus, threads) in LOPSIDED {
        let scenario = scenarios::lopsided_placement(cpus, threads, WORK);
        let floor = stealable(threads);
        let (mut best, mut widest, mut full, mut reached) = (0, 0, 0, 0);

        sweep(&scenario, LOPSIDED_SEEDS, |outcome| {
            let ran = outcome.first_exec_ns.iter().filter(|at| at.is_some()).count();
            // **Every CPU beyond cpu0 that ran was handed its first task by the
            // balance path**, because nothing else in this workload can put one
            // there: the placement is `AllOn(0)`, nothing blocks and nothing
            // wakes. An answered probe hands over exactly one task, so a machine
            // in which `ran` CPUs did work owes at least `ran - 1` migrations.
            assert!(
                outcome.migrations + 1 >= ran as u64,
                "at {cpus} cpus {ran} cpu(s) executed a step on a machine whose every thread \
                 was spawned onto cpu0, and the balance path moved only {} task(s) — one of \
                 those CPUs was given work by something that is not the balance path",
                outcome.migrations,
            );
            assert_moves_have_causes(outcome, cpus, threads, "the shipped policy");
            best = best.max(outcome.migrations);
            widest = widest.max(ran);
            full += usize::from(ran == cpus);
            reached += usize::from(ran > 1);
        });

        println!(
            "lopsided cpus={cpus} threads={threads}: {full}/{LOPSIDED_SEEDS} seeds reached \
             every cpu, {reached}/{LOPSIDED_SEEDS} reached a second, widest={widest}, best \
             seed moved {best} with {floor} stealable",
        );
        // **The drain, asserted where it is a law.** The best seed is the one in
        // which a thief was awake for the whole run, and there the balance path
        // has to bring cpu0 down to an even split.
        let even = even_drain(threads, cpus);
        assert!(
            best >= even,
            "at {cpus} cpus and {threads} threads on cpu0, the best of {LOPSIDED_SEEDS} \
             schedules moved {best} tasks, and leaving cpu0 an even split takes {even}: the \
             balance path gave up while cpu0 still weighed more than its peers",
        );
        // **The shipped default reaches every CPU in every schedule.** This is
        // the push's whole reason to ship: under plain `Balance::Pull` a CPU
        // that halted before cpu0 published its surplus was never probed again.
        // cpu0 publishes threads − 1 at its first pass and pushes to one
        // sleeping CPU per pass, cursor-walking them, so every sleeper is rung.
        // Dropping the producer-side poke reds here (verified 2026-08-23); the
        // idler's re-read behind `balance_fence` is loom's to hold, because
        // this model cannot interleave inside a pass.
        assert_eq!(
            full, LOPSIDED_SEEDS as usize,
            "at {cpus} cpus and {threads} threads on cpu0, {} of {LOPSIDED_SEEDS} schedules \
             under the shipped balance policy left at least one CPU asleep beside a published \
             surplus — the push half is not reaching every sleeper",
            LOPSIDED_SEEDS as usize - full,
        );
        table.push((cpus, threads, full, reached, best, floor));
    }

    for &(cpus, threads, full, reached, best, floor) in &table {
        println!(
            "lopsided cpus={cpus} threads={threads}: every-cpu {full}/{LOPSIDED_SEEDS}, \
             second-cpu {reached}/{LOPSIDED_SEEDS}, moved {best} with {floor} stealable",
        );
    }
}

/// The negative control for the case above: the same lopsided machine with the
/// pull half of the balance path switched off.
///
/// Without it "the balance path drains the CPU an adversary loaded" is a number
/// with nothing to compare it to — the simulator would report the same drain if
/// the tasks had been placed by the shipped policy in the first place, and the
/// case above would be measuring the placement knob rather than the balance
/// path. With `Env::steal` off nothing can move a task off cpu0 at all.
///
/// **Measured**: at every width the drain is 0 against a floor of 6, 14, 62 and
/// 62, and exactly one CPU of the machine ever executes a step — so the
/// assertion next door reds on its first width.
#[test]
fn without_the_balance_path_a_lopsided_machine_stays_lopsided() {
    for (cpus, threads) in LOPSIDED {
        let scenario =
            scenarios::lopsided_placement(cpus, threads, WORK).with_balance(Balance::None);
        let floor = stealable(threads);
        let (mut most, mut widest) = (0, 0);

        sweep(&scenario, LOPSIDED_SEEDS, |outcome| {
            most = most.max(outcome.migrations);
            widest = widest.max(outcome.first_exec_ns.iter().filter(|at| at.is_some()).count());
        });

        println!(
            "lopsided-control cpus={cpus} threads={threads}: {most} migrations against a \
             {floor} floor, {widest} of {cpus} cpus ran",
        );
        assert_eq!(
            most, 0,
            "with `Env::steal` off the balance path still moved {most} task(s) at {cpus} cpus \
             — nothing else in this protocol migrates, so this is not a control",
        );
        assert_eq!(
            widest, 1,
            "with the balance path off, {widest} of {cpus} cpus executed a step on a machine \
             whose every thread was spawned onto cpu0 — the placement knob is not staging the \
             machine the case above is about",
        );
    }
}

/// How long a cure waits before probing again, and how many times.
///
/// One quantum, because a loaded CPU reaches a pass at least that often — at
/// each fair request's end, or a real-time turn's — so a thief that re-probes
/// faster than that is asking a question whose answer may not have changed. Four, because the re-arm has to be **bounded** — `times × every_ns`
/// is the whole window a cure of this shape can see a surplus in, and past it
/// the CPU halts for good rather than ticking for ever on a machine with nothing
/// to run.
const REARM_EVERY_NS: u64 = QUANTUM_NS;
const REARM_TIMES: u32 = 4;

/// The surplus a push fires at: `SchedPass::post_steal_probe`'s own inequality,
/// so a push never wakes a CPU whose victim would refuse it. The core's
/// constant, which is also the kernel's shipped `threshold`.
const PUSH_THRESHOLD: u32 = kernel::sched::cpu::PUSH_THRESHOLD;

/// The four policies the two cure cases and the cost table are read across.
fn policies() -> [(&'static str, Balance); 4] {
    [
        ("pull", Balance::Pull),
        (
            "re-arm ×1",
            Balance::PullWithRearm {
                every_ns: REARM_EVERY_NS,
                times: 1,
            },
        ),
        (
            "re-arm ×4",
            Balance::PullWithRearm {
                every_ns: REARM_EVERY_NS,
                times: REARM_TIMES,
            },
        ),
        (
            "push (ships)",
            Balance::PushOnSurplus {
                threshold: PUSH_THRESHOLD,
            },
        ),
    ]
}

/// What one policy did with one lopsided machine, over a whole sweep.
#[derive(Default)]
struct Recovery {
    /// Seeds in which every CPU of the machine executed a step.
    full: usize,
    /// Seeds in which more than cpu0 did.
    second: usize,
    /// The most tasks the balance path moved in any seed.
    best_migrations: u64,
    /// The worst [`Outcome::probe_gap_ns`] any seed produced.
    gap_ns: u64,
    /// The worst and best `machine_working_at_ns` over the seeds that reached
    /// every CPU; `None` if no seed did.
    working_worst: Option<u64>,
    working_best: Option<u64>,
    /// Idle wakes summed over the sweep, and the worst single run's rate.
    wakes: u64,
    worst_rate: f64,
}

/// Wakes of a halted CPU that found nothing to do, per second of simulated time
/// — the unit a balance policy's cost is quoted in.
fn wake_rate(outcome: &Outcome) -> f64 {
    if outcome.elapsed == 0 {
        return 0.0;
    }
    outcome.idle_wakes_total() as f64 * 1e9 / outcome.elapsed as f64
}

/// Run the lopsided machine under one policy and fold what it did.
///
/// The two ceilings the shipped path already obeys are asserted here rather than
/// in each case, because every policy below is built on the same balance path:
/// no CPU is given work by anything but the balance path, and the migrations
/// each have a cause ([`assert_moves_have_causes`]).
fn recover(cpus: usize, threads: usize, balance: Balance) -> Recovery {
    let scenario = scenarios::lopsided_placement(cpus, threads, WORK).with_balance(balance);
    let mut r = Recovery::default();
    sweep(&scenario, LOPSIDED_SEEDS, |outcome| {
        let ran = outcome.cpus_reached();
        assert!(
            outcome.migrations + 1 >= ran as u64,
            "{balance:?} at {cpus} cpus: {ran} cpu(s) executed a step on a machine whose every \
             thread was spawned onto cpu0, and the balance path moved only {} task(s) — one of \
             those CPUs was given work by something that is not the balance path",
            outcome.migrations,
        );
        assert_moves_have_causes(outcome, cpus, threads, &format!("{balance:?}"));
        r.full += usize::from(ran == cpus);
        r.second += usize::from(ran > 1);
        r.best_migrations = r.best_migrations.max(outcome.migrations);
        r.gap_ns = r.gap_ns.max(outcome.probe_gap_ns);
        r.wakes += outcome.idle_wakes_total();
        r.worst_rate = r.worst_rate.max(wake_rate(outcome));
        if let Some(at) = outcome.machine_working_at_ns() {
            r.working_worst = Some(r.working_worst.unwrap_or(0).max(at));
            r.working_best = Some(r.working_best.unwrap_or(u64::MAX).min(at));
        }
    });
    r
}

/// The condition under which "every CPU is reached" is a statement about the
/// balance path rather than about arithmetic: cpu0 can be drained to
/// [`stealable`] and every other CPU needs one task, so there has to be at least
/// one task per starved CPU above the floor.
fn enough_to_go_round(cpus: usize, threads: usize) {
    assert!(
        stealable(threads) >= cpus as u64 - 1,
        "at {cpus} cpus and {threads} threads only {} task(s) stand above cpu0's surplus floor \
         and {} CPU(s) need one each — no balance policy can reach every CPU here, so a case \
         asserting that it does would be asserting arithmetic",
        stealable(threads),
        cpus - 1,
    );
}

/// **Cure one, measured: a bounded re-arm of the probe.** The one the owner
/// declined (2026-08-23) in favour of the push below.
///
/// Of the two ways out of the one-shot probe, this is the one that needs no
/// observation of anything: a CPU that halts with nothing to run programs its
/// one-shot timer [`REARM_EVERY_NS`] ahead and probes again when it fires, up to
/// [`REARM_TIMES`] times per idle period. Nothing has to notice it, nothing has
/// to publish anything, and the timer fires whether or not a surplus ever
/// appeared — which is both why it works and what it costs.
///
/// **The derivation, and it is the assertion.** A CPU that halted at `H` is
/// woken at `H + every_ns`: the model's step relation forbids an execution step
/// anywhere while a CPU owes a timer delivery, so the clock cannot run past the
/// armed instant unpunished. Three [`RUN_CHUNK_NS`] chunks of granularity sit on
/// top, and each is a step the model permits before the probe is posted — the
/// execution step that carries the clock over the armed instant, the one chunk
/// of grace `Vm::enabled` gives a CPU that owes a rescheduling pass, and the
/// execution step that carries the clock over *that*. So no CPU may sit halted
/// beside a published surplus with no probe outstanding for longer than
/// `every_ns + 3 × RUN_CHUNK` — [`Outcome::probe_gap_ns`] is that quantity, and
/// the surplus in this workload is published at clock 0, before any CPU can have
/// halted, so one re-arm is inside the window at every width.
///
/// The derived bound is 13,000,000 ns and the measurement is 10,000,000 ns at
/// every width — 0.77 of it, which is where the rest of this file's numbers sit.
/// It is *exactly* `every_ns`, because this workload's clock is a multiple of
/// `RUN_CHUNK_NS` throughout and none of the three granularity chunks is ever
/// spent; they stay in the bound because the model permits them, not because a
/// schedule was found that needs them. `Balance::Pull`'s own gap is the whole
/// run at every width, because nothing in that protocol can close it.
///
/// **One re-arm is enough here**: the ×1 and ×4 columns are the same recovery
/// for two and a half times the wakes. What this workload needs is *a* second
/// look, not a periodic one — and `times` is what decides how much of the second
/// kind is bought with it.
///
/// **The negative control.** With the cure off and everything else identical
/// (`Balance::Pull` substituted for the policy under test) this case reds on its
/// first width, at the recovery assertion, and with that assertion elided at the
/// derived timing one behind it.
///
/// A CPU's first execution step is reported and **not** asserted: the model does
/// not oblige a CPU that has a task loaded to take one, so `working_at` measures
/// the explorer's freedom as much as the protocol's recovery. That is the same
/// limit the case above states about makespan, and it is why recovery is
/// counted in CPUs and timed in probe gaps.
#[test]
fn a_bounded_re_arm_reaches_every_cpu_the_pull_path_left_asleep() {
    let bound = REARM_EVERY_NS + 3 * RUN_CHUNK_NS;
    let mut table = Vec::new();
    for (cpus, threads) in LOPSIDED {
        enough_to_go_round(cpus, threads);
        let floor = stealable(threads);
        let control = recover(cpus, threads, Balance::Pull);
        for times in [1, REARM_TIMES] {
            let balance = Balance::PullWithRearm {
                every_ns: REARM_EVERY_NS,
                times,
            };
            let cured = recover(cpus, threads, balance);
            println!(
                "re-arm ×{times} cpus={cpus} threads={threads}: every-cpu \
                 {}/{LOPSIDED_SEEDS} (pull {}/{LOPSIDED_SEEDS}), gap={} ns (pull {} ns, bound \
                 {bound} ns), drained {}/{floor}, working_at {:?}..{:?} ns, {} idle wakes over \
                 the sweep, worst run {:.2}/s",
                cured.full,
                control.full,
                cured.gap_ns,
                control.gap_ns,
                cured.best_migrations,
                cured.working_best,
                cured.working_worst,
                cured.wakes,
                cured.worst_rate,
            );

            // **The law.** A cure that re-probes every `every_ns` cannot leave a
            // CPU asleep beside a surplus that was published before it halted,
            // and in this workload every surplus is.
            assert_eq!(
                cured.full, LOPSIDED_SEEDS as usize,
                "with a re-arm every {REARM_EVERY_NS} ns, {} of {LOPSIDED_SEEDS} schedules left \
                 a CPU of {cpus} asleep on a machine whose whole workload sits on cpu0. cpu0 \
                 publishes its surplus at clock 0 — before any CPU can have halted, since no \
                 execution step is enabled until it has taken its first pass — so every halt is \
                 inside the first re-arm's window and every CPU must be probed.",
                LOPSIDED_SEEDS as usize - cured.full,
            );
            assert!(
                cured.gap_ns <= bound,
                "a CPU sat halted beside a published surplus with no probe outstanding for {} \
                 ns, against a derived {bound} ns — one re-arm period at {REARM_EVERY_NS} ns \
                 plus three {RUN_CHUNK_NS} ns chunks of the model's own granularity. Past this \
                 the timer is not what is waking the CPU.",
                cured.gap_ns,
            );
            // And the drain is the shipped one: the re-arm changes when a probe
            // is posted and nothing about what answering one hands over, and
            // `recover` holds every run to the ceiling.
            let even = even_drain(threads, cpus);
            assert!(
                cured.best_migrations >= even,
                "the best of {LOPSIDED_SEEDS} schedules moved {} tasks, and leaving cpu0 an \
                 even split takes {even}",
                cured.best_migrations,
            );
            table.push((cpus, threads, times, cured, control.full, control.gap_ns));
        }
    }

    // The negative control, stated where the comparison is: with the cure off,
    // the same assertion has to fail. It does — at eight CPUs not one of the
    // twenty schedules reaches every CPU.
    let (_, _, _, _, worst_control_full, _) = table
        .iter()
        .map(|&(c, t, times, ref cured, full, gap)| (c, t, times, cured.full, full, gap))
        .min_by_key(|&(_, _, _, _, full, _)| full)
        .expect("the sweep ran");
    assert!(
        worst_control_full < LOPSIDED_SEEDS as usize,
        "`Balance::Pull` reached every CPU in every one of {LOPSIDED_SEEDS} schedules at every \
         width — then the case above is not measuring a cure, because there is nothing left to \
         cure",
    );
}

/// **Cure two, measured: a push on surplus.**
///
/// The other way out of the one-shot probe, and the one that costs almost
/// nothing: a pass that publishes a surplus of [`PUSH_THRESHOLD`] or more rings
/// the doorbell of one CPU that reads SLEEPING. No task moves on that ring — the
/// woken CPU runs its own idle pass and posts an ordinary probe — so the push
/// adds no second way to migrate anything, only a way to make the pull half run
/// on a CPU that had stopped asking.
///
/// **The derivation, and it is the assertion.** A pass pushes to one CPU, and
/// `SchedPass::push_on_surplus`'s cursor makes consecutive pushes walk the
/// machine, so `k` sleeping CPUs are reached in `k` passes of the CPU holding the
/// surplus. That CPU is running a task, so it reaches a pass at each request's
/// end: `k × DISPATCH_NS`. The kick is an IPI ([`IPI_LATENCY_NS`], which the
/// model enforces by disabling every execution step while a delivery is overdue)
/// and the pass it asks for is one chunk of grace plus the chunk that carries the
/// clock over it. Here `k` is `cpus − 1`, every CPU but the loaded one, so
///
/// ```text
/// (cpus − 1) × DISPATCH_NS + IPI_LATENCY_NS + 2 × RUN_CHUNK_NS
/// ```
///
/// **The cursor is load-bearing.** Without it every push goes to the
/// lowest-numbered sleeper, which posts its probe and halts again with SLEEPING
/// still set — so the next pass re-pokes the CPU that is already coming, and the
/// one behind it waits for the first one's probe to be *answered*: two passes
/// per sleeper.
///
/// **The negative control**, `Balance::Pull` substituted for the policy under
/// test and nothing else changed, reds at the recovery assertion first and at
/// the derived timing one behind it.
#[test]
fn a_push_on_surplus_reaches_every_cpu_the_pull_path_left_asleep() {
    let balance = Balance::PushOnSurplus {
        threshold: PUSH_THRESHOLD,
    };
    let mut reached_all = 0;
    for (cpus, threads) in LOPSIDED {
        enough_to_go_round(cpus, threads);
        let floor = stealable(threads);
        let sleepers = cpus as u64 - 1;
        let bound = sleepers * DISPATCH_NS + IPI_LATENCY_NS + 2 * RUN_CHUNK_NS;
        let control = recover(cpus, threads, Balance::Pull);
        let cured = recover(cpus, threads, balance);

        println!(
            "push ≥{PUSH_THRESHOLD} cpus={cpus} threads={threads}: every-cpu \
             {}/{LOPSIDED_SEEDS} (pull {}/{LOPSIDED_SEEDS}), gap={} ns (pull {} ns, bound \
             {bound} ns), drained {}/{floor}, working_at {:?}..{:?} ns, {} idle wakes over the \
             sweep, worst run {:.2}/s",
            cured.full,
            control.full,
            cured.gap_ns,
            control.gap_ns,
            cured.best_migrations,
            cured.working_best,
            cured.working_worst,
            cured.wakes,
            cured.worst_rate,
        );

        assert_eq!(
            cured.full, LOPSIDED_SEEDS as usize,
            "with a push at a surplus of {PUSH_THRESHOLD}, {} of {LOPSIDED_SEEDS} schedules \
             left a CPU of {cpus} asleep on a machine whose whole workload sits on cpu0. cpu0 \
             publishes a surplus of {} at clock 0 and pushes to one sleeping CPU per pass, \
             walking them in turn, so every one of them is rung inside {sleepers} passes.",
            LOPSIDED_SEEDS as usize - cured.full,
            threads - 1,
        );
        assert!(
            cured.gap_ns <= bound,
            "a CPU sat halted beside a published surplus with no probe outstanding for {} ns, \
             against a derived {bound} ns — {sleepers} sleeping CPU(s) at one push per pass and \
             {DISPATCH_NS} ns a pass, plus {IPI_LATENCY_NS} ns of IPI latency and two \
             {RUN_CHUNK_NS} ns chunks for the pass the kick asks for. Past this the push is \
             reaching the same CPU twice instead of walking the machine.",
            cured.gap_ns,
        );
        let even = even_drain(threads, cpus);
        assert!(
            cured.best_migrations >= even,
            "the best of {LOPSIDED_SEEDS} schedules moved {} tasks, and leaving cpu0 an even \
             split takes {even} — the push changes which CPU asks and nothing about what \
             answering hands over",
            cured.best_migrations,
        );
        reached_all += usize::from(control.full == LOPSIDED_SEEDS as usize);
    }
    assert!(
        reached_all < LOPSIDED.len(),
        "`Balance::Pull` reached every CPU in every schedule at every width — then the case \
         above is not measuring a cure",
    );
}

/// **What the two cures cost the idle path**, on the workloads a desktop
/// actually runs.
///
/// `kernel/CLAUDE.md` makes anything added to the idle loop an audio change, and
/// both cures add exactly one kind of thing: a wake of a CPU that had halted.
/// [`Outcome::idle_wakes`] counts them — a wake whose first pass reaches the idle
/// disposition again — so the count is a property of the run rather than of the
/// policy, and `Balance::Pull`'s own figure is the baseline the others are read
/// against. What it holds is late kicks alone ([`Outcome::late_kick_wakes`]), a
/// cost of kicking busy CPUs that every policy pays alike, which is what makes
/// the comparison one.
///
/// **`audio_pipeline` is the decision.** It is four threads on four CPUs, so the
/// push has next to nothing to announce and costs the idle path nothing over
/// the pull, while the re-arm ticks every idle CPU regardless, because a timer
/// cannot ask whether there is anything to come for. That is the difference
/// between the two cures stated in the unit the owner has to decide in, on the
/// workload the owner cares about. Under a wakeup storm the qualification shows:
/// a storm is a machine that keeps producing surplus beside CPUs that keep going
/// idle, so the observation the push rests on keeps coming back true, and the
/// push is cheap where nothing is queued and dear where a great deal is.
///
/// **What a wake costs the CPU it wakes is the kernel's own measurement to
/// make**: the model charges a scheduler pass zero nanoseconds
/// (`SimHwState::pass_cost_ns`), so it can *count* an extra wake and cannot
/// bill one.
#[test]
fn the_two_cures_are_priced_against_the_pull_path() {
    const COST_SEEDS: u64 = 20;
    let points: [(&str, usize, usize, Scenario); 6] = [
        ("interactive_mix(2,4)", 2, 4, scenarios::interactive_mix(2, 4)),
        ("interactive_mix(2,16)", 2, 16, scenarios::interactive_mix(2, 16)),
        ("interactive_mix(4,16)", 4, 16, scenarios::interactive_mix(4, 16)),
        ("wakeup_storm(4,16)", 4, 16, scenarios::wakeup_storm(4, 16)),
        ("wakeup_storm(8,64)", 8, 64, scenarios::wakeup_storm(8, 64)),
        ("audio_pipeline(4)", 4, 0, scenarios::audio_pipeline(4)),
    ];
    for (label, cpus, hogs, scenario) in points {
        // Whose wake latency this workload is about: the interactive thread, the
        // storm's waiters, or the audio clients.
        let watched = scenario
            .procs
            .iter()
            .position(|p| matches!(p.name, "sleeper" | "waiters" | "client"))
            .expect("every workload here has a thread whose wake latency is the point");
        // One request per runnable thread one run queue holds, and one more: a
        // bound loose enough for all three workloads, whose own sharper ones
        // are the cases above. `audio_pipeline` carries its own thread count
        // rather than a hog parameter, so it is counted from the scenario.
        let rivals = if hogs > 0 {
            hogs.div_ceil(cpus) as u64 + 1
        } else {
            scenario.procs.iter().map(|p| p.initial.len() as u64).sum::<u64>() / cpus as u64 + 1
        };
        let bound = (rivals + 1) * DISPATCH_NS;

        for (name, balance) in policies() {
            let scenario = scenario.clone().with_balance(balance);
            let mut woken = Latency::default();
            let (mut wakes, mut late, mut worst_rate) = (0u64, 0u64, 0.0f64);
            sweep(&scenario, COST_SEEDS, |outcome| {
                woken.merge(outcome.wait(watched, ReadyCause::Woken));
                wakes += outcome.idle_wakes_total();
                late += outcome.late_kick_wakes_total();
                worst_rate = worst_rate.max(wake_rate(outcome));
            });
            println!(
                "cost {label} [{name}]: idle wakes {wakes} ({late} late kicks) over {COST_SEEDS} \
                 runs, worst run {worst_rate:.2}/s; woken[{}] bound={bound} ns",
                woken.summary(),
            );

            assert!(
                woken.count() >= COST_SEEDS,
                "{label} [{name}]: the watched thread was woken {} time(s) over {COST_SEEDS} \
                 runs — the distribution below is a measurement of nothing",
                woken.count(),
            );
            // The audio-relevant claim, asserted under every policy and not only
            // the shipped one: a cure that lengthened the wake path would be
            // paying for the balance path out of the interactive one.
            assert!(
                woken.max_ns() <= bound,
                "{label} [{name}]: a wake waited {} ns against a derived {bound} ns — {rivals} \
                 runnable thread(s) on one run queue and one more, at {DISPATCH_NS} ns a \
                 request. A balance policy may cost idle wakes; it may not cost wake latency. \
                 Distribution: {}",
                woken.max_ns(),
                woken.summary(),
            );
            // And the baseline is a baseline. Under `Pull` nothing arms a timer
            // on a CPU with an empty queue and nothing rings a halted CPU's
            // doorbell without a message behind it, so every wake of a halted
            // CPU here carries work or is a late kick — one sent while the CPU
            // was busy, landing after it had drained what the kick was for. If
            // anything else appears, the column the cures are read against has
            // stopped meaning "what the shipped path costs".
            if balance == Balance::Pull {
                assert_eq!(
                    wakes - late,
                    0,
                    "{label}: the shipped balance path woke a halted CPU {} time(s) for \
                     nothing over {COST_SEEDS} runs that were not late kicks. Every column \
                     beside it is quoted as a cost *over* this one",
                    wakes - late,
                );
            }
        }
    }
}

/// **The starvation bound**: the worst wait of a runnable task under saturation,
/// measured, against the fair band's own granularity.
///
/// The same quantity the cases above measure from their own ends — an
/// interactive wake and a storm drain are both a task waiting for a run queue
/// to reach it — asked here of *every* task in a workload where nothing ever
/// blocks and no CPU is ever idle. There is nothing to wait for but the band.
///
/// **What it found**, 20 seeds per point:
///
/// | workload | groups | largest group | waits seen | worst wait | derived bound | ratio |
/// |---|---|---|---|---|---|---|
/// | `fairness_storm(1)` | 2 |  3 | 1,860 |  20 ms |  36 ms | 0.56 |
/// | `sibling_storm`     | 2 |  3 | 9,060 |  20 ms |  36 ms | 0.56 |
/// | `share_gain(4)`     | 2 |  4 | 1,580 |  28 ms |  48 ms | 0.58 |
/// | `share_gain(16)`    | 2 | 16 | 5,420 | 120 ms | 192 ms | 0.63 |
///
/// A thread in a group of `n` waits for its `n − 1` siblings' requests, served
/// at the rate its group is — one `groups`-th of the CPU — so the bound is
/// `groups × n` requests. **Nothing starves, and the price is set by the
/// thread's own job**: a thread among 16 siblings waits out 15 of their
/// requests, and no number of other jobs' threads makes a job's thread wait
/// longer than its group's turn.
#[test]
fn a_runnable_task_waits_at_most_its_siblings_requests() {
    const STARVE_SEEDS: u64 = 20;
    for scenario in [
        scenarios::fairness_storm(1),
        scenarios::sibling_storm(),
        scenarios::share_gain(4, WORK),
        scenarios::share_gain(16, WORK),
    ] {
        let name = scenario.name;
        // Every thread of these workloads is runnable from the first dispatch to
        // its own exit, and every one of them is on the one CPU.
        let threads: u64 = scenario.procs.iter().map(|p| p.initial.len() as u64).sum();
        let groups = scenario.procs.len() as u64;
        let largest = scenario.procs.iter().map(|p| p.initial.len() as u64).max().unwrap_or(0);
        assert_eq!(scenario.cpus(), 1, "{name}: the bound below is one run queue's");
        let bound = groups * largest * DISPATCH_NS;

        let mut worst = 0;
        let mut samples = 0;
        sweep(&scenario, STARVE_SEEDS, |outcome| {
            worst = worst.max(outcome.worst_run_wait_ns());
            samples += outcome
                .run_wait
                .iter()
                .map(kernel_sim::latency::RunWait::samples)
                .sum::<u64>();
        });
        println!(
            "starvation {name} ({groups} groups, largest {largest}): worst={worst} ns \
             bound={bound} ns over {samples} waits",
        );
        assert!(
            samples > threads * STARVE_SEEDS,
            "{name}: only {samples} run-queue waits were recorded over {STARVE_SEEDS} runs of \
             {threads} threads, so this bound is being met by an instrument that stopped \
             measuring",
        );
        assert!(
            worst <= bound,
            "{name}: a runnable task waited {worst} ns for a CPU against a derived {bound} ns \
             — its group's siblings' requests at its group's share of the CPU. Past this, the \
             fair band is passing some task over more often than EEVDF can account for.",
        );
        assert!(
            worst * 2 > bound,
            "{name}: the worst wait is {worst} ns against a {bound} ns bound, more than twice \
             under it — the bound has stopped constraining anything",
        );
    }
}

/// **What a machine that has lost a CPU does with everything it spawns next.**
///
/// The measurement is `never_ran`: programs created and never given a single
/// instruction. Every other number in this file is a wait that *ended*, and a
/// task nobody dispatches contributes to none of them — which is why a suite
/// built from maxima reads such a machine as a quiet one.
///
/// **The bound is derived.** A stopped CPU is a legal target while its doorbell
/// edge is still down, or while [`kernel::sched::cpu::STALE_PASS_NS`] has not
/// elapsed since the pass it never took; both windows close at `STALE_PASS_NS`
/// from clock zero, so the losses are the launcher's spawns inside it.
///
/// Measured over the sweep: **1 program of 24 on every one of 16 seeds**, where
/// `--features placement-ignores-staleness` — the whole rule reverted — loses
/// **10 at worst and 114 in total**, and this assertion reds.
#[test]
fn a_stopped_cpu_stops_taking_work() {
    let scenario = scenarios::stopped_cpu();
    // Ceiling division: the spawn at `STALE_PASS_NS` itself is already outside
    // the window, and the one before it is the last that can be lost.
    let inside_the_window =
        (STALE_PASS_NS as usize).div_ceil(scenarios::STOPPED_CPU_PERIOD_NS as usize);
    let mut worst = 0usize;
    let mut total = 0usize;
    sweep(&scenario, SEEDS, |outcome| {
        worst = worst.max(outcome.never_ran);
        total += outcome.never_ran;
    });
    println!(
        "stopped_cpu: worst never_ran {worst}, total {total} over {SEEDS} seeds, \
         bound {inside_the_window} of {}",
        scenarios::STOPPED_CPU_PROGRAMS,
    );
    assert!(
        worst <= inside_the_window,
        "a stopped cpu took {worst} of {} programs, against the {inside_the_window} its \
         published numbers are believed for",
        scenarios::STOPPED_CPU_PROGRAMS,
    );
}

/// **Masks hold under stealing**: pinned, partial and moving masks on four
/// CPUs with every thread spawned on cpu0, across seeds, with the core's own
/// check that no CPU runs a task outside its mask after every pass.
#[test]
fn masked_threads_run_only_where_their_masks_name() {
    const SEEDS: u64 = 40;
    let scenario = scenarios::masked_steals();
    sweep(&scenario, SEEDS, |outcome| {
        assert!(outcome.migrations > 0, "nothing was ever moved, so no mask was tested: {}", outcome.report());
    });
}

/// **A steal reaches the queue a mask allows past a sibling's that it does
/// not**: on [`scenarios::masked_siblings`], each idle CPU's nearest victim
/// holds only work it may not run, and the work it may run sits one core
/// away. Across seeds the run ends with every thread done and I15 holding.
#[test]
fn a_masked_queue_past_a_siblings_is_stolen() {
    sweep(&scenarios::masked_siblings(), 40, |outcome| {
        assert!(outcome.migrations > 0, "nothing was ever stolen: {}", outcome.report());
    });
}
