//! A lock over the kernel's futex: one `u32`, 0 free, 1 held, 2 held with
//! waiters, as Drepper's "Futexes Are Tricky" (2011) builds its mutex in
//! §"Mutex, Take 2". A holder that finds 2 at its release wakes one waiter,
//! and a waiter sleeps only while the word still says 2. It allocates nothing,
//! so a `static` can hold one.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

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

/// Takes the lock `state` is the word of, sleeping while another holds it.
pub fn lock_word(state: &AtomicU32) {
    if state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        return;
    }
    while state.swap(2, Ordering::Acquire) != 0 {
        // SAFETY: `state` is a live, aligned u32.
        unsafe { syscall::futex_wait(state.as_ptr(), 2, None) };
    }
}

/// Lets go of the lock `state` is the word of, waking one sleeper if any.
pub fn unlock_word(state: &AtomicU32) {
    if state.swap(0, Ordering::Release) == 2 {
        // SAFETY: `state` is a live, aligned u32.
        unsafe { syscall::futex_wake(state.as_ptr(), 1) };
    }
}
