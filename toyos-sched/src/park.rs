//! The one park: a running task parks on its own state word, and the one wake
//! primitive, [`notify`], is what ends it.
//!
//! There is no queue here. What a task waits *for* is a [`crate::watch::Watch`]
//! it registered on; where it parks is itself. Every wake in the system — a
//! post, a direct notify, a remote waker — terminates in
//! [`TaskShared::notify`]'s read-modify-write of that word (a revoke is the
//! same write with one more bit), and the local deadline fire in
//! [`TaskShared::claim_wake`]'s. There is no third path.
//!
//! The shape a blocking site must have:
//!
//! ```text
//! watch.register(task)
//! loop {
//!     if ready() || task.revoked() { break }   // a revoke ends the wait
//!     let Ok(t) = prepare(cur) else { continue };   // a post since registering
//!     match t.commit() { Parked => block, AlreadyWoken => continue, Killed => unwind }
//! }
//! watch.unregister(task)
//! ```
//!
//! **The recheck is the commit's, not the caller's.** A post that lands after
//! the registration and before [`prepare`] sets the task's notified bit, and
//! `prepare` refuses to begin a commit over it; one that lands after `prepare`
//! claims the `Committing` word, and the commit refuses to park. Neither needs
//! the caller to look at the subject a second time, so a site that forgets to
//! cannot lose a wake.

use core::marker::PhantomData;

use crate::cpu::CpuHandles;
use crate::hw::{CpuId, Kicker};
use crate::mailbox::{Kick, PreemptGuard, SchedMsg};
use crate::sync::Arc;
use crate::task::{Gen, Notified, Notify, ParkOutcome, TaskShared, WaitClass, WakeCause};

/// The running task, as the wait path sees it. Only `CpuSched::current_task`
/// hands one out, so [`prepare`] cannot be called for anybody else's task.
pub struct CurrentTask<'a, M> {
    shared: &'a Arc<TaskShared<M>>,
    cpu: CpuId,
}

impl<'a, M> CurrentTask<'a, M> {
    pub fn new(shared: &'a Arc<TaskShared<M>>, cpu: CpuId) -> Self {
        Self { shared, cpu }
    }

    pub fn cpu(&self) -> CpuId {
        self.cpu
    }

    pub fn shared(&self) -> &'a Arc<TaskShared<M>> {
        self.shared
    }
}

/// Phase 1: move the running task's word to `Committing(gen)`, or answer
/// [`Notified`] when a post reached it since it registered — in which case
/// nothing is owed and the caller rechecks.
#[must_use = "a wait ticket must be committed or cancelled"]
pub fn prepare<M: SchedMsg>(
    cur: &CurrentTask<'_, M>,
    cancel: Cancel,
    class: WaitClass,
) -> Result<WaitTicket<M>, Notified> {
    let generation = cur.shared.begin_commit(cur.cpu)?;
    Ok(WaitTicket {
        shared: cur.shared.clone(),
        cpu: cur.cpu,
        generation,
        cancel,
        class,
        armed: true,
        _not_send: PhantomData,
    })
}

/// Post to one task: claim it if it is parked or committing, flag it
/// otherwise, and post `Msg::Wake` to its home CPU when the claim owes one.
/// The only producer of wake messages in the system.
pub fn notify<M: SchedMsg>(
    shared: &Arc<TaskShared<M>>,
    cause: WakeCause,
    cpus: &CpuHandles<M>,
    kicker: &impl Kicker,
    preempt: &impl PreemptGuard,
) -> Notify {
    deliver(shared, shared.notify(), cause, cpus, kicker, preempt)
}

/// [`notify`] for a registration a watch has taken out: the task is woken or
/// flagged the same way, and it cannot park again until it registers again,
/// because no post can reach that park.
pub fn revoke<M: SchedMsg>(
    shared: &Arc<TaskShared<M>>,
    cause: WakeCause,
    cpus: &CpuHandles<M>,
    kicker: &impl Kicker,
    preempt: &impl PreemptGuard,
) -> Notify {
    deliver(shared, shared.revoke(), cause, cpus, kicker, preempt)
}

