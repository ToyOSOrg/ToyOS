//! An event's state: one coalescing pending bit, and whether each side still
//! has a holder.
//!
//! A signal sets the bit and a take clears it; any number of signals before a
//! take are one. A wait side whose every signaler has gone is ready for good,
//! but a take there answers [`Taken::Gone`], never a signal nobody raised.

#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 1 << 0;
const SIGNALERS_GONE: u8 = 1 << 1;
const WAITERS_GONE: u8 = 1 << 2;

/// What a signal found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signaled {
    /// The event was idle and is pending now: its waiters are owed a post.
    Raised,
    /// It was already pending, so this signal coalesced into that one.
    Coalesced,
    /// No wait side is left to take it.
    NoWaiter,
}

/// What a take found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Taken {
    /// A signal was pending, and is taken.
    Signal,
    /// Nothing pending, and a signaler may still raise one.
    Empty,
    /// Nothing pending, and no signaler is left to raise one.
    Gone,
}

pub struct State(AtomicU8);

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    /// Raise the event. Release, so what the signaler wrote before it is seen
    /// by the take that answers [`Taken::Signal`].
    pub fn signal(&self) -> Signaled {
        let was = self.0.fetch_or(PENDING, Ordering::AcqRel);
        if was & WAITERS_GONE != 0 {
            Signaled::NoWaiter
        } else if was & PENDING != 0 {
            Signaled::Coalesced
        } else {
            Signaled::Raised
        }
    }

    /// Take the pending signal, if any.
    pub fn take(&self) -> Taken {
        let was = self.0.fetch_and(!PENDING, Ordering::AcqRel);
        if was & PENDING != 0 {
            Taken::Signal
        } else if was & SIGNALERS_GONE != 0 {
            Taken::Gone
        } else {
            Taken::Empty
        }
    }

    /// Whether a wait on the event answers now: a signal is pending, or no
    /// signaler is left. Observes; takes nothing.
    pub fn ready(&self) -> bool {
        self.0.load(Ordering::Acquire) & (PENDING | SIGNALERS_GONE) != 0
    }

    /// The signal side's last handle has gone.
    pub fn end_signalers(&self) {
        self.0.fetch_or(SIGNALERS_GONE, Ordering::AcqRel);
    }

    /// The wait side's last handle has gone.
    pub fn end_waiters(&self) {
        self.0.fetch_or(WAITERS_GONE, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_before_a_take_are_one() {
        let event = State::new();
        assert_eq!(event.signal(), Signaled::Raised);
        assert_eq!(event.signal(), Signaled::Coalesced);
        assert_eq!(event.take(), Taken::Signal);
        assert_eq!(event.take(), Taken::Empty);
        assert_eq!(event.signal(), Signaled::Raised, "a taken event is idle again");
    }

    #[test]
    fn readiness_observes_and_does_not_take() {
        let event = State::new();
        assert!(!event.ready());
        event.signal();
        assert!(event.ready());
        assert!(event.ready());
        assert_eq!(event.take(), Taken::Signal);
        assert!(!event.ready());
    }

    /// A pending signal is answered before the signalers' end, and the end
    /// after it is never read as a signal.
    #[test]
    fn signalers_gone_is_ready_but_never_a_signal() {
        let event = State::new();
        event.signal();
        event.end_signalers();
        assert!(event.ready());
        assert_eq!(event.take(), Taken::Signal);
        assert!(event.ready(), "an event with no signaler left stays ready");
        assert_eq!(event.take(), Taken::Gone);
        assert_eq!(event.take(), Taken::Gone);
    }

    #[test]
    fn an_end_with_nothing_pending_is_gone_at_once() {
        let event = State::new();
        event.end_signalers();
        assert!(event.ready());
        assert_eq!(event.take(), Taken::Gone);
    }

    #[test]
    fn a_signal_with_no_waiter_left_says_so() {
        let event = State::new();
        event.end_waiters();
        assert_eq!(event.signal(), Signaled::NoWaiter);
        assert_eq!(event.signal(), Signaled::NoWaiter);
    }
}
