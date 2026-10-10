//! One claimed function's interrupt record: what its ISR writes, and what its
//! holder reads back.
//!
//! No `crate::` references: `kernel-loom` compiles this file directly under
//! `feature = "loom"`, because what these words prevent is an interleaving and
//! not an ordering, and no guest test in this suite lands on it.
//!
//! **Two parties race**: the ISR, on whichever CPU the unit routed the message
//! to, and the holder reading its record through a syscall on any CPU. One
//! handler at a time: a slot has one vector, delivered to one CPU.
//!
//! **The invariant is that every message is counted exactly once.** The count
//! is a read-modify-write on both sides and never a load followed by a store:
//! a reader that loaded a count and then cleared it drops every message the ISR
//! recorded in between, and a driver that misses one waits for a device that
//! has already spoken.
//!
//! **And a read's times are its own messages'.** The count shares its word
//! with the number of reads that have taken one, so which read a message falls
//! to and whether it is that read's first are one compare-exchange. The first
//! message of read `n` stamps the slot of `n`'s parity, before the exchange
//! that counts it publishes the stamp (`Release`, taken by the read's
//! `Acquire`), and read `n + 1`'s first message stamps the other slot: one
//! landing while a reader is between its exchange and its load never
//! overwrites the time that reader is about to load. A second reader racing
//! the first can take read `n + 1` and let read `n + 2`'s first message into
//! the slot the first is loading; that costs the racing holder its own time,
//! never a count.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use toyos_abi::pci::DeviceIrqRecord;

/// The words that say only what they hold: the latch, the fault and the
/// newest stamp, whose reader takes it after the exchange that orders it.
const ORDER: Ordering = Ordering::Relaxed;

/// The count is the word's low half and the reads that took one its high half.
const COUNT: u64 = 0xFFFF_FFFF;
const READ: u64 = 1 << 32;

/// The negative control: each compare-exchange on the word becomes a load and
/// a store, which is the whole of what this record's design is. Never on in a
/// kernel build.
#[cfg(feature = "device-irq-lossy")]
macro_rules! exchange {
    ($word:expr, $held:expr, $new:expr, $success:expr) => {{
        let seen = $word.load(ORDER);
        if seen == $held {
            $word.store($new, ORDER);
            Ok(seen)
        } else {
            Err(seen)
        }
    }};
}
#[cfg(not(feature = "device-irq-lossy"))]
macro_rules! exchange {
    ($word:expr, $held:expr, $new:expr, $success:expr) => {
        $word.compare_exchange_weak($held, $new, $success, ORDER)
    };
}

#[cfg(feature = "device-irq-lossy")]
macro_rules! take_word {
    ($word:expr, $empty:expr) => {{
        let held = $word.load(ORDER);
        $word.store($empty, ORDER);
        held
    }};
}
#[cfg(not(feature = "device-irq-lossy"))]
macro_rules! take_word {
    ($word:expr, $empty:expr) => {
        $word.swap($empty, ORDER)
    };
}

/// What the ISR writes and the claim reads back.
///
/// Atomics only: the handler allocates nothing.
pub struct Interrupt {
    /// Messages since the holder's last read, under the reads that took one.
    word: AtomicU64,
    /// When each read's first message landed, by the read's parity.
    first: [AtomicU64; 2],
    /// When the newest message landed.
    last: AtomicU64,
    /// The unit refused this function an access. Every call the claim answers
    /// refuses from here on: its bus mastering is gone, so a driver that kept
    /// going would be driving nothing.
    faulted: AtomicBool,
    /// True until this slot's first message has been announced.
    ///
    /// **A count of zero at the end of a boot is two facts**: a device that was
    /// never made to speak, and a message that never reached a CPU. Only the
    /// arrival of a first one tells them apart, and a census that can count
    /// cannot say when.
    unannounced: AtomicBool,
}

impl Interrupt {
    /// Must stay `const`: the slots are a `static` array.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
            first: [AtomicU64::new(0), AtomicU64::new(0)],
            last: AtomicU64::new(0),
            faulted: AtomicBool::new(false),
            unannounced: AtomicBool::new(true),
        }
    }

    // No `Default`: `Default::default` cannot be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
            first: [AtomicU64::new(0), AtomicU64::new(0)],
            last: AtomicU64::new(0),
            faulted: AtomicBool::new(false),
            unannounced: AtomicBool::new(true),
        }
    }

    /// Record one message, which landed at `now`. Called from the vector's
    /// ISR, so it takes no lock and allocates nothing.
    ///
    /// A compare-exchange and not a store: the reader may take the count
    /// between any two of these, and what it took plus what is left has to be
    /// what arrived.
    pub fn took(&self, now: u64) {
        self.last.store(now, ORDER);
        let mut word = self.word.load(ORDER);
        loop {
            if word & COUNT == 0 {
                self.first[(word / READ) as usize & 1].store(now, ORDER);
            }
            match exchange!(self.word, word, word.wrapping_add(1), Ordering::Release) {
                Ok(_) => return,
                Err(seen) => word = seen,
            }
        }
    }

    /// The messages since the last read and when they landed, or `None` for
    /// none.
    ///
    /// A compare-exchange and not a load followed by a store: a message the
    /// ISR records between the two would be cleared without ever having been
    /// counted.
    pub fn take(&self) -> Option<DeviceIrqRecord> {
        let mut word = self.word.load(ORDER);
        loop {
            if word & COUNT == 0 {
                return None;
            }
            match exchange!(self.word, word, (word / READ).wrapping_add(1) * READ, Ordering::Acquire) {
                Ok(_) => break,
                Err(seen) => word = seen,
            }
        }
        Some(DeviceIrqRecord {
            count: (word & COUNT) as u32,
            _pad: 0,
            first_nanos: self.first[(word / READ) as usize & 1].load(ORDER),
            last_nanos: self.last.load(ORDER),
        })
    }

    /// Whether a message is waiting, for a readiness check that consumes
    /// nothing.
    pub fn armed(&self) -> bool {
        self.word.load(ORDER) & COUNT != 0
    }

    /// Whether this is the first message this slot has taken. Answers `true`
    /// once per claim and `false` ever after, so a caller may log on it.
    ///
    /// `swap` for [`Self::take`]'s reason: two reads that both loaded `true`
    /// would both announce one message.
    pub fn take_unannounced(&self) -> bool {
        take_word!(self.unannounced, false)
    }

    /// The unit refused this function an access. Called from the fault handler:
    /// every call the claim answers refuses from here on.
    pub fn fault(&self) {
        self.faulted.store(true, ORDER);
    }

    pub fn faulted(&self) -> bool {
        self.faulted.load(ORDER)
    }

    /// Back to the state a fresh slot is in, for a claim being minted or given
    /// up. The holder is not running at either point.
    pub fn clear(&self) {
        self.word.store(0, ORDER);
        self.faulted.store(false, ORDER);
        self.unannounced.store(true, ORDER);
    }
}
