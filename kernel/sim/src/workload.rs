//! The workload script DSL — the opcode set a thread script is written in; see
//! [`Op`].
//!
//! A scenario is *data*: CPUs, waitable objects, processes and their thread
//! scripts. Everything a scenario can express is something the kernel's own
//! blocking sites do, so a scenario that passes is a statement about the
//! protocol rather than about the harness.
//!
//! Futexes get no opcode of their own: a futex bucket *is* a `Watch`, so a
//! futex storm is `Block`/`Wake` on an object whose class is `Futex`. Giving it a
//! second opcode would be modelling a second wake path, and there is exactly
//! one.

use kernel::sched::cpu::Balance;
use kernel::sched::task::WaitClass;

use crate::topology::Shape;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// Consume CPU time. The VM splits it into chunks so that a quantum can
    /// expire in the middle of one.
    Run(u64),
    /// Preempt-disabled kernel work: consumed atomically, so it bounds RT
    /// wake latency exactly as a real preempt-off section does (invariant
    /// I4's `max KernelSection` term).
    KernelSection(u64),
    /// The uniform blocking shape: try, register, re-check, park.
    Block {
        queue: usize,
        deadline: Option<u64>,
    },
    /// Make the queue's condition true and wake a waiter (or all of them).
    Wake {
        queue: usize,
        all: bool,
        /// Lend the woken task RT for this long — soundserver signalling its
        /// clients.
        boost: Option<u64>,
    },
    Yield,
    /// Start another thread of this process from the process's template list.
    Spawn {
        template: usize,
    },
    /// Become RT permanently (the privilege-gated syscall).
    SetRt,
    /// Set the running thread's own mask to the CPUs whose bits are set, as
    /// `SYS_THREAD_SET_AFFINITY` does: published, then its CPU kicked.
    SetAffinity(u64),
    /// Retire every *other* thread of this process, then exit: process
    /// teardown, the shape the recorded double-drop died in.
    Teardown,
    Exit,
}

#[derive(Clone, Debug)]
pub struct Script {
    pub ops: Vec<Op>,
    /// How many times to run `ops` before falling off the end (which exits).
    pub repeat: usize,
}

impl Script {
    pub fn new(ops: Vec<Op>) -> Self {
        Self { ops, repeat: 1 }
    }

    pub fn looping(ops: Vec<Op>, repeat: usize) -> Self {
        Self { ops, repeat }
    }
}

#[derive(Clone, Debug)]
pub struct ProcSpec {
    pub name: &'static str,
    /// Threads started with the process.
    pub initial: Vec<usize>,
    /// Scripts a `Spawn` op can instantiate, indexed by `template`.
    pub templates: Vec<Script>,
    /// Threads of this process start out real-time (soundserver).
    pub rt: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct QueueSpec {
    pub class: WaitClass,
}

/// A periodic device interrupt — the audio card's completion IRQ. Delivery is
/// a step the explorer schedules, so its position relative to everything else
/// is part of the search space.
#[derive(Clone, Copy, Debug)]
pub struct IrqSpec {
    pub period_ns: u64,
    pub queue: usize,
    pub boost_ns: Option<u64>,
}

/// Where phase 2 of the wait handshake — the `Committing(gen) → Blocked` CAS —
/// runs relative to the blocking pass.
///
/// This is a scenario dimension rather than a constant because the kernel has
/// had two of these answers, and the difference between them was a real lost
/// wake (commit `8508b37`). The VM makes the *step boundary* the thing that
/// moves: a remote CPU can act between two steps and cannot act inside one, so
/// which side of the boundary the commit falls on is exactly what decides
/// whether the window is reachable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockShape {
    /// The shipped shape: the commit runs inside the pass, after its mailbox
    /// drain. Every claim then lands on one side of the drain or the other.
    CommitInPass,
    /// The kernel before `8508b37`: the commit runs at the call site, so the
    /// task's word reads `Blocked` while it is still the running task and its
    /// own CPU has not yet drained. See `scenarios::old_commit_before_pass`.
    CommitAtCallSite,
    /// `CommitAtCallSite` with the call site and the pass **fused into one
    /// step** — the *simulator's* own shape until the split. Nothing can
    /// interleave, so the window is outside the step relation and the bug is
    /// invisible. It exists so that the harness's blind spot is a test rather
    /// than a comment (`blind_spot_needed_the_step_split`).
    CommitAtCallSiteFused,
}

