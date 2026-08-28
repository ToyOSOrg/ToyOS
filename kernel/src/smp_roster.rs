//! The CPU roster and the one release/answer word.
//!
//! Compiled a second time into `kernel-loom/` against loom's atomics, so this
//! file holds no `crate::` reference. Two facts other code depends on are held
//! here as a type rather than as discipline spread across a boot loop:
//!
//! - `0..count()` is the online set with no gap. [`commit`](Roster::commit) is
//!   the only writer of both the count and a slot, and it stores the slot before
//!   the count under a release the count carries, so a reader that sees a count
//!   sees every slot below it. An id is handed out by [`begin_attempt`] but
//!   committed only once the AP's startup handshake has landed, so a failed AP
//!   commits nothing and leaves no dead slot for a TLB shootdown to target.
//! - Releasing the APs and answering their shootdowns are one store. The word
//!   [`release`](Roster::release) sets is the word [`answering`](Roster::answering)
//!   reads, so no CPU can see the machine released without seeing it answering —
//!   the window a second store would open cannot be represented.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Matches `sched::MAX_CPUS`; the roster refuses an id at or above it, so a
/// firmware over-reporting CPUs is bounded here rather than by an out-of-range
/// store far downstream.
pub const MAX_CPUS: usize = 8;

/// An uncommitted slot: no physical LAPIC id is `u32::MAX`, so a reader that
/// finds it read a slot the count does not yet cover.
const NO_LAPIC: u32 = u32::MAX;

/// A CPU id and the token of the attempt bringing it up.
///
/// Returned by [`begin_attempt`](Roster::begin_attempt); consumed by
/// [`commit`](Roster::commit) or dropped on a failed handshake. `Copy` so the
/// boot loop can hold it across the wait and still hand it to `commit`.
#[derive(Clone, Copy)]
pub struct Attempt {
    id: u32,
    token: u32,
}

impl Attempt {
    /// The dense id this attempt would take if it commits.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The token the AP must echo, so a stale AP from an earlier attempt cannot
    /// answer for this one.
    pub fn token(&self) -> u32 {
        self.token
    }
}

pub struct Roster {
    /// Number of committed CPUs. The BSP is 1 from the start; every other write
    /// is a [`commit`](Roster::commit).
    count: AtomicU32,
    /// `apic_ids[i]` is a committed LAPIC id iff `i < count`; [`NO_LAPIC`] until
    /// then.
    apic_ids: [AtomicU32; MAX_CPUS],
    /// Released and answering: one fact, one store.
    ready: AtomicBool,
    /// The base's second store, reachable only under the negative control that
    /// reintroduces it; a kernel build never sees this field written or read.
    #[cfg(feature = "smp-ready-split")]
    answer: AtomicBool,
    /// Source of per-attempt tokens; `0` is reserved for "no attempt".
    next_token: AtomicU32,
}

impl Roster {
    /// Must stay `const`: the kernel's single instance is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            count: AtomicU32::new(1),
            apic_ids: [const { AtomicU32::new(NO_LAPIC) }; MAX_CPUS],
            ready: AtomicBool::new(false),
            #[cfg(feature = "smp-ready-split")]
            answer: AtomicBool::new(false),
            next_token: AtomicU32::new(1),
        }
    }

    // Not `const`: loom's atomics have no const constructor.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            count: AtomicU32::new(1),
            apic_ids: core::array::from_fn(|_| AtomicU32::new(NO_LAPIC)),
            ready: AtomicBool::new(false),
            #[cfg(feature = "smp-ready-split")]
            answer: AtomicBool::new(false),
            next_token: AtomicU32::new(1),
        }
    }

    /// Record the BSP's own LAPIC id in slot 0; the count already covers it.
    pub fn set_bsp(&self, lapic: u32) {
        self.apic_ids[0].store(lapic, Ordering::Relaxed);
    }

    /// The online set is `0..count()`.
    pub fn count(&self) -> u32 {
        // Acquire: pairs with `commit`'s release so a caller that sees the count
        // sees every slot the count covers.
        self.count.load(Ordering::Acquire)
    }

    /// The committed LAPIC id of `id`. Caller guarantees `id < count()`.
    pub fn apic_id(&self, id: u32) -> u32 {
        self.apic_ids[id as usize].load(Ordering::Relaxed)
    }

    /// Reserve the next dense id and a token for one AP bring-up. `None` once the
    /// roster is full, which is where a firmware over-reporting CPUs is refused.
    ///
    /// Nothing is committed here: the id is not counted and its slot stays
    /// [`NO_LAPIC`] until [`commit`](Roster::commit).
    pub fn begin_attempt(&self) -> Option<Attempt> {
        let id = self.count.load(Ordering::Relaxed);
        if id as usize >= MAX_CPUS {
            return None;
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        Some(Attempt { id, token })
    }

    /// Commit a started AP: fill its slot, then publish the count that covers it.
    /// Only the BSP calls this, one attempt at a time, so `at.id` is the current
    /// count and the range stays dense.
    pub fn commit(&self, at: Attempt, lapic: u32) {
        debug_assert!(at.id == self.count.load(Ordering::Relaxed));
        self.apic_ids[at.id as usize].store(lapic, Ordering::Relaxed);
        // The slot store above must land before the count that exposes it. The
        // release is what carries that ordering to a reader's acquire; the
        // negative control drops it to relaxed and the model finds the reader
        // seeing a count over an unfilled slot.
        #[cfg(not(feature = "roster-commit-relaxed"))]
        self.count.store(at.id + 1, Ordering::Release);
        #[cfg(feature = "roster-commit-relaxed")]
        self.count.store(at.id + 1, Ordering::Relaxed);
    }

    /// Release the APs and, by the same store, start answering their shootdowns.
    #[cfg(not(feature = "smp-ready-split"))]
    pub fn release(&self) {
        self.ready.store(true, Ordering::Release);
    }

    /// The base's two-store release, reachable only under the negative control.
    #[cfg(feature = "smp-ready-split")]
    pub fn release(&self) {
        self.ready.store(true, Ordering::Release);
        self.answer.store(true, Ordering::Release);
    }

    /// True once the APs are released; an AP spins on this before it joins.
    pub fn released(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// True once a shootdown must wait for siblings — the same word as
    /// [`released`](Roster::released) in a kernel build.
    #[cfg(not(feature = "smp-ready-split"))]
    pub fn answering(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Under the control, a second word that can lag [`released`](Roster::released).
    #[cfg(feature = "smp-ready-split")]
    pub fn answering(&self) -> bool {
        self.answer.load(Ordering::Acquire)
    }
}
