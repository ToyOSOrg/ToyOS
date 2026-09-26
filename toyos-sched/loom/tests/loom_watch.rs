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
//! Three more controls: `notify-flag-load-only` lets a post that finds its bits
//! already set answer off a load, and
//! `a_second_post_is_not_lost_to_a_flag_the_waiter_consumed` must red;
//! `gate-fence-off` removes the [`Gate`]'s two fences, and
//! `a_transition_racing_an_opening_gate_is_never_missed` must red; and
//! `poll-fire-load-store`, the kernel's own control for the poll's one-shot
//! answer, which the ring models below compile, must red both poll models here.
//!
//! **The ring entry is the kernel's [`Once`], compiled from
//! `kernel/src/inbox/once.rs`**, the decision a `PollEntry` makes; what else a
//! `PollEntry` is — the ring's page, its lock, the completion it writes — names
//! half the kernel and cannot be compiled here, so the model's entry counts its
//! answers instead of writing them.
//!
//! [`TaskShared::notify`]: toyos_sched_loom::task::TaskShared::notify
//! [`Gate`]: toyos_sched_loom::watch::Gate
//! [`Once`]: once::Once
//! [`prepare`]: toyos_sched_loom::park::prepare
//! [`Ring::fire`]: toyos_sched_loom::watch::Ring::fire

