//! Loom: the watch — a registration racing a post, for both kinds of waiter.
//!
//! The invariant, in one sentence: **after a post and a registration, either
//! the registrant saw what the poster changed or the poster reached the
//! registrant.** Never neither — that is a thread parked, or a poll pending,
//! over a condition that already holds.
//!
//! For a thread the "reached" half is [`TaskShared::notify`]'s write to the
//! word, which the waiter's own next [`prepare`] or commit reads; for a ring it
//! is [`Ring::fire`], one-shot against the registrant's own recheck. Both
//! sides of every model are spawned threads: loom runs the model's own thread
//! first, so a side written on it never sees the other not having run yet.
//!
//! The negative control is a cargo feature, `commit-ignores-notify`: it makes
//! `begin_commit` blind to the notified bit — a waiter that checks, registers,
//! and parks without the post that landed in between being able to stop it —
//! and `a_post_racing_a_registration_leaves_nobody_parked` must red:
//!
//! ```text
//! cargo test -p toyos-sched-loom --features commit-ignores-notify --test loom_watch
//! ```
//!
//! [`TaskShared::notify`]: toyos_sched_loom::task::TaskShared::notify
//! [`prepare`]: toyos_sched_loom::park::prepare
//! [`Ring::fire`]: toyos_sched_loom::watch::Ring::fire

