//! The console backend's one lock: a word that only the [`Held`] a won
//! exchange returns ever clears, by its drop. A lost `try_lock` builds no
//! [`Held`], so it can never release the lock another CPU holds.
//! A fatal path's hold names its CPU ([`BackendLock::seize`]): a fatal path
//! that CPU enters on top of it finds the lock its own, where waiting would
//! wait for a frame that never runs again.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

pub struct BackendLock {
    /// [`FREE`], [`LIVE`], or [`FATAL`] plus the CPU whose fatal path holds it.
    word: AtomicU64,
}

const FREE: u64 = 0;
/// A holder that lets go once its burst, look or read is done.
const LIVE: u64 = 1;
const FATAL: u64 = 2;

/// The lock, held for as long as this lives.
pub struct Held<'a> {
    lock: &'a BackendLock,
}

/// What a fatal path's [`BackendLock::seize`] came back with.
pub enum Seized<'a> {
    /// Free, or let go of inside the bound.
    Taken(Held<'a>),
    /// This CPU's own fatal path holds it, underneath the one asking.
    Reentered,
    /// Another holder kept it through every try.
    Expired,
}

impl BackendLock {
    /// Must stay `const`: the backend's lock is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { word: AtomicU64::new(FREE) }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { word: AtomicU64::new(FREE) }
    }

    pub fn lock(&self) -> Held<'_> {
        loop {
            if let Some(held) = self.try_lock() {
                return held;
            }
            while self.word.load(Ordering::Relaxed) != FREE {
                core::hint::spin_loop();
            }
        }
    }

    /// `None` if another holder has it.
    pub fn try_lock(&self) -> Option<Held<'_>> {
        // Not `then_some`: its argument is built whether or not the exchange
        // won, and a `Held` built on a loss drops, and its drop releases the
        // lock another CPU holds.
        let won = self.word.compare_exchange(FREE, LIVE, Ordering::Acquire, Ordering::Relaxed).is_ok();
        #[cfg(feature = "serial-try-lock-then-some")]
        return won.then_some(Held { lock: self });
        #[cfg(not(feature = "serial-try-lock-then-some"))]
        if won {
            Some(Held { lock: self })
        } else {
            None
        }
    }

    /// The lock for the fatal path running on `cpu`, asked for at most `tries`
    /// times: nothing on that path may wait for ever.
    pub fn seize(&self, cpu: u32, tries: u64) -> Seized<'_> {
        let mine = FATAL + u64::from(cpu);
        within(tries, || match self.word.compare_exchange(FREE, mine, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => Some(Seized::Taken(Held { lock: self })),
            // Only this CPU stores `mine`, so seeing it needs no edge.
            Err(word) if word == mine => Some(Seized::Reentered),
            Err(_) => None,
        })
        .unwrap_or(Seized::Expired)
    }
}

/// `attempt` until it answers, or `tries` times: the console's one bounded wait.
pub fn within<T>(tries: u64, mut attempt: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..tries {
        if let Some(answer) = attempt() {
            return Some(answer);
        }
        core::hint::spin_loop();
    }
    None
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.lock.word.store(FREE, Ordering::Release);
    }
}
