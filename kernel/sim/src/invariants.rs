//! The global walks: the invariants no single CPU can check from its own state.
//!
//! These run after **every** step. They are the reason the simulator exists:
//! the linear types make most of these states unrepresentable in *scheduler*
//! code, but the protocol above them — who posts what to whom, and when — is
//! not something the compiler can check, and the old scheduler's failures all
//! lived exactly there.
//!
//! Division of labour with loom: loom owns the primitives (mailbox
//! linearizability, doorbell edges, the ticket CAS protocol, weak memory);
//! this file owns the protocol above them and does not model memory ordering
//! at all.

use std::collections::BTreeMap;
use std::sync::Arc as StdArc;

use kernel::sched::hw::CpuId;
use kernel::sched::invariants::{residents, Container};
use kernel::sched::queue::{LAG_BOUND_NS, SLICE_NS};
use kernel::sched::task::{TaskKey, TaskState, QUANTUM_NS};

use kernel::sched::cpu::{Balance, DYING_AGE_NS, DYING_CHUNK_NS};

use crate::latency::ReadyCause;
use crate::vm::{FairEpoch, Vm, IPI_LATENCY_NS, PROBE_WORTH_IT, RUN_CHUNK_NS, UNWIND_NS};

/// How long a CPU may keep running a normal task while an RT task is ready on
/// it (invariant I4): the interrupt's own delivery bound, plus the preempt-off
/// section it may have to wait out, plus the granularity of one execution step,
/// **plus one aged corpse's chunk**. Measured in the CPU's *own* busy time, so
/// another CPU's progress cannot inflate it.
///
/// **The fourth term is what bounded deferral costs, and it is priced here
/// rather than declared away.** The cancellable kill introduced the dying
/// list; the first attempt served it *before* `rq` and the term would have
/// been the whole unwind, once per killed task, turning this
/// bound into a statement about how long a kernel teardown takes. The second
/// attempt served it strictly *after* `rq` and had no term at all — at the price
/// of a corpse that never runs under a saturated RT band.
///
/// What ships is neither absolute. `CpuSched::pick` takes the dying list ahead
/// of the RT band only once its head has waited
/// [`kernel::sched::cpu::DYING_AGE_NS`], and dispatches it for exactly
/// [`kernel::sched::cpu::DYING_CHUNK_NS`] with `CpuSched::aged_grant` holding the
/// preemption off for that long and no longer. So a ready RT task gives up one
/// chunk, and gives it up **at most once per age window**: the window this
/// bound measures closes the instant the RT task runs, and the corpse's stamp
/// restarts when it is preempted back, so the next aged chunk is a full
/// `DYING_AGE_NS` away. `DYING_AGE_NS`'s own doc carries the inequality that
/// makes that a fact — it has to exceed this bound, and 10 ms against 3.7 ms
/// does.
///
/// **Measured, and conservative rather than load-bearing.** On
/// `scenarios::rt_saturated_retire` seed 0 the aged chunk shows up as exactly
/// one I4 wait of 1,000,000 ns — `DYING_CHUNK_NS` to the nanosecond, read off
/// by forcing this bound to zero and collecting the reported `waited`. The
/// suite would not red without the term, because `2 × RUN_CHUNK_NS` of
/// observation granularity is already wider than the chunk. It is carried
/// anyway: what the kernel gives up is a real millisecond of a real RT task's
/// latency, and a bound that omits a term because the model's own resolution
/// hides it is a bound that will be wrong the moment either number moves.
fn rt_latency_bound(max_kernel_section: u64) -> u64 {
    IPI_LATENCY_NS + max_kernel_section + 2 * RUN_CHUNK_NS + DYING_CHUNK_NS
}

/// How long a retire may take to reach `Hw::release` (invariant I14), measured
/// on the **wall clock** — see [`crate::vm::Killed`] for why there is no
/// second clock any more.
///
/// Hop by hop, from the claim to the call:
///
/// 1. `IPI_LATENCY_NS` — `retire::post` kicks the CPU the word names with
///    `Urgency::Preempt`, and this is how long that delivery may take.
/// 2. `max_kernel_section` — the preempt-off section the target may be inside
///    when the interrupt lands.
/// 3. `QUANTUM_NS` — the pass drains the retire and the victim reaches the
///    dying list, but the pick can only take it once the CPU is free to switch:
///    a *running* task keeps the CPU until its next decision — a fair thread's
///    request or a real-time task's quantum — which is what bounds this hop.
/// 4. **`(1 + peers) × UNWIND_NS × STRETCH`** — the unwind itself, the unwinds
///    already queued ahead of it on that CPU, and the real-time band's bounded
///    share of the same CPU. See [`rt_deferral_stretch`].
/// 5. `QUANTUM_NS` again — `die` publishes `Dead` and leaves the record as this
///    CPU's *zombie*, because a pass cannot free the stack it is standing on;
///    the payload is released by the **next** pass on that CPU
///    (`SchedPass::begin`), and if that CPU dispatched another task its next
///    pass is that task's quantum expiry.
///
/// plus `2 × RUN_CHUNK_NS`, invariant I4's granularity term: the model observes
/// each hop up to one execution chunk late.
///
/// **`peers` is a workload-shaped term.** Two corpses on one CPU means the second
/// waits out the first: one CPU cannot run two unwinds at once, and pretending
/// otherwise would price the machine rather than the protocol. `peers` is the
/// greatest number of *other* corpses that CPU has held since this retire was
/// claimed.
fn retire_latency_bound(max_kernel_section: u64, peers: usize) -> u64 {
    2 * QUANTUM_NS
        + IPI_LATENCY_NS
        + max_kernel_section
        + 2 * RUN_CHUNK_NS
        + (1 + peers as u64) * UNWIND_NS * rt_deferral_stretch()
}

