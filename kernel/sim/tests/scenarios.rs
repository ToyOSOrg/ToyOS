//! The scheduler harness's exit criterion, and the register of what proves it
//! has teeth.
//!
//! The criterion is two claims, and the second is worth nothing without the
//! first:
//!
//! 1. **Every negative gate fails**, and fails for the *right* reason. A
//!    harness that has never rejected the bug class it was written for is
//!    decoration.
//! 2. **Every scenario passes**, over a seed sweep and a fuzz-byte sweep.
//!
//! # The negative gates, and the control
//!
//! Each is a `scenarios::` constructor that *is* a broken scheduler, and each
//! has a `#[test]` below asserting both that it is caught and which invariant
//! catches it. That second column is the point: a gate caught by the wrong
//! check says nothing about the one it was written for.
//!
//! | gate | breaks | caught by |
//! |---|---|---|
//! | `old_steal_port` | the old steal-and-scan algorithm | I1 **and** I8 |
//! | `old_commit_before_pass` | the pre-`8508b37` blocking shape | I1, on every seed that commits at the call site |
//! | `old_preemptible_window` | preemption left on in the registration window | an abort inside `check_cpu` |
//! | `old_migrate_kept_the_corpse` | the balance path handing on a killed task | I14 |
//! | `old_rt_starved_the_corpse` | the RT band outranking the dying list without a bound | I14, on every seed |
//! | `old_park_kept_the_lend` | commit `9c2fc4d`'s park keeping a lapsed lend | I9 |
//! | `fair_job_per_thread` | one job per thread, not per process | I5, and nothing else |
//! | `fair_double_charge` | a job group charged twice for what it runs | I5, in the opposite direction |
//! | `fair_lowest_key_within_group` | a group's lowest-keyed thread served, not its EEVDF pick | I13, and nothing else |
//! | `overlong_pass` | a pass costing five times its budget | `cpu::PassCosts`, which records it rather than aborting |
//!
//! The control is `old_commit_fused`, which must come back **clean**: the same
//! blocking bug with no step boundary to expose it, which is the blind spot
//! this harness used to have.
//!
//! # The liveness gates, which are a different claim
//!
//! Three checks below are not negative gates. They guard against a gate that
//! goes *quiet* rather than red — a change that narrows the gate's own coverage
//! instead of violating it — by asserting that an invariant had a comparison
//! open for a recorded fraction of the run:
//! `the_fairness_storm_is_measured_and_holds` for I5,
//! `invariant_i13_is_measured_and_holds` for I13, and
//! `a_retire_completes_inside_its_derived_bound` for I14, which requires some
//! retire to have outlived the instant it was posted in. A change that closes
//! those windows is then as loud as one that violates them.
//!
//! # Running it
//!
//! The budgets here are what a `cargo test` can afford. The full criterion —
//! 10⁴ seeds and 10⁷ fuzz steps per scenario class — runs from the CLI, where
//! `gate` carries every gate and the control:
//!
//! ```text
//! cargo run --release -p kernel-sim -- gate 10000
//! cargo run --release -p kernel-sim -- fuzz-sweep 10000000
//! ```

use std::collections::BTreeMap;

use kernel_sim::choice::ChoiceStream;
use kernel_sim::explore::run;
use kernel_sim::scenarios;
use kernel_sim::shrink;
use kernel_sim::sweep;

/// Seeds per scenario in the in-test sweep. Every seed is a complete
/// exploration with all of I1–I13 checked after every step.
const SEEDS: u64 = 500;

/// Steps per scenario in the in-test fuzz sweep.
const FUZZ_STEPS: u64 = 20_000;

/// Seeds for the fairness gates and their controls. A fifth of [`SEEDS`],
/// because `fairness_storm` is by far the longest scenario in the tree — it has
/// to be, since a contention window must be many quanta wide before a broken
/// split can clear the bound — and `every_scenario_survives_a_seed_sweep`
/// already runs it at the full count. What these tests add is the *direction* of
/// each failure, and a structural 3:1 split does not need 500 schedules to show
/// up in.
const FAIR_SEEDS: u64 = 100;

#[test]
fn every_scenario_survives_a_seed_sweep() {
    let mut failures = Vec::new();
    let mut total = 0u64;
    for scenario in scenarios::all() {
        let result = sweep::seed_sweep(&scenario, SEEDS, 3);
        total += result.steps;
        if !result.passed() {
            failures.push(result.report());
        }
    }
    assert!(
        failures.is_empty(),
        "seed sweep found violations:\n{}",
        failures.join("\n"),
    );
    assert!(
        total > 100_000,
        "the sweep must actually explore: {total} steps"
    );
}

