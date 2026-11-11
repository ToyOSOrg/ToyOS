//! Two protocols with no hardware in them, by which one CPU asks others and
//! waits for their answers.
//!
//! [`Shootdown`]: a generation one CPU issues and every other acknowledges: a
//! machine-wide TLB shootdown's, and a counters read's round. The read must
//! happen before the answer, or a target could publish a generation its answer
//! does not cover; and an initiator that sees a generation served sees
//! everything that answer wrote. A generation is narrow or wide, and an answer
//! reaches as far as the widest generation it covers: one answer covers every
//! generation owed, so the reach is the target's to read and never the IPI's.
//!
//! [`Asks`]: one user space's range a CPU asks the CPUs it names to flush,
//! each answering that CPU alone, so a CPU not asked owes nothing and the
//! asking CPU owes itself nothing.
//!
//! Compiled a second time into `kernel/loom/` against loom's atomics, so this
//! file names nothing of the kernel but [`CpuSlice`], which `kernel/loom/`
//! compiles beside it.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};

use crate::cpu_slice::CpuSlice;

// Acquire: makes the flush that follows see the initiator's page-table write.
#[cfg(not(feature = "shootdown-serve-relaxed"))]
const OWED: Ordering = Ordering::Acquire;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-serve-relaxed")]
const OWED: Ordering = Ordering::Relaxed;

// Acquire: pairs with `serve`'s Release, so what its closure wrote is visible once a generation reads served.
#[cfg(not(feature = "shootdown-served-relaxed"))]
const SERVED: Ordering = Ordering::Acquire;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-served-relaxed")]
const SERVED: Ordering = Ordering::Relaxed;

/// Which shootdown a flush answers for: monotonic and machine-wide, so one target's flush answers every initiator waiting on it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Generation(u64);

/// How far an answer must reach: what a wide generation asks of it, a narrow one does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    Narrow,
    Wide,
}

/// One CPU's side, which its serve writes every round.
struct Answer {
    flushed: AtomicU64,
    /// `wide` as the last wide answer this CPU finished read it.
    widened: AtomicU64,
}

pub struct Shootdown {
    requested: AtomicU64,
    /// Wide generations issued, each counted before it is issued.
    wide: AtomicU64,
    answers: CpuSlice<Answer>,
}

