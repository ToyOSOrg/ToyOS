//! Loom: the two-phase park handshake on a task's own word.
//!
//! Three races, each a lost-wake window: a notify against the commit, a notify
//! against a cancel, and a notify against the next iteration's phase 1. The
//! race a *registration* runs against a post is the watch's, and
//! `loom_watch.rs` has it.

use loom::sync::Arc;
use toyos_sched_loom::cpu::{CpuHandle, CpuHandles};
use toyos_sched_loom::mailbox::{mailbox, MailboxConsumer};
use toyos_sched_loom::model::{model, Kicks, Msg, PreemptModel, RemoteGuard, CPU0};
use toyos_sched_loom::park::{notify, prepare, Cancel, Cancelled, Commit, CurrentTask};
use toyos_sched_loom::task::{
    Notify, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason,
};

/// These models are about notify-versus-commit; nothing in them retires
/// anything, so `Commit::Killed` cannot arise. The kill-versus-commit race is
/// `loom_retire.rs`'s.
const NO_RETIRER: &str = "no thread in this model sets the kill bit";

struct World {
    cpus: CpuHandles<Msg>,
    kicks: Kicks,
    preempt: PreemptModel,
}

fn world() -> (Arc<World>, MailboxConsumer<Msg>) {
    let (tx, rx) = mailbox::<Msg>();
    (
        Arc::new(World {
            cpus: CpuHandles::new(vec![CpuHandle::new(CPU0, tx)]),
            kicks: Kicks::new(),
            preempt: PreemptModel::new(),
        }),
        rx,
    )
}

fn task(key: u64) -> Arc<TaskShared<Msg>> {
    Arc::new(TaskShared::new(TaskKey(key), TaskState::Running(CPU0)))
}

fn drain(rx: &mut MailboxConsumer<Msg>, preempt: &PreemptModel) -> Vec<Msg> {
    let guard = preempt.disable();
    let mut msgs = Vec::new();
    while let Some(msg) = rx.pop(&guard) {
        msgs.push(msg);
    }
    msgs
}

fn post(world: &World, waiter: &Arc<TaskShared<Msg>>) -> Notify {
    notify(
        waiter,
        WakeCause::new(WakeReason::Woken),
        &world.cpus,
        &world.kicks,
        &RemoteGuard,
    )
}

/// A pre-park claim posts no message (`Notify::PrePark`): the waiter's own
/// commit observes it and refuses to park, so no `Wake` is queued and no
/// switch happens.
#[test]
fn a_pre_park_claim_never_posts_a_message() {
    model(|| {
        let (world, mut rx) = world();
        let waiter = task(1);
        let ticket = prepare(&CurrentTask::new(&waiter, CPU0), Cancel::Answers, WaitClass::Pipe)
            .expect("nothing has posted yet");

        let waker = {
            let world = world.clone();
            let waiter = waiter.clone();
            loom::thread::spawn(move || post(&world, &waiter))
        };

        let outcome = ticket.commit();
        let posted = waker.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        match outcome {
            Commit::Parked(_) => {
                assert_eq!(posted, Notify::Parked(CPU0), "the post found us parked");
                assert_eq!(msgs, [Msg::Wake(TaskKey(1), WakeReason::Woken)]);
            }
            Commit::AlreadyWoken => {
                assert_eq!(posted, Notify::PrePark, "somebody must have claimed us");
                assert!(msgs.is_empty(), "a pre-park claim posts nothing: {msgs:?}");
                assert_eq!(waiter.state(), TaskState::Running(CPU0));
            }
            Commit::Killed => unreachable!("{NO_RETIRER}"),
        }
    });
}

/// Cancel against a notify: exactly one of them wins, and the loser reports
/// it. A notify that lost to the cancel leaves the bit, so the post is still
/// owed to the next phase 1.
#[test]
fn cancel_and_notify_agree_on_who_won() {
    model(|| {
        let (world, mut rx) = world();
        let waiter = task(1);
        let ticket = prepare(&CurrentTask::new(&waiter, CPU0), Cancel::Answers, WaitClass::Pipe)
            .expect("nothing has posted yet");

        let waker = {
            let world = world.clone();
            let waiter = waiter.clone();
            loom::thread::spawn(move || post(&world, &waiter))
        };

        let cancelled = ticket.cancel();
        let posted = waker.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        match cancelled {
            Cancelled::Clean => {
                assert_eq!(posted, Notify::Flagged, "we withdrew before any claim");
                assert!(
                    prepare(&CurrentTask::new(&waiter, CPU0), Cancel::Answers, WaitClass::Pipe)
                        .is_err(),
                    "the post landed after the withdrawal and is owed to the next commit",
                );
            }
            Cancelled::AlreadyWoken => assert_eq!(posted, Notify::PrePark),
        }
        assert!(msgs.is_empty(), "nothing was ever parked: {msgs:?}");
        assert_eq!(waiter.state(), TaskState::Running(CPU0));
    });
}

/// The waiting loop's iterations against one notify: whichever phase 1 the
/// post lands beside, it ends exactly one iteration — a refused phase 1, or a
/// claimed commit — or it finds the task parked and owns its wake.
#[test]
fn a_notify_ends_exactly_one_iteration() {
    model(|| {
        let (world, mut rx) = world();
        let waiter = task(1);

        let waker = {
            let world = world.clone();
            let waiter = waiter.clone();
            loom::thread::spawn(move || post(&world, &waiter))
        };

        let mut ended = 0;
        let mut parked = false;
        for _ in 0..2 {
            let Ok(ticket) =
                prepare(&CurrentTask::new(&waiter, CPU0), Cancel::Answers, WaitClass::Pipe)
            else {
                ended += 1;
                continue;
            };
            match ticket.commit() {
                Commit::Parked(_) => {
                    parked = true;
                    break;
                }
                Commit::AlreadyWoken => ended += 1,
                Commit::Killed => unreachable!("{NO_RETIRER}"),
            }
        }
        let posted = waker.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        assert!(parked, "one post cannot end two iterations");
        if posted == Notify::Parked(CPU0) {
            assert_eq!(ended, 0);
            assert_eq!(msgs, [Msg::Wake(TaskKey(1), WakeReason::Woken)]);
        } else {
            assert_eq!(ended, 1, "the post ended the first iteration: {posted:?}");
            assert!(msgs.is_empty(), "{msgs:?}");
        }
    });
}