use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use loom::sync::Arc;
use toyos_sched_loom::cpu::{CpuHandle, CpuHandles};
use toyos_sched_loom::mailbox::{mailbox, MailboxConsumer};
use toyos_sched_loom::model::{
    model, watch_list, Kicks, LoomLock, Msg, PreemptModel, RemoteGuard, CPU0, CPU1,
};
use toyos_sched_loom::park::{prepare, Cancel, Commit, CurrentTask};
use toyos_sched_loom::task::{Claim, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use toyos_sched_loom::watch::{Fire, Gate, Poster, Ring, Waiters, Watch};

#[path = "../../../kernel/src/inbox/once.rs"]
mod once;

/// One poll, as the kernel's is: its answer taken once, by the kernel's own
/// [`once::Once`], across everything that may fire it; it counts what it
/// posted.
struct Poll {
    state: once::Once,
    posts: AtomicU32,
}

#[derive(Clone)]
struct Entry(Arc<Poll>);

impl Entry {
    fn new() -> Self {
        Self(Arc::new(Poll {
            state: once::Once::new(),
            posts: AtomicU32::new(0),
        }))
    }

    fn posts(&self) -> u32 {
        self.0.posts.load(Ordering::Acquire)
    }
}

impl Ring for Entry {
    fn fire(&self, _how: Fire) {
        if self.0.state.fire() {
            self.0.posts.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn live(&self) -> bool {
        self.0.state.armed()
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

/// One waiter, two producers, each storing its own condition and then posting.
/// The first post flags the waiter and its commit consumes the flag; the
/// second post may *load* the word from before that consumption. **Every arm
/// of the notify writes the word**, so the second post's exchange fails
/// against the consumed word and sets the bit again, or claims the commit —
/// a post that answered off the stale load would leave the waiter parked with
/// both conditions true and nothing owed to it.
#[test]
fn a_second_post_is_not_lost_to_a_flag_the_waiter_consumed() {
    model(|| {
        let (world, mut rx) = world();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let waiter = task(1);

        let waiting = {
            let world = world.clone();
            let (first, second) = (first.clone(), second.clone());
            let waiter = waiter.clone();
            loom::thread::spawn(move || {
                world.watch.register(&waiter, 0);
                // Each post ends at most one iteration, so two posts bound the
                // loop at three; a fourth is never reached.
                for _ in 0..4 {
                    if first.load(Ordering::Acquire) && second.load(Ordering::Acquire) {
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
                unreachable!("two posts ended more than three iterations")
            })
        };
        let producers: Vec<_> = [first, second]
            .into_iter()
            .map(|condition| {
                let world = world.clone();
                loom::thread::spawn(move || {
                    condition.store(true, Ordering::Release);
                    world.post();
                })
            })
            .collect();

        let parked = waiting.join().unwrap();
        for producer in producers {
            producer.join().unwrap();
        }
        let msgs = drain(&mut rx, &world.preempt);

        if parked {
            assert_eq!(
                msgs,
                [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                "parked with both conditions true and no wake owed: a post answered off a load",
            );
        } else {
            assert!(msgs.is_empty(), "a waiter that never parked is owed nothing: {msgs:?}");
        }
        world.watch.unregister(&waiter);
    });
}

/// **The machine's stop against one thread's park**, the gate included. The
/// stop opens its [`Gate`], registers on its watch and sweeps the thread's
/// word, parking if the thread is still running; the thread parks — the
/// transition the stop waits for — and then posts only if it finds the gate
/// open, which is `quiesce::note_progress`. Either the sweep sees the park or
/// the post reaches the stop: a stop parked over a thread that has parked,
/// with no post owed to it, sleeps out its whole budget.
#[test]
fn a_transition_racing_an_opening_gate_is_never_missed() {
    model(|| {
        let (world, mut rx) = world();
        let gate = Arc::new(Gate::new(0));
        let stopper = task(1);
        let thread = Arc::new(TaskShared::<Msg>::new(TaskKey(2), TaskState::Running(CPU1)));

        let poster = {
            let world = world.clone();
            let gate = gate.clone();
            let thread = thread.clone();
            loom::thread::spawn(move || {
                let ticket =
                    prepare(&CurrentTask::new(&thread, CPU1), Cancel::Answers, WaitClass::Other)
                        .expect("nothing posts to the stopped thread");
                assert!(matches!(ticket.commit(), Commit::Parked(_)));
                if gate.after_write() != 0 {
                    world.post();
                }
            })
        };
        let stop = {
            let world = world.clone();
            let stopper = stopper.clone();
            let thread = thread.clone();
            loom::thread::spawn(move || {
                gate.open(1);
                world.watch.register(&stopper, 0);
                // One post ends at most one iteration.
                for _ in 0..3 {
                    if thread.stop_pending() || thread.stop_if_blocked() {
                        return false;
                    }
                    let Ok(ticket) =
                        prepare(&CurrentTask::new(&stopper, CPU0), Cancel::Ignores, WaitClass::Other)
                    else {
                        continue;
                    };
                    match ticket.commit() {
                        Commit::Parked(_) => return true,
                        Commit::AlreadyWoken => continue,
                        Commit::Killed => unreachable!("an uncancellable wait"),
                    }
                }
                unreachable!("one post ended more than two iterations")
            })
        };

        let parked = stop.join().unwrap();
        poster.join().unwrap();
        let msgs = drain(&mut rx, &world.preempt);

        if parked {
            assert_eq!(
                msgs,
                [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                "the stop parked over a thread that had parked, and nothing posted it",
            );
        }
        world.watch.unregister(&stopper);
    });
}

/// `inbox::process_watch`'s own sequence for one poll on two watches: admitted
/// armed, withdrawing the older poll on the same handle, added to the object's
/// read watch and then its write watch with no lock held between, then
/// rechecked in both directions. Each direction's producer makes its condition
/// true and posts its own watch. The new poll is answered exactly once — by
/// whichever post or recheck takes it first — and the older one is answered or
/// withdrawn, never both and never neither.
#[test]
fn a_poll_on_two_watches_racing_both_posts_completes_exactly_once() {
    model(|| {
        let (read, _rx) = world();
        let (write, _wx) = world();
        let readable = Arc::new(AtomicBool::new(false));
        let writable = Arc::new(AtomicBool::new(false));
        let poll = Entry::new();
        let older = Entry::new();
        read.watch.add_ring(older.clone());

        let registrant = {
            let (read, write) = (read.clone(), write.clone());
            let (readable, writable) = (readable.clone(), writable.clone());
            let (poll, older) = (poll.clone(), older.clone());
            loom::thread::spawn(move || {
                let withdrew = older.0.state.withdraw();
                read.watch.add_ring(poll.clone());
                write.watch.add_ring(poll.clone());
                if readable.load(Ordering::Acquire) || writable.load(Ordering::Acquire) {
                    poll.fire(Fire::Ready);
                }
                withdrew
            })
        };
        let producers: Vec<_> = [(read, readable), (write, writable)]
            .into_iter()
            .map(|(side, condition)| {
                loom::thread::spawn(move || {
                    condition.store(true, Ordering::Release);
                    side.post();
                })
            })
            .collect();
        let withdrew = registrant.join().unwrap();
        for producer in producers {
            producer.join().unwrap();
        }

        assert_eq!(poll.posts(), 1, "a poll over a ready object completes once");
        assert!(
            withdrew != (older.posts() == 1),
            "the replaced poll was answered {} time(s), withdrawn={withdrew}",
            older.posts(),
        );
    });
}
