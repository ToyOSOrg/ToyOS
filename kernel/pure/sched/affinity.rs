//! A thread's affinity: the CPUs it may run on.
//!
//! A task's owner CPU decides with its own copy, [`crate::sched::task`]'s
//! owned mask, and adopts the published [`SharedMask`] when its version moves:
//! when it takes the task off its run queue, places it, or finds it running.
//! The owned copy changes only while the task is in no queue, so what a queue
//! counted at insert is still true at removal.

use crate::sched::hw::CpuId;
use crate::sched::mailbox::PreemptGuard;
use crate::sched::sync::{fence, AtomicU64, Ordering};

pub use toyos_abi::syscall::CPU_MASK_WORDS as MASK_WORDS;

/// The CPUs a mask can name.
pub const MAX_CPUS: usize = MASK_WORDS * 64;

/// A set of CPUs by dense kernel index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CpuMask([u64; MASK_WORDS]);

/// Why a requested mask is not a mask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaskRefused {
    /// More words than [`MASK_WORDS`].
    TooLong,
    /// No CPU at all.
    Empty,
    /// A CPU at or past the online count.
    Offline,
}

impl CpuMask {
    /// Every CPU, whichever come online: a spawn before the last CPU commits
    /// still runs on it.
    pub const ANY: Self = Self([u64::MAX; MASK_WORDS]);

    /// No CPU at all: not a mask a task holds, but the start of a union.
    pub const NONE: Self = Self([0; MASK_WORDS]);

    pub fn one(cpu: CpuId) -> Self {
        let mut words = [0; MASK_WORDS];
        words[cpu.0 as usize / 64] = 1 << (cpu.0 % 64);
        Self(words)
    }

    /// The mask `words` names on a machine of `online` CPUs, where word `i`
    /// bit `b` is CPU `64 * i + b`. A missing word is empty.
    pub fn from_words(words: &[u64], online: usize) -> Result<Self, MaskRefused> {
        if words.len() > MASK_WORDS {
            return Err(MaskRefused::TooLong);
        }
        let mut mask = [0; MASK_WORDS];
        mask[..words.len()].copy_from_slice(words);
        let mask = Self(mask);
        if mask.0.iter().all(|&w| w == 0) {
            return Err(MaskRefused::Empty);
        }
        if mask != mask.within(online) {
            return Err(MaskRefused::Offline);
        }
        Ok(mask)
    }

    /// This mask less every CPU at or past `online`.
    pub fn within(self, online: usize) -> Self {
        let mut words = self.0;
        for (i, word) in words.iter_mut().enumerate() {
            let first = i * 64;
            *word &= match online.saturating_sub(first) {
                0 => 0,
                n if n >= 64 => u64::MAX,
                n => (1 << n) - 1,
            };
        }
        Self(words)
    }

    pub fn contains(&self, cpu: CpuId) -> bool {
        self.0.get(cpu.0 as usize / 64).is_some_and(|w| w & 1 << (cpu.0 % 64) != 0)
    }

    /// The one CPU this mask names, if it names exactly one.
    pub fn single(&self) -> Option<CpuId> {
        let mut set = self.0.iter().enumerate().filter(|(_, &w)| w != 0);
        let (i, &w) = set.next()?;
        (set.next().is_none() && w.is_power_of_two()).then(|| CpuId((i * 64) as u32 + w.trailing_zeros()))
    }

    /// The CPUs either mask names.
    pub fn union(self, other: Self) -> Self {
        Self(core::array::from_fn(|i| self.0[i] | other.0[i]))
    }

    pub fn words(&self) -> &[u64; MASK_WORDS] {
        &self.0
    }
}

/// The mask its setters publish, as a sequence lock: an odd version is a
/// write in flight.
///
/// **A writer holds preemption off for its stores**, so no pass on its own
/// CPU can find the version odd, and a reader on another waits out at most
/// [`MASK_WORDS`] stores.
pub struct SharedMask {
    version: AtomicU64,
    words: [AtomicU64; MASK_WORDS],
}

impl SharedMask {
    pub fn new(mask: CpuMask) -> Self {
        Self { version: AtomicU64::new(0), words: mask.0.map(AtomicU64::new) }
    }

