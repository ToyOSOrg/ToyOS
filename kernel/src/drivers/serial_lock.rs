//! The console backend's one lock: a flag that only the [`Held`] a won
//! exchange returns ever clears, by its drop. A lost `try_lock` builds no
//! [`Held`], so it can never release the lock another CPU holds.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, Ordering};

pub struct BackendLock {
    locked: AtomicBool,
}

/// The lock, held for as long as this lives.
pub struct Held<'a> {
    lock: &'a BackendLock,
}

impl BackendLock {
    /// Must stay `const`: the backend's lock is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { locked: AtomicBool::new(false) }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { locked: AtomicBool::new(false) }
    }

    pub fn lock(&self) -> Held<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        Held { lock: self }
    }

    /// `None` if another holder has it.
    pub fn try_lock(&self) -> Option<Held<'_>> {
        // Not `then_some`: its argument is built whether or not the exchange
        // won, and a `Held` built on a loss drops, and its drop releases the
        // lock another CPU holds.
        let won = self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok();
        #[cfg(feature = "serial-try-lock-then-some")]
        return won.then_some(Held { lock: self });
        #[cfg(not(feature = "serial-try-lock-then-some"))]
        if won {
            Some(Held { lock: self })
        } else {
            None
        }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