#[test]
fn every_scenario_survives_raw_fuzz_bytes() {
    let mut failures = Vec::new();
    for scenario in scenarios::all() {
        let result = sweep::fuzz_sweep(&scenario, FUZZ_STEPS, 3);
        if !result.passed() {
            failures.push(result.report());
        }
    }
    assert!(
        failures.is_empty(),
        "fuzz sweep found violations:\n{}",
        failures.join("\n"),
    );
}

/// The first self-validation gate. Both failure modes the old algorithm has are
/// required to show up, because they are different bugs wearing one name:
///
/// * **I1** — the task is in no container at all (carried on the thief's
///   stack) or in a queue its state word does not name. Single ownership,
///   lost.
/// * **I8** — the teardown drew a proof of absence against a task that was
///   merely in transit, and freed the address space that task still holds.
///   That is the recorded double-drop failure itself.
#[test]
fn old_steal_port_is_caught() {
    let scenario = scenarios::old_steal_port();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut caught = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert!(
        caught > 0,
        "the old steal-and-scan protocol went undetected in {SEEDS} schedules — \
         nothing this harness says about the new one means anything",
    );
    assert!(
        kinds.contains_key("I1"),
        "expected a single-ownership violation; got {kinds:?}",
    );
    assert!(
        kinds.contains_key("I8"),
        "expected the address-space-freed-while-referenced violation \
         (the double-drop shape itself); got {kinds:?}",
    );
}

/// The second self-validation gate: the kernel's pre-`8508b37` blocking shape,
/// where phase 2 of the wait handshake ran at the call site instead of inside
/// the blocking pass. On `--smp 8` that was a panic plus a 30 s hang in roughly
/// two of five audio suite runs.
///
/// It must be caught, and caught by **I1** specifically: a task whose word says
/// `Blocked` while its own CPU still has it in `running` is a single-ownership
/// break, and it is the break that makes the lost wake possible — the waker
/// reads `Blocked`, posts to the home CPU, and that CPU's own pass drains the
/// message before the task is anywhere a wake can find it.
#[test]
fn old_commit_before_pass_is_caught() {
    let scenario = scenarios::old_commit_before_pass();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let (mut caught, mut reached) = (0, 0);
    let mut worst_steps = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        // The shape is reached the first time a block commits at the call
        // site and goes on to park: its word then reads `Blocked` while its
        // CPU still runs it. A schedule in which every block found a token
        // waiting never executes the defect.
        let reaches = outcome.call_site_commits > 0;
        reached += usize::from(reaches);
        assert_eq!(
            !outcome.passed(),
            reaches,
            "seed {seed}: {} call-site commit(s), and the run {} — the gate must catch exactly \
             the schedules that commit before the pass",
            outcome.call_site_commits,
            if outcome.passed() { "passed" } else { "failed" },
        );
        if outcome.passed() {
            continue;
        }
        caught += 1;
        worst_steps = worst_steps.max(outcome.steps);
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    // Coverage, not detection: the workload must keep reaching the window. In
    // 85 of 500 schedules the producer stays ahead and every consumer block
    // finds a token waiting (507 at entry, 2 at the re-check), so none commits.
    assert!(
        reached * 4 >= 3 * SEEDS as usize,
        "only {reached}/{SEEDS} schedules reached a call-site commit; the workload has stopped \
         exercising the window",
    );
    // 50 steps at worst, measured with `caught` above.
    assert!(
        worst_steps < 64,
        "the worst of {caught} caught schedules took {worst_steps} steps to expose it; this is \
         a structural break and should surface almost immediately",
    );
    assert!(
        kinds.contains_key("I1"),
        "expected a single-ownership violation; got {kinds:?}",
    );
}

/// The fourth self-validation gate: the balance path allowed to hand on a task
/// whose kill bit is already set.
///
/// This is the shape of the panic the owner's T14 took at 949 s of uptime, with
/// doom exiting — `retire_task: task not released after 1s: InTransit(CpuId(1))`
/// — and the reason `CpuSched::hand_off` now reads the kill bit. `InTransit` is
/// the one state whose reap is carried by an adopt rather than by the retire,
/// and a destination that is running owes the task nothing until its pick.
/// Handing a corpse there buys the thief a dead task and pays for it with the
/// retirer's deadline.
///
/// It must be caught by **I14** specifically. The failure rate is a rate and not
/// a certainty — the migration needs a boosted wake to land on the RT daemon's
/// CPU in the window between the teardown and that CPU's drain — so this asserts
/// a floor over the sweep rather than every seed.
#[test]
fn old_migrate_keeping_the_corpse_is_caught() {
    let scenario = scenarios::old_migrate_kept_the_corpse();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut caught = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert!(
        caught > 0,
        "migrating a task that is already dead went undetected in {SEEDS} \
         schedules — I14 says nothing about the balance path it was written for",
    );
    assert!(
        kinds.contains_key("I14"),
        "expected a retire-promptness violation; got {kinds:?}",
    );
}