fn deliver<M: SchedMsg>(
    shared: &Arc<TaskShared<M>>,
    outcome: Notify,
    cause: WakeCause,
    cpus: &CpuHandles<M>,
    kicker: &impl Kicker,
    preempt: &impl PreemptGuard,
) -> Notify {
    if let Notify::Parked(cpu) = outcome {
        // The claim CAS admits exactly one poster, so the node is free (I12).
        let slot = shared
            .wake_node()
            .claim()
            .expect("the wake claim admits one poster: node must be free");
        let handle = cpus.get(cpu);
        if handle.post(slot, M::wake(shared.key(), cause), cause.urgency(), preempt) == Kick::Send {
            kicker.kick(cpu);
        }
    }
    outcome
}

/// Registered on its own word, not yet parked. `!Send` (the registration
/// belongs to the CPU that made it) and drop-bombed: exactly one of
/// [`WaitTicket::cancel`] and [`WaitTicket::commit`] must consume it.
#[must_use = "a wait ticket must be committed or cancelled"]
pub struct WaitTicket<M: SchedMsg> {
    shared: Arc<TaskShared<M>>,
    cpu: CpuId,
    generation: Gen,
    cancel: Cancel,
    class: WaitClass,
    /// Disarmed by `cancel`/`commit`; still armed at drop means a
    /// registration was abandoned.
    armed: bool,
    _not_send: PhantomData<*mut ()>,
}

/// Whether a kill ends this wait.
///
/// A property of the *wait* and not of the task: the same thread may hold both
/// kinds, one after the other — a cancellable park on the way in and an
/// uncancellable one on the way out, in the teardown its own cancel sent it to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cancel {
    /// The ordinary park. A kill refuses the commit, the caller is told, and
    /// it unwinds.
    Answers,
    /// The park a kill may not end. The caller cannot propagate a cancel and
    /// is bounded by something else — the retirer waiting for its victim's
    /// release is the one.
    Ignores,
}

/// The result of withdrawing a ticket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cancelled {
    /// The word is back at `Running`; the task keeps running.
    Clean,
    /// A post had already claimed it. The wait counts as satisfied.
    AlreadyWoken,
}

/// The result of committing a ticket.
#[must_use]
pub enum Commit<M> {
    /// The state word is `Blocked`; the pass may park the task with the ticket.
    Parked(CommittedTicket<M>),
    /// A post landed between phase 1 and the commit: do not park, do not switch.
    AlreadyWoken,
    /// A retire landed while this task was deciding to park. The word is back
    /// to `Running(cpu)`: the caller must dispose the pass by *exiting*, not by
    /// parking.
    Killed,
}

/// Proof that the task's word is `Blocked`. Consumed by
/// `SchedPass::dispose_block`.
pub struct CommittedTicket<M> {
    shared: Arc<TaskShared<M>>,
    cpu: CpuId,
    class: WaitClass,
}

impl<M> CommittedTicket<M> {
    pub fn shared(&self) -> &Arc<TaskShared<M>> {
        &self.shared
    }

    pub fn cpu(&self) -> CpuId {
        self.cpu
    }

    pub fn class(&self) -> WaitClass {
        self.class
    }
}

impl<M: SchedMsg> WaitTicket<M> {
    /// The caller is not going to park after all: withdraw.
    pub fn cancel(mut self) -> Cancelled {
        self.armed = false;
        match self.shared.cancel_commit(self.cpu, self.generation) {
            ParkOutcome::Parked => Cancelled::Clean,
            ParkOutcome::AlreadyWoken => Cancelled::AlreadyWoken,
        }
    }

