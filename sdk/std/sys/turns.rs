//! A value held in turn: each `lock` takes a ticket, and the value goes to the
//! tickets in the order they were taken. std's mutex alone lets the thread
//! that let go take it straight back ahead of the one it woke, so threads
//! sharing one directory's connection would not be served in turn.
//!
//! It names nothing but `crate::sync`, so a host compiles it as it is
//! (`sdk/std/copies`).

use crate::sync::{Condvar, Mutex, MutexGuard};

pub struct Turnstile<T> {
    value: Mutex<T>,
    turns: Mutex<Turns>,
    served: Condvar,
}

struct Turns {
    next: u64,
    serving: u64,
}

/// The value, held for one turn.
pub struct Turn<'a, T> {
    // Dropped before `_ticket`, so the next ticket finds the value free.
    value: MutexGuard<'a, T>,
    _ticket: Ticket<'a, T>,
}

struct Ticket<'a, T>(&'a Turnstile<T>);

impl<T> Turnstile<T> {
    pub const fn new(value: T) -> Self {
        Self { value: Mutex::new(value), turns: Mutex::new(Turns { next: 0, serving: 0 }), served: Condvar::new() }
    }

    /// The value, once every ticket taken before this one has had its turn.
    pub fn lock(&self) -> Turn<'_, T> {
        let mut turns = self.turns.lock().unwrap();
        let ticket = turns.next;
        turns.next += 1;
        drop(self.served.wait_while(turns, |t| t.serving != ticket).unwrap());
        let ticket = Ticket(self);
        Turn { value: self.value.lock().unwrap(), _ticket: ticket }
    }
}

impl<T> Drop for Ticket<'_, T> {
    fn drop(&mut self) {
        self.0.turns.lock().unwrap().serving += 1;
        self.0.served.notify_all();
    }
}

impl<T> crate::ops::Deref for Turn<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> crate::ops::DerefMut for Turn<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;

    /// A thread that lets the value go and asks for it again waits behind
    /// the threads that asked first, in the order they asked.
    #[test]
    fn the_value_goes_to_the_tickets_in_the_order_they_were_taken() {
        let stile = Arc::new(Turnstile::new(Vec::new()));
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
                while stile.turns.lock().unwrap().next < i as u64 + 2 {
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