/// **The other direction of the same law, and the one the first fix created.**
/// The gate above is a corpse handed away and left waiting; this is a corpse
/// never dispatched at all, because the pick asked only `rq.has_rt()` and one
/// permanently-RT thread that never parks answered yes for ever.
///
/// That shape shipped on this branch between the two fixes.
///
/// It must be caught by **I14**, on every seed: nothing in this scenario is a
/// race. The corpse is queued, the RT thread runs, and the only question is
/// whether the pick ever takes the dying list. `rt_saturated_retire` is the
/// positive half and is in `all()`.
#[test]
fn rt_starving_the_corpse_is_caught() {
    let scenario = scenarios::old_rt_starved_the_corpse();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut caught = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert_eq!(
        caught, SEEDS as usize,
        "a corpse held off for ever by the RT band went undetected in \
         {}/{SEEDS} schedules — and the tree that does it panics the kernel",
        SEEDS as usize - caught,
    );
    assert!(
        kinds.contains_key("I14"),
        "expected a retire-promptness violation; got {kinds:?}",
    );
}

/// The positive half of the same pair: with the kill bit read, no schedule of
/// that workload puts a corpse in transit, and every retire completes well
/// inside the derived bound.
#[test]
fn a_retire_completes_inside_its_derived_bound() {
    let scenario = scenarios::retire_under_balance();
    let mut worst = 0;
    let mut bound = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        assert!(outcome.passed(), "{}", outcome.report());
        if outcome.retire_latency > worst {
            worst = outcome.retire_latency;
            bound = outcome.retire_bound;
        }
    }
    assert!(
        worst > 0,
        "in {SEEDS} schedules no retire ever outlived the instant it was posted \
         in, so I14's latency half never measured anything",
    );
    println!("I14: worst retire {worst} ns against a {bound} ns bound");
}

/// **A fidelity gate, not a scheduler gate**: the model's own unwind deferral
/// lasts one chunk plus what the model can prove it owes elsewhere, which is
/// what its doc promises and what the code did not do.
///
/// [`kernel_sim::vm::Vm::unwind_at`] closes `Vm::enabled`'s gate on every
/// *other* CPU's `Exec` while one CPU owes an unwind step. Stamped only on the
/// false→true transition, and read with a `find` that named one owed CPU and
/// denied the step to every other, that gate stayed shut for a CPU's whole
/// teardown window — several consecutive corpses — and every other CPU was
/// frozen for all of it: 17,000,000 ns measured, 17 x `RUN_CHUNK_NS`, against a
/// doc saying "the same grace" as the one-chunk `resched_at`.
///
/// That is not a scheduler defect; it is the explorer being denied exactly the
/// interleavings the surrounding chunk is about, over exactly the window I14 is
/// measured across. So the promise is asserted as a number, term by term, for a
/// stamp taken at T on a scenario of `cpus` CPUs:
///
/// 1. **one chunk** — the grace itself. The gate does not close until the clock
///    reaches `T + RUN_CHUNK_NS`, and until it does every CPU may step freely.
/// 2. **one chunk** — the step that carries the clock past that threshold, which
///    is somebody's execution step and may be a whole chunk long. (An
///    `Op::KernelSection` advances by its own length instead; the largest in the
///    suite is `MS / 2`, half a chunk, so a chunk is the wider price.)
/// 3. **`cpus - 1` chunks** — the other CPUs whose own stamp is no later than
///    this one. Debts are discharged oldest first, ties by CPU number, and a CPU
///    that takes its step restamps to *now* — so each peer can go ahead of this
///    one at most once. A workload-shaped term in a derived bound, exactly as
///    invariant I14's `(1 + peers)` is.
/// 4. **one chunk** — the step that discharges it.
#[test]
fn the_unwind_gate_lasts_one_chunk_and_not_one_unwind() {
    let scenario = scenarios::retire_under_balance();
    let bound = (2 + scenario.cpus() as u64) * kernel_sim::vm::RUN_CHUNK_NS;
    let mut worst = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, scenario.cpus(), 3)
        };
        let outcome = run(scenario.clone(), &mut choices);
        assert!(outcome.passed(), "{}", outcome.report());
        worst = worst.max(outcome.unwind_gate_ns);
    }
    assert!(
        worst > 0,
        "in {SEEDS} schedules no CPU ever held an unwind across a step, so this \
         measured nothing at all",
    );
    assert!(
        worst <= bound,
        "the unwind gate stood for {worst} ns against a derived {bound} ns — the \
         explorer was frozen over the very window I14 is measured across",
    );
    println!("unwind gate: worst {worst} ns against a {bound} ns bound");
}