/// Whether an *involuntary* scheduler pass can land inside the registration
/// window — between phase 1 of the wait handshake and phase 2.
///
/// The window is two steps, so an interrupt can arrive in the middle of it;
/// `DeliverIpi` and `FireTimer` are enabled there and set `need_resched`. What
/// this decides is whether the pass that request asks for may *run* before the
/// commit, which in the kernel is decided by the preempt count and by nothing
/// else. It is a scenario dimension rather than a constant for the same reason
/// [`BlockShape`] is: the kernel has had both answers, and the difference
/// between them was a live panic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowShape {
    /// The kernel's `sched::driver::Ticket` holds the preempt count raised for
    /// the whole window, so the request is deferred to the blocking pass.
    PreemptOff,
    /// No guard: the `preempt::enable` at the tail of any lock the re-check
    /// takes drops the count to zero and runs a pass on a task whose word
    /// reads `Committing`. See `scenarios::old_preemptible_window`.
    Preemptible,
}

/// What a `park` does to a borrowed RT window.
///
/// A scenario dimension rather than a constant for the same reason
/// [`BlockShape`] and [`WindowShape`] are: the kernel has had both answers, and
/// the difference between them is invariant I9's whole content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParkShape {
    /// The shipped shape: a block ends the hold outright, whatever the clock
    /// says.
    ReleaseLend,
    /// Commit `9c2fc4d`: clear only `if now >= until`, so a lend blocked on
    /// before it ran out survives the block and `RtState::arm` re-arms it at the
    /// next dispatch. One lend then buys unbounded RT.
    /// See `scenarios::old_park_kept_the_lend`.
    KeepLapsedLend,
}

/// What the balance path does with a ready task whose kill bit is already set.
///
/// A scenario dimension rather than a constant for the same reason
/// [`BlockShape`] and [`ParkShape`] are: the kernel has had both answers, and
/// the difference between them was a live panic on the owner's T14 —
/// `retire_task: task not released after 1s: InTransit(CpuId(1))`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MigrateShape {
    /// The retire's promptness carried into the balance path: a killed task is
    /// kept by the CPU that holds it and dispatched there, never handed on.
    /// The name predates the dispatch replacing the reap; the promptness
    /// argument is unchanged by that.
    ReapTheCorpse,
    /// The balance path before it read the kill bit: a killed ready task is
    /// migrated like any other, and its unwind then waits for the destination's
    /// pick. See `scenarios::old_migrate_kept_the_corpse`.
    KeepTheCorpse,
}

/// How the pick weighs a ready real-time task against a corpse waiting to
/// unwind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgeShape {
    /// What ships: the RT band goes first until the head of the dying list has
    /// waited `DYING_AGE_NS`, and then that corpse takes one `DYING_CHUNK_NS`
    /// ahead of it. Bounded in both directions.
    BoundedDeferral,
    /// The shape this branch shipped between the two fixes: `pick` asks only
    /// `rq.has_rt()`, so a permanently-RT thread that never parks holds the
    /// dying list closed for ever. See `scenarios::old_rt_starved_the_corpse`.
    RtOutranksEveryCorpse,
}

/// What a job is: the owner a thread's CPU time is shared out by.
///
/// A scenario dimension rather than a constant for the same reason
/// [`BlockShape`] and [`ParkShape`] are, except that the other answer is one
/// the design *rejected* rather than one it shipped: a share per thread lets a
/// process buy CPU by forking. Invariant I5 has to be able to tell the two
/// apart, or it is not measuring the shipped policy at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobShape {
    /// The shipped policy: one [`kernel::sched::job::Job`] per process,
    /// held by every thread of it.
    PerProcess,
    /// One job per thread. See `scenarios::fair_job_per_thread`.
    PerThread,
}

/// What a running thread's job group is charged for the time it runs.
///
/// The honest answer is "the time it ran", and it is the core that charges it
/// (`SchedPass::begin`). The other charges a named process's group twice, the
/// shape of a charge applied at two transitions instead of one — and the shape
/// invariant I5 must notice, because a job whose group is charged past its
/// service is a job throttled for work it never did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChargeShape {
    Honest,
    /// See `scenarios::fair_double_charge`.
    Double { process: &'static str },
}

/// Which of a job group's threads the fair band runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupPick {
    /// What ships: EEVDF over the group's threads.
    Eevdf,
    /// The group's lowest-keyed thread, every time, so one thread takes all of
    /// its job's time on its CPU while its siblings wait. See
    /// `scenarios::fair_lowest_key_within_group`.
    LowestKey,
}

