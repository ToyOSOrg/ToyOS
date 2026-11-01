//! Waiting on one thread's executor. Nothing here blocks: a waiter is
//! pending until whoever changed what it waits on wakes it, and looks again.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};

/// Tasks waiting their turn, first in first out. A waiter proceeds only at
/// the head of the queue and only when its condition holds, so a large
/// request is never passed by smaller ones.
#[derive(Default)]
pub(crate) struct Queue {
    waiting: RefCell<VecDeque<(u64, Option<Waker>)>>,
    next: Cell<u64>,
}

/// A place in a [`Queue`], kept until dropped.
pub(crate) struct Ticket<'a> {
    queue: &'a Queue,
    ticket: u64,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut waiting = self.queue.waiting.borrow_mut();
        let head = waiting.front().map(|w| w.0) == Some(self.ticket);
        waiting.retain(|w| w.0 != self.ticket);
        drop(waiting);
        if head {
            self.queue.wake();
        }
    }
}

impl Ticket<'_> {
    /// Waits until this place is first in its queue and `ready` answers
    /// `Some`, and returns that, keeping the place. `ready` is asked each
    /// time the head is woken.
    pub(crate) async fn until<T>(&self, mut ready: impl FnMut() -> Option<T>) -> T {
        let queue = self.queue;
        poll_fn(|cx| {
            let head = queue.waiting.borrow().front().map(|w| w.0) == Some(self.ticket);
            if head {
                if let Some(out) = ready() {
                    return Poll::Ready(out);
                }
            }
            if let Some(w) = queue.waiting.borrow_mut().iter_mut().find(|w| w.0 == self.ticket) {
                w.1 = Some(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}

impl Queue {
    /// A place at the end of the queue.
    pub(crate) fn enter(&self) -> Ticket<'_> {
        let ticket = self.next.get();
        self.next.set(ticket + 1);
        self.waiting.borrow_mut().push_back((ticket, None));
        Ticket { queue: self, ticket }
    }

    /// Waits until this task is first in the queue and `ready` answers
    /// `Some`, and returns that, leaving the queue.
    pub(crate) async fn wait<T>(&self, ready: impl FnMut() -> Option<T>) -> T {
        self.enter().until(ready).await
    }

    /// Wakes the head of the queue to look again.
    pub(crate) fn wake(&self) {
        if let Some(w) = self.waiting.borrow().front().and_then(|w| w.1.as_ref()) {
            w.wake_by_ref();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.waiting.borrow().is_empty()
    }
}

/// Every waiter woken at each change, each looking at its own condition.
#[derive(Default)]
pub(crate) struct Broadcast {
    waiting: RefCell<Vec<Waker>>,
}

impl Broadcast {
    pub(crate) async fn until(&self, mut ready: impl FnMut() -> bool) {
        poll_fn(|cx| {
            if ready() {
                return Poll::Ready(());
            }
            self.waiting.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    pub(crate) fn wake(&self) {
        let waiting = core::mem::take(&mut *self.waiting.borrow_mut());
        for w in waiting {
            w.wake();
        }
    }
}
