//! The one thread usbd runs on, as tasks that each wait on one thing.
//!
//! **A task waits only at a leaf**: [`until`], which is ready when its
//! condition holds, or with a deadline when the clock passes it. A task's own
//! code runs only between a leaf that came ready and the next one it waits
//! at, so a round of [`Tasks::run`] in which no leaf came ready changed
//! nothing, and the next round would be the same: that is where a run stops,
//! with the earliest deadline any leaf asked for. So nothing spins, and
//! nothing is missed between two wakes.
//!
//! Nothing here allocates per poll, and no waker is ever woken: every task is
//! polled every round, which is cheap at a root hub's handful of tasks.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

/// Nanoseconds since boot, as the caller's clock counts them.
pub type Nanos = u64;

/// What usbd's process gives the driver: the clock its deadlines are on, one
/// counter read, and where its lines go.
pub trait Host {
    fn now(&self) -> Nanos;
    fn say(&self, line: core::fmt::Arguments<'_>);
}

/// What every leaf reports to: the clock, the earliest deadline asked for,
/// and whether any leaf came ready this round.
pub struct Wake<C> {
    clock: C,
    at: Cell<Option<Nanos>>,
    moved: Cell<bool>,
}

impl<C: Host> Wake<C> {
    pub fn new(clock: C) -> Self {
        Self { clock, at: Cell::new(None), moved: Cell::new(false) }
    }

    pub fn now(&self) -> Nanos {
        self.clock.now()
    }

    pub fn host(&self) -> &C {
        &self.clock
    }

    fn deadline(&self, at: Nanos) {
        self.at.set(Some(self.at.get().map_or(at, |was| was.min(at))));
    }

    /// Something a task waits on changed outside every task: the next run
    /// polls them all.
    pub fn stir(&self) {
        self.moved.set(true);
    }
}

/// Ready with `f`'s answer once it has one.
pub fn until<'a, C: Host, T>(wake: &'a Wake<C>, mut f: impl FnMut() -> Option<T> + 'a) -> impl Future<Output = T> + 'a {
    poll_fn(move |_| match f() {
        Some(t) => {
            wake.moved.set(true);
            Poll::Ready(t)
        }
        None => Poll::Pending,
    })
}

/// Ready with `f`'s answer once it has one, or with `None` once the clock
/// reaches `at`, whichever is first.
pub fn until_or<'a, C: Host, T>(
    wake: &'a Wake<C>,
    at: Nanos,
    mut f: impl FnMut() -> Option<T> + 'a,
) -> impl Future<Output = Option<T>> + 'a {
    poll_fn(move |_| {
        if let Some(t) = f() {
            wake.moved.set(true);
            return Poll::Ready(Some(t));
        }
        if wake.now() >= at {
            wake.moved.set(true);
            return Poll::Ready(None);
        }
        wake.deadline(at);
        Poll::Pending
    })
}

/// Ready once the clock reaches `at`.
pub async fn sleep_until<C: Host>(wake: &Wake<C>, at: Nanos) {
    until_or(wake, at, || None::<()>).await;
}

/// A task, or one function of a device's: it ends with the driver's death or
/// its own.
pub type Task<'a> = Pin<Box<dyn Future<Output = Result<(), Dead>> + 'a>>;

/// Every one of `all`, run together; ready once each is, or with the first
/// that ended the driver.
pub async fn join(mut all: Vec<Task<'_>>) -> Result<(), Dead> {
    poll_fn(move |cx| {
        let mut i = 0;
        while i < all.len() {
            match all[i].as_mut().poll(cx) {
                Poll::Pending => i += 1,
                Poll::Ready(Ok(())) => drop(all.remove(i)),
                Poll::Ready(Err(dead)) => return Poll::Ready(Err(dead)),
            }
        }
        if all.is_empty() { Poll::Ready(Ok(())) } else { Poll::Pending }
    })
    .await
}

impl<C: Host> Wake<C> {
    /// Ask the run to end no later than `at`: for a hand-written leaf.
    pub fn wake_at(&self, at: Nanos) {
        self.deadline(at);
    }
}

/// Why the driver stopped: the controller no longer answers, and nothing
/// here can take back what it holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dead(pub &'static str);


/// Every task, polled in the order spawned.
#[derive(Default)]
pub struct Tasks<'a> {
    tasks: Vec<Task<'a>>,
}

/// Rounds one run may take before a task is called a spin: every round but
/// the last moved something, and no sequence in this driver moves that often
/// without waiting.
const ROUNDS: usize = 100_000;

impl<'a> Tasks<'a> {
    pub fn spawn(&mut self, task: impl Future<Output = Result<(), Dead>> + 'a) {
        self.tasks.push(Box::pin(task));
    }

    /// Poll every task until a round moves nothing; answer the earliest
    /// deadline a leaf is waiting on, or why the driver is dead.
    pub fn run<C: Host>(&mut self, wake: &Wake<C>) -> Result<Option<Nanos>, Dead> {
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..ROUNDS {
            wake.moved.set(false);
            wake.at.set(None);
            let mut i = 0;
            while i < self.tasks.len() {
                match self.tasks[i].as_mut().poll(&mut cx) {
                    Poll::Pending => i += 1,
                    Poll::Ready(Ok(())) => {
                        drop(self.tasks.remove(i));
                        wake.moved.set(true);
                    }
                    Poll::Ready(Err(dead)) => return Err(dead),
                }
            }
            if !wake.moved.get() {
                return Ok(wake.at.get());
            }
        }
        panic!("usbd: {ROUNDS} rounds in one run each moved something: a task is spinning");
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct Fake(Cell<Nanos>);
    impl Host for &Fake {
        fn now(&self) -> Nanos {
            self.0.get()
        }
        fn say(&self, _: core::fmt::Arguments<'_>) {}
    }

    /// A task that waits on another's write runs in the same run, and a run
    /// with nothing ready answers the deadline asked for.
    #[test]
    fn a_run_ends_when_nothing_moves_and_answers_the_earliest_deadline() {
        let clock = Fake(Cell::new(0));
        let wake = Wake::new(&clock);
        let flag = Cell::new(false);
        let seen = Cell::new(0);
        let mut tasks = Tasks::default();
        tasks.spawn(async {
            until(&wake, || seen.get().eq(&1).then_some(())).await;
            seen.set(2);
            Ok(())
        });
        tasks.spawn(async {
            until(&wake, || flag.get().then_some(())).await;
            seen.set(1);
            sleep_until(&wake, 50).await;
            seen.set(3);
            Ok(())
        });
        tasks.spawn(async {
            sleep_until(&wake, 20).await;
            Ok(())
        });
        assert_eq!(tasks.run(&wake), Ok(Some(20)));
        flag.set(true);
        assert_eq!(tasks.run(&wake), Ok(Some(20)));
        assert_eq!(seen.get(), 2, "the waiter on the setter's write ran in the setter's run");
        clock.0.set(20);
        assert_eq!(tasks.run(&wake), Ok(Some(50)));
        clock.0.set(50);
        assert_eq!(tasks.run(&wake), Ok(None));
        assert_eq!((seen.get(), tasks.len()), (3, 0));
    }
}
