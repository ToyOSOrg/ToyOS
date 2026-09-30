//! What makes a post legal in an interrupt handler, which interrupts whoever
//! holds the allocator's lock: nothing allocates or frees under a watch's list
//! lock, and a post in place frees nothing at all.
//!
//! The allocator below counts every allocation and free this thread makes
//! while it holds a list lock, and every one it makes inside a post in place.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::{AcqRel, Acquire, Relaxed}};
use std::sync::{Arc, Mutex};

use toyos_sched::cpu::{CpuHandle, CpuHandles};
use toyos_sched::hw::{CpuId, Kicker};
use toyos_sched::mailbox::{mailbox, PreemptGuard, SchedMsg};
use toyos_sched::park::{prepare, Cancel, Commit, CurrentTask};
use toyos_sched::sync::LeafLock;
use toyos_sched::task::{TaskKey, TaskShared, TaskState, WaitClass, WakeCause, WakeReason};
use toyos_sched::watch::{Fire, Poster, Ring, Waiters, Watch};

thread_local! {
    static HELD: Cell<bool> = const { Cell::new(false) };
    static POSTING: Cell<bool> = const { Cell::new(false) };
}

static UNDER_LOCK: AtomicUsize = AtomicUsize::new(0);
static IN_POST: AtomicUsize = AtomicUsize::new(0);

struct Counting;

impl Counting {
    fn note(&self) {
        // `try_with`: a thread allocates while its locals are torn down.
        if HELD.try_with(Cell::get).unwrap_or(false) {
            UNDER_LOCK.fetch_add(1, Relaxed);
        }
        if POSTING.try_with(Cell::get).unwrap_or(false) {
            IN_POST.fetch_add(1, Relaxed);
        }
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

impl<T: Send> LeafLock<T> for Watched<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.0.lock().unwrap();
        let was = HELD.replace(true);
        let out = f(&mut guard);
        HELD.set(was);
        out
    }
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

const C0: CpuId = CpuId(0);

fn clean(phase: &str) {
    assert_eq!(UNDER_LOCK.load(Relaxed), 0, "{phase} allocated or freed under the list lock");
    assert_eq!(IN_POST.load(Relaxed), 0, "{phase}: a post in place allocated or freed");
}

#[test]
fn nothing_allocates_or_frees_under_the_list_lock_and_a_post_in_place_frees_nothing() {
    let (tx, mut rx) = mailbox::<Msg>();
    let cpus = CpuHandles::new(vec![CpuHandle::new(C0, tx)]);
    let env = Poster { cpus: &cpus, kicker: &NoKick, preempt: &NoPreempt };
    let w: Watch<Msg, Entry, Watched<Waiters<Msg, Entry>>> =
        Watch::new(Watched(Mutex::new(Waiters::new())));

    // Nine of each grows both lists from nothing three times over.
    let tasks: Vec<_> =
        (0..9).map(|k| Arc::new(TaskShared::new(TaskKey(k), TaskState::Running(C0)))).collect();
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

    // Every entry is dead now, and a withdrawn one joins them: these two
    // registrations sweep all ten.
    let withdrawn = Arc::new(Poll::default());
    w.add_ring(Entry(withdrawn.clone()));
    withdrawn.0.store(2, Relaxed);
    w.add_ring(Entry(Arc::new(Poll::default())));
    clean("sweeping");
    assert!(polls.iter().all(|p| Arc::strong_count(p) == 1), "the sweep let go of every fired entry");

    // A thread's post frees what it fired, with the lock let go.
    let later = Arc::new(Poll::default());
    w.add_ring(Entry(later.clone()));
    w.post(WakeCause::new(WakeReason::Woken), &env);
    clean("posting");
    assert_eq!(Arc::strong_count(&later), 1, "the post let go of the entry it fired");

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
