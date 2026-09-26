//! The one waitable: every object a thread or a poll ring can wait on holds
//! exactly one [`Watch`], and making an object waitable is that one field.
//!
//! A waiter is one of two things. A **thread** is registered for the length of
//! one wait and is *woken*: a post runs [`crate::park::notify`] on it, which
//! claims its word if it is parked or committing and flags it otherwise, so its
//! own next commit rechecks instead of parking. A **ring** entry is one poll a
//! process submitted and is *posted*: a post hands it to [`Ring::fire`], once,
//! and the environment writes that poll's completion into the ring it names.
//!
//! **A lost wake has no expression here.** A thread registers before it reads
//! its condition, under the list lock a post also takes, so a post either
//! precedes the registration — and the read after it sees what the poster
//! changed — or finds the thread in the list and writes its word. Nothing the
//! waiting side does between its registration and its park can be skipped:
//! `park::prepare` consumes the flag, and the commit consumes the claim.
//!
//! **A post allocates nothing.** Every ring entry is one-shot, so a post takes
//! them all out of the list and fires them with the list lock let go; what it
//! frees is those entries. A registration sweeps the entries a withdrawal left
//! behind, so the list never holds more than the polls live at its last post or
//! registration, plus that one.
//!
//! **Lock order.** The list lock is a leaf the environment supplies:
//! [`Ring::fire`] is always called with it let go, and so is the drop of every
//! entry the list lets go of, because an entry's last reference may own another
//! watch. So a post may be made under any lock but a ring's own.

use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::cpu::CpuHandles;
use crate::hw::Kicker;
use crate::mailbox::{PreemptGuard, SchedMsg};
use crate::park::{notify, revoke};
use crate::sync::{fence, Arc, AtomicU32, LeafLock, Ordering};
use crate::task::{TaskShared, WakeCause};

/// Why a ring entry is posted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fire {
    /// The watch's object became ready.
    Ready,
    /// The watch's object ended, or its last handle closed: nothing will ever
    /// make this poll ready.
    Gone,
}

/// One poll a ring is waiting on, as the watch holds it.
pub trait Ring {
    /// Post this poll's completion. One-shot across every watch the poll is
    /// registered on: an entry that already fired, or whose poll was withdrawn,
    /// posts nothing. Called with no watch's list lock held; may take only its
    /// ring's own lock and post only the watch its ring's submitters park on.
    fn fire(&self, how: Fire);
    /// Whether a fire would still post anything. `false` is permanent.
    fn live(&self) -> bool;
}

/// Everything a watch holds, behind the environment's leaf lock.
pub struct Waiters<M, R> {
    /// In registration order, so a bounded post reaches the longest waiter
    /// first.
    threads: Vec<Waiter<M>>,
    rings: Vec<R>,
}

struct Waiter<M> {
    task: Arc<TaskShared<M>>,
    /// The waiter's own word for a bounded post: a futex's address, a sleep
    /// lock's ticket. Opaque here.
    token: u64,
}

impl<M, R> Waiters<M, R> {
    /// `const` so a watch can be a `static`: the keyboard's, the futex
    /// buckets', a device's.
    pub const fn new() -> Self {
        Self {
            threads: Vec::new(),
            rings: Vec::new(),
        }
    }
}

impl<M, R> Default for Waiters<M, R> {
    fn default() -> Self {
        Self::new()
    }
}

/// What the notify needs from the environment: its CPUs, its IPI and the
/// proof preemption is off while a wake message is pushed.
pub struct Poster<'a, M, K, P> {
    pub cpus: &'a CpuHandles<M>,
    pub kicker: &'a K,
    pub preempt: &'a P,
}

pub struct Watch<M, R: Ring, L: LeafLock<Waiters<M, R>>> {
    list: L,
    _msg: PhantomData<fn() -> (M, R)>,
}