/// A `Retire` that lands inside the registration window, and the fact that the
/// workload driver no longer has a check of its own for it.
///
/// `Vm::block_pass` used to cancel the ticket and exit when it found the kill
/// bit set. That was a compensation: the kernel's `pass_block` had no such
/// check, so the simulator was papering over a hole rather than modelling it —
/// the drain answers a `Retire` aimed at the *running* task with `need_resched`
/// and consumes it, and a task that then parks is never picked, never reaped,
/// and holds its address space forever. Deleting that arm reproduced it in 3+
/// of 400 `double_drop_exit_race` seeds.
///
/// The arm is gone and the sweeps are clean, which is only evidence about
/// `WaitTicket::commit` if the case still *happens*. So it is counted.
#[test]
fn a_retire_inside_the_registration_window_is_honoured_by_the_core() {
    let mut killed = 0;
    for seed in 0..SEEDS {
        let mut choices = if seed % 2 == 0 {
            ChoiceStream::from_seed(seed)
        } else {
            ChoiceStream::pct(seed, 2, 3)
        };
        let outcome = run(scenarios::double_drop_exit_race(), &mut choices);
        assert!(outcome.passed(), "{}", outcome.report());
        killed += outcome.killed_at_park;
    }
    assert!(
        killed > 0,
        "in {SEEDS} schedules no retire ever landed inside a registration \
         window, so `Commit::Killed` is dead code here and these runs say \
         nothing about the hole that arm used to hide",
    );
}

/// The third self-validation gate: the registration window with preemption
/// left enabled, which is what the kernel had until the wait ticket grew a
/// guard.
///
/// This one is the reason `Vm::enabled` withholds `Step::Pass` while a CPU is
/// mid-block. That withholding is a *model* of the kernel's preempt count, and
/// a model nobody can falsify is a comment: flip the scenario's window to
/// `Preemptible` and the same harness executes the step, on the same
/// schedules, and the core aborts. It aborts rather than reporting a violation
/// because a task whose word reads `Committing` has no legal preempt edge —
/// which is exactly why the window had to be closed rather than tolerated.
/// Teaching `RunningTask::preempt` to accept `Committing` would publish
/// `Ready`, and every waker that pops the registration would then report
/// `Claim::Lost` and move on: a lost wake, silently, in place of a panic.
#[test]
fn a_pass_inside_the_registration_window_is_caught() {
    let scenario = scenarios::old_preemptible_window();
    let caught = sweep::abort_gate(&scenario, SEEDS);
    let Some((seed, message)) = caught else {
        panic!(
            "an involuntary pass inside the registration window went undetected \
             in {SEEDS} schedules — the preempt guard the kernel holds there is \
             then unfalsifiable, and so is this model of it",
        );
    };
    // The word reads `Committing`, or `WakeQueued` where a waker claimed the
    // task inside the window before the pass reached it: both are the window.
    assert!(
        message.contains("disagrees with its state word")
            && (message.contains("Committing") || message.contains("WakeQueued")),
        "expected the running-task word check to be what fires (seed {seed}); got: {message}",
    );

    // And the control: the identical workload with the guard modelled comes
    // back clean over the same seeds, so the gate is measuring the guard and
    // not the workload.
    let guarded = sweep::seed_sweep(&scenarios::double_drop_exit_race(), SEEDS, 1);
    assert!(guarded.passed(), "{}", guarded.report());
}

/// The wait handshake's *residual* window, named and deliberately left open: a
/// waker may claim the task in the instructions between the commit
/// publishing `Blocked` and the park itself. That is why `RunningTask::park`
/// accepts `WakeQueued` — and until the block became two steps, no simulator
/// run had ever executed it, so that acceptance was a claim backed by nothing.
///
/// The window cannot be a step boundary (a `SchedPass` borrows `CpuSched` and
/// cannot be held across one), so it is reached by injection. What this test
/// asserts is that it is reached *at all*: an assertion that the arm is
/// exercised, not that it is correct — the sweeps say the latter.
#[test]
fn the_park_sees_claims_that_landed_after_its_commit() {
    let mut claims = 0;
    for seed in 0..SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(scenarios::lost_wake_pipe(), &mut choices);
        assert!(outcome.passed(), "{}", outcome.report());
        claims += outcome.pre_park_claims;
    }
    assert!(
        claims > 0,
        "in {SEEDS} schedules no park ever saw a claim land after its own \
         commit, so `RunningTask::park`'s `WakeQueued` arm is dead code here \
         and the residual window is back outside the model",
    );
}