impl Shootdown {
    /// Must stay `const`: the kernel's instances are `static`s, each
    /// [`Self::size`]d before an AP is attempted.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { requested: AtomicU64::new(0), wide: AtomicU64::new(0), answers: CpuSlice::new() }
    }

    // Not `const`: loom's atomics have no const constructor.
    // No `Default` impl: it cannot be `const`, which the kernel's `static` requires.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { requested: AtomicU64::new(0), wide: AtomicU64::new(0), answers: CpuSlice::new() }
    }

    /// An answer for each of the `cpus` CPUs the firmware names.
    pub fn size(&self, cpus: usize) {
        self.answers.size(cpus, |_| Answer { flushed: AtomicU64::new(0), widened: AtomicU64::new(0) });
    }

    /// Names the generation every other CPU now owes a flush for, of `reach`.
    pub fn issue(&self, reach: Reach) -> Generation {
        if reach == Reach::Wide {
            // Before the generation exists, and Release: a serve that owes the generation reads this
            // count, and an answer that reads the count follows the write it was raised for.
            self.wide.fetch_add(1, Ordering::Release);
        }
        // AcqRel, not Release: the acquire half stops a later load being hoisted above this fetch_add.
        Generation(self.requested.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// A target's whole side of the protocol: `flush` is told how far its answer must reach.
    pub fn serve(&self, cpu: usize, flush: impl FnOnce(Reach)) {
        let owed = self.requested.load(OWED);
        let wide = self.wide.load(Ordering::Acquire);
        let reach = if wide > self.answers[cpu].widened.load(Ordering::Acquire) { Reach::Wide } else { Reach::Narrow };
        flush(reach);
        if reach == Reach::Wide {
            // After the flush: a serve nested inside this one, reading this record first, answers wide itself.
            self.answers[cpu].widened.fetch_max(wide, Ordering::Release);
        }
        // A max, not a store: a serve nested inside this one may already have published a later generation.
        self.answers[cpu].flushed.fetch_max(owed, Ordering::Release);
    }

    /// `wide` as `cpu`'s last wide answer read it: a change across a span is a wide answer inside it.
    #[cfg(feature = "test-actuators")]
    pub fn widened(&self, cpu: usize) -> u64 {
        self.answers[cpu].widened.load(Ordering::Relaxed)
    }

    /// Does `cpu` owe anyone a flush?
    pub fn owes(&self, cpu: usize) -> bool {
        // `serve` carries the ordering of the flush. A poll that reads a stale
        // `false` answers on a later turn; the interrupt handler, which gets
        // no later turn, reads what the IPI asks because its send is ordered
        // after the issue (`arch::tlb::serve_ipi`).
        self.requested.load(Ordering::Relaxed) > self.answers[cpu].flushed.load(Ordering::Relaxed)
    }

    /// Has `cpu` flushed since `generation` was issued?
    pub fn served(&self, cpu: usize, generation: Generation) -> bool {
        self.answers[cpu].flushed.load(SERVED) >= generation.0
    }

    /// [`serve`](Self::serve) for a CPU not taking the interrupt; whether it served.
    pub fn serve_if_owed(&self, cpu: usize, flush: impl FnOnce(Reach)) -> bool {
        let owed = self.owes(cpu);
        if owed {
            self.serve(cpu, flush);
        }
        owed
    }
}

// Acquire: makes the flush that follows see the asking CPU's page-table write
// and its range.
#[cfg(not(feature = "shootdown-take-relaxed"))]
const TAKE: Ordering = Ordering::Acquire;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-take-relaxed")]
const TAKE: Ordering = Ordering::Relaxed;

// Release: an asking CPU that reads its answer follows the flush before it.
#[cfg(not(feature = "shootdown-answer-relaxed"))]
const ANSWER: Ordering = Ordering::Release;
// Relaxed arm: kernel-loom's negative-control mutation, never selected by a kernel build.
#[cfg(feature = "shootdown-answer-relaxed")]
const ANSWER: Ordering = Ordering::Relaxed;

/// A set of CPUs, a bit each, of the first [`ASKABLE`]: as many as a user
/// change asks, past which it reaches every CPU by a [`Shootdown`].
pub type CpuSet = [u64; WORDS];
const WORDS: usize = 4;
pub const ASKABLE: usize = 64 * WORDS;

/// What one CPU asks: the range of the space it names.
struct Ask {
    tag: AtomicU64,
    start: AtomicU64,
    end: AtomicU64,
}

/// One CPU's side of [`Asks`].
struct Mailbox {
    /// A bit per CPU asking this one.
    asking: [AtomicU64; WORDS],
    /// What this CPU asks.
    ask: Ask,
    /// A bit per CPU that has answered this one's ask.
    answered: [AtomicU64; WORDS],
}

/// What an answer must flush: the one range it was asked, or, asked more than
/// one at once, every user translation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flush {
    Range { tag: u16, start: u64, end: u64 },
    User,
}

pub struct Asks {
    boxes: CpuSlice<Mailbox>,
}