impl<M, R: Ring, L: LeafLock<Waiters<M, R>>> Watch<M, R, L> {
    pub const fn new(list: L) -> Self {
        Self {
            list,
            _msg: PhantomData,
        }
    }
}

impl<M: SchedMsg, R: Ring, L: LeafLock<Waiters<M, R>>> Watch<M, R, L> {
    /// Register the running task for one wait. After this, and before its
    /// condition is read: that order is the whole lost-wake argument. A post
    /// that reached this task during an earlier wait is forgotten here, before
    /// any post for this one can find it.
    pub fn register(&self, task: &Arc<TaskShared<M>>, token: u64) {
        assert!(task.set_waiting(), "a task waits on at most one watch");
        task.forget_posts();
        let dead = self.list.with(|w| {
            w.threads.push(Waiter {
                task: task.clone(),
                token,
            });
            sweep(&mut w.rings)
        });
        drop(dead);
    }

    /// End one wait. Idempotent against [`Self::revoke`], which may have taken
    /// the registration out already.
    pub fn unregister(&self, task: &Arc<TaskShared<M>>) {
        self.list.with(|w| {
            if let Some(at) = w.threads.iter().position(|t| Arc::ptr_eq(&t.task, task)) {
                w.threads.remove(at);
            }
        });
        task.clear_waiting();
    }

    /// Hand a ring's poll to this watch. Its registrant rechecks the object
    /// after this returns and fires the entry itself if the object is already
    /// ready — the ring's half of the same order a thread keeps.
    pub fn add_ring(&self, entry: R) {
        let dead = self.list.with(|w| {
            let dead = sweep(&mut w.rings);
            w.rings.push(entry);
            dead
        });
        drop(dead);
    }

    /// Something changed: wake every registered thread, and fire and let go of
    /// every ring entry.
    pub fn post<K: Kicker, P: PreemptGuard>(&self, cause: WakeCause, env: &Poster<'_, M, K, P>) {
        let fired = self.list.with(|w| {
            for waiter in &w.threads {
                notify(&waiter.task, cause, env.cpus, env.kicker, env.preempt);
            }
            core::mem::take(&mut w.rings)
        });
        for ring in &fired {
            ring.fire(Fire::Ready);
        }
    }

    /// Wake at most `limit` threads registered with `token`, in registration
    /// order, and answer how many this post woke. A thread that was not parked
    /// is flagged and spends nothing: its own commit rechecks, and the post
    /// moves on to the next, so a wake is never satisfied by a waiter that was
    /// already on its way out. Rings take no part: a bounded post is a
    /// thread's.
    pub fn post_n<K: Kicker, P: PreemptGuard>(
        &self,
        token: u64,
        limit: usize,
        cause: WakeCause,
        env: &Poster<'_, M, K, P>,
    ) -> usize {
        if limit == 0 {
            return 0;
        }
        self.list.with(|w| {
            let mut woken = 0;
            for waiter in w.threads.iter().filter(|t| t.token == token) {
                if notify(&waiter.task, cause, env.cpus, env.kicker, env.preempt).woke() {
                    woken += 1;
                    if woken == limit {
                        break;
                    }
                }
            }
            woken
        })
    }

    /// End every wait whose token `names`, taking the registrations out before
    /// posting so no later post for a token that now names something else can
    /// reach them; answer how many there were. A revoked thread cannot park
    /// again in this wait, whatever its condition reads, and its unregister is
    /// then a no-op.
    pub fn revoke<K: Kicker, P: PreemptGuard>(
        &self,
        names: impl Fn(u64) -> bool,
        cause: WakeCause,
        env: &Poster<'_, M, K, P>,
    ) -> usize {
        self.list.with(|w| {
            let mut ended = 0;
            let mut at = 0;
            while at < w.threads.len() {
                if !names(w.threads[at].token) {
                    at += 1;
                    continue;
                }
                let waiter = w.threads.remove(at);
                revoke(&waiter.task, cause, env.cpus, env.kicker, env.preempt);
                ended += 1;
            }
            ended
        })
    }

