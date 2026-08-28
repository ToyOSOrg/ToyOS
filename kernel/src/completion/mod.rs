//! Completion primitive: a record, an inbox, and a watch a waiter lends to
//! the object it is waiting on.
//!
//! Every wait in the kernel rechecks one predicate, [`Inbox::has_record`], in
//! [`wait_inner`] — no wait source can add a second one.
//!
//! A [`Subject`] is a borrowed reference to the watched object: there is no
//! registry and no id, so a destroyed subject cannot be named.

pub mod inbox;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use inbox::{Inbox, Outcome, Reason, Record, Token};

use toyos_sched::hw::Nanos;

use toyos_sched::task::WaitClass;

use crate::sched::payload::{KShared, TaskHandle};
use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::time::Deadline;

pub use toyos_sched::waitq::Cancel;

/// The waiters armed on one object.
pub struct Watch {
    armed: AtomicUsize,
    waiters: Lock<Vec<Watcher>>,
}

struct Watcher {
    /// `Arc`, not a reference: an abandoned arm becomes a bounded leak, not a use-after-free.
    task: Arc<TaskHandle>,
    /// The claim half of the post; kept beside `task` since the two are minted separately.
    shared: Arc<KShared>,
    token: Token,
}

impl Watch {
    pub const fn new() -> Self {
        Self {
            armed: AtomicUsize::new(0),
            waiters: Lock::new(Vec::new()),
        }
    }
}

/// What is being waited on: a reference, never an id.
#[derive(Clone, Copy)]
pub struct Subject<'a>(&'a Watch);

impl<'a> Subject<'a> {
    pub const fn of(watch: &'a Watch) -> Self {
        Self(watch)
    }
}

/// Proof that a record will arrive on the armed inbox for this token.
#[must_use = "an arm must outlive the park it was made for"]
pub struct Armed<'a> {
    subject: Subject<'a>,
    task: Arc<TaskHandle>,
    shared: Arc<KShared>,
    token: Token,
    /// Wait class for the blocked-time breakdown; the park queue carries no subject to read one from.
    class: WaitClass,
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        let watch = self.subject.0;
        let mut waiters = watch.waiters.lock();
        if let Some(at) = waiters.iter().position(|w| Arc::ptr_eq(&w.task, &self.task)) {
            waiters.swap_remove(at);
        }
        watch.armed.store(waiters.len(), Ordering::Relaxed);
        drop(waiters);
        let inbox = self.task.inbox();
        inbox.disarm();
        while let Some(record) = inbox.take() {
            // A mismatched token means two posters wrote this inbox; the
            // overflow notice is exempt, minted by the taker without a subject.
            assert!(
                record.token == self.token
                    || record.outcome == Outcome::Gone(Reason::Overflowed),
                "completion: a record posted at {} reached an inbox armed on another subject",
                record.at.nanos_since_boot(),
            );
        }
    }
}