/// By how much a saturated real-time band stretches an unwind's wall-clock
/// length, which is the factor term 4 of [`retire_latency_bound`] carries.
///
/// `CpuSched::pick` delivers an aged corpse one
/// [`kernel::sched::cpu::DYING_CHUNK_NS`] per
/// `DYING_AGE_NS + DYING_CHUNK_NS`, so `UNWIND_NS` of the victim's own CPU time
/// takes that multiple of wall clock to spend when the RT band never empties.
/// It is charged unconditionally because a bound is a worst case, and it is a
/// *finite* factor rather than the unbounded term the previous form of this
/// derivation declined to price at all.
fn rt_deferral_stretch() -> u64 {
    (DYING_AGE_NS + DYING_CHUNK_NS) / DYING_CHUNK_NS
}

pub fn check_all(vm: &mut Vm<'_>) {
    check_single_ownership(vm);
    check_sleeping_cpus(vm);
    check_timers(vm);
    check_rt_latency(vm);
    check_retires(vm);
    let rt_present = note_rt_service(vm);
    check_fairness(vm, rt_present);
    check_waits(vm);
    check_job_counts(vm);
    check_address_spaces(vm);
    check_boost_windows(vm);
    check_work_conservation(vm);
    note_run_latency(vm);
}

/// Each process's fair-band threads, Ready or Running, per CPU: row `process`,
/// column `cpu`. A task in transit is in no band, nor is a corpse or a stopped
/// task: none of them competes for its job's share.
fn fair_per_cpu(vm: &Vm<'_>) -> Vec<u32> {
    let cpus = vm.scenario.cpus();
    let mut counted = vec![0u32; vm.procs.len() * cpus];
    for (cpu, sched) in vm.cpus.iter().enumerate() {
        let running = sched.running().filter(|_| sched.rq().running_is_fair()).map(|t| t.key());
        let ready = sched.rq().tasks().filter(|t| !t.rt().is_rt()).map(|t| t.key());
        for key in running.into_iter().chain(ready) {
            if let Some(process) = vm.process_of(key) {
                counted[process * cpus + cpu] += 1;
            }
        }
    }
    counted
}

/// I1: every live task is in exactly one container system-wide, and its state
/// word agrees with where it is.
///
/// This is what catches the ported old steal: a task carried on an idle CPU's
/// stack is in no container at all, and one installed into the thief's queue
/// has a word that still names the victim.
fn check_single_ownership(vm: &mut Vm<'_>) {
    let mut seen: BTreeMap<TaskKey, (usize, Container)> = BTreeMap::new();
    let mut problems = Vec::new();

    for cpu in 0..vm.scenario.cpus() {
        for (key, container) in residents(&vm.cpus[cpu]) {
            if let Some((other, other_container)) = seen.insert(key, (cpu, container)) {
                problems.push(format!(
                    "I1: {key:?} is in two places at once — cpu{other} {other_container:?} and \
                     cpu{cpu} {container:?}",
                ));
            }
            let state = vm.shared[&key].state();
            let agrees = match (container, state) {
                (Container::Running, TaskState::Running(c))
                | (Container::Ready, TaskState::Ready(c))
                | (Container::Dying, TaskState::Ready(c))
                | (Container::Outbound, TaskState::Ready(c))
                | (Container::Parked, TaskState::Blocked(c))
                | (Container::Parked, TaskState::WakeQueued(c)) => c.0 as usize == cpu,
                // A task that has registered on a watch and not yet parked
                // is still the running value. Exactly two words are legal
                // there: `Committing`, while its own commit is still owed, and
                // `WakeQueued`, once a remote claim has taken it pre-park —
                // `Claim::PrePark`, which posts no message precisely because
                // the waiter has not parked. Both are legal *only* inside that
                // window, which is why this consults the CPU's pending block
                // instead of accepting the words outright: a word that says the
                // task is blocked while its CPU still runs it is otherwise
                // exactly a single-ownership break.
                (Container::Running, TaskState::Committing(c, _))
                | (Container::Running, TaskState::WakeQueued(c)) => {
                    c.0 as usize == cpu && vm.blocking[cpu].is_some()
                }
                (Container::Zombie, TaskState::Dead) => true,
                _ => false,
            };
            if !agrees {
                problems.push(format!(
                    "I1: {key:?} sits in cpu{cpu} {container:?} but its word says {state:?}",
                ));
            }
        }
    }

    // A live task that is in no container must be inside an unconsumed
    // message — which the word records as `InTransit`. Anything else has been
    // dropped on the floor.
    for &key in &vm.live {
        if seen.contains_key(&key) {
            continue;
        }
        match vm.shared[&key].state() {
            TaskState::InTransit(_) => {}
            state => problems.push(format!(
                "I1: {key:?} is in no container and its word says {state:?}",
            )),
        }
    }

    for problem in problems {
        vm.violate(problem);
    }
}

