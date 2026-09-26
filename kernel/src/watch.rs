//! The one way to wait: every waitable object holds exactly one [`Watch`], and
//! a waiter on it is either a thread, which a post *wakes*, or a user poll
//! ring's entry, which a post *completes*.
//!
//! The protocol is `toyos_sched::watch`'s and `toyos_sched::park`'s, and its
//! lost-wake argument is theirs: a thread registers before it reads its
//! condition, and a post either precedes the registration or writes the
//! thread's state word, which its own next commit reads. Nothing here keeps a
//! record of what was posted — a waiter re-reads the object, never the post.
//!
//! **A post allocates nothing and may be made under any lock but a poll
//! ring's** (`crate::inbox`), so a driver posts from its scheduler-pass drain
//! and never from its ISR, which publishes to `irq_ring` and nothing else.
//! Registration allocates, in the syscall that registers.
//!
//! A [`Watch`] is a borrowed reference for the whole of a wait: [`Armed`]
//! holds it, so an object cannot be freed under a thread waiting on it, and
//! there is no registry and no id to name one by.

use alloc::sync::Arc;

use toyos_sched::hw::Nanos;
use toyos_sched::task::{Refused, WaitClass, WakeCause, WakeReason};
use toyos_sched::watch::{Poster, Waiters};

pub use toyos_sched::park::Cancel;

use crate::hw::HW;
use crate::inbox::PollEntry;
use crate::sched::driver::{cpus, preempt_off};
use crate::sched::payload::{KMsg, KShared, KernelLock, TaskHandle};
use crate::scheduler::Parkable;
use crate::time::Deadline;

type Inner = toyos_sched::watch::Watch<KMsg, PollEntry, KernelLock<Waiters<KMsg, PollEntry>>>;

/// What an object holds to be waitable.
pub struct Watch(Inner);

impl Watch {
    pub const fn new() -> Self {
        Self(Inner::new(KernelLock::new(Waiters::new())))
    }

    /// Something about the object changed: wake every thread waiting on it and
    /// complete every poll.
    pub fn post(&self) {
        self.post_as(WakeCause::new(WakeReason::Woken));
    }

    /// The same, lending the poster's real-time window to whoever it wakes.
    pub fn post_boosted(&self, until: Nanos) {
        self.post_as(WakeCause::boosted(WakeReason::Woken, until));
    }

    fn post_as(&self, cause: WakeCause) {
        preempt_off(|p| {
            let env = Poster { cpus: cpus(), kicker: &HW, preempt: p };
            self.0.post(cause, &env);
        });
    }

    /// Wake at most `limit` threads waiting with `token`, and answer how many
    /// were parked; one that was not is told anyway and spends nothing.
    pub fn post_n(&self, token: u64, limit: usize) -> usize {
        preempt_off(|p| {
            let env = Poster { cpus: cpus(), kicker: &HW, preempt: p };
            self.0.post_n(token, limit, WakeCause::new(WakeReason::Woken), &env)
        })
    }

    /// End every wait whose token lies in `[from, from + len)`, taken out
    /// before woken so a later post for a reused token cannot reach it; answer
    /// how many.
    pub fn revoke_range(&self, from: u64, len: u64) -> usize {
        preempt_off(|p| {
            let env = Poster { cpus: cpus(), kicker: &HW, preempt: p };
            self.0.revoke(
                |token| token >= from && token - from < len,
                WakeCause::new(WakeReason::Woken),
                &env,
            )
        })
    }

    /// A poll ring's entry, from `inbox`'s registration and nowhere else.
    pub(crate) fn add_poll(&self, entry: PollEntry) {
        self.0.add_ring(entry);
    }

    /// Answer every poll registered here as gone: the source it watched ended.
    pub fn cancel_polls(&self) {
        self.0.cancel_rings();
    }
}

/// A thread's registration on one watch, held across its wait and ended by
/// its drop.
#[must_use = "a registration must outlive the park it was made for"]
pub struct Armed<'a> {
    watch: &'a Watch,
    shared: Arc<KShared>,
    task: Arc<TaskHandle>,
    /// Wait class for the blocked-time breakdown; the park carries no subject
    /// to read one from.
    class: WaitClass,
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        self.watch.0.unregister(&self.shared);
    }
}

/// Register the running task on `watch`; `None` when there is no current task.
/// Call before reading the condition the wait is for.
pub fn arm(watch: &Watch, token: u64, class: WaitClass) -> Option<Armed<'_>> {
    let task = crate::sched::driver::current_handle()?;
    let shared = crate::sched::driver::current_shared()?;
    watch.0.register(&shared, token);
    Some(Armed { watch, shared, task, class })
}

/// The answer a killed thread gets instead of a wake; kernel code cannot
/// construct one.
#[derive(Debug)]
pub struct Cancelled(());

/// Why one park ended the wait rather than returning for a recheck.
enum Ended {
    Cancelled,
    /// A revoke took the registration out: no post can end a park made for
    /// it, and every phase 1 refuses until the thread registers again.
    Revoked,
}

/// Only a futex's watch is revoked, and only [`wait_until`] waits on one.
#[track_caller]
fn not_revocable() -> ! {
    panic!("watch: a revoke reached a wait that does not end on one");
}

/// Park once: until a post reaches this registration, the deadline passes, or
/// this thread is cancelled. A return is not an answer — the caller re-reads
/// its condition.
#[track_caller]
pub fn wait(p: &Parkable, armed: &Armed<'_>, deadline: Deadline) -> Result<(), Cancelled> {
    match wait_inner(p, armed, deadline, Cancel::Answers) {
        Ok(()) => Ok(()),
        Err(Ended::Cancelled) => Err(Cancelled(())),
        Err(Ended::Revoked) => not_revocable(),
    }
}