/// Where a spawn is placed.
///
/// A scenario dimension rather than a constant because the shipped answer is a
/// *policy*, and it is the one that decides which machines the rest of the
/// scheduler is ever measured on: least-loaded-with-rotation spreads every
/// burst by construction, so under it a machine whose runnable threads all sit
/// on one CPU is not expressible from a workload at all — and that machine is
/// exactly what the steal request's pull half exists for. Both answers below
/// are the kernel's own code either way; what this decides is which of the two
/// numbers `Vm::spawn` places by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlacementShape {
    /// The shipped policy, `kernel::sched::driver`'s `placement`: the
    /// least-loaded CPU from the published counters, ties rotating so a freshly
    /// booted system does not put everything on cpu0.
    LeastLoadedRotating,
    /// Every spawn onto one named CPU, whatever the counters say.
    ///
    /// **Not a policy this kernel has ever had, and that is the point.** It is
    /// the adversary: the lopsided machine no legal placement produces and no
    /// workload can otherwise stage, so that what the balance path does with
    /// one is a measurement rather than a belief. See
    /// `scenarios::lopsided_placement`.
    AllOn(usize),
    /// Spawn `i` onto the `i`th CPU listed, whatever the counters say: an
    /// adversary like [`PlacementShape::AllOn`], for a machine no workload
    /// stages. See `scenarios::group_imbalance`.
    Listed(&'static [usize]),
}

/// What a pass's published surplus says to a CPU choosing whom to probe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurplusShape {
    /// The core's own: every pass publishes the fair band it could give away,
    /// the number its push rang on.
    Published,
    /// Every pass's published surplus is overwritten with zero once it ends,
    /// after its push rang a sleeper on the real one: the CPU it rang wakes,
    /// finds nothing worth a probe and sleeps again. An adversary like
    /// [`PlacementShape::AllOn`] — the defect two spellings of one inequality
    /// write (`SchedPass::best_victim`), staged from outside the core, where a
    /// rung CPU declines to probe.
    Withheld,
}

/// Which teardown/balance algorithm the VM drives the core with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    /// This spec's protocol: balance by `StealRequest` message, retire by
    /// message to the home CPU named in the state word.
    New,
    /// The OLD kernel's: an idle CPU pops a ready task straight out of a
    /// sibling's queue and carries it, unlocked, on its own stack; a retirer
    /// scans every container and treats "not found" as proof of absence.
    /// See `scenarios::old_steal_port`.
    OldSteal,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    /// The machine: its CPUs, and the cache tree they sit in.
    pub topology: Shape,
    pub queues: Vec<QueueSpec>,
    pub procs: Vec<ProcSpec>,
    pub irqs: Vec<IrqSpec>,
    pub protocol: Protocol,
    pub block: BlockShape,
    pub window: WindowShape,
    pub park: ParkShape,
    pub migrate: MigrateShape,
    pub age: AgeShape,
    pub job: JobShape,
    pub charge: ChargeShape,
    pub placement: PlacementShape,
    /// A CPU that takes no scheduler pass for the whole run — the machine a
    /// shed core leaves, where everything placed on it afterwards is lost.
    ///
    /// **An adversary and not a policy**, exactly like [`PlacementShape::AllOn`]:
    /// nothing in the protocol produces one, and what the rest of the scheduler
    /// does about it is a measurement only while it can be staged. The CPU keeps
    /// whatever it last published, which is what makes it the one every
    /// least-loaded reader prefers.
    pub stopped: Option<usize>,
    /// What the balance path does. A scenario dimension for [`PlacementShape`]'s
    /// reason and with the same roles: the shipped answer
    /// ([`Balance::PushOnSurplus`], which is what `kernel::sched::driver::env`
    /// selects and the scenario default names), the control that says what the
    /// measurement would be without it ([`Balance::None`]), and the two
    /// policies the decision was priced across ([`Balance::Pull`], the pull
    /// half alone, and [`Balance::PullWithRearm`], the declined cure).
    ///
    /// **The core's own type rather than a parallel copy of it**: every setting
    /// is a policy the core implements, so the simulator drives the real code
    /// whichever one a scenario names.
    pub balance: Balance,
    pub surplus: SurplusShape,
    /// Which of a group's threads the fair band runs. The broken pick lives in
    /// `queue.rs` behind `protocol-port`, so the kernel cannot reach it and the
    /// simulator drives the real code either way.
    pub group_pick: GroupPick,
    /// What one scheduler pass is modelled to cost. Zero everywhere but
    /// `scenarios::overlong_pass`; see [`crate::hw_impl::SimHwState`].
    pub pass_cost_ns: u64,
    /// Invariant I5's ceiling, lifted above the derived bound only by a control
    /// that measures what a rejected policy does rather than asserting that it
    /// fails. Zero means the derived bound is the ceiling.
    pub fair_allowance_ns: u64,
    /// Safety net: a run that has not quiesced by here is reported as a
    /// non-termination failure rather than looping forever.
    pub max_steps: usize,
    /// Cap on concurrently live tasks, so a spawn storm stays bounded.
    pub max_tasks: usize,
}