impl Asks {
    /// Must stay `const`: the kernel's instance is a `static`, [`Self::size`]d
    /// before an AP is attempted.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { boxes: CpuSlice::new() }
    }

    // Not `const`: loom's atomics have no const constructor.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { boxes: CpuSlice::new() }
    }

    /// A mailbox for each of the first [`ASKABLE`] of the `cpus` CPUs the
    /// firmware names.
    pub fn size(&self, cpus: usize) {
        let zero = || core::array::from_fn(|_| AtomicU64::new(0));
        self.boxes.size(cpus.min(ASKABLE), |_| Mailbox {
            asking: zero(),
            ask: Ask { tag: AtomicU64::new(0), start: AtomicU64::new(0), end: AtomicU64::new(0) },
            answered: zero(),
        });
    }

    /// `me` asks each CPU `targets` holds, a bit per CPU, to flush
    /// `[start, end)` of the space tagged `tag`. The caller's precondition,
    /// on which the protocol rests: every CPU `me` asked last has answered, so
    /// no CPU still reads its range or will answer it but the ones this asks.
    pub fn ask(&self, me: usize, tag: u16, start: u64, end: u64, targets: &CpuSet) {
        let own = &self.boxes[me];
        // Kernel-loom's negative controls, never selected by a kernel build,
        // leave the last ask's answers standing and write the range late.
        if !cfg!(feature = "shootdown-ask-stale-answers") {
            own.answered.iter().for_each(|word| word.store(0, Ordering::Relaxed));
        }
        let late = cfg!(feature = "shootdown-ask-range-late");
        if !late {
            own.ask.store(tag, start, end);
        }
        for_each(targets, |cpu| {
            // Release: the target that takes this bit reads the range and the
            // page-table write behind it.
            self.boxes[cpu].asking[me / 64].fetch_or(1 << (me % 64), Ordering::Release);
        });
        if late {
            own.ask.store(tag, start, end);
        }
    }

    /// Has `cpu` answered `me`'s ask?
    pub fn answered(&self, me: usize, cpu: usize) -> bool {
        // Acquire: pairs with `serve`'s Release, so the answer's flush comes first.
        self.boxes[me].answered[cpu / 64].load(Ordering::Acquire) & 1 << (cpu % 64) != 0
    }

    /// `cpu`'s whole side: take every ask, flush as `flush` is told, and
    /// answer each asking CPU. Whether anything was asked.
    pub fn serve(&self, cpu: usize, flush: impl FnOnce(Flush)) -> bool {
        let Some(own) = self.boxes.get(cpu) else { return false };
        let mut taken = CpuSet::default();
        for (word, asking) in taken.iter_mut().zip(&own.asking) {
            // A stale 0 leaves the bit for a later serve; the interrupt
            // handler reads it because the IPI's send is ordered after it.
            if asking.load(Ordering::Relaxed) != 0 {
                *word = asking.swap(0, TAKE);
            }
        }
        let mut asker = None;
        let mut asked = 0;
        for_each(&taken, |one| {
            asker = Some(one);
            asked += 1;
        });
        let Some(asker) = asker else { return false };
        // Kernel-loom's negative control answers many as one, never selected by a kernel build.
        let one = asked == 1 || cfg!(feature = "shootdown-ask-many-as-one");
        flush(if one { self.boxes[asker].ask.load() } else { Flush::User });
        for_each(&taken, |asker| {
            // Release: an asking CPU that reads its answer follows the flush.
            self.boxes[asker].answered[cpu / 64].fetch_or(1 << (cpu % 64), ANSWER);
        });
        true
    }
}

impl Ask {
    fn store(&self, tag: u16, start: u64, end: u64) {
        self.tag.store(u64::from(tag), Ordering::Relaxed);
        self.start.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Relaxed);
    }

    fn load(&self) -> Flush {
        // Every tag fits: it was stored from a `u16`.
        let tag = self.tag.load(Ordering::Relaxed) as u16;
        Flush::Range { tag, start: self.start.load(Ordering::Relaxed), end: self.end.load(Ordering::Relaxed) }
    }
}

/// Each CPU `set` holds, a bit per CPU.
pub fn for_each(set: &CpuSet, mut f: impl FnMut(usize)) {
    for (w, &word) in set.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            f(w * 64 + bits.trailing_zeros() as usize);
            bits &= bits - 1;
        }
    }
}