    /// Phase 2, run by the blocking pass.
    ///
    /// A park is a safe point, so a killed task's promise to die at its next
    /// one has to be kept *here*: `handle_retire` already consumed the retire
    /// message and answered it with `need_resched` because the task was still
    /// running. Park it anyway and nothing brings the task back. The kill check
    /// comes first because it subsumes the wake: a task about to die has no use
    /// for one, and `cancel_commit` puts the word back to `Running(cpu)` either
    /// way, which is what the exit disposition needs.
    pub fn commit(mut self) -> Commit<M> {
        self.armed = false;
        if self.cancel == Cancel::Answers && self.shared.kill_pending() {
            let _ = self.shared.cancel_commit(self.cpu, self.generation);
            return Commit::Killed;
        }
        match self.shared.commit_park(self.cpu, self.generation) {
            ParkOutcome::Parked => Commit::Parked(CommittedTicket {
                shared: self.shared.clone(),
                cpu: self.cpu,
                class: self.class,
            }),
            ParkOutcome::AlreadyWoken => Commit::AlreadyWoken,
        }
    }
}

impl<M: SchedMsg> Drop for WaitTicket<M> {
    fn drop(&mut self) {
        assert!(
            !self.armed,
            "wait ticket dropped: it must be committed or cancelled",
        );
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::cpu::{CpuHandle, CpuHandles};
    use crate::mailbox::{mailbox, MailboxConsumer, MailboxNode, NoPreempt};
    use crate::task::{TaskKey, TaskState, WakeReason};
    use alloc::vec;
    use alloc::vec::Vec;
    use std::sync::Mutex;

    #[derive(Debug, PartialEq, Eq)]
    enum Msg {
        Wake(TaskKey, WakeReason),
        Retire(TaskKey),
    }

    impl SchedMsg for Msg {
        fn wake(key: TaskKey, cause: WakeCause) -> Self {
            Msg::Wake(key, cause.reason)
        }
        fn retire(shared: Arc<TaskShared<Self>>) -> Self {
            Msg::Retire(shared.key())
        }
    }

    #[derive(Default)]
    struct Kicks(Mutex<Vec<CpuId>>);
    impl Kicker for Kicks {
        fn kick(&self, target: CpuId) {
            self.0.lock().unwrap().push(target);
        }
    }

    impl Kicks {
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    const C0: CpuId = CpuId(0);

    fn task(key: u64) -> Arc<TaskShared<Msg>> {
        Arc::new(TaskShared::new(TaskKey(key), TaskState::Running(C0)))
    }

    fn cpus() -> (CpuHandles<Msg>, MailboxConsumer<Msg>) {
        let (tx, rx) = mailbox();
        (CpuHandles::new(vec![CpuHandle::new(C0, tx)]), rx)
    }

    fn woken() -> WakeCause {
        WakeCause::new(WakeReason::Woken)
    }

    fn ticket(t: &Arc<TaskShared<Msg>>) -> WaitTicket<Msg> {
        prepare(&CurrentTask::new(t, C0), Cancel::Answers, WaitClass::Pipe)
            .expect("nothing notified this task")
    }

    fn park(t: &Arc<TaskShared<Msg>>) -> CommittedTicket<Msg> {
        match ticket(t).commit() {
            Commit::Parked(committed) => committed,
            Commit::AlreadyWoken => panic!("expected the park to commit, got AlreadyWoken"),
            Commit::Killed => panic!("expected the park to commit, got Killed"),
        }
    }

    #[test]
    fn park_then_notify_delivers_exactly_one_message() {
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);

        let committed = park(&t);
        assert_eq!(committed.class(), WaitClass::Pipe);
        assert_eq!(notify(&t, woken(), &handles, &kicks, &NoPreempt), Notify::Parked(C0));
        assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(1), WakeReason::Woken)));
        assert_eq!(rx.pop(&NoPreempt), None);
        assert!(t.finish_wake(C0));
    }

    #[test]
    fn a_notify_before_the_commit_refuses_the_park_and_sends_nothing() {
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);

        let pending = ticket(&t);
        assert_eq!(notify(&t, woken(), &handles, &kicks, &NoPreempt), Notify::PrePark);
        assert!(matches!(pending.commit(), Commit::AlreadyWoken));
        assert_eq!(rx.pop(&NoPreempt), None, "pre-park claims post no message");
        assert_eq!(t.state(), TaskState::Running(C0));
        assert_eq!(kicks.count(), 0);
    }

    /// A notify before phase 1 is the window the notified bit exists for:
    /// `prepare` refuses, and nothing is registered, parked or posted.
    #[test]
    fn a_notify_before_phase_one_refuses_it() {
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);

        assert_eq!(notify(&t, woken(), &handles, &kicks, &NoPreempt), Notify::Flagged);
        assert!(prepare(&CurrentTask::new(&t, C0), Cancel::Answers, WaitClass::Pipe).is_err());
        assert_eq!(t.state(), TaskState::Running(C0));
        assert_eq!(rx.pop(&NoPreempt), None);
        let _ = park(&t);
    }

    /// The park is a safe point, and a killed task dies at its next one.
    #[test]
    fn a_kill_that_lands_before_the_commit_refuses_the_park() {
        let t = task(1);
        let pending = ticket(&t);
        t.mark_kill();
        assert!(matches!(pending.commit(), Commit::Killed));
        assert_eq!(
            t.state(),
            TaskState::Running(C0),
            "the exit disposition needs the word back at Running",
        );
    }

    #[test]
    fn a_kill_beats_a_pre_park_claim_to_the_commit() {
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);

        let pending = ticket(&t);
        assert!(notify(&t, woken(), &handles, &kicks, &NoPreempt).woke());
        t.mark_kill();
        assert!(matches!(pending.commit(), Commit::Killed));
        assert_eq!(t.state(), TaskState::Running(C0));
        assert_eq!(rx.pop(&NoPreempt), None, "pre-park claims post no message");
    }

    #[test]
    fn an_uncancellable_ticket_parks_a_killed_task() {
        let t = task(1);
        let pending = prepare(&CurrentTask::new(&t, C0), Cancel::Ignores, WaitClass::Other)
            .expect("nothing notified this task");
        t.mark_kill();
        assert!(matches!(pending.commit(), Commit::Parked(_)));
        assert_eq!(t.state(), TaskState::Blocked(C0));
    }

    #[test]
    fn cancel_reports_a_claim_it_lost() {
        let (handles, _rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);

        assert_eq!(ticket(&t).cancel(), Cancelled::Clean);

        let pending = ticket(&t);
        notify(&t, woken(), &handles, &kicks, &NoPreempt);
        assert_eq!(pending.cancel(), Cancelled::AlreadyWoken);
        assert_eq!(t.state(), TaskState::Running(C0));
    }

    #[test]
    fn a_boosted_notify_always_kicks() {
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);
        let _ = park(&t);

        let cause = WakeCause::boosted(WakeReason::Woken, crate::hw::Nanos(1_000));
        assert!(notify(&t, cause, &handles, &kicks, &NoPreempt).woke());
        assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(1), WakeReason::Woken)));
        assert_eq!(kicks.count(), 1, "boost wakes always kick");
    }

    #[test]
    #[should_panic(expected = "wait ticket dropped")]
    fn dropping_a_ticket_is_loud() {
        let t = task(1);
        let _ = ticket(&t);
    }

    #[test]
    fn one_wake_node_serves_the_whole_lifecycle() {
        // Re-parking after a delivered wake reuses the same embedded node,
        // which is only legal because the consumer released it (I12).
        let (handles, mut rx) = cpus();
        let kicks = Kicks::default();
        let t = task(1);
        let node: *const MailboxNode<Msg> = t.wake_node();

        for _ in 0..3 {
            let _ = park(&t);
            assert!(notify(&t, woken(), &handles, &kicks, &NoPreempt).woke());
            assert!(t.wake_node().in_flight());
            assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(1), WakeReason::Woken)));
            assert!(!t.wake_node().in_flight(), "released on consume");
            assert!(t.finish_wake(C0));
            assert!(t.transition(TaskState::Ready(C0), TaskState::Running(C0)));
        }
        assert_eq!(node, t.wake_node() as *const _, "the node never moved");
    }
}
