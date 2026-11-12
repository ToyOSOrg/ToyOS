//! Loom: the watch — a registration racing a post, for both kinds of waiter.
//!
//! The invariant, in one sentence: **after a post and a registration, either
//! the registrant saw what the poster changed or the poster reached the
//! registrant.** Never neither — that is a thread parked, or a poll pending,
//! over a condition that already holds.
//!
//! For a thread the "reached" half is [`TaskShared::notify`]'s write to the
//! word, which the waiter's own next [`prepare`] or commit reads; for a ring it
//! is [`Ring::fire`], one-shot against the registrant's own recheck.
//!
//! The negative control is a cargo feature, `commit-ignores-notify`: it makes
//! `begin_commit` blind to the notified bit — a waiter that checks, registers,
//! and parks without the post that landed in between being able to stop it —
//! and `a_post_racing_a_registration_leaves_nobody_parked` must red:
//!
//! ```text
//! cargo test -p kernel-loom --features commit-ignores-notify --test loom_watch
//! ```
//!
//! Three more controls: `notify-flag-load-only` lets a post that finds its bits
//! already set answer off a load, and
//! `a_second_post_is_not_lost_to_a_flag_the_waiter_consumed` must red;
//! `poll-fire-load-store`, the kernel's own control for the poll's one-shot
//! answer, which the ring models below compile, must red every
//! `*_completes_exactly_once` model here; and `fault-posted-before-it-is-set`
//! posts before the readiness is stored, and both `a_poll_registered_racing_*`
//! models must red with a poll completed by neither.
//!
//! **The ring entry is the kernel's [`Once`], compiled from
//! `kernel/pure/sched/poll.rs`**, the decision a `PollEntry` makes, and the
//! ring models' ring is that file whole: its polls, the submitter's `deliver`,
//! its park predicate, a registration through a handle's table and a ring's
//! teardown. What else a ring is — its page, the objects its looks read, the
//! handle table itself — names half the kernel and cannot be compiled here, so
//! the models keep answers instead of writing them, and a table is a lock over
//! whether its one handle is there.
//!
//! The ring models' controls: `register-outside-table` must red
//! `a_close_racing_a_registration_leaves_nothing_through_the_handle`,
//! `admit-keeps-the-replaced`
//! `a_replacing_watch_racing_a_post_answers_once_under_the_newest_token`, and
//! `teardown-posts-nothing` `a_teardown_racing_a_park_returns_the_submit`.
//!
//! [`TaskShared::notify`]: kernel_loom::sched::task::TaskShared::notify
//! [`Once`]: once::Once
//! [`prepare`]: kernel_loom::sched::park::prepare
//! [`Ring::fire`]: kernel_loom::sched::watch::Ring::fire

#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use loom::sync::Arc;
use kernel_loom::sched::affinity::CpuMask;
use kernel_loom::sched::cpu::{CpuHandle, CpuHandles};
use kernel_loom::sched::topology::Topology;
use kernel_loom::sched::hw::CpuId;
use kernel_loom::sched::mailbox::{mailbox, MailboxConsumer};
use kernel_loom::sched_model::{
    model, watch_list, Kicks, LoomLock, Msg, PreemptModel, RemoteGuard, CPU0,
};
use kernel_loom::sched::park::{prepare, Cancel, Commit, CurrentTask};
use kernel_loom::sched::task::{
    Claim, Refused, TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason,
};
use kernel_loom::sched::sync::CellLock;
use kernel_loom::sched::watch::{Fire, Poster, Ring, Waiters, Watch};

use kernel_loom::sched::poll::{self as polls, Once, Via};
use toyos_abi::syscall::SyscallError;

/// One poll, as the kernel's is: its answer taken once, by the kernel's own
/// [`Once`], across everything that may fire it; it counts what it posted.
struct Poll {
    state: Once,
    posts: AtomicU32,
    /// Whether the end of its source is what took it.
    ended: AtomicBool,
}

#[derive(Clone)]
struct Entry(Arc<Poll>);

impl Entry {
    fn new() -> Self {
        Self(Arc::new(Poll {
            state: Once::new(),
            posts: AtomicU32::new(0),
            ended: AtomicBool::new(false),
        }))
    }

    fn posts(&self) -> u32 {
        self.0.posts.load(Ordering::Acquire)
    }
}

