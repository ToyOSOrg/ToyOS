//! What makes a post legal in an interrupt handler, which interrupts whoever
//! holds the allocator's lock: nothing allocates or frees under a watch's list
//! lock, and a post in place frees nothing at all.
//!
//! The allocator below counts every allocation and free this thread makes
//! while it holds a list lock, and every one it makes inside a post in place.
//! The list lock counts its sections, and can run a stage between two of them.

// A `GlobalAlloc` is `unsafe` to implement.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering::{AcqRel, Acquire, Relaxed}};
use std::sync::Mutex;
use std::vec::Vec;
use std::{thread_local, vec};

use crate::sched::cpu::{CpuHandle, CpuHandles};
use crate::sched::hw::{CpuId, Kicker};
use crate::sched::mailbox::{mailbox, PreemptGuard, SchedMsg};
use crate::sched::park::{prepare, Cancel, Commit, CurrentTask};
use crate::sched::sync::{Arc, CellLock};
use crate::sched::task::{TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use crate::sched::watch::{Fire, Poster, Ring, Waiters, Watch, FEW};

thread_local! {
    static HELD: Cell<bool> = const { Cell::new(false) };
    static POSTING: Cell<bool> = const { Cell::new(false) };
    static UNDER_LOCK: Cell<usize> = const { Cell::new(0) };
    static IN_POST: Cell<usize> = const { Cell::new(0) };
    static SECTIONS: Cell<usize> = const { Cell::new(0) };
    /// Sections left to start before [`BETWEEN`] runs, ahead of the last one.
    static COUNTDOWN: Cell<usize> = const { Cell::new(0) };
    static BETWEEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

struct Counting;

impl Counting {
    fn note(&self) {
        // `try_with`: a thread allocates while its locals are torn down.
        let count = |flag: &'static std::thread::LocalKey<Cell<bool>>,
                     counter: &'static std::thread::LocalKey<Cell<usize>>| {
            if flag.try_with(Cell::get).unwrap_or(false) {
                let _ = counter.try_with(|n| n.set(n.get() + 1));
            }
        };
        count(&HELD, &UNDER_LOCK);
        count(&POSTING, &IN_POST);
    }
}

// SAFETY: every method forwards to `System` unchanged after a count.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.note();
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.note();
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        self.note();
        // SAFETY: the caller's contract, passed on.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The list lock, marking this thread as holding it.
struct Watched<T>(Mutex<T>);

impl<T: Send> CellLock<T> for Watched<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        // Before the lock is taken, so a stage lands between two sections.
        let due = COUNTDOWN.get();
        if due > 0 {
            COUNTDOWN.set(due - 1);
            if due == 1 {
                let between = BETWEEN.take().expect("a countdown is staged with its stage");
                between();
            }
        }
        SECTIONS.set(SECTIONS.get() + 1);
        let mut guard = self.0.lock().unwrap();
        let was = HELD.replace(true);
        let out = f(&mut guard);
        HELD.set(was);
        out
    }
}

/// Run `between` with no list lock held, just before the `nth` section from
/// here takes its lock.
fn stage(nth: usize, between: impl FnOnce() + 'static) {
    BETWEEN.set(Some(Box::new(between)));
    COUNTDOWN.set(nth);
}

fn sections(f: impl FnOnce()) -> usize {
    let before = SECTIONS.get();
    f();
    SECTIONS.get() - before
}

#[derive(Debug)]
enum Msg {
    Wake,
    Retire,
}

impl SchedMsg for Msg {
    fn wake(_key: TaskKey, _cause: WakeCause) -> Self {
        Msg::Wake
    }
    fn retire(_shared: Arc<TaskShared<Self>>) -> Self {
        Msg::Retire
    }
}

struct NoPreempt;

// SAFETY: one thread and no scheduler, so nothing can deschedule a push.
unsafe impl PreemptGuard for NoPreempt {}

struct NoKick;

impl Kicker for NoKick {
    fn kick(&self, _target: CpuId) {}
}

/// 0 armed, 1 fired, 2 withdrawn.
#[derive(Default)]
struct Poll(AtomicU32);

struct Entry(Arc<Poll>);

impl Ring for Entry {
    fn fire(&self, _how: Fire) {
        let _ = self.0 .0.compare_exchange(0, 1, AcqRel, Acquire);
    }
    fn live(&self) -> bool {
        self.0 .0.load(Acquire) == 0
    }
}

type TestWatch = Watch<Msg, Entry, Watched<Waiters<Msg, Entry>>>;

const C0: CpuId = CpuId(0);

fn watch() -> TestWatch {
    Watch::new(Watched(Mutex::new(Waiters::new())))
}

fn task(key: u64) -> Arc<TaskShared<Msg>> {
    Arc::new(TaskShared::new(TaskKey(key), TaskState::Running(C0)))
}

fn clean(phase: &str) {
    assert_eq!(UNDER_LOCK.get(), 0, "{phase} allocated or freed under the list lock");
    assert_eq!(IN_POST.get(), 0, "{phase}: a post in place allocated or freed");
}