/// I2: a sleeping CPU has nothing to do — or an IPI is on its way to tell it
/// otherwise. This is B4, the ready task stranded on a halted CPU.
fn check_sleeping_cpus(vm: &mut Vm<'_>) {
    let mut problems = Vec::new();
    let (halted, pending) = vm.hw.with(|s| (s.halted.clone(), s.pending_ipi.clone()));
    for cpu in 0..vm.scenario.cpus() {
        // The stopped CPU is the modelled fault, not a protocol break: it is
        // *given* work it never takes, which is the whole of what the scenario
        // stages. Every other CPU still answers for itself.
        if vm.is_stopped(cpu) || !halted[cpu] || pending[cpu] > 0 {
            continue;
        }
        if !vm.cpus[cpu].rq().is_empty() {
            problems.push(format!(
                "I2: cpu{cpu} halted with {} ready task(s) and no IPI pending",
                vm.cpus[cpu].rq().len(),
            ));
        }
        if !vm.cpus[cpu].mailbox_is_empty() {
            problems.push(format!(
                "I2: cpu{cpu} halted with a non-empty mailbox and no IPI pending",
            ));
        }
    }
    for problem in problems {
        vm.violate(problem);
    }
}

/// I14: a retire is prompt, and the balance path does not undo that.
///
/// Two halves of one property, because the protocol's promptness rests on two
/// different things. **A killed task is never migrated**: the retire that would
/// chase it is dropped by a destination that knows the adopt is coming, so a
/// CPU that hands on a task it knows is dead trades an unwind it could start
/// here for a wait on another CPU's pick. **And a retire completes within
/// [`retire_latency_bound`]**.
///
/// **Both halves survive the cancellable kill and only one of them
/// changed.** It makes a killed task *run* rather than be reaped where it
/// lies, so the first half's justification is now about where the unwind can
/// start rather than where the reap can happen — the sentence above is
/// written in those terms and the check is the same check.
/// The second half gained one term and is re-derived at its own definition.
///
/// The first half is what `scenarios::old_migrate_kept_the_corpse` proves has
/// teeth; the second is a bound in the shape of I4's, and it is what a future
/// change that lengthens the retire path would go red on.
fn check_retires(vm: &mut Vm<'_>) {
    let mut problems = Vec::new();

    let migrated: Vec<(TaskKey, u32)> = vm
        .step_trace
        .iter()
        .filter_map(|ev| match ev.kind {
            kernel::sched::hw::TraceKind::Migrate { task, to, .. } => Some((task, to.0)),
            _ => None,
        })
        .collect();
    // Counted on the same walk that judges them: the policy suite's wakeup-storm
    // case claims a machine drains in parallel, and that claim is only about the
    // balance path if the balance path moved something.
    vm.migrations += migrated.len() as u64;
    if vm.exits == 0 {
        vm.migrations_before_exit += migrated.len() as u64;
    }
    for (key, to) in migrated {
        if vm.killed.contains_key(&key) {
            problems.push(format!(
                "I14: {key:?} was killed and then migrated to cpu{to} — a task in transit is \
                 dispatched only by the adopt that carries it, so the unwind waits for the \
                 destination's pick",
            ));
        }
    }

    let keys: Vec<TaskKey> = vm.killed.keys().copied().collect();
    // How many outstanding retires each CPU is holding right now. One CPU runs
    // one unwind at a time, so this is the queue every one of them is behind.
    let mut per_cpu: BTreeMap<usize, usize> = BTreeMap::new();
    for &key in &keys {
        if !vm.live.contains(&key) {
            continue;
        }
        if let Some(cpu) = owner_of(vm.shared[&key].state()) {
            *per_cpu.entry(cpu).or_default() += 1;
        }
    }

    let mut done = Vec::new();
    for key in keys {
        // Remembered while the word still names a CPU, so a victim that has
        // reached `Dead` is still measured against the queue it stood in. The
        // CPU itself is no longer remembered beside it: the second field this
        // block used to write existed to select *whose* per-CPU fair clock to
        // read, and I14 has been on the wall clock since that clock was
        // deleted.
        if let Some(cpu) = owner_of(vm.shared[&key].state()) {
            let peers = per_cpu.get(&cpu).copied().unwrap_or(1) - 1;
            let entry = vm.killed.get_mut(&key).expect("came from the map");
            entry.max_peers = entry.max_peers.max(peers);
        }
        let bound = retire_latency_bound(vm.max_kernel_section(), vm.killed[&key].max_peers);
        let elapsed = retire_elapsed(vm, key);
        // Recorded as a pair, so the number the sweep prints and the bound it
        // is read against are one victim's and not two.
        if elapsed > vm.retire_latency {
            vm.retire_latency = elapsed;
            vm.retire_bound = bound;
        }
        if !vm.live.contains(&key) {
            done.push(key);
            continue;
        }
        if elapsed > bound {
            problems.push(format!(
                "I14: {key:?} was retired {elapsed} ns ago and is still {:?} \
                 (bound {bound} ns, on the wall clock)",
                vm.shared[&key].state(),
            ));
        }
    }
    for key in done {
        vm.killed.remove(&key).expect("came from the map");
    }

    for problem in problems {
        vm.violate(problem);
    }
}

