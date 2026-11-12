//! Loom: a parked task's record, taken by the claim that ends its park.
//!
//! The parking pass writes the record into the task's own slot and moves its
//! word `Committing -> Blocked`, and the CPU's switch then saves the task's
//! context and clears its on-CPU word with release. Whoever claims
//! `Blocked -> WakeQueued` may take the record and send the task to an idle
//! CPU only once it reads that word clear; before then the wake goes home,
//! whose next pass follows the switch. The models run every pass's switch
//! ([`Silent`]), so a CPU that resumes a context its home has not saved is a
//! race on that context's cell.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering as StdOrdering};

use kernel_loom::sched::cpu::{Action, Balance, CpuHandle, CpuHandles, CpuSched, Env, SchedPass};
use kernel_loom::sched::hw::{Hw, Nanos};
use kernel_loom::sched::mailbox::mailbox;
use kernel_loom::sched::park::{notify, prepare, Cancel, Commit, CommittedTicket};
use kernel_loom::sched::task::{Notify, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use kernel_loom::sched::topology::Topology;
use kernel_loom::sched_model::{
    model, IrqGuard, ModelCtx, Owner, Payload, RemoteGuard, Silent, TaskMsg, CPU0, CPU1, NOW,
};
use loom::sync::Arc;

/// The task that parks and is woken.
const KEY: TaskKey = TaskKey(1);
/// The task that keeps its home busy in the fair band, which is what sends a
/// wake that finds the park done to the idle CPU, and what keeps the home's
/// passes out of `try_sleep`'s retry (`loom_retire.rs` says why that matters).
const OTHER: TaskKey = TaskKey(2);

/// The stack of the thread that is cpu1.
const CPU_STACK: usize = 256 * 1024;

/// cpu0, the home, running `KEY` with `OTHER` queued behind it, and cpu1
/// asleep. `OTHER` is queued before any race, so no pass in one posts to cpu1
/// while cpu1 passes: a pass that ends idle retries for as long as a post to
/// it is in flight, which in a model is unbounded (`loom_retire.rs`).
struct Machine {
    owner: Arc<Owner>,
    home: CpuSched<Payload>,
    /// `None` while the thread that is cpu1 holds it.
    idle: Option<CpuSched<Payload>>,
    shared: Arc<TaskShared<TaskMsg>>,
}

/// cpu1's scheduler state, handed to the thread that is cpu1 for a race and
/// back after it.
struct Cpu(CpuSched<Payload>);

// SAFETY: one thread at a time is the CPU that owns it: the model's main
// thread before the spawn and after the join, the spawned thread in between.
#[allow(unsafe_code)]
unsafe impl Send for Cpu {}

impl Machine {
    fn new() -> Self {
        let (tx0, rx0) = mailbox::<TaskMsg>();
        let (tx1, rx1) = mailbox::<TaskMsg>();
        let owner = Arc::new(Owner {
            cpus: CpuHandles::new(vec![CpuHandle::new(CPU0, tx0), CpuHandle::new(CPU1, tx1)], Topology::flat(2)),
            hw: Silent,
        });
        let mut machine = Self {
            home: CpuSched::new(CPU0, rx0, ModelCtx::live()),
            idle: Some(CpuSched::new(CPU1, rx1, ModelCtx::live())),
            shared: owner.spawn(CPU0, KEY),
            owner,
        };
        pass(&machine.owner, &mut machine.home, NOW);
        machine.owner.spawn(CPU0, OTHER);
        pass(&machine.owner, &mut machine.home, NOW);
        assert_eq!(machine.home.rq().keys().collect::<Vec<_>>(), [OTHER], "queued behind KEY");
        let owner = machine.owner.clone();
        pass(&owner, machine.idle(), NOW);
        assert!(owner.cpus.get(CPU1).doorbell().sleeping_unrung(), "cpu1 sleeps");
        machine
    }

    fn idle(&mut self) -> &mut CpuSched<Payload> {
        self.idle.as_mut().expect("cpu1 is back from its thread")
    }

    fn park(&mut self, committed: CommittedTicket<TaskMsg>, deadline: Option<Nanos>) {
        let action = SchedPass::begin(&mut self.home, env(&self.owner), NOW).dispose_block(committed, deadline).finish();
        execute(&self.owner, action);
    }

    /// Where `KEY` is once both CPUs have passed at `now`: at home or on
    /// cpu1, and never both or neither.
    fn sent_on(&mut self, now: Nanos) -> bool {
        pass(&self.owner, &mut self.home, now);
        let owner = self.owner.clone();
        pass(&owner, self.idle(), now);
        let at_home = self.home.running().map(|t| t.key()) == Some(KEY) || self.home.rq().keys().any(|key| key == KEY);
        let sent_on = self.idle().running().map(|t| t.key()) == Some(KEY);
        assert!(at_home != sent_on, "the task is in {} CPUs' containers", if at_home { 2 } else { 0 });
        if sent_on {
            assert_eq!(self.shared.state(), TaskState::Running(CPU1));
        }
        sent_on
    }

    /// Every task dies by the only death there is, so the model leaves
    /// nothing alive.
    fn wind_down(mut self, now: Nanos) {
        let idle = self.idle.as_mut().expect("cpu1 is back from its thread");
        for cpu in [&mut self.home, idle] {
            pass(&self.owner, cpu, now);
            while cpu.running().is_some() {
                let action = SchedPass::begin(&mut *cpu, env(&self.owner), now).dispose_exit().finish();
                execute(&self.owner, action);
            }
            pass(&self.owner, cpu, now);
        }
        assert_eq!(self.shared.state(), TaskState::Dead);
    }
}

fn env(owner: &Owner) -> Env<'_, Silent, IrqGuard> {
    // A pass runs from the IRQ-exit path, which cannot be preempted (N3).
    Env { hw: &owner.hw, cpus: &owner.cpus, preempt: &IrqGuard, balance: Balance::None }
}