    /// Fire every ring entry as [`Fire::Gone`] and let go of them all: the
    /// object's source ended. Threads are not touched — each is registered
    /// for its own wait, which its own condition ends.
    pub fn cancel_rings(&self) {
        let ended = self.list.with(|w| core::mem::take(&mut w.rings));
        for ring in &ended {
            ring.fire(Fire::Gone);
        }
    }

    /// Registered threads, for a model or a report.
    pub fn threads(&self) -> usize {
        self.list.with(|w| w.threads.len())
    }

    /// Ring entries still live.
    pub fn live_rings(&self) -> usize {
        self.list.with(|w| w.rings.iter().filter(|r| r.live()).count())
    }
}

/// A word in front of a watch that its posters read to learn whether a post is
/// owed at all, for a waiter whose condition is a sweep of words the posters
/// write: the machine's stop, whose posters are every thread's park, band and
/// exit, and whose watch no poster may take a lock for until a stop begins.
///
/// **Each side is a store followed by a load of another location**: the waiter
/// opens the gate and then sweeps the posters' words; a poster writes its own
/// word and then reads the gate. So each side puts a `SeqCst` fence between the
/// two, and either the poster sees the gate open and posts, or the sweep sees
/// the poster's write. Without the fences both may read the old value, which
/// x86's locked read-modify-writes hide and ARM64 does not.
/// `loom_watch`'s `a_transition_racing_an_opening_gate_is_never_missed` is the
/// model and `gate-fence-off` its control.
pub struct Gate(AtomicU32);

impl Gate {
    #[cfg(not(feature = "loom"))]
    pub const fn new(value: u32) -> Self {
        Self(AtomicU32::new(value))
    }

    // Loom's atomics have no const constructor, so this arm alone drops `const`.
    #[cfg(feature = "loom")]
    pub fn new(value: u32) -> Self {
        Self(AtomicU32::new(value))
    }

    /// The waiter's side: publish `value`, then sweep.
    pub fn open(&self, value: u32) {
        self.0.store(value, Ordering::Release);
        gate_fence();
    }

    /// The poster's side, after the write its waiter sweeps for.
    pub fn after_write(&self) -> u32 {
        gate_fence();
        self.0.load(Ordering::Acquire)
    }

    /// A reader that owes nobody a post.
    pub fn read(&self) -> u32 {
        self.0.load(Ordering::Acquire)
    }
}

fn gate_fence() {
    // `gate-fence-off` is the negative control: without the fence either side
    // may read the other's old value, and `loom_watch` reds.
    if cfg!(not(feature = "gate-fence-off")) {
        fence(Ordering::SeqCst);
    }
}

/// Take out the entries that can no longer fire, for the caller to drop once
/// the list lock is let go.
fn sweep<R: Ring>(rings: &mut Vec<R>) -> Vec<R> {
    rings.extract_if(.., |r| !r.live()).collect()
}

/// An object that ends with polls still registered answers them: dropping a
/// watch fires every live ring entry as [`Fire::Gone`].
impl<M, R: Ring, L: LeafLock<Waiters<M, R>>> Drop for Watch<M, R, L> {
    fn drop(&mut self) {
        let ended = self.list.with(|w| core::mem::take(&mut w.rings));
        for ring in &ended {
            ring.fire(Fire::Gone);
        }
    }
}

/// The `watch-window` actuator's line: the kernel writes it once per [`STEP`]
/// held windows a post ended, and the harness reads the count after [`HELD`].
///
/// [`STEP`]: window::STEP
/// [`HELD`]: window::HELD
pub mod window {
    /// The line's words; the running count follows them.
    pub const HELD: &str = "watch-window: a post landed in the held window";
    /// One line per this many holds a post ended.
    pub const STEP: u64 = 64;
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::cpu::{CpuHandle, CpuHandles};
    use crate::hw::CpuId;
    use crate::mailbox::{mailbox, MailboxConsumer, NoPreempt};
    use crate::park::{prepare, Cancel, Commit, CurrentTask};
    use crate::task::{Notify, Refused, TaskKey, TaskState, WaitClass, WakeReason};
    use alloc::vec;
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    #[derive(Debug, PartialEq, Eq)]
    enum Msg {
        Wake(TaskKey),
        Retire(TaskKey),
    }