impl Ring for Entry {
    // As the kernel's `PollEntry`: readiness fires the poll, an end ends it.
    fn fire(&self, how: Fire) {
        let took = match how {
            Fire::Ready => self.0.state.fire(),
            Fire::Gone => self.0.state.end(),
        };
        if took {
            self.0.ended.store(how == Fire::Gone, Ordering::Release);
            self.0.posts.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn live(&self) -> bool {
        self.0.state.armed()
    }

    fn via(&self) -> Via {
        via(0)
    }
}

/// A handle of the one table these models' process holds.
fn via(handle: u32) -> Via {
    Via { table: 1, handle: toyos_abi::handle::RawHandle(handle) }
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

    fn post_in_place(&self) {
        let env = Poster {
            cpus: &self.cpus,
            kicker: &self.kicks,
            preempt: &RemoteGuard,
        };
        self.watch.post_in_place(WakeCause::new(WakeReason::Woken), &env);
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
    fn revoke(&self, token: u64) -> usize {
        let env = Poster {
            cpus: &self.cpus,
            kicker: &self.kicks,
            preempt: &RemoteGuard,
        };
        self.watch
            .revoke(|t| t == token, WakeCause::new(WakeReason::Woken), &env)
    }

}

fn world() -> (Arc<World>, MailboxConsumer<Msg>) {
    let (tx, rx) = mailbox::<Msg>();
    (
        Arc::new(World {
            watch: Watch::new(watch_list()),
            cpus: CpuHandles::new(vec![CpuHandle::new(CPU0, tx)], Topology::flat(1)),
            kicks: Kicks::new(),
            preempt: PreemptModel::new(),
        }),
        rx,
    )
}

fn task(key: u64) -> Arc<TaskShared<Msg>> {
    Arc::new(TaskShared::new(TaskKey(key), TaskState::Running(CPU0), CpuMask::ANY))
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
    model(|| poll_racing(World::post));
}

/// The same, against the post an interrupt handler makes.
#[test]
fn a_poll_registered_racing_a_post_in_place_completes_exactly_once() {
    model(|| poll_racing(World::post_in_place));
}

/// `ready` is `Relaxed` on both sides, as a claim's `faulted` is: the list lock
/// alone orders it against the registrant's recheck.
fn poll_racing(post: fn(&World)) {
    let (world, _rx) = world();
    let ready = Arc::new(AtomicBool::new(false));
    let poll = Entry::new();

    let registrant = {
        let world = world.clone();
        let ready = ready.clone();
        let poll = poll.clone();
        loom::thread::spawn(move || {
            world.watch.add_ring(poll.clone());
            if ready.load(Ordering::Relaxed) {
                poll.fire(Fire::Ready);
            }
        })
    };
    let producer = {
        let world = world.clone();
        loom::thread::spawn(move || {
            // `fault-posted-before-it-is-set` is the control: posted first, the
            // readiness can land after both the post and the recheck.
            if cfg!(feature = "fault-posted-before-it-is-set") {
                post(&world);
                ready.store(true, Ordering::Relaxed);
            } else {
                ready.store(true, Ordering::Relaxed);
                post(&world);
            }
        })
    };
    registrant.join().unwrap();
    producer.join().unwrap();

    // While `world` lives: its watch's drop answers a live entry as gone.
    assert_ne!(poll.posts(), 0, "a poll over a ready object was completed by neither");
    assert_eq!(poll.posts(), 1, "a poll over a ready object completes once");
    drop(world);
}

/// The object's end racing its readiness: the poll is answered once, as ready
/// or as gone.
#[test]
fn an_end_racing_a_post_answers_a_poll_once() {
    model(|| end_racing(|w| w.watch.cancel_rings(), World::post));
}

/// The same, for a thread's end racing a handler's post, which is made in
/// place.
#[test]
fn an_end_racing_a_post_in_place_answers_a_poll_once() {
    model(|| end_racing(|w| w.watch.cancel_rings(), World::post_in_place));
}

fn end_racing(end: fn(&World), post: fn(&World)) {
    let (world, _rx) = world();
    let poll = Entry::new();
    world.watch.add_ring(poll.clone());

    let ender = {
        let world = world.clone();
        loom::thread::spawn(move || end(&world))
    };
    let poster = {
        let world = world.clone();
        loom::thread::spawn(move || post(&world))
    };
    ender.join().unwrap();
    poster.join().unwrap();

    assert_eq!(poll.posts(), 1);
    assert!(!poll.live());
    assert_eq!(
        poll.0.state.ended(),
        poll.0.ended.load(Ordering::Acquire),
        "the one-shot names the wrong taker"
    );
    drop(world);
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

/// How `kernel::watch::wait_until`'s loop ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Parked,
    Revoked,
}

/// `wait_until`'s loop over a condition the revoke does not make true — a
/// futex word whose frame came back zeroed at the same address — so only a
/// park or the revoke's refusal ends it.
fn wait_out(waiter: &Arc<TaskShared<Msg>>) -> End {
    // One revoke ends at most one iteration short of its refusal.
    for _ in 0..3 {
        match prepare(&CurrentTask::new(waiter, CPU0), Cancel::Answers, WaitClass::Futex) {
            Err(Refused::Notified) => continue,
            Err(Refused::Revoked) => return End::Revoked,
            Ok(ticket) => match ticket.commit() {
                Commit::Parked(_) => return End::Parked,
                Commit::AlreadyWoken => continue,
                Commit::Killed => unreachable!("nothing retires in this model"),
            },
        }
    }
    unreachable!("one revoke ended more than two iterations")
}

/// **A revoke ends the wait wherever it finds the waiter**: running between its
/// registration and phase 1, committing, or parked. The revoke is a sibling's
/// unmap on another CPU and the waiter's condition stays false, so a waiter
/// that parks after the revoke parks where no post reaches. Every arm of the
/// revoke's write is reached here, and each must carry the refusal: a park is
/// owed exactly the one wake, and the waiter that wakes from it cannot park
/// again.
#[test]
fn a_revoke_racing_the_wait_loop_ends_it_in_every_arm() {
    model(|| {
        let (world, mut rx) = world();
        let waiter = task(1);
        world.watch.register(&waiter, 0x1000);

        let waiting = {
            let waiter = waiter.clone();
            loom::thread::spawn(move || wait_out(&waiter))
        };
        let revoker = {
            let world = world.clone();
            loom::thread::spawn(move || world.revoke(0x1000))
        };

        let end = waiting.join().unwrap();
        assert_eq!(revoker.join().unwrap(), 1, "the registration was there to revoke");
        let msgs = drain(&mut rx, &world.preempt);

        match end {
            End::Parked => {
                assert_eq!(
                    msgs,
                    [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                    "parked after its registration was revoked, and no wake owed",
                );
                assert!(waiter.finish_wake(CPU0));
                assert!(waiter.transition(TaskState::Ready(CPU0), TaskState::Running(CPU0)));
                assert_eq!(
                    wait_out(&waiter),
                    End::Revoked,
                    "the revoke's wake was spent and the waiter parked again where no post reaches",
                );
            }
            End::Revoked => {
                assert!(msgs.is_empty(), "a waiter that never parked is owed nothing: {msgs:?}");
            }
        }
        world.watch.unregister(&waiter);
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

/// A poll ring as its fires and its submitter see it: the kernel's polls,
/// which its teardown takes, the answers its submitter wrote, how many fires
/// took a poll, and the watch that submitter parks on, which holds threads and
/// no ring.
struct PollRing {
    cpus: CpuHandles<Msg>,
    kicks: Kicks,
    polls: LoomLock<Option<polls::Polls<RingRef>>>,
    answers: LoomLock<Vec<(u64, i32)>>,
    wakes: AtomicU32,
    parked: RingWatch,
}

#[derive(Clone)]
struct RingRef(Arc<PollRing>);

type RingPoll = Arc<polls::Poll<RingRef>>;

impl RingRef {
    fn answered(&self) -> u32 {
        self.0.answers.with(|answers| answers.len() as u32)
    }
}

impl polls::Wake for RingRef {
    fn wake(&self) {
        let ring = &self.0;
        ring.wakes.fetch_add(1, Ordering::AcqRel);
        let env = Poster { cpus: &ring.cpus, kicker: &ring.kicks, preempt: &RemoteGuard };
        ring.parked.post_in_place(WakeCause::new(WakeReason::Woken), &env);
    }
}

/// The ring's one submitter. Every look finds its object ready: each device
/// posts once, for good.
impl polls::Submitter<RingRef> for RingRef {
    fn room(&self) -> bool {
        true
    }

    fn answer(&self, user_data: u64, result: i32) {
        self.0.answers.with(|answers| answers.push((user_data, result)));
    }

    fn look(&self, poll: &RingPoll) -> polls::Look {
        polls::Look::Ready(poll.flags)
    }

    fn polls<R>(&self, f: impl FnOnce(&mut polls::Polls<RingRef>) -> R) -> Option<R> {
        self.0.polls.with(|polls| polls.as_mut().map(f))
    }
}

/// One of that ring's polls, as one device's watch holds it.
struct RingEntry(RingPoll);

type RingWatch = Watch<Msg, RingEntry, LoomLock<Waiters<Msg, RingEntry>>>;

impl Ring for RingEntry {
    // As the kernel's `PollEntry`.
    fn fire(&self, how: Fire) {
        match how {
            Fire::Ready => self.0.fire(self.0.flags),
            Fire::Gone => self.0.end(),
        }
    }

    fn live(&self) -> bool {
        self.0.armed()
    }

    fn via(&self) -> Via {
        self.0.via
    }
}

/// `inbox::submit`'s wait for `want` answers on `cpu`, as far as its first
/// park: whether it parked. A park leaves the submitter registered, as a
/// thread blocked in `watch::wait_until` is. A ring torn down returns it, as
/// `submit` returns `NotFound`.
fn submit_parks(ring: &RingRef, submitter: &Arc<TaskShared<Msg>>, cpu: CpuId, want: u32) -> bool {
    let enough = || ring.answered() >= want;
    let gone = || ring.0.polls.with(|polls| polls.is_none());
    // A fire ends at most one iteration short of the last, and so does the
    // teardown.
    for _ in 0..=want + 1 {
        polls::deliver(ring);
        if enough() || gone() {
            return false;
        }
        // `watch::wait_until` over `submit`'s predicate.
        if polls::awake(ring, enough) {
            continue;
        }
        ring.0.parked.register(submitter, 0);
        while !polls::awake(ring, enough) {
            let Ok(ticket) =
                prepare(&CurrentTask::new(submitter, cpu), Cancel::Answers, WaitClass::Io)
            else {
                continue;
            };
            match ticket.commit() {
                Commit::Parked(_) => return true,
                Commit::AlreadyWoken => continue,
                Commit::Killed => unreachable!("nothing retires in this model"),
            }
        }
        ring.0.parked.unregister(submitter);
    }
    unreachable!("{want} fires and a teardown ended more than {} iterations", want + 1)
}

/// A ring nobody waits on yet, and the mailbox of each CPU a submitter of it
/// runs on.
fn poll_ring<const CPUS: usize>(cpus: [CpuId; CPUS]) -> (RingRef, [MailboxConsumer<Msg>; CPUS]) {
    let (handles, mailboxes): (Vec<_>, Vec<_>) = cpus
        .into_iter()
        .map(|cpu| {
            let (tx, rx) = mailbox::<Msg>();
            (CpuHandle::new(cpu, tx), rx)
        })
        .unzip();
    let ring = RingRef(Arc::new(PollRing {
        cpus: { let count = handles.len() as u32; CpuHandles::new(handles, Topology::flat(count)) },
        kicks: Kicks::new(),
        polls: LoomLock::new(Some(polls::Polls::new())),
        answers: LoomLock::new(Vec::new()),
        wakes: AtomicU32::new(0),
        parked: Watch::new(watch_list()),
    }));
    let Ok(mailboxes) = mailboxes.try_into() else {
        unreachable!("one mailbox per CPU");
    };
    (ring, mailboxes)
}

/// A poll of `ring` through `handle`, answered under `token`, kept as that
/// handle's one poll.
fn kept(ring: &RingRef, handle: u32, token: u64) -> RingPoll {
    let poll = Arc::new(polls::Poll::new(ring.clone(), token, via(handle), toyos_abi::inbox::READABLE));
    assert!(polls_of(ring, |polls| polls.admit(poll.clone(), 8)));
    poll
}

fn polls_of<R>(ring: &RingRef, f: impl FnOnce(&mut polls::Polls<RingRef>) -> R) -> R {
    ring.0.polls.with(|polls| f(polls.as_mut().expect("a ring not torn down")))
}

/// `devices` watches, each holding one armed poll of `ring`.
fn devices_polled(ring: &RingRef, devices: usize) -> Vec<Arc<RingWatch>> {
    (0..devices)
        .map(|handle| {
            let poll = kept(ring, handle as u32, handle as u64);
            let device = Arc::new(Watch::new(watch_list()));
            device.add_ring(RingEntry(poll));
            device
        })
        .collect()
}

/// A thread's post of `device`.
fn posted(ring: &RingRef, device: &RingWatch) {
    let env = Poster { cpus: &ring.0.cpus, kicker: &ring.0.kicks, preempt: &RemoteGuard };
    device.post(WakeCause::new(WakeReason::Woken), &env);
}

/// An interrupt handler's post of `device`, on its own thread.
fn posted_in_place(ring: &RingRef, device: &Arc<RingWatch>) -> loom::thread::JoinHandle<()> {
    let (device, ring) = (device.clone(), ring.clone());
    loom::thread::spawn(move || {
        let env = Poster { cpus: &ring.0.cpus, kicker: &ring.0.kicks, preempt: &RemoteGuard };
        device.post_in_place(WakeCause::new(WakeReason::Woken), &env);
    })
}

/// **A submitter parks on its polls, and a fire posts the watch it parks on.**
/// Two devices' watches each hold a poll of one ring and are posted at once,
/// from an interrupt handler's post in place, while the ring's submitter waits
/// for both answers. A fire takes its poll and posts the ring's watch; the
/// submitter answers what was fired and, registered, reads its polls again
/// before it parks. A submitter parked over a fired poll was owed the wake
/// that fire posted.
#[test]
fn a_fire_racing_a_submitters_park_is_never_lost() {
    model(|| {
        let (ring, [mut rx]) = poll_ring([CPU0]);
        let devices = devices_polled(&ring, 2);
        let submitter = task(1);

        let waiting = {
            let ring = ring.clone();
            let submitter = submitter.clone();
            loom::thread::spawn(move || submit_parks(&ring, &submitter, CPU0, 2))
        };
        let posters: Vec<_> = devices.iter().map(|device| posted_in_place(&ring, device)).collect();

        let parked = waiting.join().unwrap();
        for poster in posters {
            poster.join().unwrap();
        }
        let guard = PreemptModel::new();
        let msgs = drain(&mut rx, &guard);

        if parked {
            assert_eq!(
                msgs,
                [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                "parked over a fired poll and no wake owed: a fire was lost",
            );
        } else {
            assert_eq!(ring.answered(), 2, "the wait ended short of its answers");
            assert!(msgs.is_empty(), "a submitter that never parked is owed nothing: {msgs:?}");
        }
        ring.0.parked.unregister(&submitter);
        // The ring's teardown: its polls hold it.
        polls_of(&ring, polls::Polls::withdraw_all);
    });
}

/// **A poll is answered once, whichever of a post and its handle's close
/// reaches it first.** One poll made through a handle stands on two watches,
/// its read and its write direction, as a connection's does; a post of the
/// read watch and that handle's close, which ends its entries on both, race.
/// One watch's list lock lets only one of the two take that watch's entry, so
/// the race is the other watch's: exactly one of the post and the close takes
/// the poll and wakes the ring, and the submitter's answer is the readiness or
/// `NotFound`, once.
#[test]
fn a_close_racing_a_post_answers_a_poll_once() {
    model(|| {
        let (ring, _mailboxes) = poll_ring([CPU0]);
        let poll = kept(&ring, 1, 7);
        let read: Arc<RingWatch> = Arc::new(Watch::new(watch_list()));
        let write: Arc<RingWatch> = Arc::new(Watch::new(watch_list()));
        read.add_ring(RingEntry(poll.clone()));
        write.add_ring(RingEntry(poll));

        let poster = {
            let (ring, read) = (ring.clone(), read.clone());
            loom::thread::spawn(move || posted(&ring, &read))
        };
        // `ops::end_polls`: every watch the object has.
        let closer = loom::thread::spawn(move || {
            read.end_rings(via(1));
            write.end_rings(via(1));
        });
        poster.join().unwrap();
        closer.join().unwrap();

        assert_eq!(ring.0.wakes.load(Ordering::Acquire), 1, "a post and a close both took the poll");
        polls::deliver(&ring);
        let answers = ring.0.answers.with(core::mem::take);
        let gone = -(SyscallError::NotFound as i32);
        assert!(
            answers == [(7, toyos_abi::inbox::READABLE as i32)] || answers == [(7, gone)],
            "a poll answered other than once: {answers:?}",
        );
    });
}

/// A table holding its one handle, and whether a poll was made through it;
/// `None` once the handle left.
struct OneHandle(LoomLock<Option<bool>>);

impl polls::Table for OneHandle {
    fn while_held(&self, _handle: toyos_abi::handle::RawHandle, add: impl FnOnce()) -> bool {
        self.0.with(|held| match held {
            Some(polled) => {
                *polled = true;
                add();
                true
            }
            None => false,
        })
    }
}

/// **A close racing a registration leaves nothing made through the closed
/// handle.** A poll's registration and its handle's close race: the
/// registration adds the entry in the table's hold that reads the handle still
/// there, and the close, in its own hold, takes the handle out and ends the
/// entries made through it. Either the close finds the entry, or the
/// registration finds no handle; the poll ends either way, and no live entry
/// is left for a later post to fire.
#[test]
fn a_close_racing_a_registration_leaves_nothing_through_the_handle() {
    model(|| {
        let (ring, _mailboxes) = poll_ring([CPU0]);
        let device: Arc<RingWatch> = Arc::new(Watch::new(watch_list()));
        let table = Arc::new(OneHandle(LoomLock::new(Some(false))));
        let poll = kept(&ring, 1, 7);

        let registrant = {
            let (device, table, poll) = (device.clone(), table.clone(), poll.clone());
            loom::thread::spawn(move || polls::register(&poll, &*table, || device.add_ring(RingEntry(poll.clone()))))
        };
        let closer = {
            let (device, table) = (device.clone(), table);
            // `HandleTable::remove`, in the caller's hold of its table.
            loom::thread::spawn(move || {
                table.0.with(|held| {
                    if held.take() == Some(true) {
                        device.end_rings(via(1));
                    }
                })
            })
        };
        registrant.join().unwrap();
        closer.join().unwrap();

        assert_eq!(device.live_rings(), 0, "an entry made through a closed handle is live");
        assert!(poll.ended(), "the poll through a closed handle was not ended");
        polls_of(&ring, polls::Polls::withdraw_all);
    });
}

/// **A watch that replaces its handle's poll while a post fires it is
/// answered once, under the newest token.** One watch holds a handle's poll;
/// the object becomes ready and posts while the submitter replaces that poll
/// with a newer watch on the same handle, registers it and rechecks. Whatever
/// the order, the answer is one, the newer watch's.
#[test]
fn a_replacing_watch_racing_a_post_answers_once_under_the_newest_token() {
    model(|| {
        let (ring, _mailboxes) = poll_ring([CPU0]);
        let device: Arc<RingWatch> = Arc::new(Watch::new(watch_list()));
        device.add_ring(RingEntry(kept(&ring, 1, 1)));
        let ready = Arc::new(AtomicBool::new(false));

        let poster = {
            let (ring, device, ready) = (ring.clone(), device.clone(), ready.clone());
            loom::thread::spawn(move || {
                ready.store(true, Ordering::Release);
                posted(&ring, &device);
            })
        };
        let replacer = {
            let ring = ring.clone();
            loom::thread::spawn(move || {
                let newer = kept(&ring, 1, 2);
                device.add_ring(RingEntry(newer.clone()));
                if ready.load(Ordering::Acquire) {
                    newer.fire(0);
                }
            })
        };
        poster.join().unwrap();
        replacer.join().unwrap();

        polls::deliver(&ring);
        let answers = ring.0.answers.with(core::mem::take);
        assert_eq!(answers, [(2, toyos_abi::inbox::READABLE as i32)], "one handle's one arrival");
        polls_of(&ring, polls::Polls::withdraw_all);
    });
}

/// **A ring torn down under a parked submitter returns its submit.** The
/// ring's one poll never fires; its submitter waits for one answer while the
/// ring's last handle closes and the teardown takes its polls. A submitter
/// parked with the ring gone and no wake owed would wait for good.
#[test]
fn a_teardown_racing_a_park_returns_the_submit() {
    model(|| {
        let (ring, [mut rx]) = poll_ring([CPU0]);
        let _device = devices_polled(&ring, 1);
        let submitter = task(1);

        let waiting = {
            let (ring, submitter) = (ring.clone(), submitter.clone());
            loom::thread::spawn(move || submit_parks(&ring, &submitter, CPU0, 1))
        };
        let teardown = {
            let ring = ring.clone();
            loom::thread::spawn(move || {
                let taken = ring.0.polls.with(Option::take).expect("torn down once");
                polls::torn_down(taken, &ring);
            })
        };
        let parked = waiting.join().unwrap();
        teardown.join().unwrap();
        let guard = PreemptModel::new();
        let msgs = drain(&mut rx, &guard);

        if parked {
            assert_eq!(
                msgs,
                [Msg::Wake(TaskKey(1), WakeReason::Woken)],
                "parked on a ring torn down and no wake owed",
            );
        } else {
            assert!(msgs.is_empty(), "a submitter that never parked is owed nothing: {msgs:?}");
        }
        assert_eq!(ring.answered(), 0, "a ring torn down answered");
        ring.0.parked.unregister(&submitter);
    });
}