#[test]
fn nothing_allocates_or_frees_under_the_list_lock_and_a_post_in_place_frees_nothing() {
    let (tx, mut rx) = mailbox::<Msg>();
    let cpus = CpuHandles::new(vec![CpuHandle::new(C0, tx)]);
    let env = Poster { cpus: &cpus, kicker: &NoKick, preempt: &NoPreempt };
    let w = watch();

    // Nine of each grows both lists from nothing three times over.
    let tasks: Vec<_> = (0..9).map(task).collect();
    for (token, t) in tasks.iter().enumerate() {
        w.register(t, token as u64);
        let ticket = prepare(&CurrentTask::new(t, C0), Cancel::Answers, WaitClass::Other)
            .expect("nothing has posted yet");
        assert!(matches!(ticket.commit(), Commit::Parked(_)));
    }
    let polls: Vec<_> = (0..9).map(|_| Arc::new(Poll::default())).collect();
    for poll in &polls {
        w.add_ring(Entry(poll.clone()));
    }
    clean("registering");

    POSTING.set(true);
    w.post_in_place(WakeCause::new(WakeReason::Woken), &env);
    POSTING.set(false);
    clean("posting in place");
    assert!(polls.iter().all(|p| p.0.load(Acquire) == 1), "a post in place fired every entry");

    // Every entry is dead now, and a withdrawn one joins them: these three
    // registrations sweep all ten, more than one section takes out.
    let withdrawn = Arc::new(Poll::default());
    let first = Entry(withdrawn.clone());
    assert_eq!(sections(|| w.add_ring(first)), 1, "a re-arm after a post in place took the lock twice");
    withdrawn.0.store(2, Relaxed);
    let fresh: Vec<_> = (0..2).map(|_| Arc::new(Poll::default())).collect();
    for poll in &fresh {
        w.add_ring(Entry(poll.clone()));
    }
    clean("sweeping");
    assert!(polls.iter().all(|p| Arc::strong_count(p) == 1), "the sweep let go of every fired entry");
    assert_eq!(Arc::strong_count(&withdrawn), 1, "the sweep let go of the withdrawn entry");

    // A thread's post frees what it fired, with the lock let go, and leaves
    // the list's buffer to the next registration.
    let later = Arc::new(Poll::default());
    w.add_ring(Entry(later.clone()));
    w.post(WakeCause::new(WakeReason::Woken), &env);
    clean("posting");
    assert_eq!(Arc::strong_count(&later), 1, "the post let go of the entry it fired");
    let rearmed = Entry(Arc::new(Poll::default()));
    assert_eq!(sections(|| w.add_ring(rearmed)), 1, "a re-arm after a post regrew the list");

    assert_eq!(w.post_n(3, 1, WakeCause::new(WakeReason::Woken), &env), 0, "every waiter is claimed");
    assert_eq!(w.revoke(|token| token % 2 == 0, WakeCause::new(WakeReason::Woken), &env), 5);
    for t in &tasks {
        w.unregister(t);
    }
    w.cancel_rings();
    clean("ending");

    while rx.pop(&NoPreempt).is_some() {}
    drop(w);
    clean("dropping");
}

/// A live entry that is the last owner of a heap cell, so its drop frees, and
/// that counts its fires on a word the test keeps.
struct Owned {
    fired: Arc<AtomicU32>,
    _cell: Box<u64>,
}

impl Ring for Owned {
    fn fire(&self, _how: Fire) {
        self.fired.fetch_add(1, AcqRel);
    }
    fn live(&self) -> bool {
        self.fired.load(Acquire) == 0
    }
}

type OwnedWatch = Watch<Msg, Owned, Watched<Waiters<Msg, Owned>>>;

fn owned(fired: &Arc<AtomicU32>) -> Owned {
    Owned { fired: fired.clone(), _cell: Box::new(0) }
}

/// A thread's post of `n` live entries: every entry is fired once, and freed
/// with the list lock let go.
fn post_live(n: usize) -> OwnedWatch {
    let (tx, _rx) = mailbox::<Msg>();
    let cpus = CpuHandles::new(vec![CpuHandle::new(C0, tx)]);
    let env = Poster { cpus: &cpus, kicker: &NoKick, preempt: &NoPreempt };
    let w: OwnedWatch = Watch::new(Watched(Mutex::new(Waiters::new())));
    let fired: Vec<_> = (0..n).map(|_| Arc::new(AtomicU32::new(0))).collect();
    for word in &fired {
        w.add_ring(owned(word));
    }
    clean("registering");
    w.post(WakeCause::new(WakeReason::Woken), &env);
    clean("posting");
    for (at, word) in fired.iter().enumerate() {
        assert_eq!(word.load(Acquire), 1, "entry {at} of {n} fired other than once");
        assert_eq!(Arc::strong_count(word), 1, "entry {at} of {n} was not let go of");
    }
    w
}

/// One entry past the [`FEW`] a post takes out on its stack.
#[test]
fn a_post_of_one_entry_past_its_stack_fires_each_once_and_frees_none_under_the_lock() {
    post_live(FEW + 1);
}

/// Exactly the [`FEW`] a post takes out on its stack: the list's buffer is
/// left to the re-arm.
#[test]
fn a_re_arm_after_a_post_of_few_entries_is_one_section() {
    let w = post_live(FEW);
    let rearmed = owned(&Arc::new(AtomicU32::new(0)));
    assert_eq!(sections(|| w.add_ring(rearmed)), 1, "a re-arm after a post regrew the list");
}

/// A registration that finds the list full sizes a bigger buffer with the lock
/// let go. Registrations that fill the list past that buffer before it comes
/// back leave it too small to take the list, and moving the list into it then
/// would grow it under the lock.
#[test]
fn a_list_outgrowing_the_buffer_sized_for_it_is_not_moved_into_it() {
    let w = Arc::new(watch());
    let tasks: Vec<_> = (0..17).map(task).collect();
    // Four fill the list's first buffer, so the fifth grows it.
    for t in &tasks[..4] {
        w.register(t, 0);
    }
    // Twelve more fill it to sixteen, the buffer the fifth sized holds eight.
    let (others, rest) = (w.clone(), tasks[5..].to_vec());
    stage(2, move || {
        for t in &rest {
            others.register(t, 0);
        }
    });
    w.register(&tasks[4], 0);
    clean("growing past a buffer sized before");
    assert_eq!(w.threads(), 17, "a registration was lost");
    for t in &tasks {
        w.unregister(t);
    }
}