use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use loom::sync::Arc;
use toyos_sched_loom::cpu::{CpuHandle, CpuHandles};
use toyos_sched_loom::mailbox::{mailbox, MailboxConsumer};
use toyos_sched_loom::model::{
    model, watch_list, Kicks, LoomLock, Msg, PreemptModel, RemoteGuard, CPU0,
};
use toyos_sched_loom::park::{prepare, Cancel, Commit, CurrentTask};
use toyos_sched_loom::task::{Claim, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use toyos_sched_loom::watch::{Fire, Poster, Ring, Waiters, Watch};

/// One poll, as the kernel's ring entry is: one-shot across everything that
/// may fire it, and it counts what it posted.
struct Poll {
    /// 0 armed, 1 fired ready, 2 fired gone.
    state: AtomicU32,
    posts: AtomicU32,
}

#[derive(Clone)]
struct Entry(Arc<Poll>);

impl Entry {
    fn new() -> Self {
        Self(Arc::new(Poll {
            state: AtomicU32::new(0),
            posts: AtomicU32::new(0),
        }))
    }

    fn posts(&self) -> u32 {
        self.0.posts.load(Ordering::Acquire)
    }
}

impl Ring for Entry {
    fn fire(&self, how: Fire) {
        let to = match how {
            Fire::Ready => 1,
            Fire::Gone => 2,
        };
        if self
            .0
            .state
            .compare_exchange(0, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.0.posts.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn live(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == 0
    }
}

type ModelWatch = Watch<Msg, Entry, LoomLock<Waiters<Msg, Entry>>>;

struct World {
    watch: ModelWatch,
    cpus: CpuHandles<Msg>,
    kicks: Kicks,
    preempt: PreemptModel,
}

impl World {
    fn post(&self) {
        let env = Poster {
            cpus: &self.cpus,
            kicker: &self.kicks,
            preempt: &RemoteGuard,
        };
        self.watch.post(WakeCause::new(WakeReason::Woken), &env);
    }

    fn post_one(&self, token: u64) -> usize {
        let env = Poster {
            cpus: &self.cpus,
            kicker: &self.kicks,
            preempt: &RemoteGuard,
        };
        self.watch
            .post_n(token, 1, WakeCause::new(WakeReason::Woken), &env)
    }
}

fn world() -> (Arc<World>, MailboxConsumer<Msg>) {
    let (tx, rx) = mailbox::<Msg>();
    (
        Arc::new(World {
            watch: Watch::new(watch_list()),
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

fn park(t: &Arc<TaskShared<Msg>>) {
    let ticket = prepare(&CurrentTask::new(t, CPU0), Cancel::Answers, WaitClass::Pipe)
        .expect("nothing has posted yet");
    assert!(matches!(ticket.commit(), Commit::Parked(_)));
}

/// **The lost wake, closed by construction.** A waiter registers, reads its
/// condition, and parks; a producer makes the condition true and posts. No
/// schedule may leave the waiter parked with the condition true and no wake
/// owed to it — the check-then-register window, which the watch closes by
/// writing the waiter's word and the commit closes by reading it.
#[test]
fn a_post_racing_a_registration_leaves_nobody_parked() {
    model(|| {
        let (world, mut rx) = world();
        let ready = Arc::new(AtomicBool::new(false));
        let waiter = task(1);

        let waiting = {
            let world = world.clone();
            let ready = ready.clone();
            let waiter = waiter.clone();
            loom::thread::spawn(move || {
                world.watch.register(&waiter, 0);
                // A `continue` is spent only by the one post, so the loop is
                // bounded by it; a third iteration is never reached.
                for _ in 0..3 {
                    if ready.load(Ordering::Acquire) {
                        return false;
                    }
                    let Ok(ticket) =
                        prepare(&CurrentTask::new(&waiter, CPU0), Cancel::Answers, WaitClass::Pipe)
                    else {
                        continue;
                    };
                    match ticket.commit() {
                        Commit::Parked(_) => return true,
                        Commit::AlreadyWoken => continue,
                        Commit::Killed => unreachable!("nothing retires in this model"),
                    }
                }
                unreachable!("one post ended more than one iteration")
            })
        };
        let producer = {
            let world = world.clone();
            loom::thread::spawn(move || {
                ready.store(true, Ordering::Release);
                world.post();
            })
        };

        let parked = waiting.join().unwrap();
        producer.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        if parked {
            assert_eq!(
                msgs,
                [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                "parked with the condition true and no wake owed: the post was lost",
            );
            assert_eq!(waiter.state(), TaskState::WakeQueued(CPU0));
        } else {
            assert!(msgs.is_empty(), "a waiter that never parked is owed nothing: {msgs:?}");
        }
        world.watch.unregister(&waiter);
        assert_eq!(world.watch.threads(), 0, "no registration is left behind");
    });
}

/// A bounded post racing one waiter's deadline must still reach a live
/// waiter: a waiter the deadline already claimed is flagged and spends nothing,
/// so the post moves on — a wake is never satisfied by a waiter on its way out.
#[test]
fn a_bounded_post_racing_a_timeout_reaches_a_live_waiter() {
    model(|| {
        let (world, mut rx) = world();
        let first = task(1);
        let second = task(2);
        for t in [&first, &second] {
            world.watch.register(t, 7);
            park(t);
        }

        let timeout = {
            let first = first.clone();
            // The home CPU's deadline fire: the claim, and no message.
            loom::thread::spawn(move || first.claim_wake() == Claim::Parked(CPU0))
        };
        let waker = {
            let world = world.clone();
            loom::thread::spawn(move || world.post_one(7))
        };

        let woken = waker.join().unwrap();
        let timeout_won = timeout.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        assert_eq!(woken, 1, "a live waiter must always be found");
        let expected = if timeout_won { TaskKey(2) } else { TaskKey(1) };
        assert_eq!(
            msgs,
            [Msg::Wake(expected, WakeReason::Woken)],
            "the wake must reach a live waiter (timeout_won={timeout_won})",
        );
        for t in [&first, &second] {
            world.watch.unregister(t);
        }
    });
}

/// A ring's poll registered racing a post is completed exactly once: by the
/// post, if the post found it, or by the registrant's own recheck, if the post
/// came first. Never by neither, which is a poll pending over a ready object,
/// and never by both, which is a completion the process did not ask for.
#[test]
fn a_poll_registered_racing_a_post_completes_exactly_once() {
    model(|| {
        let (world, _rx) = world();
        let ready = Arc::new(AtomicBool::new(false));
        let poll = Entry::new();

        let registrant = {
            let world = world.clone();
            let ready = ready.clone();
            let poll = poll.clone();
            loom::thread::spawn(move || {
                world.watch.add_ring(poll.clone());
                if ready.load(Ordering::Acquire) {
                    poll.fire(Fire::Ready);
                }
            })
        };
        let producer = loom::thread::spawn(move || {
            ready.store(true, Ordering::Release);
            world.post();
        });
        registrant.join().unwrap();
        producer.join().unwrap();

        assert_eq!(poll.posts(), 1, "a poll over a ready object completes once");
    });
}

/// The object's end racing its readiness: the poll is answered once, as ready
/// or as gone, and whichever answered it no longer holds it.
#[test]
fn an_end_racing_a_post_answers_a_poll_once() {
    model(|| {
        let (world, _rx) = world();
        let poll = Entry::new();
        world.watch.add_ring(poll.clone());

        let ender = {
            let world = world.clone();
            loom::thread::spawn(move || world.watch.cancel_rings())
        };
        let poster = loom::thread::spawn(move || world.post());
        ender.join().unwrap();
        poster.join().unwrap();

        assert_eq!(poll.posts(), 1);
        assert!(!poll.live());
    });
}