/// A pass, and the switch it ends in.
fn pass(owner: &Owner, cpu: &mut CpuSched<Payload>, now: Nanos) {
    let action = SchedPass::begin(cpu, env(owner), now).dispose_none().finish();
    execute(owner, action);
}

#[allow(unsafe_code)]
fn execute(owner: &Owner, action: Action<Payload>) {
    if let Action::Run(token) = action {
        // SAFETY: the token is from the pass that just finished, and its CPU
        // has run no pass since.
        unsafe { owner.hw.switch(token) };
    }
}

fn committed(machine: &Machine) -> Commit<TaskMsg> {
    let current = machine.home.current_task().expect("the task is running");
    prepare(&current, Cancel::Answers, WaitClass::Pipe).expect("nothing has posted to this task").commit()
}

/// **A claim sends a parked task on only once its home has saved its
/// context.** cpu1 is the claimant: it claims `KEY` while cpu0 commits, parks
/// and switches away from it, and then passes, dispatching whatever it was
/// sent. A claim before the park is answered by the home's own commit; one
/// after the park and before the switch goes home; one after the switch sends
/// the task to cpu1, which resumes the context the switch saved. Each of the
/// three is reached.
#[test]
fn a_claim_sends_a_task_on_only_once_its_home_has_saved_it() {
    static PRE_PARK: AtomicBool = AtomicBool::new(false);
    static WENT_HOME: AtomicBool = AtomicBool::new(false);
    static SENT_ON: AtomicBool = AtomicBool::new(false);

    model(|| {
        let mut machine = Machine::new();
        let current = machine.home.current_task().expect("the task is running");
        let ticket = prepare(&current, Cancel::Answers, WaitClass::Pipe).expect("nothing has posted to this task");
        let claimant = {
            let owner = machine.owner.clone();
            let shared = machine.shared.clone();
            let mut cpu1 = Cpu(machine.idle.take().expect("cpu1 is here"));
            // A pass and a switch, and the report of a panic in them, need more
            // than loom's default coroutine stack.
            loom::thread::Builder::new().stack_size(CPU_STACK).spawn(move || {
                let notified =
                    notify(&shared, WakeCause::new(WakeReason::Woken), &owner.cpus, &owner.hw, &RemoteGuard);
                pass(&owner, &mut cpu1.0, NOW);
                let ran = cpu1.0.running().map(|t| t.key()) == Some(KEY);
                (cpu1, notified, ran)
            })
            .expect("loom spawns a thread")
        };
        match ticket.commit() {
            Commit::Parked(committed) => machine.park(committed, None),
            Commit::AlreadyWoken => {}
            Commit::Killed => unreachable!("nothing kills in this model"),
        }
        let (cpu1, notified, ran) = claimant.join().unwrap();
        machine.idle = Some(cpu1.0);
        machine.sent_on(NOW);
        match notified {
            Notify::Parked(cpu) => {
                assert_eq!(cpu, CPU0);
                if ran {
                    SENT_ON.store(true, StdOrdering::Relaxed);
                } else {
                    WENT_HOME.store(true, StdOrdering::Relaxed);
                }
            }
            Notify::PrePark => {
                assert!(!ran, "the home's commit answered the claim");
                PRE_PARK.store(true, StdOrdering::Relaxed);
            }
            other => panic!("a post to a committing or parked task claims it: {other:?}"),
        }
        machine.wind_down(NOW);
    });

    let reached = [&PRE_PARK, &WENT_HOME, &SENT_ON].map(|seen| seen.load(StdOrdering::Relaxed));
    assert_eq!(
        reached,
        [true; 3],
        "claims before the park, before the switch and after it: an exploration missing one never raced them",
    );
}