/// Arms a watch for the running task; `None` when there is no current task.
pub fn arm(subject: Subject<'_>, token: Token, class: WaitClass) -> Option<Armed<'_>> {
    let task = crate::sched::driver::current_handle()?;
    let shared = crate::sched::driver::current_shared()?;
    let inbox = task.inbox();
    assert!(
        !inbox.is_armed(),
        "completion::arm: this task is already armed on a subject",
    );
    inbox.arm_to(token);
    let watch = subject.0;
    let mut waiters = watch.waiters.lock();
    waiters.push(Watcher {
        task: task.clone(),
        shared: shared.clone(),
        token,
    });
    watch.armed.store(waiters.len(), Ordering::Relaxed);
    drop(waiters);
    Some(Armed { subject, task, shared, token, class })
}

/// Tell everyone armed on `subject` that something happened; callable from an interrupt handler or inside a lock.
pub fn post(subject: Subject<'_>, outcome: Outcome) {
    post_with(subject, outcome, None)
}

/// The same as [`post`], lending the poster's real-time window to whoever it wakes.
pub fn post_boosted(subject: Subject<'_>, outcome: Outcome, until: Nanos) {
    post_with(subject, outcome, Some(until))
}

/// Tell at most `limit` waiters armed on `subject` for `token`; returns the count told.
pub fn post_n(subject: Subject<'_>, outcome: Outcome, token: Token, limit: usize) -> usize {
    let watch = subject.0;
    if limit == 0 || watch.armed.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    let at = crate::clock::now();
    let waiters = watch.waiters.lock();
    let mut woken = 0;
    for waiter in waiters.iter() {
        if woken == limit {
            break;
        }
        if waiter.token != token {
            continue;
        }
        // A waiter another claim already won is skipped and does not spend `limit`.
        if post_to(waiter, outcome, at, None) {
            woken += 1;
        }
    }
    woken
}

/// End every wait armed on `subject` whose token lies in `[from, from + len)`,
/// remove those watchers, and answer how many there were.
pub fn revoke_range(subject: Subject<'_>, from: u64, len: u64) -> usize {
    let watch = subject.0;
    if watch.armed.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    let at = crate::clock::now();
    let mut waiters = watch.waiters.lock();
    let mut ended = 0;
    let mut at_index = 0;
    while at_index < waiters.len() {
        let token = waiters[at_index].token.raw();
        if token < from || token - from >= len {
            at_index += 1;
            continue;
        }
        // Removed before posted: a revoked token may already name something
        // else, so no post can reach this watcher after the swap_remove.
        let waiter = waiters.swap_remove(at_index);
        post_to(&waiter, Outcome::Gone(Reason::Closed), at, None);
        ended += 1;
    }
    watch.armed.store(waiters.len(), Ordering::Relaxed);
    ended
}

fn post_with(subject: Subject<'_>, outcome: Outcome, boost: Option<Nanos>) {
    let watch = subject.0;
    // Read before the lock: an idle subject's post costs no lock acquire.
    if watch.armed.load(Ordering::Relaxed) == 0 {
        return;
    }
    let at = crate::clock::now();
    let waiters = watch.waiters.lock();
    for waiter in waiters.iter() {
        let _ = post_to(waiter, outcome, at, boost);
    }
}

/// Invariant W: the record is stored before the claim is attempted, both under `subject`'s leaf lock.
/// Returns whether this call won the waiter's claim, not merely left a record.
fn post_to(
    waiter: &Watcher,
    outcome: Outcome,
    at: crate::time::Instant,
    boost: Option<Nanos>,
) -> bool {
    waiter.task.inbox().post(Record {
        token: waiter.token,
        outcome,
        at,
    });
    crate::scheduler::wake_sched(&waiter.shared, boost)
}

/// The answer a killed thread gets instead of a record; kernel code cannot construct one.
#[derive(Debug)]
pub struct Cancelled(());

/// Park until a record arrives, the deadline passes, or this thread is cancelled.
#[track_caller]
pub fn wait(p: &Parkable, armed: &Armed<'_>, deadline: Deadline) -> Result<Record, Cancelled> {
    wait_inner(p, armed, deadline, Cancel::Answers)
}

/// The same as [`wait`], for a wait a kill may not end.
#[track_caller]
pub fn wait_uncancellable(p: &Parkable, armed: &Armed<'_>, deadline: Deadline) -> Record {
    match wait_inner(p, armed, deadline, Cancel::Ignores) {
        Ok(record) => record,
        Err(_) => unreachable!("an uncancellable wait never reports a cancel"),
    }
}

/// Arm, then park until `ready()` holds, the deadline passes, or this thread is cancelled.
#[track_caller]
pub fn wait_until(
    p: &Parkable,
    subject: Subject<'_>,
    token: Token,
    class: WaitClass,
    deadline: Deadline,
    ready: impl Fn() -> bool,
) -> Result<(), Cancelled> {
    if ready() {
        return Ok(());
    }
    let Some(armed) = arm(subject, token, class) else {
        // No current task (boot or an idle CPU): neither reaches a blocking syscall.
        return Ok(());
    };
    loop {
        if ready() {
            return Ok(());
        }
        // A record means state may have moved, not that `ready` now holds.
        let record = wait(p, &armed, deadline)?;
        // Overflowed is excluded: it means re-derive, which the loop already does.
        if let Outcome::Gone(Reason::Expired | Reason::Closed) = record.outcome {
            // Returns Ok without the predicate: the deadline is the caller's,
            // and a closed subject cannot be re-derived against safely.
            return Ok(());
        }
    }
}

/// Arm, then park until `ready()` holds, for a wait a kill may not end and no deadline bounds.
#[track_caller]
pub fn wait_uncancellable_until(
    p: &Parkable,
    subject: Subject<'_>,
    token: Token,
    ready: impl Fn() -> bool,
) {
    if ready() {
        return;
    }
    // No current task cannot be answered by returning: the caller would carry
    // on believing it holds a lock it never took.
    // Class is fixed at `Other`: blocked time on a lock is the holder's reason, not the contender's.
    let armed = arm(subject, token, WaitClass::Other)
        .expect("completion: an uncancellable wait with no task to park");
    // Exits only on the predicate: returning without it here means returning
    // without the lock held.
    while !ready() {
        let _ = wait_inner(p, &armed, Deadline::never(), Cancel::Ignores);
    }
}

/// Parks forever rather than exiting: exiting frees a stack a producer may still write to.
#[cfg(feature = "boot-actuators")]
#[track_caller]
pub fn park_forever() -> ! {
    let parkable = crate::scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("a kernel thread is a task");
    let armed =
        arm(Subject::of(handle.watch()), Token::new(0), WaitClass::Other).expect("a task can arm");
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
) -> Result<Record, Cancelled> {
    let task = &armed.task;
    loop {
        if let Some(record) = task.inbox().take() {
            return Ok(record);
        }
        if cancel == Cancel::Answers && task.take_cancel(armed.shared.kill_pending()) {
            return Err(Cancelled(()));
        }
        if deadline.reached(crate::clock::now()) {
            return Ok(Record {
                token: armed.token,
                outcome: Outcome::Gone(Reason::Expired),
                at: crate::clock::now(),
            });
        }
        // Registration precedes the recheck: that ordering is the whole lost-wake argument.
        let ticket = crate::scheduler::prepare_wait(task.park_queue(), cancel, armed.class);
        if task.inbox().has_record()
            || (cancel == Cancel::Answers && armed.shared.kill_pending())
        {
            ticket.cancel();
            continue;
        }
        crate::scheduler::block_on(ticket, deadline);
    }
}