/// The control that turns "the simulator structurally could not see this" from
/// a claim into a measurement.
///
/// `old_commit_fused` is the *same* workload and the *same* pre-`8508b37`
/// blocking shape, with one difference: the call site and the pass are one VM
/// step, which is what this simulator did until the block was split. Nothing
/// can interleave inside a step, so the window is not in the step relation and
/// every schedule comes back clean — which is exactly why the harness
/// certified a protocol whose lost wake it could not execute.
#[test]
fn blind_spot_needed_the_step_split() {
    let fused = sweep::seed_sweep(&scenarios::old_commit_fused(), SEEDS, 1);
    assert!(
        fused.passed(),
        "with the two halves fused into one step the bug is invisible; if this \
         now fails, the control has stopped controlling for anything: {}",
        fused.report(),
    );

    let split = sweep::seed_sweep(&scenarios::old_commit_before_pass(), SEEDS, 1);
    assert!(
        !split.passed(),
        "the identical shape with the halves as separate steps must fail: {}",
        split.report(),
    );
}

/// The A/B that makes the gate a comparison rather than an assertion: the
/// *same* schedule, run against both protocols. Only the algorithm differs.
#[test]
fn the_new_protocol_survives_the_schedule_that_breaks_the_old_one() {
    let decisions: Vec<usize> =
        shrink::decode(include_str!("../corpus/old_steal_port_i8.trace")).decisions;

    let mut old = ChoiceStream::replay(decisions.clone());
    let old = run(scenarios::old_steal_port(), &mut old);
    assert!(
        !old.passed(),
        "the old protocol must fail this schedule; it passed",
    );
    assert!(
        old.violations.iter().any(|v| v.starts_with("I8")),
        "expected the address space to be freed under a live task: {:?}",
        old.violations,
    );

    let mut new = ChoiceStream::replay(decisions);
    let new = run(scenarios::double_drop_exit_race(), &mut new);
    assert!(
        new.passed(),
        "the new protocol must survive the very schedule that breaks the old \
         one: {}",
        new.report(),
    );
}

/// Committed traces are permanent regressions, including the negative one.
#[test]
fn corpus_traces_still_do_what_they_were_committed_for() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("the corpus directory exists") {
        let path = entry.expect("readable entry").path();
        if path.extension().is_none_or(|e| e != "trace") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable trace");
        let entry = shrink::decode(&text);
        let scenario =
            scenarios::by_name(&entry.scenario).expect("the trace names a known scenario");
        let outcome = shrink::replay(&entry, scenario);
        assert_eq!(
            !outcome.passed(),
            entry.expect_failure,
            "{}: expected {}, got {}",
            path.display(),
            if entry.expect_failure {
                "failure"
            } else {
                "success"
            },
            outcome.report(),
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected the committed corpus, found {checked}"
    );
}

/// Determinism is the property everything else rests on: replaying a run's
/// decisions must reproduce it exactly, whatever driver produced it.
#[test]
fn a_run_replays_exactly() {
    for scenario in scenarios::all() {
        let mut original = ChoiceStream::pct(42, scenario.cpus(), 3);
        let first = run(scenario.clone(), &mut original);
        let mut replay = ChoiceStream::replay(first.decisions.clone());
        let again = run(scenario.clone(), &mut replay);
        assert_eq!(
            (first.steps, first.elapsed, first.switches, first.kicks),
            (again.steps, again.elapsed, again.switches, again.kicks),
            "{} did not replay identically",
            scenario.name,
        );
        assert_eq!(first.decisions, again.decisions, "{}", scenario.name);
    }
}

/// A shrunk trace is only useful if it still fails, and only readable if it
/// is much shorter than what it came from.
#[test]
fn shrinking_keeps_the_failure_and_loses_the_noise() {
    let scenario = scenarios::old_steal_port();
    // A failure the explorer trips over in two steps has no noise to lose;
    // what the shrinker is for is only visible on one it took a while to
    // reach.
    let outcome = (0..SEEDS)
        .find_map(|seed| {
            let mut choices = ChoiceStream::from_seed(seed);
            let outcome = run(scenario.clone(), &mut choices);
            (!outcome.passed() && outcome.steps > 16).then_some(outcome)
        })
        .expect("some seed must reach the old protocol's failure the long way");
    let minimized = shrink::shrink(&scenario, outcome.decisions.clone());
    assert!(
        minimized.len() < outcome.decisions.len(),
        "shrinking removed nothing: {} decisions",
        minimized.len(),
    );

    let mut replay = ChoiceStream::replay(minimized);
    assert!(
        !run(scenario, &mut replay).passed(),
        "the shrunk trace must still fail",
    );
}

/// `cpus = 1` is first-class: it is the configuration Doom runs in, and the one
/// where a scheduling mistake is audible.
#[test]
fn the_audio_pipeline_holds_on_one_cpu() {
    let result = sweep::seed_sweep(&scenarios::audio_pipeline(1), SEEDS, 3);
    assert!(result.passed(), "{}", result.report());
    assert_eq!(result.scenario, "audio_pipeline");
}

