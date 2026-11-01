//! One thread's tasks, polled when woken.
//!
//! **Nothing here waits.** A task that cannot go on is pending until a device
//! answers one of its requests; what makes a device answer is its owner's —
//! the block service's session, read when its connection is — so
//! [`Executor::run_until_stalled`] returns when no task can go on, and
//! [`Executor::block_on`] calls the `wait` it is given then.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::{pin, Pin};
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};

/// Set by a wake, cleared by the poll it asks for.
struct Woken(AtomicBool);

impl Woken {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicBool::new(true)))
    }

    fn take(&self) -> bool {
        self.0.swap(false, Ordering::Acquire)
    }
}

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

struct Task<'a> {
    future: Pin<Box<dyn Future<Output = ()> + 'a>>,
    woken: Arc<Woken>,
    waker: Waker,
}

/// Tasks that borrow for `'a`, run on the thread that drives this.
#[derive(Default)]
pub struct Executor<'a> {
    tasks: RefCell<Vec<Task<'a>>>,
    /// Spawned since the last pass, which a task may do while it is polled.
    spawned: RefCell<Vec<Task<'a>>>,
}

impl<'a> Executor<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + 'a) {
        let woken = Woken::new();
        let waker = Waker::from(Arc::clone(&woken));
        self.spawned.borrow_mut().push(Task { future: Box::pin(future), woken, waker });
    }

    /// Poll every woken task, again and again, until none is woken. Answers
    /// how many tasks have not finished.
    pub fn run_until_stalled(&self) -> usize {
        while self.pass() {}
        self.tasks.borrow().len()
    }

    /// `future` polled, beside every spawned task, until it is ready. When
    /// nothing can go on, `wait` is called, and must return once something
    /// may: a device that answers only when its owner reads it is read there.
    pub fn block_on<F: Future>(&self, future: F, mut wait: impl FnMut()) -> F::Output {
        let mut future = pin!(future);
        let woken = Woken::new();
        let waker = Waker::from(Arc::clone(&woken));
        let mut cx = Context::from_waker(&waker);
        loop {
            if woken.take()
                && let Poll::Ready(out) = future.as_mut().poll(&mut cx)
            {
                return out;
            }
            let ran = self.pass();
            if !ran && !woken.0.load(Ordering::Acquire) {
                wait();
            }
        }
    }

    /// One poll of each task woken since its last; whether any was.
    fn pass(&self) -> bool {
        let mut tasks = self.tasks.borrow_mut();
        tasks.append(&mut self.spawned.borrow_mut());
        let mut ran = false;
        tasks.retain_mut(|task| {
            if !task.woken.take() {
                return true;
            }
            ran = true;
            task.future.as_mut().poll(&mut Context::from_waker(&task.waker)).is_pending()
        });
        ran || !self.spawned.borrow().is_empty()
    }
}

/// [`Executor::block_on`] on an executor of its own.
pub fn block_on<F: Future>(future: F, wait: impl FnMut()) -> F::Output {
    Executor::new().block_on(future, wait)
}

/// [`block_on`] over devices that answer every request as it is asked, such
/// as [`crate::Bytes`]: a future that stalls there waits on nothing that will
/// ever wake it, which is a bug.
pub fn block_on_ready<F: Future>(future: F) -> F::Output {
    block_on(future, || panic!("a future over devices that are always ready stalled"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use core::cell::Cell;

    /// Pending until `open`, then ready; wakes whoever polled it last when
    /// opened.
    #[derive(Default)]
    struct Gate {
        open: Cell<bool>,
        waiter: RefCell<Option<Waker>>,
    }

    impl Gate {
        fn open(&self) {
            self.open.set(true);
            if let Some(w) = self.waiter.take() {
                w.wake();
            }
        }

        async fn pass(&self) {
            core::future::poll_fn(|cx| {
                if self.open.get() {
                    Poll::Ready(())
                } else {
                    *self.waiter.borrow_mut() = Some(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
        }
    }

    #[test]
    fn a_stalled_task_is_left_until_it_is_woken() {
        let gate = Gate::default();
        let done = Cell::new(0);
        let exec = Executor::new();
        exec.spawn(async {
            gate.pass().await;
            done.set(done.get() + 1);
        });
        exec.spawn(async { done.set(done.get() + 1) });
        assert_eq!(exec.run_until_stalled(), 1);
        assert_eq!(done.get(), 1);
        assert_eq!(exec.run_until_stalled(), 1, "nothing woke it");
        gate.open();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(done.get(), 2);
    }

    #[test]
    fn a_task_spawned_by_a_task_runs_in_the_same_drive() {
        let ran = Rc::new(Cell::new(false));
        let exec = Rc::new(Executor::new());
        let (inner, flag) = (Rc::clone(&exec), Rc::clone(&ran));
        exec.spawn(async move {
            inner.spawn(async move { flag.set(true) });
        });
        assert_eq!(exec.run_until_stalled(), 0);
        assert!(ran.get());
    }

    #[test]
    fn block_on_waits_only_when_nothing_can_go_on_and_runs_spawned_tasks() {
        let gate = Gate::default();
        let side = Cell::new(false);
        let waits = Cell::new(0);
        let exec = Executor::new();
        exec.spawn(async { side.set(true) });
        let out = exec.block_on(
            async {
                gate.pass().await;
                7
            },
            || {
                waits.set(waits.get() + 1);
                gate.open();
            },
        );
        assert_eq!((out, waits.get(), side.get()), (7, 1, true));
    }

    #[test]
    #[should_panic(expected = "always ready stalled")]
    fn a_stall_over_ready_devices_is_a_bug() {
        let gate = Gate::default();
        block_on_ready(gate.pass());
    }
}
