//! The lock a contender parks on.
//! A contender takes a ticket, arms on the lock's own watch, and gives the
//! CPU back; the release posts to the ticket whose turn it now is. Unlike a
//! [`crate::sync::Lock`] guard, a [`SleepGuard`] does not raise the preempt
//! count. Compiled a second time by `kernel-loom`, which drives the real
//! acquire; `#[allow(dead_code)]` until a kernel static converts to it.

// Unused until a kernel static converts to it — see kernel-loom/tests/sleep_lock.rs.
#![allow(dead_code)]

#[cfg(not(feature = "loom"))]
use core::cell::UnsafeCell;
#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use crate::cell::UnsafeCell;
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use core::ops::{Deref, DerefMut};

use crate::completion::{self, Outcome, Subject, Token, Watch};
use crate::scheduler::{current_task, Parkable, TaskId};

/// Acquire: orders the previous holder's writes before this read; `sleeplock-acquire-off` flips it to `Relaxed` so `kernel-loom` can prove the gap is real.
#[cfg(not(feature = "sleeplock-acquire-off"))]
const TURN: Ordering = Ordering::Acquire;
#[cfg(feature = "sleeplock-acquire-off")]
const TURN: Ordering = Ordering::Relaxed;

/// `u32::MAX` in the pid half, which `id_map` never issues.
const FREE: u64 = u64::MAX;

/// Held by a context with no task — boot, or a caller through [`SleepLock::try_lock`]; distinct from [`FREE`], though [`SleepLock::holder`] answers `None` to both.
const NOT_A_TASK: u64 = u64::MAX - 1;

fn word_of(task: Option<TaskId>) -> u64 {
    match task {
        Some(id) => id.pack(),
        None => NOT_A_TASK,
    }
}

/// A lock whose contended acquire parks.
pub struct SleepLock<T> {
    /// Next ticket to hand out; an uncontended acquire CASes it from [`Self::now`].
    ticket: AtomicU32,
    /// Whose turn it is; the release publishes through this and [`TURN`] loads it.
    now: AtomicU32,
    /// [`FREE`], [`NOT_A_TASK`], or the holder's packed [`TaskId`]; not part of the ticket pair's exclusion.
    holder: AtomicU64,
    /// Contenders arm here with their own ticket as token; `completion::arm` refuses a second arm per inbox, so [`Self::lock`] must not be called from inside an armed wait's predicate.
    watch: Watch,
    data: UnsafeCell<T>,
}

// SAFETY: exclusivity comes from the ticket pair, not from `Sync`; `T: Send` because any task may become the holder.
unsafe impl<T: Send> Sync for SleepLock<T> {}

impl<T> SleepLock<T> {
    /// Must stay `const`: every converted lock in the kernel is a `static`; loom's atomics have no const constructor, hence the second arm.
    #[cfg(not(feature = "loom"))]
    pub const fn new(value: T) -> Self {
        Self {
            ticket: AtomicU32::new(0),
            now: AtomicU32::new(0),
            holder: AtomicU64::new(FREE),
            watch: Watch::new(),
            data: UnsafeCell::new(value),
        }
    }

    #[cfg(feature = "loom")]
    pub fn new(value: T) -> Self {
        Self {
            ticket: AtomicU32::new(0),
            now: AtomicU32::new(0),
            holder: AtomicU64::new(FREE),
            watch: Watch::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// Must not be called while a [`crate::sync::Lock`] is held — use [`Self::try_lock`] there instead.
    #[track_caller]
    pub fn lock<'p>(&'p self, p: &'p Parkable) -> SleepGuard<'p, T> {
        let me = current_task();
        // RT6: a second acquire by the holder would take a ticket nothing will ever serve.
        assert!(
            me.is_none() || self.holder() != me,
            "sleeplock: {} already holds this lock",
            OwnerName(word_of(me)),
        );
        let owner = word_of(me);
        if let Some(guard) = self.take(owner) {
            return guard;
        }
        // Arms before re-reading `now`, so a release landing in between is not lost.
        let mine = self.ticket.fetch_add(1, Ordering::Relaxed);
        // Uncancellable: a killed holder still releases via `Drop` on unwind, so the wait is bounded.
        completion::wait_uncancellable_until(
            p,
            Subject::of(&self.watch),
            Token::new(u64::from(mine)),
            || self.now.load(TURN) == mine,
        );
        self.holder.store(owner, Ordering::Relaxed);
        SleepGuard { lock: self }
    }

    /// Takes the lock if free, from any context — an interrupt handler, the panic path, boot, or a caller already holding a [`crate::sync::Lock`].
    pub fn try_lock(&self) -> Option<SleepGuard<'_, T>> {
        self.take(word_of(current_task()))
    }

    /// Fails whenever anyone is queued, because `ticket` has already moved past `now`.
    fn take(&self, owner: u64) -> Option<SleepGuard<'_, T>> {
        let turn = self.now.load(TURN);
        self.ticket
            .compare_exchange(
                turn,
                turn.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .ok()?;
        self.holder.store(owner, Ordering::Relaxed);
        Some(SleepGuard { lock: self })
    }

    /// `None` means "not a task" (free, or acquired via [`Self::try_lock`]), never "held by task 0".
    pub fn holder(&self) -> Option<TaskId> {
        match self.holder.load(Ordering::Relaxed) {
            FREE | NOT_A_TASK => None,
            word => Some(TaskId::unpack(word)),
        }
    }
}

/// Prints as a task where it names one, or "a context with no task" rather than the raw packed word.
struct OwnerName(u64);

impl core::fmt::Display for OwnerName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            FREE | NOT_A_TASK => write!(f, "a context with no task"),
            word => write!(f, "{}", TaskId::unpack(word)),
        }
    }
}

pub struct SleepGuard<'a, T> {
    lock: &'a SleepLock<T>,
}

impl<T> Deref for SleepGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the ticket pair grants this caller the lock until the release below.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SleepGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as `deref`; `&mut self` gives the exclusive borrow.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SleepGuard<'_, T> {
    // `holder` is cleared before the release: after the release another CPU may already hold the lock and have overwritten it.
    fn drop(&mut self) {
        self.lock.holder.store(FREE, Ordering::Relaxed);
        let next = self
            .lock
            .now
            .fetch_add(1, Ordering::Release)
            .wrapping_add(1);
        let _ = completion::post_n(
            Subject::of(&self.lock.watch),
            Outcome::Ready,
            Token::new(u64::from(next)),
            1,
        );
    }
}