    /// The version a [`Self::load`] would return, without the words.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The last mask stored and its version.
    pub fn load(&self) -> (CpuMask, u64) {
        loop {
            let before = self.version.load(Ordering::Acquire);
            if before.is_multiple_of(2) {
                let words = core::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
                fence(Ordering::Acquire);
                if self.version.load(Ordering::Relaxed) == before {
                    return (CpuMask(words), before);
                }
            }
            core::hint::spin_loop();
        }
    }

    /// Publish `mask`, then [`affinity_fence`].
    pub fn store(&self, mask: CpuMask, _preempt: &impl PreemptGuard) {
        let mut version = self.version.load(Ordering::Relaxed);
        loop {
            if !version.is_multiple_of(2) {
                core::hint::spin_loop();
                version = self.version.load(Ordering::Relaxed);
                continue;
            }
            match self.version.compare_exchange_weak(version, version + 1, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => break,
                Err(now) => version = now,
            }
        }
        fence(Ordering::Release);
        for (word, &value) in self.words.iter().zip(mask.0.iter()) {
            word.store(value, Ordering::Relaxed);
        }
        self.version.store(version + 2, Ordering::Release);
        affinity_fence();
    }
}

#[cfg(not(feature = "affinity-fence-relaxed"))]
const AFFINITY_FENCE: Ordering = Ordering::SeqCst;
#[cfg(feature = "affinity-fence-relaxed")]
const AFFINITY_FENCE: Ordering = Ordering::Release;

/// The barrier of a store-buffering pair, so `SeqCst` on both sides. A setter
/// stores the version and then reads the task's state word, in
/// [`crate::sched::cpu::CpuHandles::realign`]; an adopter stores the state word
/// and then reads the version, in [`crate::sched::task::TransitTask::adopt`].
/// Either the setter finds the adopter's CPU and kicks it, or the adopter's
/// refresh finds the new mask. `kernel-loom`'s `affinity-fence-relaxed` makes it
/// a release fence and `loom_affinity.rs` must red; the kernel declares the name
/// only so `cfg` checking knows it.
pub fn affinity_fence() {
    fence(AFFINITY_FENCE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_names_only_online_cpus_and_at_least_one() {
        assert_eq!(CpuMask::from_words(&[0b101], 3), Ok(CpuMask([0b101, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        assert_eq!(CpuMask::from_words(&[0b1000], 3), Err(MaskRefused::Offline));
        assert_eq!(CpuMask::from_words(&[0, 1], 64), Err(MaskRefused::Offline));
        assert_eq!(CpuMask::from_words(&[0, 1], 65), Ok(CpuMask::one(CpuId(64))));
        assert_eq!(CpuMask::from_words(&[], 8), Err(MaskRefused::Empty));
        assert_eq!(CpuMask::from_words(&[0, 0], 8), Err(MaskRefused::Empty));
        assert_eq!(CpuMask::from_words(&[1; MASK_WORDS + 1], MAX_CPUS), Err(MaskRefused::TooLong));
    }

    #[test]
    fn single_names_the_one_cpu_of_a_one_cpu_mask() {
        assert_eq!(CpuMask::one(CpuId(70)).single(), Some(CpuId(70)));
        assert_eq!(CpuMask::from_words(&[1, 1], 128).unwrap().single(), None);
        assert_eq!(CpuMask::from_words(&[0b11], 2).unwrap().single(), None);
        assert_eq!(CpuMask::ANY.single(), None);
        assert!(CpuMask::ANY.contains(CpuId(MAX_CPUS as u32 - 1)));
        assert!(!CpuMask::ANY.contains(CpuId(MAX_CPUS as u32)));
    }

    #[test]
    fn within_keeps_exactly_the_cpus_below_the_count() {
        let any = CpuMask::ANY.within(65);
        assert!(any.contains(CpuId(64)) && !any.contains(CpuId(65)));
        assert_eq!(CpuMask::ANY.within(0), CpuMask::NONE);
    }

    #[test]
    fn a_store_bumps_the_version_by_one_write() {
        struct NoPreempt;
        // SAFETY: a host test has no preemption to hold off.
        #[allow(unsafe_code)]
        unsafe impl PreemptGuard for NoPreempt {}
        let shared = SharedMask::new(CpuMask::ANY);
        assert_eq!(shared.load(), (CpuMask::ANY, 0));
        shared.store(CpuMask::one(CpuId(3)), &NoPreempt);
        assert_eq!(shared.load(), (CpuMask::one(CpuId(3)), 2));
        assert_eq!(shared.version(), 2);
    }
}