/// Invariant I5 is *alive*, which is a separate claim from I5 being green.
///
/// A fairness check whose contention windows never open reports "clean" on every
/// scenario in the tree and certifies nothing. So the spread is required to be
/// *non-zero* — some window opened, stayed open long enough for a separation to
/// accumulate, and was measured — and within a factor of four of its own bound
/// at some width, or the bound has stopped constraining anything.
///
/// **And the *reach* is asserted, not only the verdict.** I5's window closes for
/// an RT task, an idle CPU, a changed member set or an uneven mix, and the pick
/// or the placement can narrow any of them without a verdict going red. The
/// recorded figures are 99% and 71% of executed time
/// (`measure fairness_storm:<cpus> 200`, on `e74dbf1` with this change
/// applied), where the worst spreads were 4 ms of 20 and 12 ms of 40; a halving
/// of the reach reds.
#[test]
fn the_fairness_storm_is_measured_and_holds() {
    let mut tightest = u64::MAX;
    for (cpus, reach) in [(1, 99), (2, 71)] {
        let result = sweep::seed_sweep(&scenarios::fairness_storm(cpus), FAIR_SEEDS, 3);
        assert!(
            result.fair_coverage_pct() * 2 >= reach,
            "at {cpus} cpu(s) invariant I5 had a comparison open for {}% of the \
             run against {reach}% recorded. Its *reach* has collapsed, which is \
             not the same as its verdict being clean — read this as loudly as a \
             violation and find what closed the windows: {}",
            result.fair_coverage_pct(),
            result.report(),
        );
        assert!(result.passed(), "{}", result.report());
        assert!(
            result.worst_fair_spread > 0,
            "at {cpus} cpu(s) invariant I5 never opened a contention window it \
             could measure, so its clean verdict on every other scenario is a \
             verdict about nothing: {}",
            result.report(),
        );
        assert_eq!(result.worst_over_bound, 0, "{}", result.report());
        tightest = tightest.min(result.worst_fair_bound / result.worst_fair_spread.max(1));
    }
    assert!(
        tightest <= 4,
        "every width sat more than 4x under the fairness bound (closest was \
         {tightest}x); the bound has stopped constraining anything",
    );
}

