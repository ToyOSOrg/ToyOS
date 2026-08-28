//! A task's own bounded record ring, distinct from the shared-memory
//! `Inbox` in [`object::inbox`](crate::object::inbox). Compiled a second
//! time by `kernel-loom`, so it names only atomics, `UnsafeCell`,
//! `toyos_abi`'s error type and `crate::time`. A poster writes the slot
//! then publishes `tail` with `Release`; a taker acquires `tail` before
//! reading the slot. `tail` and `head` are plain load/store, sound only
//! because exactly one poster — serialized by the subject's leaf lock —
//! and one taker, the owning task, ever touch them.

#[cfg(not(feature = "loom"))]
use core::cell::UnsafeCell;
#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use crate::cell::UnsafeCell;
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::time::Instant;

// `inbox-release-off` relaxes both so loom can prove the release/acquire pair matters; no kernel build sets it.
#[cfg(not(feature = "inbox-release-off"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "inbox-release-off")]
const PUBLISH: Ordering = Ordering::Relaxed;
#[cfg(not(feature = "inbox-release-off"))]
const OBSERVE: Ordering = Ordering::Acquire;
#[cfg(feature = "inbox-release-off")]
const OBSERVE: Ordering = Ordering::Relaxed;

/// Records an inbox holds before it starts dropping them.
#[cfg(not(feature = "loom"))]
pub const MAX_INBOX: usize = 8;
#[cfg(feature = "loom")]
pub const MAX_INBOX: usize = 2;

/// Opaque handle chosen by the waiter; names no object.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Token(u64);

impl Token {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The opaque value, for callers that must store a token in a word.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Why a subject is gone; never collapsed to a bare timeout.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The peer's last handle closed, or the subject told its waiters it will never answer.
    Closed,
    Expired,
    /// The inbox filled while this waiter was not running; the lost record is not replayed.
    Overflowed,
}

/// What happened; the caller must match, there is no absent case.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ready,
    Gone(Reason),
}

/// A record that something happened.
#[derive(Clone, Copy)]
pub struct Record {
    pub token: Token,
    pub outcome: Outcome,
    /// When the event happened, not when it was drained.
    pub at: Instant,
}

impl Record {
    // head == tail marks a slot empty; EMPTY is never read.
    const EMPTY: Self = Self {
        token: Token::new(0),
        outcome: Outcome::Ready,
        at: Instant::from_nanos_since_boot(0),
    };
}

/// A bounded ring of records, owned by whoever waits.
pub struct Inbox {
    slots: [UnsafeCell<Record>; MAX_INBOX],
    // Written by the poster holding the subject's leaf lock; read by the owner with acquire.
    tail: AtomicU32,
    // Written only by the owner; read by a poster to gauge free space.
    head: AtomicU32,
    // Set by a poster that finds the ring full; cleared once by the taker that reports it.
    overflowed: AtomicBool,
    // Set by any lock-free producer via `signal`; cleared by the owner.
    signalled: AtomicBool,
    // Written only by the owner, at arm and at disarm.
    armed: AtomicBool,
    // Written only by the owner, at arm.
    armed_token: AtomicU64,
}

// SAFETY: slots are written only under the poster's leaf lock and read only by the owner, ordered by the tail/head release-acquire pair.
unsafe impl Sync for Inbox {}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Inbox {
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            slots: [const { UnsafeCell::new(Record::EMPTY) }; MAX_INBOX],
            tail: AtomicU32::new(0),
            head: AtomicU32::new(0),
            overflowed: AtomicBool::new(false),
            signalled: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            armed_token: AtomicU64::new(0),
        }
    }

    // Loom's atomics have no const constructor, so this arm alone drops `const`.
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            slots: [(); MAX_INBOX].map(|()| UnsafeCell::new(Record::EMPTY)),
            tail: AtomicU32::new(0),
            head: AtomicU32::new(0),
            overflowed: AtomicBool::new(false),
            signalled: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            armed_token: AtomicU64::new(0),
        }
    }

    /// Store a record; must be called with the subject's leaf lock held.
    pub fn post(&self, record: Record) {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) as usize >= MAX_INBOX {
            self.overflowed.store(true, Ordering::Release);
            return;
        }
        let slot = tail as usize % MAX_INBOX;
        // SAFETY: this poster owns the slot until `tail` publishes it; the leaf lock admits only one poster at a time.
        unsafe { self.slots[slot].get().write(record) };
        self.tail.store(tail.wrapping_add(1), PUBLISH);
    }

    /// Say that something happened, without saying what; for producers that hold no lock.
    pub fn signal(&self) {
        self.signalled.store(true, Ordering::Release);
    }

    /// Whether the owner has anything to take; the park-time recheck predicate.
    pub fn has_record(&self) -> bool {
        self.tail.load(OBSERVE) != self.head.load(Ordering::Relaxed)
            || self.overflowed.load(Ordering::Acquire)
            || self.signalled.load(Ordering::Acquire)
    }

    /// Take the oldest record; owner only.
    pub fn take(&self) -> Option<Record> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(OBSERVE);
        if head == tail {
            // An empty-ring signal still reports the token the live arm named.
            if self.signalled.swap(false, Ordering::AcqRel) {
                return Some(Record {
                    token: Token::new(self.armed_token.load(Ordering::Relaxed)),
                    outcome: Outcome::Ready,
                    at: Instant::from_nanos_since_boot(0),
                });
            }
            // An empty-ring overflow is still reported once.
            return self.overflowed.swap(false, Ordering::AcqRel).then(|| Record {
                token: Token::new(0),
                outcome: Outcome::Gone(Reason::Overflowed),
                at: Instant::from_nanos_since_boot(0),
            });
        }
        let slot = head as usize % MAX_INBOX;
        // SAFETY: `tail` was published with release after the write; the acquire above pairs with it.
        let record = unsafe { self.slots[slot].get().read() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(record)
    }

    /// Whether an arm is live; only the owner writes it.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    /// Arm on `token`; does not drain the ring — `Armed`'s `Drop` does that, under the lock.
    // `pub`, not `pub(super)`: `kernel-loom` compiles this file with a different `super` and would flag it unused.
    pub fn arm_to(&self, token: Token) {
        self.armed_token.store(token.raw(), Ordering::Relaxed);
        self.armed.store(true, Ordering::Relaxed);
    }

    /// Disarm; the caller drains under the subject's lock, not this fn.
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Relaxed);
    }
}
