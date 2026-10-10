//! The CPU roster and the one release/answer word, two invariants held as a type:
//! an id commits only after its AP's handshake and `commit` publishes the slot
//! before the count, so `0..count()` has no dead slot; and the word `release` sets
//! is the word `answering` reads. Of the kernel it names only
//! `crate::arch::cpu::hardware_id`, which `kernel-loom` supplies to compile this file.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Matches `sched::MAX_CPUS`; the roster refuses an id at or above it.
pub const MAX_CPUS: usize = 16;

const NO_HARDWARE_ID: u32 = u32::MAX;

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
    /// `hardware_ids[i]` is committed iff `i < count`; [`NO_HARDWARE_ID`] until then.
    hardware_ids: [AtomicU32; MAX_CPUS],
    /// Released and answering: one fact, one store.
    ready: AtomicBool,
    #[cfg(feature = "smp-ready-split")]
    answer: AtomicBool,
    /// Source of per-attempt tokens; `0` is "no attempt".
    next_token: AtomicU32,
    /// The token the latest-started AP echoed, not a flag, so a stale AP cannot
    /// be read as this one; `0` means none has. Its low half is the hardware id
    /// that AP reads as its own, one store with the token.
    echoed: AtomicU64,
}

impl Roster {
    /// `const`: the kernel's single instance is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            count: AtomicU32::new(1),
            hardware_ids: [const { AtomicU32::new(NO_HARDWARE_ID) }; MAX_CPUS],
            ready: AtomicBool::new(false),
            #[cfg(feature = "smp-ready-split")]
            answer: AtomicBool::new(false),
            next_token: AtomicU32::new(1),
            echoed: AtomicU64::new(0),
        }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            count: AtomicU32::new(1),
            hardware_ids: core::array::from_fn(|_| AtomicU32::new(NO_HARDWARE_ID)),
            ready: AtomicBool::new(false),
            #[cfg(feature = "smp-ready-split")]
            answer: AtomicBool::new(false),
            next_token: AtomicU32::new(1),
            echoed: AtomicU64::new(0),
        }
    }

    /// The BSP's own `arch::cpu::hardware_id` in slot 0; the count already covers it.
    pub fn set_bsp(&self, hardware_id: u32) {
        self.hardware_ids[0].store(hardware_id, Ordering::Relaxed);
    }

    pub fn count(&self) -> u32 {
        // Acquire: a caller that sees the count sees every slot it covers.
        self.count.load(Ordering::Acquire)
    }

    /// Caller guarantees `id < count()`.
    pub fn hardware_id(&self, id: u32) -> u32 {
        self.hardware_ids[id as usize].load(Ordering::Relaxed)
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

    /// The AP's half of the handshake, run on that AP once it can take its
    /// first interrupt: the token of the attempt that started it, and the
    /// hardware id this CPU reads as its own.
    pub fn echo(&self, token: u32) {
        let read = crate::arch::cpu::hardware_id();
        self.echoed.store((u64::from(token) << 32) | u64::from(read), Ordering::Release);
    }

    /// Whether `at`'s AP has echoed; an acquire, so what the AP did before its
    /// echo is visible after.
    pub fn echoed(&self, at: Attempt) -> bool {
        self.echoed.load(Ordering::Acquire) >> 32 == u64::from(at.token)
    }

    /// Whether `at`'s AP echoed before `spent` said the BSP's budget for it is gone.
    pub fn await_echo(&self, at: Attempt, spent: impl Fn() -> bool) -> bool {
        loop {
            if self.echoed(at) {
                return true;
            }
            if spent() {
                return false;
            }
            core::hint::spin_loop();
        }
    }

    /// Fill a started AP's slot, then publish the count that covers it. Only the
    /// BSP calls this, one at a time and once `at` has echoed, so `at.id` is the
    /// current count and the echo is `at`'s.
    ///
    /// Refuses an AP that reads its own hardware id as other than
    /// `hardware_id`, the id its slot and every IPI name it by: its fatal paths
    /// and the console lock name it by its read. The BSP refuses, because before
    /// the release an AP's own panic stops no other CPU.
    pub fn commit(&self, at: Attempt, hardware_id: u32) {
        debug_assert!(at.id == self.count.load(Ordering::Relaxed));
        // Relaxed: the read is the echo's own word, which `await_echo` acquired.
        let read = self.echoed.load(Ordering::Relaxed) as u32;
        assert_eq!(
            read,
            hardware_id,
            "smp: cpu{} reads its own hardware id as {read:#x}, and its roster slot and every IPI name it {hardware_id:#x}",
            at.id
        );
        self.hardware_ids[at.id as usize].store(hardware_id, Ordering::Relaxed);
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
