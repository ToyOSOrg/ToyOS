//! The one decision a poll makes that two CPUs race for: which of a post, the
//! registrant's own recheck and a withdrawal answers it. Exactly one wins, so
//! a poll completes at most once and a withdrawn poll never does.
//!
//! Compiled a second time by `kernel-loom`, so it names only atomics.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU8, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU8, Ordering};

const ARMED: u8 = 0;
const FIRED: u8 = 1;
const WITHDRAWN: u8 = 2;

/// A poll's answer, taken once.
pub struct Once(AtomicU8);

impl Once {
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self(AtomicU8::new(ARMED))
    }

    // Loom's atomics have no const constructor, so this arm alone drops `const`.
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self(AtomicU8::new(ARMED))
    }

    /// `true` for the one caller that answers the poll; everybody after it,
    /// and everybody after a withdrawal, is told `false`.
    pub fn fire(&self) -> bool {
        self.take(FIRED)
    }

    /// Take the poll back unanswered; `true` if it had not been answered.
    pub fn withdraw(&self) -> bool {
        self.take(WITHDRAWN)
    }

    /// Whether nothing has taken the poll yet. `false` is permanent.
    pub fn armed(&self) -> bool {
        self.0.load(Ordering::Acquire) == ARMED
    }

    // `poll-fire-load-store` is the negative control: the exchange split into
    // a load and a store lets two callers both answer, and `poll_once` reds.
    #[cfg(not(feature = "poll-fire-load-store"))]
    fn take(&self, to: u8) -> bool {
        self.0
            .compare_exchange(ARMED, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[cfg(feature = "poll-fire-load-store")]
    fn take(&self, to: u8) -> bool {
        if self.0.load(Ordering::Acquire) != ARMED {
            return false;
        }
        self.0.store(to, Ordering::Release);
        true
    }
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}