    impl SchedMsg for Msg {
        fn wake(key: TaskKey, _cause: WakeCause) -> Self {
            Msg::Wake(key)
        }
        fn retire(shared: Arc<TaskShared<Self>>) -> Self {
            Msg::Retire(shared.key())
        }
    }

    struct StdLock<T>(Mutex<T>);
    impl<T: Send> LeafLock<T> for StdLock<T> {
        fn with<U>(&self, f: impl FnOnce(&mut T) -> U) -> U {
            f(&mut self.0.lock().unwrap())
        }
    }

    struct NoKick;
    impl Kicker for NoKick {
        fn kick(&self, _target: CpuId) {}
    }

    /// A poll: one-shot, and it remembers what fired it.
    #[derive(Default)]
    struct Poll {
        /// 0 armed, 1 fired ready, 2 fired gone, 3 withdrawn.
        state: AtomicU32,
        posts: AtomicU32,
    }

    impl Poll {
        fn withdraw(&self) {
            let _ = self.state.compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    impl Ring for Arc<Poll> {
        fn fire(&self, how: Fire) {
            let to = match how {
                Fire::Ready => 1,
                Fire::Gone => 2,
            };
            if self.state.compare_exchange(0, to, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                self.posts.fetch_add(1, Ordering::AcqRel);
            }
        }
        fn live(&self) -> bool {
            self.state.load(Ordering::Acquire) == 0
        }
    }

    type TestWatch = Watch<Msg, Arc<Poll>, StdLock<Waiters<Msg, Arc<Poll>>>>;

    const C0: CpuId = CpuId(0);

    fn watch() -> TestWatch {
        Watch::new(StdLock(Mutex::new(Waiters::new())))
    }

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

    fn parked(t: &Arc<TaskShared<Msg>>) {
        let ticket = prepare(&CurrentTask::new(t, C0), Cancel::Answers, WaitClass::Other)
            .expect("nothing notified this task");
        assert!(matches!(ticket.commit(), Commit::Parked(_)));
    }

    /// The window a check-then-register waiter loses: registered, condition
    /// read false, post, then the park. The post flags the task, and its
    /// commit refuses.
    #[test]
    fn a_post_between_registration_and_park_is_not_lost() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let t = task(1);
        w.register(&t, 0);
        w.post(woken(), &env);
        assert!(prepare(&CurrentTask::new(&t, C0), Cancel::Answers, WaitClass::Other).is_err());
        assert_eq!(rx.pop(&NoPreempt), None, "a flag posts no message");
        w.unregister(&t);
        assert_eq!(w.threads(), 0);
    }