/// Which CPU currently owes this retire, if any. `InTransit` owes it to
/// nobody, which is I14's first half stated as an absence.
fn owner_of(state: TaskState) -> Option<usize> {
    match state {
        TaskState::Running(cpu)
        | TaskState::Ready(cpu)
        | TaskState::Committing(cpu, _)
        | TaskState::Blocked(cpu)
        | TaskState::WakeQueued(cpu) => Some(cpu.0 as usize),
        TaskState::InTransit(_) | TaskState::Dead => None,
    }
}

/// How long this retire has been outstanding: the wall clock, every CPU's
/// and no CPU's, with nothing subtracted from it.
///
/// [`crate::vm::Killed`] carries why there is no second clock any more.
fn retire_elapsed(vm: &Vm<'_>, key: TaskKey) -> u64 {
    vm.clock.since(vm.killed[&key].at)
}

/// I3 / invariant T: the armed deadline is never later than the earliest
/// thing the CPU owes. Delegated to the core's own checker, which
/// is the same code a kernel `feature="check"` build runs.
fn check_timers(vm: &mut Vm<'_>) {
    for cpu in 0..vm.scenario.cpus() {
        let armed = vm.cpus[cpu].armed();
        let quantum = vm.cpus[cpu].running().map(|_| vm.cpus[cpu].decide_at());
        let deadline = vm.cpus[cpu].parked().filter_map(|p| p.deadline()).min();
        let earliest = match (quantum, deadline) {
            (Some(q), Some(d)) => Some(q.min(d)),
            (Some(q), None) => Some(q),
            (None, d) => d,
        };
        match (armed, earliest) {
            (Some(armed), Some(due)) if armed > due => vm.violate(format!(
                "I3: cpu{cpu} armed at {armed:?} but owes an event at {due:?}",
            )),
            (None, Some(due)) => vm.violate(format!(
                "I3: cpu{cpu} owes an event at {due:?} with no timer armed",
            )),
            _ => {}
        }
    }
}

/// I4: an RT task must not sit ready while the CPU that owns it keeps running
/// a normal task — B7, the wake that did not preempt.
fn check_rt_latency(vm: &mut Vm<'_>) {
    let bound = rt_latency_bound(vm.max_kernel_section());
    let mut problems = Vec::new();
    for cpu in 0..vm.scenario.cpus() {
        // `serves_rt_band` and not `is_rt`, for the reason that method's own doc
        // gives: a killed thread that holds the RT right is unwinding, not doing
        // real-time work, so a real-time sibling waiting behind it **is** being
        // starved and this check has to see it. Reading `is_rt` here made the
        // model agree with the defect rather than with the law.
        let starving = vm.cpus[cpu].rq().has_rt()
            && vm.cpus[cpu]
                .running()
                .is_some_and(|task| !task.serves_rt_band());
        match (starving, vm.rt_pending_since[cpu]) {
            (true, None) => vm.rt_pending_since[cpu] = Some(vm.busy_ns[cpu]),
            (true, Some(since)) => {
                let waited = vm.busy_ns[cpu] - since;
                if waited > bound {
                    problems.push(format!(
                        "I4: cpu{cpu} ran a normal task for {waited} ns with an RT task ready \
                         (bound {bound} ns)",
                    ));
                    vm.rt_pending_since[cpu] = Some(vm.busy_ns[cpu]);
                }
            }
            (false, _) => vm.rt_pending_since[cpu] = None,
        }
    }
    for problem in problems {
        vm.violate(problem);
    }
}

/// Mark every process one of whose tasks is currently in the RT band, whether
/// permanently or on a lend, and report whether the band is occupied at all.
/// Both answers are invariant I5's.
///
/// The per-process mark is checked over *all* containers rather than only over
/// running tasks: a client woken with `WakeCause::boosted` is in the RT band
/// from the moment it is queued, and I5 must stop measuring it before it has
/// run out of band, not after. The machine-wide answer counts only Ready and
/// Running, because a *parked* RT task is consuming nothing.
fn note_rt_service(vm: &mut Vm<'_>) -> bool {
    let mut rt = Vec::new();
    let mut occupied = false;
    for cpu in 0..vm.scenario.cpus() {
        let sched = &vm.cpus[cpu];
        if let Some(task) = sched.running() {
            if task.rt().is_rt() {
                rt.push(task.key());
                occupied = true;
            }
        }
        for task in sched.rq().tasks() {
            if task.rt().is_rt() {
                rt.push(task.key());
                occupied = true;
            }
        }
        for parked in sched.parked() {
            if parked.is_rt() {
                rt.push(parked.key());
            }
        }
    }
    for key in rt {
        if let Some(process) = vm.process_of(key) {
            vm.procs[process].rt_service = true;
        }
    }
    occupied
}

