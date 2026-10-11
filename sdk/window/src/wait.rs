//! One wait over any number of windows and a wake from another thread.
//!
//! A window's own [`Window::poll_event`](crate::Window::poll_event) waits on
//! that window alone, so a program with two windows, or with a worker thread
//! that has news for its event loop, has nothing to block on. A [`Waiter`] is
//! one [`Poller`] over every window's connection and one [`Bell`] its
//! [`Waker`] rings: the same readiness watch every other handle is waited on
//! through, and no second way of waiting.
//!
//! The wait says only *that* something is ready. The caller then drains each
//! window with `poll_event(0)` and calls [`Waiter::take_wake`], so a readiness
//! the wait did not name is still read, and a wake raised after the drain is
//! still pending at the next wait.

use std::sync::Arc;
use std::time::Duration;

use toyos::poller::{Poller, READABLE};
use toyos::wake::{self, Bell};
use toyos::RawHandle;

pub use toyos::wake::Waker;

/// How a [`Waiter::wait`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    /// A window or the wake became ready.
    Ready,
    /// The timeout passed with nothing ready.
    TimedOut,
}

/// One wait over a set of windows and a [`Waker`].
pub struct Waiter {
    poller: Poller,
    /// The handles `poller` was declared for; see [`Poller::new`].
    capacity: u32,
    bell: Bell,
    waker: Arc<Waker>,
}

impl Waiter {
    /// A waiter with room for a few windows; it grows when asked to wait on
    /// more.
    pub fn new() -> Self {
        // As `Poller::new` does with its inbox: a kernel out of pipes is a
        // machine this process cannot wait on at all.
        let (waker, bell) = wake::pair().expect("Waiter::new: no pipe for the wake");
        let capacity = 4;
        Self { poller: Poller::new(capacity), capacity, bell, waker: Arc::new(waker) }
    }

    /// What ends this waiter's waits, for another thread.
    pub fn waker(&self) -> Arc<Waker> {
        self.waker.clone()
    }

    /// Block until one of `windows` has an event to read, the wake is raised,
    /// or `timeout` passes. `None` waits for as long as it takes; a zero
    /// timeout only looks.
    ///
    /// Each of `windows` is the [`handle`](crate::Window::handle) of a window
    /// this process still holds: the kernel ends a process that watches a
    /// handle it does not hold.
    pub fn wait(
        &mut self,
        windows: impl Iterator<Item = RawHandle> + Clone,
        timeout: Option<Duration>,
    ) -> Woke {
        let watched = u32::try_from(windows.clone().count())
            .ok()
            .and_then(|n| n.checked_add(1))
            .expect("Waiter::wait: more windows than a handle count can name");
        if watched > self.capacity {
            // `Poller::new` refuses past its widest ring by name, and the
            // compositor stops granting windows long before that.
            self.capacity = watched.next_power_of_two();
            self.poller = Poller::new(self.capacity);
        }
        for window in windows {
            self.poller.watch_raw(window, READABLE, 0);
        }
        // The tokens are not read: the caller drains every window after any wait.
        self.poller.watch(&self.bell, READABLE, 0);
        let timeout_nanos = match timeout {
            None => u64::MAX,
            // `u64::MAX` is the kernel's "forever", so a finite wait stops
            // one short of it.
            Some(timeout) => {
                u64::try_from(timeout.as_nanos()).map_or(u64::MAX - 1, |n| n.min(u64::MAX - 1))
            }
        };
        let mut woke = Woke::TimedOut;
        self.poller.wait(1, timeout_nanos, |_| woke = Woke::Ready);
        woke
    }

    /// Take every wake raised so far, and say whether there was one.
    ///
    /// Take it before reading whatever the wakers were announcing: a wake
    /// raised after this call is then still pending at the next
    /// [`wait`](Self::wait), and one raised before it is never lost.
    pub fn take_wake(&self) -> bool {
        self.bell.take()
    }
}

impl std::fmt::Debug for Waiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiter").field("capacity", &self.capacity).finish_non_exhaustive()
    }
}