impl Scenario {
    pub fn cpus(&self) -> usize {
        self.topology.cpus()
    }

    /// The longest preempt-off section any thread of this scenario runs. It
    /// is a *term* of invariant I4's RT latency bound, not an excuse for it:
    /// making the budget visible is what stops "the sim cannot see kernel
    /// critical sections" from being a blind spot.
    pub fn max_kernel_section(&self) -> u64 {
        self.procs
            .iter()
            .flat_map(|p| p.templates.iter())
            .flat_map(|s| s.ops.iter())
            .filter_map(|op| match op {
                Op::KernelSection(ns) => Some(*ns),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}

impl Scenario {
    pub fn with_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// The same workload on another machine.
    pub fn with_topology(mut self, topology: Shape) -> Self {
        self.topology = topology;
        self
    }

    pub fn with_block(mut self, block: BlockShape) -> Self {
        self.block = block;
        self
    }

    pub fn with_window(mut self, window: WindowShape) -> Self {
        self.window = window;
        self
    }

    pub fn with_park(mut self, park: ParkShape) -> Self {
        self.park = park;
        self
    }

    pub fn with_migrate(mut self, migrate: MigrateShape) -> Self {
        self.migrate = migrate;
        self
    }

    pub fn with_age(mut self, age: AgeShape) -> Self {
        self.age = age;
        self
    }

    pub fn with_job(mut self, job: JobShape) -> Self {
        self.job = job;
        self
    }

    pub fn with_charge(mut self, charge: ChargeShape) -> Self {
        self.charge = charge;
        self
    }

    /// **Checked here rather than at the placement site**, which runs once per
    /// spawn: a CPU index outside the machine is a scenario that was written
    /// wrong, and a panic at the first spawn of a sweep would name the VM
    /// instead of the scenario that asked for it.
    pub fn with_placement(mut self, placement: PlacementShape) -> Self {
        let named: &[usize] = match &placement {
            PlacementShape::LeastLoadedRotating => &[],
            PlacementShape::AllOn(cpu) => std::slice::from_ref(cpu),
            PlacementShape::Listed(cpus) => cpus,
        };
        for cpu in named {
            assert!(
                *cpu < self.cpus(),
                "{}: placement names cpu{cpu} on a {}-cpu machine",
                self.name,
                self.cpus(),
            );
        }
        self.placement = placement;
        self
    }

    /// Checked here for [`Scenario::with_placement`]'s reason: a CPU index
    /// outside the machine is a scenario written wrong, and the first spawn of a
    /// sweep is the wrong place to find out.
    pub fn with_stopped(mut self, cpu: usize) -> Self {
        assert!(
            cpu < self.cpus(),
            "{}: stopping cpu{cpu} on a {}-cpu machine",
            self.name,
            self.cpus(),
        );
        self.stopped = Some(cpu);
        self
    }

    pub fn with_balance(mut self, balance: Balance) -> Self {
        self.balance = balance;
        self
    }

    pub fn with_surplus(mut self, surplus: SurplusShape) -> Self {
        self.surplus = surplus;
        self
    }

    pub fn with_group_pick(mut self, pick: GroupPick) -> Self {
        self.group_pick = pick;
        self
    }

    pub fn with_pass_cost(mut self, ns: u64) -> Self {
        self.pass_cost_ns = ns;
        self
    }

    pub fn with_fair_allowance(mut self, ns: u64) -> Self {
        self.fair_allowance_ns = ns;
        self
    }

    /// The index of a process by name, for the dimensions that name one.
    pub fn process_index(&self, name: &str) -> Option<usize> {
        self.procs.iter().position(|p| p.name == name)
    }
}
