//! One wait over any number of windows and a wake from another thread.
//!
//! A window's own [`Window::poll_event`](crate::Window::poll_event) waits on
//! that window alone, so a program with two windows, or with a worker thread
//! that has news for its event loop, has nothing to block on. A [`Waiter`] is
//! one [`Poller`] over every window's connection and one pipe a [`Waker`]
//! writes: the same readiness watch every other handle is waited on through,
//! and no second way of waiting.
//!
//! The wait says only *that* something is ready. The caller then drains each
//! window with `poll_event(0)` and calls [`Waiter::take_wake`], so a readiness
//! the wait did not name is still read, and a wake raised after the drain is
//! still pending at the next wait.

use std::sync::Arc;
use std::time::Duration;

use toyos::poller::{Poller, READABLE};
use toyos::{Pipe, RawHandle};
use toyos_abi::syscall::SyscallError;

/// The two ends of the wake pipe. Every [`Waker`] and the [`Waiter`] hold both,
/// so neither end closes while anything can still write or wait on it.
struct WakePipe {
    read: Pipe,
    write: Pipe,
}

/// Ends a [`Waiter::wait`] from any thread.
///
/// Wakes coalesce: one byte in the pipe is "woken", so any number of them
/// before the waiter takes them are one.
#[derive(Clone)]
pub struct Waker(Arc<WakePipe>);

impl Waker {
    /// Raise the wake. A full pipe is a wake already pending, which is the
    /// same answer.
    pub fn wake(&self) {
        match self.0.write.write_nonblock(&[1]) {
            Ok(_) | Err(SyscallError::WouldBlock) => {}
            // This process holds the read end, so nothing but a kernel that
            // broke the pipe's contract can say anything else.
            Err(e) => panic!("Waker::wake: the wake pipe refused a write: {e:?}"),
        }
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waker").finish_non_exhaustive()
    }
}

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
    wake: Waker,
}

impl Waiter {
    /// A waiter with room for a few windows; it grows when asked to wait on
    /// more.
    pub fn new() -> Self {
        // As `Poller::new` does with its inbox: a kernel out of pipes is a
        // machine this process cannot wait on at all.
        let (read, write) = toyos::pipe_pair().expect("Waiter::new: no pipe for the wake");
        let capacity = 4;
        Self {
            poller: Poller::new(capacity),
            capacity,
            wake: Waker(Arc::new(WakePipe { read, write })),
        }
    }

    /// A handle that ends this waiter's waits, for another thread.
    pub fn waker(&self) -> Waker {
        self.wake.clone()
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
        self.poller.watch(&self.wake.0.read, READABLE, 0);
        let timeout_nanos = match timeout {
            None => u64::MAX,
            // `u64::MAX` is the kernel's "forever", so a finite wait stops
            // one short of it.
            Some(timeout) => u64::try_from(timeout.as_nanos()).map_or(u64::MAX - 1, |n| n.min(u64::MAX - 1)),
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
        let mut buf = [0u8; 64];
        let mut woken = false;
        loop {
            match self.wake.0.read.read_nonblock(&mut buf) {
                Ok(0) => panic!("Waiter::take_wake: the wake pipe ended while its writer is held"),
                Ok(_) => woken = true,
                Err(SyscallError::WouldBlock) => return woken,
                Err(e) => panic!("Waiter::take_wake: the wake pipe refused a read: {e:?}"),
            }
        }
    }
}

impl std::fmt::Debug for Waiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiter").field("capacity", &self.capacity).finish_non_exhaustive()
    }
}
