//! Locks over the kernel's futex, which allocate nothing, so a `static` can
//! hold one.
//!
//! [`Lock`] is one `u32`, 0 free, 1 held, 2 held with waiters, as Drepper's
//! "Futexes Are Tricky" (2011) builds its mutex in §"Mutex, Take 2". A holder
//! that finds 2 at its release wakes one waiter, and a waiter sleeps only while
//! the word still says 2. [`Turnstile`] serves its holders in the order they
//! asked.

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

/// How a [`Turnstile`]'s waiters sleep on and wake from its word.
pub trait Sleep {
    /// Sleep while `word` still says `expected`; a spurious return is allowed.
    fn wait(word: &AtomicU32, expected: u32);
    /// Wake every sleeper on `word`.
    fn wake_all(word: &AtomicU32);
}

/// The kernel's futex.
pub struct Futex;

impl Sleep for Futex {
    fn wait(word: &AtomicU32, expected: u32) {
        // SAFETY: `word` is a live, aligned u32.
        unsafe { syscall::futex_wait(word.as_ptr(), expected, None) };
    }
    fn wake_all(word: &AtomicU32) {
        // SAFETY: `word` is a live, aligned u32.
        unsafe { syscall::futex_wake(word.as_ptr(), u32::MAX) };
    }
}

/// `T` held in turn: each [`Turnstile::lock`] takes a ticket, and the value
/// goes to the tickets in the order they were taken. A plain lock lets the
/// thread that let go take it straight back ahead of the one it woke, so
/// threads sharing one connection would not be served in turn.
pub struct Turnstile<T, S: Sleep = Futex> {
    /// The next ticket handed out.
    next: AtomicU32,
    /// The ticket whose turn it is, which waiters sleep on.
    serving: AtomicU32,
    value: UnsafeCell<T>,
    sleep: core::marker::PhantomData<S>,
}

// SAFETY: the value is reached only through a `Turn`, which only the ticket
// being served holds.
unsafe impl<T: Send, S: Sleep> Sync for Turnstile<T, S> {}

/// The value, held for one turn.
pub struct Turn<'a, T, S: Sleep = Futex>(&'a Turnstile<T, S>);

impl<T, S: Sleep> Turnstile<T, S> {
    pub const fn new(value: T) -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
            value: UnsafeCell::new(value),
            sleep: core::marker::PhantomData,
        }
    }

    /// The value, once every ticket taken before this one has had its turn.
    pub fn lock(&self) -> Turn<'_, T, S> {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        loop {
            let serving = self.serving.load(Ordering::Acquire);
            if serving == ticket {
                return Turn(self);
            }
            S::wait(&self.serving, serving);
        }
    }
}

impl<T, S: Sleep> core::ops::Deref for Turn<'_, T, S> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this ticket is the one being served.
        unsafe { &*self.0.value.get() }
    }
}

impl<T, S: Sleep> core::ops::DerefMut for Turn<'_, T, S> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: this ticket is the one being served.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T, S: Sleep> Drop for Turn<'_, T, S> {
    fn drop(&mut self) {
        self.0.serving.fetch_add(1, Ordering::Release);
        // Every waiter, since only the next ticket's may go on and only it
        // knows it is the next.
        S::wake_all(&self.0.serving);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use std::vec::Vec;

    use super::*;

    /// A host's stand-in for the futex: a wait yields once.
    struct Yield;

    impl Sleep for Yield {
        fn wait(_: &AtomicU32, _: u32) {
            std::thread::yield_now();
        }
        fn wake_all(_: &AtomicU32) {}
    }

    /// A thread that lets the value go and asks for it again waits behind
    /// the threads that asked first, in the order they asked.
    #[test]
    fn the_value_goes_to_the_tickets_in_the_order_they_were_taken() {
        let stile: Arc<Turnstile<Vec<&str>, Yield>> = Arc::new(Turnstile::new(Vec::new()));
        let mut held = stile.lock();
        held.push("first");
        let queued: Vec<_> = ["second", "third"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let theirs = Arc::clone(&stile);
                let thread = std::thread::spawn(move || theirs.lock().push(name));
                // The next waits for this one's ticket, so the two are taken in order.
                let asked = Instant::now();
                while stile.next.load(Ordering::Relaxed) < i as u32 + 2 {
                    assert!(asked.elapsed() < Duration::from_secs(10), "{name} never took a ticket");
                    std::thread::yield_now();
                }
                thread
            })
            .collect();
        drop(held);
        stile.lock().push("first again");
        for thread in queued {
            thread.join().unwrap();
        }
        assert_eq!(*stile.lock(), ["first", "second", "third", "first again"]);
    }
}
