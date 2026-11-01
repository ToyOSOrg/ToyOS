//! Which CPUs a change to one user space's leaves must reach: those running
//! the space, and no other. A CPU that ran the space before keeps its
//! translations under the space's tag (x86 keeps them across `CR3` loads
//! under PCID), so instead of being reached it flushes when it next enters the
//! space, if the space changed since its own last flush of every user
//! translation.
//!
//! **The pairing that makes it sound** is [`Tags::enter`] against
//! [`Tags::changed`]: a CPU marks itself running the tag, then reads when the
//! tag last changed; a change stamps the tag, then reads who runs it. A
//! `SeqCst` fence sits between each side's store and load, so either the
//! entering CPU sees the stamp and flushes, or the change sees the CPU and
//! reaches it. The stamp is released after the leaves the change wrote and
//! acquired by the entering CPU, so its flush follows them.
//!
//! Compiled a second time by `kernel-loom`, so it names nothing of the kernel
//! but [`CpuSlice`].

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};

use crate::cpu_slice::CpuSlice;

/// The fence each side of the pairing puts between its store and its load:
/// without it either load may be satisfied before the other side's store.
#[cfg(not(feature = "tlb-tags-enter-relaxed"))]
fn pair() {
    fence(Ordering::SeqCst);
}

// Kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "tlb-tags-enter-relaxed")]
fn pair() {}

/// What [`Tags::switch`] answers.
pub struct Switched {
    /// The tag left, 0 for none, to give [`Tags::left`] after the load.
    pub from: usize,
    /// Every user translation must be flushed before the load.
    pub flush: bool,
}

pub struct Tags {
    /// Monotonic: one more for every change to any user space's leaves.
    stamp: AtomicU64,
    /// Per tag, the stamp of its last change.
    changed: CpuSlice<AtomicU64>,
    /// Per tag, a bit per CPU running it: `words` words a tag.
    running: CpuSlice<AtomicU64>,
    words: AtomicUsize,
    /// Per CPU, the stamp read before its last flush of every user
    /// translation: a tag changed since needs another.
    flushed: CpuSlice<AtomicU64>,
    /// Per CPU, the tag it runs, 0 for none.
    on: CpuSlice<AtomicUsize>,
}

impl Tags {
    /// Must stay `const`: the kernel's instance is a `static`, [`Self::size`]d
    /// before an AP is attempted.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            stamp: AtomicU64::new(0),
            changed: CpuSlice::new(),
            running: CpuSlice::new(),
            words: AtomicUsize::new(0),
            flushed: CpuSlice::new(),
            on: CpuSlice::new(),
        }
    }

    // Not `const`: loom's atomics have no const constructor.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            stamp: AtomicU64::new(0),
            changed: CpuSlice::new(),
            running: CpuSlice::new(),
            words: AtomicUsize::new(0),
            flushed: CpuSlice::new(),
            on: CpuSlice::new(),
        }
    }

    /// Room for `tags` tags on `cpus` CPUs. Once.
    pub fn size(&self, tags: usize, cpus: usize) {
        let words = cpus.div_ceil(64);
        self.words.store(words, Ordering::Relaxed);
        self.changed.size(tags, |_| AtomicU64::new(0));
        self.running.size(tags * words, |_| AtomicU64::new(0));
        self.flushed.size(cpus, |_| AtomicU64::new(0));
        self.on.size(cpus, |_| AtomicUsize::new(0));
    }

    /// Whether [`Self::size`] has run: before it the boot CPU runs alone, no
    /// user space exists, and a CPU may not yet know its own number.
    pub fn sized(&self) -> bool {
        !self.on.is_empty()
    }

    /// `cpu` is about to run the space tagged `to`, 0 for none, in place of
    /// the one it runs: called before it loads `to`'s root. Answers the tag it
    /// leaves, for [`Self::left`] once that root is replaced, and whether it
    /// must flush every user translation first, since `to` changed after its
    /// last. A CPU counts as running a space from before its root is loaded
    /// until after it is replaced, so for that instant it runs both: the
    /// walker may still use the old root's tables until the load.
    pub fn switch(&self, cpu: usize, to: usize) -> Switched {
        // Before sizing the boot CPU runs alone and no user space exists.
        let Some(on) = self.on.get(cpu) else { return Switched { from: 0, flush: false } };
        let from = on.swap(to, Ordering::Relaxed);
        if from == to {
            return Switched { from: 0, flush: false };
        }
        Switched { from, flush: to != 0 && self.enter(cpu, to) }
    }

    /// `cpu` has replaced the root of the space tagged `from`, which
    /// [`Self::switch`] answered.
    pub fn left(&self, cpu: usize, from: usize) {
        if from != 0 {
            self.leave(cpu, from);
        }
    }

    fn bit(&self, tag: usize, cpu: usize) -> (&AtomicU64, u64) {
        let words = self.words.load(Ordering::Relaxed);
        (&self.running[tag * words + cpu / 64], 1 << (cpu % 64))
    }

    /// `cpu` runs `tag` from here; `true` where it must flush every user
    /// translation before it does, since the tag changed after its last.
    fn enter(&self, cpu: usize, tag: usize) -> bool {
        let (word, bit) = self.bit(tag, cpu);
        word.fetch_or(bit, Ordering::Relaxed);
        pair();
        // Acquire: a flush this answers for follows the leaves the change wrote.
        self.changed[tag].load(Ordering::Acquire) > self.flushed[cpu].load(Ordering::Relaxed)
    }

    /// `cpu` runs `tag` no more: it touches none of its memory from here.
    fn leave(&self, cpu: usize, tag: usize) {
        let (word, bit) = self.bit(tag, cpu);
        word.fetch_and(!bit, Ordering::Release);
    }

    /// The stamp a flush of every user translation is about to cover, read
    /// before it, and given to [`Self::flushed`] after it.
    pub fn flushing(&self) -> u64 {
        self.stamp.load(Ordering::SeqCst)
    }

    /// `cpu` flushed every user translation, covering `stamp`.
    pub fn flushed(&self, cpu: usize, stamp: u64) {
        if let Some(own) = self.flushed.get(cpu) {
            own.fetch_max(stamp, Ordering::Relaxed);
        }
    }

    /// `tag`'s leaves changed: call `reach` for each CPU running it now,
    /// which must flush before the change's frames go. Every other CPU flushes
    /// when it next enters the tag.
    pub fn changed(&self, tag: usize, mut reach: impl FnMut(usize)) {
        let Some(changed) = self.changed.get(tag) else { return };
        let stamp = self.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        // Release: the leaves written before are visible to who reads the stamp.
        changed.fetch_max(stamp, Ordering::Release);
        fence(Ordering::SeqCst);
        let words = self.words.load(Ordering::Relaxed);
        for w in 0..words {
            let mut bits = self.running[tag * words + w].load(Ordering::Relaxed);
            while bits != 0 {
                reach(w * 64 + bits.trailing_zeros() as usize);
                bits &= bits - 1;
            }
        }
    }
}