/// I5: **equal jobs receive equal service on a machine that holds them alike.**
///
/// Fairness is a statement about service, so this measures service — the
/// nanoseconds the virtual CPUs actually delivered to each process — and not
/// the bookkeeping that is supposed to produce it.
///
/// **The window.** Fairness owes nothing across a block, nor on an unsaturated
/// machine, nor while the RT band is occupied (invariant I4 bounds that). And a
/// job's aggregate share follows its placement (owner decision J1): each CPU
/// splits its own time among the groups it holds, so two jobs placed unalike
/// are owed unalike. So service is compared over a *contention window* — a
/// maximal interval during which the same fair-band processes were runnable,
/// every CPU had a task loaded, the RT band was empty, and every CPU held the
/// same number of each member's runnable threads, which gives every member's
/// group the same weight on every CPU — and restarts when any of those stops
/// holding.
///
/// **The bound is derived.** On each CPU EEVDF holds every group's lag inside
/// one request plus the granularity a decision is taken late by (Stoica and
/// Abdel-Wahab, Theorem 3): the slice, a preempt-off section that overruns it,
/// and the two chunks the model observes an expiry late by. A window's spread
/// on one CPU is at most twice that, plus the lag a group carries in when it
/// re-forms, and the machine's is the sum over its CPUs.
///
/// **What reds a run** is `max(derived bound, lifted ceiling)`; only a control
/// measuring a rejected policy lifts it, and `Vm::fair_over_bound` records
/// every crossing of the derived bound regardless.
fn check_fairness(vm: &mut Vm<'_>, rt_present: bool) {
    vm.fair_window_open = false;
    let cpus = vm.scenario.cpus();
    let per_cpu = fair_per_cpu(vm);
    let saturated = (0..cpus).all(|cpu| vm.cpus[cpu].running().is_some());
    let row = |p: usize| &per_cpu[p * cpus..(p + 1) * cpus];
    let mut members: Vec<usize> = if rt_present || !saturated {
        Vec::new()
    } else {
        (0..vm.procs.len())
            .filter(|&p| row(p).iter().any(|&n| n > 0) && !vm.procs[p].rt_service)
            .collect()
    };
    if !members.iter().all(|&p| row(p).iter().min() == row(p).iter().max()) {
        members.clear();
    }
    if members != vm.fair_epoch.members {
        vm.fair_epoch = FairEpoch { members, base: vm.service_ns.clone() };
        return;
    }
    if members.len() < 2 {
        return;
    }
    let bound = cpus as u64 * 2 * (SLICE_NS + LAG_BOUND_NS + vm.max_kernel_section() + 2 * RUN_CHUNK_NS);
    let served: Vec<u64> = members.iter().map(|&p| vm.service_ns[p] - vm.fair_epoch.base[p]).collect();
    let (low, high) = (served.iter().copied().min().unwrap_or(0), served.iter().copied().max().unwrap_or(0));
    let spread = high - low;
    // Set after the early returns: a window that restarted this step has a
    // zero baseline and separates nothing yet.
    vm.fair_window_open = true;
    if spread > vm.fair_spread {
        vm.fair_spread = spread;
        vm.fair_bound = bound;
    }
    if spread > bound {
        vm.fair_over_bound = vm.fair_over_bound.max(spread);
    }
    let ceiling = bound.max(vm.scenario.fair_allowance_ns);
    if spread > ceiling {
        let detail: Vec<String> =
            members.iter().zip(&served).map(|(&p, ns)| format!("{}={ns}", vm.procs[p].name)).collect();
        vm.violate(format!(
            "I5: {spread} ns of service separates equal jobs over one contention window \
             (bound {bound} ns, lifted ceiling {} ns): {}",
            vm.scenario.fair_allowance_ns,
            detail.join(" "),
        ));
    }
}

/// I13: **a ready fair thread is not passed over without bound**, measured as
/// the service its CPU owed it while it waited ([`Vm::owed_waiting`]): its
/// group's weight over the CPU's summed group weight, shared among the group's
/// threads there, for as long as the CPU ran fair work.
///
/// The bound is the host model's (`kernel/pure/sched/job_sched.rs`): a
/// thread's group waits out the request running ahead of it, the EEVDF lag
/// bound and the lag it carried in, and the thread does the same inside its
/// group — `3 × SLICE + 2 × LAG_BOUND` — plus, at each of the two levels, a
/// preempt-off section and the two chunks a decision is observed late by.
/// Equal-service checks cannot see a group that runs one thread and starves
/// the rest: its job's total is unchanged. This can, and
/// `scenarios::fair_lowest_key_within_group` is its gate.
fn check_waits(vm: &mut Vm<'_>) {
    let bound = 3 * SLICE_NS + 2 * LAG_BOUND_NS + 2 * (vm.max_kernel_section() + 2 * RUN_CHUNK_NS);
    let Some((&key, &owed)) = vm.owed_waiting.iter().max_by(|a, b| a.1.total_cmp(b.1)) else {
        return;
    };
    let owed = owed as u64;
    if owed > vm.wait_owed {
        vm.wait_owed = owed;
        vm.wait_bound = bound;
    }
    if owed > bound {
        vm.wait_over_bound = vm.wait_over_bound.max(owed);
        let at = (0..vm.scenario.cpus())
            .find(|&cpu| residents(&vm.cpus[cpu]).any(|(k, _)| k == key))
            .map_or_else(|| "in transit".to_string(), |cpu| format!("on cpu{cpu}"));
        vm.violate(format!(
            "I13: {key:?} waited {at} through {owed} ns of service its CPU owed it (bound {bound} ns)",
        ));
    }
}

