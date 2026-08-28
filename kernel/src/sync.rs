use core::ops::{Deref, DerefMut};

#[cfg(not(feature = "loom"))]
use core::cell::UnsafeCell;
#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "loom")]
use crate::cell::UnsafeCell;
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, Ordering};

// Unlock publishes through `now`, not `ticket`; the load that reads `now`
// is the one that must carry the acquire.
// `lock-acquire-off` drops this to `Relaxed` so `kernel-loom` can prove the
// acquire is load-bearing; no real kernel build turns it on.
#[cfg(not(feature = "lock-acquire-off"))]
const ACQUIRED: Ordering = Ordering::Acquire;
#[cfg(feature = "lock-acquire-off")]
const ACQUIRED: Ordering = Ordering::Relaxed;

/// Ticket spinlock. Provides mutual exclusion via `lock() -> LockGuard`.
pub struct Lock<T> {
    ticket: AtomicU32,
    now: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: the ticket protocol serializes access to `data`; `Send` is required
// because a lock can hand `T` to a different thread than the one that owned it.
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    /// Must stay `const`: every `Lock` in the kernel is a `static`, and loom's
    /// atomics have no const constructor — hence the non-const arm below.
    #[cfg(not(feature = "loom"))]
    pub const fn new(val: T) -> Self {
        Self {
            ticket: AtomicU32::new(0),
            now: AtomicU32::new(0),
            data: UnsafeCell::new(val),
        }
    }

    #[cfg(feature = "loom")]
    pub fn new(val: T) -> Self {
        Self {
            ticket: AtomicU32::new(0),
            now: AtomicU32::new(0),
            data: UnsafeCell::new(val),
        }
    }

    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T> {
        crate::preempt::disable();
        let my_ticket = self.ticket.fetch_add(1, Ordering::Relaxed);
        let mut spins = 0u64;
        let mut next_warn = 50_000_000u64;
        while self.now.load(ACQUIRED) != my_ticket {
            core::hint::spin_loop();
            // Polls TLB shootdowns: this spin runs with `IF` clear, so skipping
            // it here can deadlock a shootdown initiator that holds a lock.
            crate::arch::tlb::poll();
            spins += 1;
            if spins == next_warn {
                let caller = core::panic::Location::caller();
                crate::log!("LOCK CONTENTION: {}M spins at {}, ticket={} now={}",
                    spins / 1_000_000, caller, my_ticket, self.now.load(Ordering::Relaxed));
                next_warn = (next_warn * 2).min(500_000_000);
            }
            if spins >= 500_000_000 {
                let caller = core::panic::Location::caller();
                panic!("DEADLOCK at {}: 500M spins, ticket={} now={}",
                    caller, my_ticket, self.now.load(Ordering::Relaxed));
            }
        }
        LockGuard { lock: self }
    }

    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        crate::preempt::disable();
        let current = self.now.load(ACQUIRED);
        match self.ticket.compare_exchange(current, current + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => Some(LockGuard { lock: self }),
            Err(_) => {
                crate::preempt::enable();
                None
            }
        }
    }

    /// Raw pointer to `data`, without locking — only for statics needing a
    /// stable address for asm (GDT, TSS, IDT).
    pub fn data_ptr(&self) -> *mut T {
        self.data.get()
    }
}

pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: a live `LockGuard` is the ticket protocol's only proof that no
        // other CPU holds `data`.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: same ticket argument as `deref`; `&mut self` also excludes an
        // outstanding `&T` from this guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.now.fetch_add(1, Ordering::Release);
        crate::preempt::enable();
    }
}