/// **The home's deadline and a remote wake that race for one park take its
/// record once**, and the deadline's entry is gone whichever wins. The
/// deadline claims only the park of its own generation, and the waker claims
/// whatever is `Blocked`: one wins the word and the other finds it claimed.
/// Either sends the task to cpu1, the waker because the home is busy and
/// the home because the task would queue behind `OTHER`.
#[test]
fn a_deadline_and_a_remote_wake_take_the_record_once() {
    static WAKE_WON: AtomicBool = AtomicBool::new(false);
    static DEADLINE_WON: AtomicBool = AtomicBool::new(false);
    const DEADLINE: Nanos = Nanos(NOW.0 + 1_000);

    model(|| {
        let mut machine = Machine::new();
        let Commit::Parked(ticket) = committed(&machine) else {
            unreachable!("nothing has touched this task yet");
        };
        machine.park(ticket, Some(DEADLINE));
        assert_eq!(machine.shared.state(), TaskState::Blocked(CPU0));
        assert_eq!(machine.home.due().count(), 1);

        let waker = {
            let owner = machine.owner.clone();
            let shared = machine.shared.clone();
            loom::thread::spawn(move || {
                notify(&shared, WakeCause::new(WakeReason::Woken), &owner.cpus, &owner.hw, &RemoteGuard)
            })
        };
        pass(&machine.owner, &mut machine.home, DEADLINE);
        let notified = waker.join().unwrap();
        assert!(machine.sent_on(DEADLINE), "the task would queue behind `OTHER` at home");
        assert_eq!(machine.home.due().count(), 0, "the deadline's entry outlived its park");
        match notified {
            Notify::Parked(cpu) => {
                assert_eq!(cpu, CPU0);
                WAKE_WON.store(true, StdOrdering::Relaxed);
            }
            Notify::Flagged => DEADLINE_WON.store(true, StdOrdering::Relaxed),
            other => panic!("a post to a parked or woken task claims or flags it: {other:?}"),
        }
        machine.wind_down(DEADLINE);
    });

    let (wake, deadline) = (WAKE_WON.load(StdOrdering::Relaxed), DEADLINE_WON.load(StdOrdering::Relaxed));
    assert!(
        wake && deadline,
        "the wake won: {wake}, the deadline won: {deadline}; an exploration missing either never raced them",
    );
}