/// I15: **work conservation.** No CPU spends more than
/// [`work_conservation_bound`] protocol rounds ([`crate::vm::Vm::round`]) idle
/// while another holds a task a probe would be handed — a fair band at least
/// [`PROBE_WORTH_IT`] deep in tasks whose masks name the idle CPU.
///
/// The bounded form of the rule rather than a predicate over one step, and the
/// reason is what a step lets the simulator see. Its safety form — no CPU parks
/// unprobed beside stealable work that nobody rang it for — turns on whether a
/// victim's push rang a parked CPU, and a ring on a doorbell whose edge is
/// already raised changes nothing outside the core: it reads exactly as no ring.
/// What is visible is the outcome, an idle CPU and a queue that stays stealable,
/// and a protocol that leaves work unannounced or refuses it keeps that outcome
/// standing round after round. So the bound is on the outcome, and every
/// failure the safety form names fails it: an idle CPU nobody rings, one that is
/// rung and declines to probe, a probe that is never answered.
///
/// **Nothing pauses the count.** A ring or a task on its way to the idle CPU
/// lands after an interrupt's latency, a matter of time, which the rest of the
/// machine could fill with rounds that take none; a round waits on every CPU an
/// IPI is on its way to ([`crate::vm::Vm::round`]), so an answer in flight costs
/// at most the round it was sent in. A CPU that is rung and declines to probe,
/// or whose probe is never answered, accrues every round it stays idle however
/// often it is rung.
///
/// Rounds and not nanoseconds, because the clock measures the explorer as much
/// as the protocol; and a stopped CPU is neither idle nor a victim, its numbers
/// being the modelled fault `stopped_cpu` measures.
fn check_work_conservation(vm: &mut Vm<'_>) {
    let cpus = vm.scenario.cpus();
    let takes = |vm: &Vm<'_>, victim: usize, thief: usize| {
        vm.cpus[victim].rq().tasks().filter(|t| !t.rt().is_rt() && t.affinity().contains(CpuId(thief as u32))).count()
    };
    let victim_for = |vm: &Vm<'_>, thief: usize| {
        (0..cpus).find(|&cpu| !vm.is_stopped(cpu) && takes(vm, cpu, thief) as u32 >= PROBE_WORTH_IT)
    };
    let bound = work_conservation_bound(vm.scenario.balance, cpus).unwrap_or(u64::MAX);
    let completed = vm.round - std::mem::replace(&mut vm.wc_round_seen, vm.round);
    let mut problems = Vec::new();
    for cpu in 0..cpus {
        let idle = !vm.is_stopped(cpu) && vm.cpus[cpu].running().is_none();
        let Some(victim) = victim_for(vm, cpu).filter(|_| idle) else {
            vm.wc_idle[cpu] = None;
            continue;
        };
        let rounds = vm.wc_idle[cpu].get_or_insert(0);
        *rounds += completed;
        let rounds = *rounds;
        vm.wc_rounds = vm.wc_rounds.max(rounds);
        if rounds > bound {
            problems.push(format!(
                "I15: cpu{cpu} has been idle for {rounds} rounds while cpu{victim} holds {} \
                 queued fair task(s) it may run (bound {bound} rounds)",
                takes(vm, victim, cpu),
            ));
        }
    }
    for problem in problems {
        vm.violate(problem);
    }
}

/// How many rounds a CPU may spend idle beside stealable work: `cpus + 2` under
/// [`Balance::PushOnSurplus`], and no bound under a policy that claims none —
/// the pull alone, no balance path and the declined re-arm, the controls
/// `sim/tests/policy.rs` prices the push against.
///
/// **Derived for one victim.** An idle CPU `p` beside a victim `v`'s stealable
/// work counts:
///
/// 1. `cpus` rounds until `p` is rung: the round open when `p` began counting,
///    whose pass of `v` may have come before it, and `cpus − 1` more. `v` runs
///    a task, so it is awake when every round opens and passes in each; every
///    such pass that leaves it stealable rings the first sleeping CPU at or
///    after its cursor and moves the cursor past it, so `p` is reached within
///    `cpus − 1` of them. A `p` the cursor passes because it is awake owes the
///    next round its pass, which probes as term 2's does.
/// 2. One round for `p`'s idle pass. A ring still in flight when the next round
///    opens holds that round open until `p` has passed, so `p` has probed `v`,
///    the only CPU publishing a surplus worth a probe, before it completes.
/// 3. One round for `v`'s answer: `v` passes in the round after the probe was
///    posted, and answers. The answer hands over a task unless that leaves `v`
///    with none to spare — and then nothing is stealable and the count is over
///    — and its post leaves `p` awake or rung, so the next round waits on the
///    pass in which `p` takes it and runs it.
///
/// With several victims the derivation does not hold: each walks its own
/// cursor, and a probe sent to one that runs dry before it answers leaves `p`
/// to wait for a ring again — the flat push's weakness, recorded in
/// `issues/the-push-bounds-an-idle-cpus-wait-only-for-one-victim.md`.
fn work_conservation_bound(balance: Balance, cpus: usize) -> Option<u64> {
    match balance {
        Balance::PushOnSurplus { .. } => Some(cpus as u64 + 2),
        Balance::None | Balance::Pull | Balance::PullWithRearm { .. } => None,
    }
}