    #[test]
    fn a_post_wakes_every_parked_thread_and_fires_every_ring() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let tasks: Vec<_> = (1..=3).map(task).collect();
        for t in &tasks {
            w.register(t, 0);
            parked(t);
        }
        let poll = Arc::new(Poll::default());
        w.add_ring(poll.clone());
        w.post(woken(), &env);
        w.post(woken(), &env);
        for key in 1..=3 {
            assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(key))));
        }
        assert_eq!(rx.pop(&NoPreempt), None, "the second post found every wake queued");
        assert_eq!(poll.posts.load(Ordering::Acquire), 1, "a poll fires once");
        for t in &tasks {
            w.unregister(t);
        }
    }

    /// A bounded post spends nothing on a thread that was not parked, and
    /// reaches the next registered thread instead.
    #[test]
    fn a_bounded_post_skips_a_waiter_that_was_not_parked() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let timed_out = task(1);
        let waiting = task(2);
        let elsewhere = task(3);
        for t in [&timed_out, &waiting] {
            w.register(t, 7);
            parked(t);
        }
        w.register(&elsewhere, 8);
        parked(&elsewhere);
        // The first waiter's deadline fired on its home CPU.
        assert!(matches!(timed_out.claim_wake(), crate::task::Claim::Parked(C0)));
        assert_eq!(w.post_n(7, 1, woken(), &env), 1);
        assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(2))));
        assert_eq!(rx.pop(&NoPreempt), None);
        assert_eq!(elsewhere.notify(), Notify::Parked(C0), "another token is not reached");
        for t in [&timed_out, &waiting, &elsewhere] {
            w.unregister(t);
        }
    }

    #[test]
    fn a_revoke_takes_the_registration_out_before_it_wakes() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let inside = task(1);
        let outside = task(2);
        w.register(&inside, 0x1000);
        parked(&inside);
        w.register(&outside, 0x9000);
        parked(&outside);
        assert_eq!(w.revoke(|token| (0x1000..0x2000).contains(&token), woken(), &env), 1);
        assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(1))));
        assert_eq!(w.post_n(0x1000, 1, woken(), &env), 0, "a revoked token is not reached");
        assert_eq!(w.threads(), 1);
        w.unregister(&inside);
        w.unregister(&outside);
        assert!(!inside.is_waiting() && !outside.is_waiting());
    }

    /// Phase 1, for a waiter whose registration a revoke ended: it must be told
    /// so, not merely refused. A ticket it did begin is withdrawn before the
    /// test fails, so the failure is this one.
    fn refused(t: &Arc<TaskShared<Msg>>, why: &str) {
        match prepare(&CurrentTask::new(t, C0), Cancel::Answers, WaitClass::Other) {
            Err(Refused::Revoked) => {}
            Err(Refused::Notified) => panic!("{why}: refused as a post, which one recheck spends"),
            Ok(ticket) => {
                let _ = ticket.cancel();
                panic!("{why}");
            }
        }
    }

    /// A revoke ends the wait. The waiter it woke re-reads a condition the
    /// revoke did not make true — a futex word whose frame came back zeroed at
    /// the same address — and no later iteration of its loop may park: the
    /// registration is gone, so no post could ever reach that park.
    #[test]
    fn a_revoked_waiter_cannot_park_again() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let t = task(1);
        w.register(&t, 0x1000);
        parked(&t);
        assert_eq!(w.revoke(|token| token == 0x1000, woken(), &env), 1);
        assert_eq!(rx.pop(&NoPreempt), Some(Msg::Wake(TaskKey(1))));
        assert!(t.finish_wake(C0));
        assert!(t.transition(TaskState::Ready(C0), TaskState::Running(C0)));
        for _ in 0..2 {
            refused(&t, "a revoked waiter began a park no post can end");
        }
        w.unregister(&t);
    }

    /// The same, for a revoke that finds the waiter still running between its
    /// registration and its park: the refusal it causes is not spent by one
    /// iteration.
    #[test]
    fn a_waiter_revoked_before_it_parks_cannot_park() {
        let (handles, _rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let t = task(1);
        w.register(&t, 0x1000);
        assert_eq!(w.revoke(|token| token == 0x1000, woken(), &env), 1);
        for _ in 0..2 {
            refused(&t, "a revoked waiter began a park no post can end");
        }
        w.unregister(&t);
    }

    /// The same, for a revoke that lands between the waiter's phase 1 and its
    /// commit — a sibling's unmap on another CPU. The claim refuses the commit,
    /// and the refusal it leaves behind outlasts that one iteration.
    #[test]
    fn a_waiter_revoked_while_it_commits_cannot_park() {
        let (handles, mut rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let t = task(1);
        w.register(&t, 0x1000);
        let ticket = prepare(&CurrentTask::new(&t, C0), Cancel::Answers, WaitClass::Other)
            .expect("nothing notified this task");
        assert_eq!(w.revoke(|token| token == 0x1000, woken(), &env), 1);
        assert!(matches!(ticket.commit(), Commit::AlreadyWoken));
        assert_eq!(rx.pop(&NoPreempt), None, "a claim on a commit posts no message");
        for _ in 0..2 {
            refused(&t, "a revoked waiter began a park no post can end");
        }
        w.unregister(&t);
    }

    /// A spent entry whose drop reports whether its watch's list lock is held:
    /// the kernel's last reference to a poll can own a second watch, whose own
    /// drop takes that watch's lock.
    struct Tattler {
        list: std::sync::Weak<Mutex<Waiters<Msg, Tattler>>>,
    }

    impl Ring for Tattler {
        fn fire(&self, _how: Fire) {}
        fn live(&self) -> bool {
            false
        }
    }

    impl Drop for Tattler {
        fn drop(&mut self) {
            let list = self.list.upgrade().expect("the watch outlives its entries");
            assert!(list.try_lock().is_ok(), "an entry was dropped under its watch's list lock");
        }
    }

    struct SharedLock(std::sync::Arc<Mutex<Waiters<Msg, Tattler>>>);
    impl LeafLock<Waiters<Msg, Tattler>> for SharedLock {
        fn with<U>(&self, f: impl FnOnce(&mut Waiters<Msg, Tattler>) -> U) -> U {
            f(&mut self.0.lock().unwrap())
        }
    }

    #[test]
    fn a_sweep_drops_what_it_took_out_with_the_list_lock_let_go() {
        let list = std::sync::Arc::new(Mutex::new(Waiters::new()));
        let w: Watch<Msg, Tattler, SharedLock> = Watch::new(SharedLock(list.clone()));
        w.add_ring(Tattler { list: std::sync::Arc::downgrade(&list) });
        w.add_ring(Tattler { list: std::sync::Arc::downgrade(&list) });
        let t = task(1);
        w.register(&t, 0);
        w.unregister(&t);
        assert_eq!(w.live_rings(), 0);
    }

    /// Dead entries do not accumulate: every registration sweeps them.
    #[test]
    fn a_registration_sweeps_the_entries_that_can_no_longer_fire() {
        let (handles, _rx) = cpus();
        let env = Poster { cpus: &handles, kicker: &NoKick, preempt: &NoPreempt };
        let w = watch();
        let fired = Arc::new(Poll::default());
        w.add_ring(fired.clone());
        w.post(woken(), &env);
        let withdrawn = Arc::new(Poll::default());
        w.add_ring(withdrawn.clone());
        withdrawn.withdraw();
        let fresh = Arc::new(Poll::default());
        w.add_ring(fresh);
        assert_eq!(Arc::strong_count(&fired), 1, "a post lets go of what it fired");
        assert_eq!(Arc::strong_count(&withdrawn), 1, "a withdrawn entry is swept");
        assert_eq!(w.live_rings(), 1);
    }

    #[test]
    fn cancel_and_drop_answer_every_live_poll_as_gone() {
        let w = watch();
        let cancelled = Arc::new(Poll::default());
        w.add_ring(cancelled.clone());
        w.cancel_rings();
        assert_eq!(cancelled.state.load(Ordering::Acquire), 2);

        let dropped = Arc::new(Poll::default());
        w.add_ring(dropped.clone());
        drop(w);
        assert_eq!(dropped.state.load(Ordering::Acquire), 2);
        assert_eq!(dropped.posts.load(Ordering::Acquire), 1);
    }

    #[test]
    #[should_panic(expected = "a task waits on at most one watch")]
    fn a_second_registration_is_loud() {
        let a = watch();
        let b = watch();
        let t = task(1);
        a.register(&t, 0);
        b.register(&t, 0);
    }
}
