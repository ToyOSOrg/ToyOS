//! The CPU roster and the one release/answer word, two invariants held as a type:
//! an id commits only after its AP's handshake and `commit` publishes the slot
//! before the count, so `0..count()` has no dead slot; and the word `release` sets
//! is the word `answering` reads. Compiled into `kernel-loom/`, so no `crate::`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Matches `sched::MAX_CPUS`; the roster refuses an id at or above it.
pub const MAX_CPUS: usize = 8;

const NO_LAPIC: u32 = u32::MAX;

/// A CPU id and the token of the attempt bringing it up.
#[derive(Clone, Copy)]
pub struct Attempt {
    id: u32,
    token: u32,
}

impl Attempt {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn token(&self) -> u32 {
        self.token
    }
}

pub struct Roster {
    /// Committed CPUs; the BSP is 1 from the start, every other write a `commit`.
    count: AtomicU32,
    /// `apic_ids[i]` is committed iff `i < count`; [`NO_LAPIC`] until then.
    apic_ids: [AtomicU32; MAX_CPUS],
    /// Released and answering: one fact, one store.
    ready: AtomicBool,
    #[cfg(feature = "smp-ready-split")]
    answer: AtomicBool,
    /// Source of per-attempt tokens; `0` is "no attempt".
    next_token: AtomicU32,
}

impl Roster {
    /// `const`: the kernel's single instance is a `static`.
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

    /// The BSP's own LAPIC id in slot 0; the count already covers it.
    pub fn set_bsp(&self, lapic: u32) {
        self.apic_ids[0].store(lapic, Ordering::Relaxed);
    }

    pub fn count(&self) -> u32 {
        // Acquire: a caller that sees the count sees every slot it covers.
        self.count.load(Ordering::Acquire)
    }

    /// Caller guarantees `id < count()`.
    pub fn apic_id(&self, id: u32) -> u32 {
        self.apic_ids[id as usize].load(Ordering::Relaxed)
    }

    /// Reserve the next dense id and a token, committing nothing; `None` at MAX_CPUS.
    pub fn begin_attempt(&self) -> Option<Attempt> {
        let id = self.count.load(Ordering::Relaxed);
        if id as usize >= MAX_CPUS {
            return None;
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        Some(Attempt { id, token })
    }

    /// Fill a started AP's slot, then publish the count that covers it. Only the
    /// BSP calls this, one at a time, so `at.id` is the current count.
    pub fn commit(&self, at: Attempt, lapic: u32) {
        debug_assert!(at.id == self.count.load(Ordering::Relaxed));
        self.apic_ids[at.id as usize].store(lapic, Ordering::Relaxed);
        // Release: the slot store above lands before the count exposes it; the
        // control drops it to relaxed and the model finds the unfilled slot.
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

    #[cfg(not(feature = "smp-ready-split"))]
    pub fn answering(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    #[cfg(feature = "smp-ready-split")]
    pub fn answering(&self) -> bool {
        self.answer.load(Ordering::Acquire)
    }
}
