//! Loom: a parked task's record, taken by the claim that ends its park.
//!
//! The parking pass writes the record into the task's own slot and then moves
//! its word `Committing -> Blocked`; whoever claims `Blocked -> WakeQueued`
//! takes the record and, where the home is busy in the task's band, hands the
//! task straight to an idle CPU. The release on the first edge and the acquire
//! on the second are all that order the write before the take. A waker that
//! lands earlier leaves the record with the home, and so does one that loses
//! to the home's own deadline.

#![cfg(feature = "loom")]

use std::sync::atomic::{AtomicBool, Ordering as StdOrdering};

use kernel_loom::sched::cpu::{Balance, CpuHandle, CpuHandles, CpuSched, Env, SchedPass};
use kernel_loom::sched::hw::Nanos;
use kernel_loom::sched::mailbox::mailbox;
use kernel_loom::sched::park::{notify, prepare, Cancel, Commit, CommittedTicket};
use kernel_loom::sched::task::{Notify, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use kernel_loom::sched::topology::Topology;
use kernel_loom::sched_model::{model, IrqGuard, Owner, Payload, RemoteGuard, Silent, TaskMsg, CPU0, CPU1, NOW};
use loom::sync::Arc;
use loom::thread::JoinHandle;

/// The task that parks and is woken.
const KEY: TaskKey = TaskKey(1);
/// The task that keeps its home busy in the fair band, which is what sends a
/// wake that finds the park done to the idle CPU, and what keeps the home's
/// passes out of `try_sleep`'s retry (`loom_retire.rs` says why that matters).
const OTHER: TaskKey = TaskKey(2);

/// cpu0, the home, running `KEY` with `OTHER` posted to it, and cpu1 asleep.
struct Machine {
    owner: Arc<Owner>,
    home: CpuSched<Payload>,
    idle: CpuSched<Payload>,
    shared: Arc<TaskShared<TaskMsg>>,
}

impl Machine {
    fn new() -> Self {
        let (tx0, rx0) = mailbox::<TaskMsg>();
        let (tx1, rx1) = mailbox::<TaskMsg>();
        let owner = Arc::new(Owner {
            cpus: CpuHandles::new(vec![CpuHandle::new(CPU0, tx0), CpuHandle::new(CPU1, tx1)], Topology::flat(2)),
            hw: Silent,
        });
        let mut machine = Self {
            home: CpuSched::new(CPU0, rx0, ()),
            idle: CpuSched::new(CPU1, rx1, ()),
            shared: owner.spawn(CPU0, KEY),
            owner,
        };
        pass(&machine.owner, &mut machine.home, NOW);
        machine.owner.spawn(CPU0, OTHER);
        pass(&machine.owner, &mut machine.idle, NOW);
        assert!(machine.owner.cpus.get(CPU1).doorbell().sleeping_unrung(), "cpu1 sleeps");
        machine
    }

    /// A remote CPU's post to `KEY`.
    fn waker(&self) -> JoinHandle<Notify> {
        let owner = self.owner.clone();
        let shared = self.shared.clone();
        loom::thread::spawn(move || notify(&shared, WakeCause::new(WakeReason::Woken), &owner.cpus, &owner.hw, &RemoteGuard))
    }

    fn park(&mut self, committed: CommittedTicket<TaskMsg>, deadline: Option<Nanos>) {
        let _ = SchedPass::begin(&mut self.home, env(&self.owner), NOW).dispose_block(committed, deadline).finish();
    }

    /// Where `KEY` is, once cpu1 has passed at `now`: at home or sent on, and
    /// never both or neither.
    fn sent_on(&mut self, now: Nanos) -> bool {
        pass(&self.owner, &mut self.idle, now);
        let at_home = self.home.running().map(|t| t.key()) == Some(KEY) || self.home.rq().keys().any(|key| key == KEY);
        let sent_on = self.idle.running().map(|t| t.key()) == Some(KEY);
        assert!(at_home != sent_on, "the task is in {} CPUs' containers", if at_home { 2 } else { 0 });
        if sent_on {
            assert_eq!(self.shared.state(), TaskState::Running(CPU1));
            assert!(self.home.mailbox_is_empty(), "and the home owes nothing for it");
        }
        sent_on
    }

    /// Every task dies by the only death there is, so the model leaves
    /// nothing alive.
    fn wind_down(mut self, now: Nanos) {
        for cpu in [&mut self.home, &mut self.idle] {
            pass(&self.owner, cpu, now);
            while cpu.running().is_some() {
                let _ = SchedPass::begin(&mut *cpu, env(&self.owner), now).dispose_exit().finish();
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

fn pass(owner: &Owner, cpu: &mut CpuSched<Payload>, now: Nanos) {
    let _ = SchedPass::begin(cpu, env(owner), now).dispose_none().finish();
}

fn committed(machine: &Machine) -> Commit<TaskMsg> {
    let current = machine.home.current_task().expect("the task is running");
    prepare(&current, Cancel::Answers, WaitClass::Pipe).expect("nothing has posted to this task").commit()
}

/// **Whichever side of the park a remote wake lands on, the task ends up in
/// exactly one CPU's containers, and a claimant that takes the record finds
/// it whole.** A wake before the park is answered by the home's own commit;
/// one after it takes the record from the slot and sends the task to the
/// idle CPU as an `Adopt`. Loom checks the slot's write and take as a cell's
/// accesses, so a publication the claim does not acquire is a causality
/// violation and not a lucky read.
#[test]
fn a_record_taken_by_a_remote_claim_is_the_one_its_park_published() {
    static FORWARDED: AtomicBool = AtomicBool::new(false);
    static STAYED: AtomicBool = AtomicBool::new(false);

    model(|| {
        let mut machine = Machine::new();
        let current = machine.home.current_task().expect("the task is running");
        let ticket = prepare(&current, Cancel::Answers, WaitClass::Pipe).expect("nothing has posted to this task");
        let waker = machine.waker();
        match ticket.commit() {
            Commit::Parked(committed) => machine.park(committed, None),
            Commit::AlreadyWoken => {}
            Commit::Killed => unreachable!("nothing kills in this model"),
        }
        let notified = waker.join().unwrap();
        let sent_on = machine.sent_on(NOW);
        match notified {
            Notify::Parked(cpu) => {
                assert_eq!(cpu, CPU0);
                assert!(sent_on, "the claim found the home busy and cpu1 asleep");
                FORWARDED.store(true, StdOrdering::Relaxed);
            }
            Notify::PrePark => {
                assert!(!sent_on, "the home's commit answered the claim");
                STAYED.store(true, StdOrdering::Relaxed);
            }
            other => panic!("a post to a committing or parked task claims it: {other:?}"),
        }
        machine.wind_down(NOW);
    });

    let (forwarded, stayed) = (FORWARDED.load(StdOrdering::Relaxed), STAYED.load(StdOrdering::Relaxed));
    assert!(
        forwarded && stayed,
        "a claim after the park: {forwarded}, before it: {stayed}; an exploration missing either never raced them",
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

        let waker = machine.waker();
        pass(&machine.owner, &mut machine.home, DEADLINE);
        let notified = waker.join().unwrap();
        let sent_on = machine.sent_on(DEADLINE);
        assert_eq!(machine.home.due().count(), 0, "the deadline's entry outlived its park");
        assert!(sent_on, "the task would queue behind `OTHER` at home");
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