/// The same as [`wait`], for a wait a kill may not end.
#[track_caller]
pub fn wait_uncancellable(p: &Parkable, armed: &Armed<'_>, deadline: Deadline) {
    match wait_inner(p, armed, deadline, Cancel::Ignores) {
        Ok(()) => {}
        Err(Ended::Cancelled) => unreachable!("an uncancellable wait never reports a cancel"),
        Err(Ended::Revoked) => not_revocable(),
    }
}

/// Register, then park until `ready()` holds, the deadline passes, a revoke
/// ends the registration, or this thread is cancelled. `Ok` on the deadline
/// and the revoke too: the deadline is the caller's, a revoked subject cannot
/// be waited on again safely, and the caller reads its own condition again to
/// tell them apart.
#[track_caller]
pub fn wait_until(
    p: &Parkable,
    watch: &Watch,
    token: u64,
    class: WaitClass,
    deadline: Deadline,
    ready: impl Fn() -> bool,
) -> Result<(), Cancelled> {
    if ready() {
        return Ok(());
    }
    let Some(armed) = arm(watch, token, class) else {
        // No current task (boot or an idle CPU): neither reaches a blocking syscall.
        return Ok(());
    };
    while !ready() {
        if deadline.reached(crate::clock::now()) {
            return Ok(());
        }
        #[cfg(feature = "boot-actuators")]
        window::hold(&armed);
        match wait_inner(p, &armed, deadline, Cancel::Answers) {
            Ok(()) => {}
            Err(Ended::Cancelled) => return Err(Cancelled(())),
            Err(Ended::Revoked) => return Ok(()),
        }
    }
    Ok(())
}

/// `watch-window`: hold a pipe waiter between reading its condition false and
/// its phase 1 until a post lands there — the post only the notified bit
/// carries to the commit — or its budget lapses. The holds a post ended are
/// counted, and every `STEP`th is a `HELD` line the harness reads: a boot
/// whose count did not move staged nothing, however green its canary.
#[cfg(feature = "boot-actuators")]
mod window {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use toyos_sched::task::WaitClass;
    use toyos_sched::watch::window::{HELD, STEP};

    use super::Armed;
    use crate::time::{Budget, Deadline, Duration};

    /// A pipe wait nothing posts — a reader whose writer is idle — ends its
    /// hold here and waits on unstaged; the count of those is on every line.
    const WINDOW: Budget = Budget::of(
        Duration::from_millis(50),
        "the wait goes on unstaged, counted as lapsed",
    );

    static POSTED: AtomicU64 = AtomicU64::new(0);
    static LAPSED: AtomicU64 = AtomicU64::new(0);

    pub(super) fn hold(armed: &Armed<'_>) {
        if !crate::actuator::watch_window() || armed.class != WaitClass::Pipe {
            return;
        }
        let deadline = Deadline::at(crate::clock::now() + WINDOW.duration());
        loop {
            // Counted only on the bit itself: a hold that ended any other way
            // staged nothing.
            if armed.shared.notified() {
                let posted = POSTED.fetch_add(1, Relaxed) + 1;
                if posted.is_multiple_of(STEP) {
                    crate::log!("{HELD} {posted} times, {} lapsed", LAPSED.load(Relaxed));
                }
                return;
            }
            if deadline.reached(crate::clock::now()) {
                LAPSED.fetch_add(1, Relaxed);
                return;
            }
            core::hint::spin_loop();
        }
    }
}

/// Register, then park until `ready()` holds, for a wait a kill may not end
/// and no deadline bounds.
#[track_caller]
pub fn wait_uncancellable_until(p: &Parkable, watch: &Watch, token: u64, ready: impl Fn() -> bool) {
    if ready() {
        return;
    }
    // No current task cannot be answered by returning: the caller would carry
    // on believing it holds a lock it never took.
    // Class is fixed at `Other`: blocked time on a lock is the holder's reason, not the contender's.
    let armed = arm(watch, token, WaitClass::Other)
        .expect("watch: an uncancellable wait with no task to park");
    // Exits only on the predicate: returning without it here means returning
    // without the lock held.
    while !ready() {
        wait_uncancellable(p, &armed, Deadline::never());
    }
}

/// Parks forever rather than exiting: exiting frees a stack a producer may still write to.
#[cfg(feature = "boot-actuators")]
#[track_caller]
pub fn park_forever() -> ! {
    let parkable = crate::scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("a kernel thread is a task");
    let armed = arm(handle.watch(), 0, WaitClass::Other).expect("a task can arm");
    loop {
        let _ = wait(&parkable, &armed, Deadline::never());
    }
}

#[track_caller]
fn wait_inner(
    _p: &Parkable,
    armed: &Armed<'_>,
    deadline: Deadline,
    cancel: Cancel,
) -> Result<(), Ended> {
    let killed = || cancel == Cancel::Answers && armed.task.take_cancel(armed.shared.kill_pending());
    if killed() {
        return Err(Ended::Cancelled);
    }
    if deadline.reached(crate::clock::now()) {
        return Ok(());
    }
    // A post since the registration refuses phase 1, and one after it claims
    // the commit: either way this returns without parking, and the caller
    // re-reads its condition.
    let ticket = match crate::scheduler::prepare_wait(cancel, armed.class) {
        Ok(ticket) => ticket,
        Err(Refused::Notified) => return Ok(()),
        Err(Refused::Revoked) => return Err(Ended::Revoked),
    };
    crate::scheduler::block_on(ticket, deadline);
    if killed() {
        return Err(Ended::Cancelled);
    }
    Ok(())
}
