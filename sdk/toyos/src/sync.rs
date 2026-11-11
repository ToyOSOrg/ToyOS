//! Locks over the kernel's futex, which allocate nothing, so a `static` can
//! hold one.
//!
//! [`Lock`] is one `u32`, 0 free, 1 held, 2 held with waiters, as Drepper's
//! "Futexes Are Tricky" (2011) builds its mutex in §"Mutex, Take 2". A holder
//! that finds 2 at its release wakes one waiter, and a waiter sleeps only while
//! the word still says 2.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(not(test))]
use toyos_abi::syscall;

/// `T` behind a futex lock.
pub struct Lock<T> {
    state: AtomicU32,
    value: UnsafeCell<T>,
}

// SAFETY: the value is reached only through a `Held`, which the lock makes one
// thread's at a time.
unsafe impl<T: Send> Sync for Lock<T> {}

/// The lock held: the value, until this drops.
pub struct Held<'a, T>(&'a Lock<T>);

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self { state: AtomicU32::new(0), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> Held<'_, T> {
        lock_word(&self.state);
        Held(self)
    }
}

impl<T> core::ops::Deref for Held<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the lock is held.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> core::ops::DerefMut for Held<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for Held<'_, T> {
    fn drop(&mut self) {
        unlock_word(&self.0.state);
    }
}

/// Sleep while `word` still says `expected`; a spurious return is allowed.
#[cfg(not(test))]
pub(crate) fn wait(word: &AtomicU32, expected: u32) {
    // SAFETY: `word` is a live, aligned u32.
    unsafe { syscall::futex_wait(word.as_ptr(), expected, None) };
}

/// Wake every sleeper on `word`.
#[cfg(not(test))]
pub(crate) fn wake_all(word: &AtomicU32) {
    // SAFETY: `word` is a live, aligned u32.
    unsafe { syscall::futex_wake(word.as_ptr(), u32::MAX) };
}

/// Wake one sleeper on `word`.
#[cfg(not(test))]
pub(crate) fn wake_one(word: &AtomicU32) {
    // SAFETY: `word` is a live, aligned u32.
    unsafe { syscall::futex_wake(word.as_ptr(), 1) };
}

// A host test has no futex: a waiter yields and spins.
#[cfg(test)]
pub(crate) fn wait(_: &AtomicU32, _: u32) {
    std::thread::yield_now();
}

#[cfg(test)]
pub(crate) fn wake_all(_: &AtomicU32) {}

#[cfg(test)]
pub(crate) fn wake_one(_: &AtomicU32) {}

/// How many times a contender looks again before it sleeps. A futex wait and
/// the wake that ends it cost a kernel round trip each way, microseconds,
/// while a holder of these locks lets go within a few hundred nanoseconds:
/// spinning about as long as a block would cost before blocking is the
/// competitive choice (Karlin, Li, Manasse and Owicki, "Empirical Studies of
/// Competitive Spinning for a Shared-Memory Multiprocessor", SOSP 1991). A
/// `spin_loop` hint is tens to about 140 cycles depending on the CPU, so 100
/// of them stay within that cost.
const SPINS: u32 = 100;

/// Takes the lock `state` is the word of, sleeping while another holds it.
pub fn lock_word(state: &AtomicU32) {
    if state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        return;
    }
    // Held with no sleeper: its holder is running and lets go soon. A word
    // that says sleepers are waiting goes straight to sleep behind them.
    for _ in 0..SPINS {
        match state.load(Ordering::Relaxed) {
            0 if state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() => return,
            2 => break,
            _ => core::hint::spin_loop(),
        }
    }
    while state.swap(2, Ordering::Acquire) != 0 {
        wait(state, 2);
    }
}

/// Lets go of the lock `state` is the word of, waking one sleeper if any.
pub fn unlock_word(state: &AtomicU32) {
    if state.swap(0, Ordering::Release) == 2 {
        wake_one(state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::vec::Vec;

    use super::*;

    /// Threads contending for one lock, each holding it only briefly, never
    /// hold it together: every increment made under it lands.
    #[test]
    fn contenders_for_a_lock_each_hold_it_alone() {
        const THREADS: usize = 4;
        const EACH: usize = 20_000;
        let lock: Arc<Lock<usize>> = Arc::new(Lock::new(0));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        let mut held = lock.lock();
                        // A read and a separate write, which two holders at once would lose.
                        let seen = *held;
                        core::hint::spin_loop();
                        *held = seen + 1;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*lock.lock(), THREADS * EACH);
    }
}
