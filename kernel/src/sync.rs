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

/// A ticket-protocol counter. Both counters wrap at `u32::MAX` — the atomic
/// `fetch_add` RMWs never trap — and `Ticket` carries no `Add`, so the wrapping
/// [`Ticket::succ`] is the only route to the next one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ticket(u32);

impl Ticket {
    pub(crate) const ZERO: Self = Self(0);
    pub(crate) fn succ(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
    pub(crate) fn raw(self) -> u32 {
        self.0
    }
}

/// A shared ticket counter whose only successor is the wrapping CAS below, so no
/// caller hand-computes one.
pub(crate) struct AtomicTicket(AtomicU32);

impl AtomicTicket {
    #[cfg(not(feature = "loom"))]
    pub(crate) const fn new(t: Ticket) -> Self {
        Self(AtomicU32::new(t.0))
    }

    #[cfg(feature = "loom")]
    pub(crate) fn new(t: Ticket) -> Self {
        Self(AtomicU32::new(t.0))
    }

    pub(crate) fn load(&self, order: Ordering) -> Ticket {
        Ticket(self.0.load(order))
    }

    pub(crate) fn fetch_advance(&self, order: Ordering) -> Ticket {
        Ticket(self.0.fetch_add(1, order))
    }

    /// Advance `current` to its wrapping successor, or fail untouched. The
    /// successor is computed here, so `try_lock` cannot supply a checked add.
    pub(crate) fn compare_advance(
        &self,
        current: Ticket,
        success: Ordering,
        failure: Ordering,
    ) -> Result<Ticket, Ticket> {
        self.0
            .compare_exchange(current.0, current.succ().0, success, failure)
            .map(Ticket)
            .map_err(Ticket)
    }
}

/// Ticket spinlock. Provides mutual exclusion via `lock() -> LockGuard`.
pub struct Lock<T> {
    ticket: AtomicTicket,
    now: AtomicTicket,
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
            ticket: AtomicTicket::new(Ticket::ZERO),
            now: AtomicTicket::new(Ticket::ZERO),
            data: UnsafeCell::new(val),
        }
    }

    #[cfg(feature = "loom")]
    pub fn new(val: T) -> Self {
        Self {
            ticket: AtomicTicket::new(Ticket::ZERO),
            now: AtomicTicket::new(Ticket::ZERO),
            data: UnsafeCell::new(val),
        }
    }

    /// A lock whose counters start at `at`, for the model driving the `u32::MAX`
    /// wrap; only the loom build compiles it.
    #[cfg(feature = "loom")]
    pub fn seeded_at(val: T, at: u32) -> Self {
        Self {
            ticket: AtomicTicket::new(Ticket(at)),
            now: AtomicTicket::new(Ticket(at)),
            data: UnsafeCell::new(val),
        }
    }

    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T> {
        crate::preempt::disable();
        let my_ticket = self.ticket.fetch_advance(Ordering::Relaxed);
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
                    spins / 1_000_000, caller, my_ticket.raw(), self.now.load(Ordering::Relaxed).raw());
                next_warn = (next_warn * 2).min(500_000_000);
            }
            if spins >= 500_000_000 {
                let caller = core::panic::Location::caller();
                panic!("DEADLOCK at {}: 500M spins, ticket={} now={}",
                    caller, my_ticket.raw(), self.now.load(Ordering::Relaxed).raw());
            }
        }
        LockGuard { lock: self }
    }

    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        crate::preempt::disable();
        let current = self.now.load(ACQUIRED);
        match self.ticket.compare_advance(current, Ordering::Relaxed, Ordering::Relaxed) {
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
        self.lock.now.fetch_advance(Ordering::Release);
        crate::preempt::enable();
    }
}


