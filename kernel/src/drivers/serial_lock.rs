//! The console backend's one lock: a word that only the [`Held`] a won
//! exchange returns ever clears, by its drop. A lost `try_lock` builds no
//! [`Held`], so it can never release the lock another CPU holds.
//! A fatal path's hold names its CPU ([`BackendLock::seize`]): a fatal path
//! that CPU enters on top of it finds the lock its own, where waiting would
//! wait for a frame that never runs again, and a burst it enters is refused.
//! Of the kernel it names only `crate::arch::cpu::hardware_id`, which
//! `kernel-loom` supplies to compile this file.

#[cfg(not(feature = "loom"))]
use core::{hint::spin_loop, sync::atomic::{AtomicU64, Ordering}};

#[cfg(feature = "loom")]
use loom::{hint::spin_loop, sync::atomic::{AtomicU64, Ordering}};

pub struct BackendLock {
    /// [`FREE`], [`LIVE`], or [`FATAL`] plus the CPU whose fatal path holds it.
    word: AtomicU64,
}

const FREE: u64 = 0;
/// A holder that lets go once its burst, look or read is done.
const LIVE: u64 = 1;
const FATAL: u64 = 2;

/// The word this CPU's fatal path holds the lock as.
fn fatal_here() -> u64 {
    FATAL + u64::from(crate::arch::cpu::hardware_id())
}

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

    /// The lock for a burst; refused on a CPU whose own fatal path holds it,
    /// since that holder is beneath this caller and cannot let go while it waits.
    pub fn lock(&self) -> Held<'_> {
        loop {
            if let Some(held) = self.try_lock() {
                return held;
            }
            let mut word = self.word.load(Ordering::Relaxed);
            while word != FREE {
                assert!(
                    word == LIVE || word != fatal_here(),
                    "serial: a burst asked for the console registers this cpu's own fatal path holds"
                );
                spin_loop();
                word = self.word.load(Ordering::Relaxed);
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

    /// The lock for the fatal path running on this CPU, asked for at most
    /// `tries` times: nothing on that path may wait for ever.
    pub fn seize(&self, tries: u64) -> Seized<'_> {
        let mine = fatal_here();
        within(tries, || match self.word.compare_exchange(FREE, mine, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => Some(Seized::Taken(Held { lock: self })),
            // Only this CPU stores `mine`, so seeing it needs no edge.
            Err(word) if word == mine => Some(Seized::Reentered),
            Err(_) => None,
        })
        .unwrap_or(Seized::Expired)
    }
}

/// `attempt` until it answers, or `tries` times: the one bounded wait for a
/// console lock.
pub fn within<T>(tries: u64, mut attempt: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..tries {
        if let Some(answer) = attempt() {
            return Some(answer);
        }
        spin_loop();
    }
    None
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.lock.word.store(FREE, Ordering::Release);
    }
}