/// The measured policy suite's instrument: how long each task waited between
/// being owed a dispatch and getting one (`crate::latency`, `sim/tests/policy.rs`).
///
/// **A measurement and not a verdict**, which is why it returns nothing and
/// fails nothing. Every wait this records is legal in *some* workload — a fair
/// task waits out a saturated RT band, a corpse waits out an aged deferral — so
/// a bound belongs to a scenario and not to the walk, and it is asserted where
/// the scenario is known. What the walk owes is the number.
///
/// **"Owed a dispatch" spans two containers**, and the second is the half a
/// naive instrument loses. A task in a run queue is owed one. So is a *parked*
/// task whose `TaskState::WakeQueued` says a wake has already claimed it and
/// posted its `Msg::Wake`: a wake's latency starts at that claim, and an
/// instrument that started at the enqueue would measure the run queue and call
/// the answer a wake latency.
///
/// A task in transit keeps its stamp, because its migration is part of the wait.
/// A task in the dying list drops it: a corpse's wait for the CPU is invariant
/// I14's quantity, on I14's clock and against I14's bound, and counting it twice
/// under a second name would make a policy bound answer for a teardown.
fn note_run_latency(vm: &mut Vm<'_>) {
    let now = vm.clock;
    let mut seen: BTreeMap<TaskKey, Container> = BTreeMap::new();
    for cpu in 0..vm.scenario.cpus() {
        for (key, container) in residents(&vm.cpus[cpu]) {
            seen.insert(key, container);
        }
    }

    let mut dispatched: Vec<(TaskKey, ReadyCause, u64)> = Vec::new();
    for (&key, &container) in &seen {
        let claimed_wake = container == Container::Parked
            && matches!(vm.shared[&key].state(), TaskState::WakeQueued(_));
        if container == Container::Ready || claimed_wake {
            if !vm.awaiting.contains_key(&key) {
                let cause = match vm.prev_container.get(&key) {
                    // Off the CPU at its own quantum, or handed back by an RT
                    // preemption: waiting for its next turn in the fair band.
                    Some(Container::Running) => ReadyCause::Preempted,
                    Some(Container::Parked) => ReadyCause::Woken,
                    // Never seen anywhere: an `Adopt` that has just landed, which
                    // is spawn placement rather than either of the above.
                    None => ReadyCause::Fresh,
                    // A task cannot arrive in a run queue *from* one, and the
                    // remaining containers are terminal for this instrument —
                    // `Stopped` most of all, which nothing leaves. `Outbound`
                    // leaves by an `Adopt`, so it is never the last seen.
                    Some(
                        Container::Ready
                        | Container::Dying
                        | Container::Stopped
                        | Container::Outbound
                        | Container::Zombie,
                    ) => ReadyCause::Fresh,
                };
                vm.awaiting.insert(key, (now, cause, vm.round));
            }
        } else if container == Container::Running {
            if let Some((since, cause, round)) = vm.awaiting.remove(&key) {
                vm.rounds_to_run = vm.rounds_to_run.max(vm.round - round);
                dispatched.push((key, cause, now.since(since)));
            }
        } else {
            vm.awaiting.remove(&key);
        }
    }
    for (key, cause, ns) in dispatched {
        if let Some(process) = vm.process_of(key) {
            vm.run_wait[process].note(cause, ns);
        }
    }

    // A task in no container at all is inside a message (invariant I1 requires
    // its word to say `InTransit`), which is a hop of the wait and not the end
    // of it. Anything else has stopped being a task.
    vm.awaiting.retain(|key, _| {
        seen.contains_key(key)
            || matches!(
                vm.shared.get(key).map(|shared| shared.state()),
                Some(TaskState::InTransit(_))
            )
    });
    vm.prev_container = seen;
}

/// I6: each job's runnable count equals its threads in a fair band, Ready or
/// Running, on every CPU. The count and the bands are driven by the same moves,
/// so a drift means a transition forgot one of the two.
fn check_job_counts(vm: &mut Vm<'_>) {
    let mut in_band: BTreeMap<kernel::sched::job::JobKey, u32> = BTreeMap::new();
    for sched in &vm.cpus {
        let running = sched.running().filter(|_| sched.rq().running_is_fair()).map(|t| t.job().key());
        for job in running.into_iter().chain(sched.rq().tasks().filter(|t| !t.rt().is_rt()).map(|t| t.job().key())) {
            *in_band.entry(job).or_default() += 1;
        }
    }
    let mut problems = Vec::new();
    for process in &vm.procs {
        for job in &process.jobs {
            let expected = in_band.get(&job.key()).copied().unwrap_or(0);
            if job.runnable() != expected {
                problems.push(format!(
                    "I6: {}'s job {:?} counts {} runnable thread(s) but {expected} are in a fair band",
                    process.name,
                    job.key(),
                    job.runnable(),
                ));
            }
        }
    }
    for problem in problems {
        vm.violate(problem);
    }
}