/// The fifth self-validation gate: one job per *thread* — the rejected
/// policy — instead of one per process.
///
/// `trio` has three times `solo`'s threads and exactly the same entitlement.
/// Under the shipped policy they split the CPU evenly; under a job per thread
/// `trio` takes three quarters. If I5 cannot see a 3:1 split of a two-way share,
/// it is not measuring fairness, and every clean I5 report in this file means
/// nothing.
#[test]
fn a_job_per_thread_is_caught() {
    let broken = scenarios::fair_job_per_thread();
    let mut caught = 0;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for seed in 0..FAIR_SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(broken.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert_eq!(
        caught, FAIR_SEEDS as usize,
        "a process taking three quarters of the machine off its thread count \
         went undetected in {}/{FAIR_SEEDS} schedules",
        FAIR_SEEDS as usize - caught,
    );
    assert!(kinds.contains_key("I5"), "expected the service-spread violation; got {kinds:?}");
    assert_eq!(
        kinds.len(),
        1,
        "a job per thread must be caught by I5 and not as collateral in some \
         other invariant — I6 counts every job of a process and must stay \
         correct under either shape: {kinds:?}",
    );

    // The control: the identical workload under the shipped policy, or the gate
    // is detecting the workload rather than the policy.
    let shipped = sweep::seed_sweep(&scenarios::fairness_storm(1), FAIR_SEEDS, 1);
    assert!(shipped.passed(), "{}", shipped.report());
}

/// The sixth self-validation gate, and I5's other direction: a job group
/// charged twice for every nanosecond it runs.
///
/// It matters that this is a *separate* gate from `a_job_per_thread_is_caught`,
/// and that the two fail in opposite directions. Fairness is service measured
/// against entitlement, and there are two ways to get it wrong — mis-attribute
/// the entitlement, or mis-measure the service.
#[test]
fn double_charging_a_group_is_caught() {
    let broken = scenarios::fair_double_charge();
    let mut caught = 0;
    let mut throttled = 0;
    for seed in 0..FAIR_SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(broken.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        // The direction is the point: the double-charged process gets *less*,
        // which is the opposite of what per-thread shares do to it.
        if outcome
            .violations
            .iter()
            .any(|v| v.starts_with("I5") && v.contains("solo=") && v.contains("trio="))
        {
            let text = outcome.violations.join(" ");
            let value = |name: &str| -> u64 {
                text.split(name)
                    .nth(1)
                    .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            };
            if value("solo=") > value("trio=") {
                throttled += 1;
            }
        }
    }
    assert_eq!(
        caught, FAIR_SEEDS as usize,
        "a job throttled for work it never did went undetected in {}/{FAIR_SEEDS} \
         schedules",
        FAIR_SEEDS as usize - caught,
    );
    assert_eq!(
        throttled, caught,
        "the double-charged process must come out *behind*; if it is ahead, this \
         gate is catching something other than the charge",
    );

    let shipped = sweep::seed_sweep(&scenarios::fairness_storm(1), FAIR_SEEDS, 1);
    assert!(shipped.passed(), "{}", shipped.report());
}

/// The seventh self-validation gate: the core's `feature = "check"` pass-cost
/// recorder, the on-target counterpart to everything else in this file.
///
/// It is the one instrument here that says something about *cost* rather than
/// about state, so it is the one the simulator cannot exercise for free: the
/// VM's clock does not move inside a step, and a histogram whose measured
/// quantity is always zero is a histogram that cannot say anything. `SimHw`
/// therefore charges every pass a modelled cost, and this gate turns that cost
/// up past the budget.
///
/// **This gate used to demand an abort and now demands a number**, because the
/// budget stopped being asserted in the kernel: elapsed time across a pass is
/// wall clock, a guest's wall clock runs while a hypervisor has the vCPU, and a
/// panic may not stand over a quantity the host inflates. What the recorder must
/// still do is see the cost — without that, "a check build measures what a pass
/// costs" would be a claim backed by an expression that computes zero a few
/// thousand times a second, and the harness gate downstream would be reading a
/// distribution of nothing.
#[test]
fn a_pass_that_overruns_its_budget_is_recorded() {
    let scenario = scenarios::overlong_pass();
    let cost = scenario.pass_cost_ns;
    let outcome = run(scenario, &mut ChoiceStream::from_seed(0));
    assert!(outcome.passed(), "{}", outcome.report());

    let measured: u64 = outcome.pass_costs.iter().map(|c| c.count).sum();
    let over: u64 = outcome.pass_costs.iter().map(|c| c.over).sum();
    assert!(
        measured > 0,
        "a run that took {} steps recorded no pass at all — either the recorder is not \
         compiled in, or `finish` stopped feeding it",
        outcome.steps,
    );
    assert_eq!(
        over, measured,
        "every one of {measured} passes was modelled at {cost} ns, five times \
         `cpu::MAX_PASS_NS`, and {over} of them were recorded over budget — so the clock \
         the recorder reads does not move, or it reads it in the wrong place",
    );
    for report in &outcome.pass_costs {
        assert_eq!(
            report.max_ns, cost,
            "cpu{} recorded a maximum of {} ns for passes modelled at {cost} ns",
            report.cpu.0, report.max_ns,
        );
        // The measurement the harness gates, on the one distribution whose
        // answer is known: every sample at the modelled cost, so every quantile
        // is the bucket that cost falls in.
        assert!(
            report.count == 0 || report.quantile_upper_ns(1, 2) > kernel::sched::cpu::MAX_PASS_NS,
            "cpu{}'s median came back at {} ns with every pass modelled at {cost}",
            report.cpu.0,
            report.quantile_upper_ns(1, 2),
        );
    }

    // The control: the identical workload at the default modelled cost of zero
    // must record nothing over budget, so the gate is measuring the cost and not
    // the workload.
    let free = run(scenarios::lost_wake_pipe(), &mut ChoiceStream::from_seed(0));
    assert!(free.passed(), "{}", free.report());
    assert!(
        free.pass_costs.iter().map(|c| c.count).sum::<u64>() > 0,
        "the control recorded no pass either, so the assertion above it is about nothing",
    );
    assert_eq!(
        free.pass_costs.iter().map(|c| c.over).sum::<u64>(),
        0,
        "a workload whose passes are modelled at zero recorded passes over budget",
    );

    let sweep = sweep::seed_sweep(&scenarios::lost_wake_pipe(), FAIR_SEEDS, 1);
    assert!(sweep.passed(), "{}", sweep.report());
}

/// The ninth self-validation gate, and the newest: invariant I9's teeth.
///
/// I9 says one lend buys at most one quantum of running time at the borrowed
/// priority. Commit `9c2fc4d` shipped a park that cleared the window only
/// `if now >= until`, so a lend blocked on before it ran out survived the
/// block — and with `RtState::arm` re-arming at every dispatch, a task that
/// obtains one lend and thereafter runs less than a quantum before blocking
/// holds inherited RT forever, off a single pipe interaction and with nobody
/// renewing anything.
///
/// The invariant I9 that shipped alongside it could not see this, and the
/// giveaway was that it needed no change: it compared a *running* task's
/// `until` against the clock, and a re-armed `until` is by construction fresh.
/// A check that passes because it stopped measuring certifies nothing, so I9 is
/// now the cumulative form and this test is what says so. If it ever stops
/// failing, the check has lost its teeth
/// again and every clean I9 report above it means nothing.
#[test]
fn old_park_keeping_the_lend_is_caught() {
    let scenario = scenarios::old_park_kept_the_lend();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut caught = 0;
    for seed in 0..SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(scenario.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert!(
        caught > 0,
        "a task holding a borrowed RT priority forever off one lend went \
         undetected in {SEEDS} schedules — invariant I9 has no teeth, and \
         nothing it certifies elsewhere means anything",
    );
    assert!(
        kinds.contains_key("I9"),
        "expected the per-lend running-time violation; got {kinds:?}",
    );

    // The control: the same workload under the shipped park must be clean, or
    // the gate is only detecting the workload.
    let fixed = scenarios::lend_then_block();
    for seed in 0..SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(fixed.clone(), &mut choices);
        assert!(
            outcome.passed(),
            "the shipped park failed its own gate's workload on seed {seed}: {:?}",
            outcome.violations,
        );
    }
}

/// Invariant I13 is *alive*, which is a separate claim from I13 being green,
/// and one nothing else here would notice was false: I5 measures service per
/// *process*, so a group that runs one thread and starves the rest leaves it
/// clean.
///
/// So the most a waiting thread was owed is required to be non-zero on all three
/// fairness workloads, and within a factor of four of its bound somewhere, or
/// the bound has stopped constraining anything. **The reach is asserted too**:
/// a change to placement or the pick could leave no thread owed service rather
/// than fail. The recorded figures are 100%, 88% and 100% of executed time, and
/// worst owed 3.3, 7.6 and 3.3 ms against 24 (`measure <scenario> 200`, on
/// `e74dbf1` with this change applied); a halving of the reach reds.
#[test]
fn invariant_i13_is_measured_and_holds() {
    let mut tightest = u64::MAX;
    for (scenario, reach) in [
        (scenarios::fairness_storm(1), 100),
        (scenarios::fairness_storm(2), 88),
        (scenarios::sibling_storm(), 100),
    ] {
        let name = scenario.name;
        let result = sweep::seed_sweep(&scenario, FAIR_SEEDS, 3);
        assert!(
            result.wait_coverage_pct() * 2 >= reach,
            "{name}: some fair thread was owed service for {}% of the run against \
             {reach}% recorded. I13's *reach* has collapsed, which is not the same \
             as its verdict being clean: {}",
            result.wait_coverage_pct(),
            result.report(),
        );
        assert!(result.passed(), "{}", result.report());
        assert!(
            result.worst_wait_owed > 0,
            "{name}: no waiting thread was ever owed service, so I13's clean \
             verdict on every other scenario is a verdict about nothing: {}",
            result.report(),
        );
        assert_eq!(result.worst_wait_over_bound, 0, "{}", result.report());
        tightest = tightest.min(result.worst_wait_bound / result.worst_wait_owed.max(1));
    }
    assert!(
        tightest <= 4,
        "every fairness workload sat more than 4x under the wait bound \
         (closest was {tightest}x); the bound has stopped constraining anything",
    );
}

/// The eighth self-validation gate, and the one I5 is structurally incapable of
/// standing in for: each group serves its *lowest-keyed* thread every time
/// instead of its EEVDF pick.
///
/// It **must fail**, and on I13 and nothing else. A group is charged for the
/// time its job ran, not for which of its threads ran it, so the per-process
/// split is exactly as it was: `solo` and `trio` still take half the machine
/// each, I5 still reports an even split, I6's counts are still right — and two
/// of `trio`'s three threads never run.
#[test]
fn the_lowest_key_within_a_group_is_caught() {
    let broken = scenarios::fair_lowest_key_within_group();
    let mut caught = 0;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for seed in 0..FAIR_SEEDS {
        let mut choices = ChoiceStream::from_seed(seed);
        let outcome = run(broken.clone(), &mut choices);
        if outcome.passed() {
            continue;
        }
        caught += 1;
        for violation in &outcome.violations {
            let id = violation.split(':').next().unwrap_or("?").to_string();
            *kinds.entry(id).or_default() += 1;
        }
    }
    assert_eq!(
        caught, FAIR_SEEDS as usize,
        "a group serving one of its three threads and starving the other two \
         went undetected in {}/{FAIR_SEEDS} schedules",
        FAIR_SEEDS as usize - caught,
    );
    assert_eq!(
        kinds.len(),
        1,
        "sibling starvation must be caught by I13 and by I13 alone — I5 sees an \
         even per-process split here, which is exactly why I13 exists: {kinds:?}",
    );
    assert!(kinds.contains_key("I13"), "expected the wait violation; got {kinds:?}");

    // The control: the identical workload under the shipped pick, or the gate
    // is detecting the workload rather than the pick.
    let shipped = sweep::seed_sweep(&scenarios::sibling_storm(), FAIR_SEEDS, 1);
    assert!(shipped.passed(), "{}", shipped.report());
}