/// I8: the mock address space's refcount equals the number of live tasks that
/// reference it, plus the process's own reference while it still holds one.
/// The double-drop detector.
fn check_address_spaces(vm: &mut Vm<'_>) {
    let mut problems = Vec::new();
    for process in 0..vm.procs.len() {
        let Some(space) = vm.procs[process].address_space.as_ref() else {
            continue;
        };
        let expected = vm.procs[process].live.len() + 1;
        // Counted through a borrow, never a clone: a clone would be one more
        // reference and the check would be measuring itself.
        let actual = StdArc::strong_count(space);
        if actual != expected {
            problems.push(format!(
                "I8: {}'s address space has {actual} reference(s), expected {expected}",
                vm.procs[process].name,
            ));
        }
    }
    for problem in problems {
        vm.violate(problem);
    }
}

/// I9: **one lend buys at most one quantum of running time at the borrowed
/// priority.**
///
/// Tested against `Vm::boosted_run`'s cumulative *running* residency per lend,
/// never by comparing a running task's `until` to the clock: `RtState::arm`
/// makes a re-armed `until` fresh by construction, so that form passes for the
/// same reason it stops measuring anything. `old_park_kept_the_lend` is the
/// standing negative gate that holds this one honest.
///
/// Queue time is deliberately outside the bound — waiting holds nothing —
/// which is why the accumulator only advances while the task is `Running`.
fn check_boost_windows(vm: &mut Vm<'_>) {
    // One quantum is the grant. The slack on top is measurement, not licence:
    // a preempt-off section can overrun the quantum it started inside, and the
    // model advances the clock in chunks, so the quantum's expiry is observed
    // one chunk late and the pass that acts on it lands a chunk after that.
    let bound = QUANTUM_NS + vm.max_kernel_section() + 2 * RUN_CHUNK_NS;
    let problems: Vec<String> = vm
        .boosted_run
        .iter()
        .filter(|(_, (_, ns))| *ns > bound)
        .map(|(key, (lends, ns))| {
            format!(
                "I9: {key:?} has run {ns} ns at a borrowed priority on lend #{lends} \
                 (bound {bound} ns) — one lend must buy at most one quantum",
            )
        })
        .collect();
    for problem in problems {
        vm.violate(problem);
    }
}

/// I7 and I10, checked once at the end of a run rather than after every step:
/// every task finalized exactly once, nothing left queued, and the accounting
/// adds up to the time the CPUs actually spent.
/// A task the run left behind on the CPU that stopped, which I10 does not judge:
/// a machine with a CPU that takes no passes leaves everything given to it
/// unfinished by construction, and that count is the *measurement*
/// ([`crate::explore::Outcome::never_ran`]) rather than a violation.
fn stranded_on_the_stopped_cpu(vm: &Vm<'_>, key: TaskKey) -> bool {
    let Some(stopped) = vm.scenario.stopped else {
        return false;
    };
    let on = match vm.shared[&key].state() {
        TaskState::InTransit(cpu)
        | TaskState::Ready(cpu)
        | TaskState::Running(cpu)
        | TaskState::Blocked(cpu)
        | TaskState::WakeQueued(cpu)
        | TaskState::Committing(cpu, _) => cpu,
        TaskState::Dead => return false,
    };
    on.0 as usize == stopped
}

pub fn check_final(vm: &mut Vm<'_>) {
    let mut problems = Vec::new();

    let live: Vec<TaskKey> = vm
        .live
        .iter()
        .copied()
        .filter(|&key| !stranded_on_the_stopped_cpu(vm, key))
        .collect();
    if !live.is_empty() {
        let stuck: Vec<String> = live
            .iter()
            .map(|key| {
                let state = vm.shared[key].state();
                // The kill bit is the discriminator between a lost wake and a
                // lost retire — two different bugs with one symptom.
                if vm.shared[key].kill_pending() {
                    format!("{key:?}={state:?} killed")
                } else {
                    format!("{key:?}={state:?}")
                }
            })
            .collect();
        problems.push(format!(
            "I10: the run quiesced with {} task(s) never finalized: {}",
            live.len(),
            stuck.join(", "),
        ));
    }
    for cpu in 0..vm.scenario.cpus() {
        if vm.is_stopped(cpu) {
            continue;
        }
        if !vm.cpus[cpu].rq().is_empty() {
            problems.push(format!("I10: cpu{cpu} quiesced with a non-empty run queue"));
        }
        if !vm.cpus[cpu].mailbox_is_empty() {
            problems.push(format!("I10: cpu{cpu} quiesced with a non-empty mailbox"));
        }
        if vm.cpus[cpu].running().is_some() {
            problems.push(format!("I10: cpu{cpu} quiesced with a task still running"));
        }
    }
    for (index, queue) in vm.queues.iter().enumerate() {
        if queue.queue.threads() != 0 {
            problems.push(format!(
                "I10: queue{index} quiesced with {} registration(s) left behind",
                queue.queue.threads(),
            ));
        }
    }

    let accounted: u64 = vm.finalized.iter().map(|(_, acct)| acct.cpu_ns).sum();
    let executed: u64 = vm.busy_ns.iter().sum();
    if accounted != executed {
        problems.push(format!(
            "I7: tasks accounted {accounted} ns of CPU but the CPUs executed {executed} ns",
        ));
    }

    for problem in problems {
        vm.violate(problem);
    }
}
