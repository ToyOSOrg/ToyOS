//! The published framebuffer descriptor: a seqlock whose payload is atomic
//! words, so a reader racing the one publisher reads torn words it throws
//! away rather than racing a plain write (a data race, and undefined, in the
//! Rust memory model whatever the check after it concludes).
//!
//! The writer marks the sequence odd, fences `Release`, stores the words
//! `Relaxed` and marks it even with a `Release` store; the reader loads the
//! sequence `Acquire`, the words `Relaxed`, fences `Acquire` and reloads the
//! sequence. A reader whose two loads agree on an even value read words the
//! publication that value ends wrote, all of them.
//! No `crate::` references: `kernel-loom` compiles this file under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

/// Words in one descriptor.
pub const WORDS: usize = 4;

/// Tries a snapshot makes before answering that the descriptor is changing.
const TRIES: usize = 4;

pub struct Published {
    seq: AtomicU32,
    words: [AtomicU64; WORDS],
}

impl Published {
    // Must stay `const`: the kernel's is a `static`, and loom's atomics have no const constructor.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { seq: AtomicU32::new(0), words: [const { AtomicU64::new(0) }; WORDS] }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { seq: AtomicU32::new(0), words: core::array::from_fn(|_| AtomicU64::new(0)) }
    }

    /// Replace the descriptor. One publisher at a time: the caller's own
    /// exclusion (the boot sequence, a mode set's single-CPU window) is what
    /// makes the load-then-store of `seq` sound.
    pub fn publish(&self, words: [u64; WORDS]) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd mark before every word below: a reader that sees a
        // new word sees the mark, and throws the read away.
        #[cfg(not(feature = "seqlock-writer-fence-off"))]
        fence(Ordering::Release);
        for (slot, word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// The descriptor, whole, or `None` when every try met a publication in
    /// progress.
    pub fn snapshot(&self) -> Option<[u64; WORDS]> {
        for _ in 0..TRIES {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let words = core::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
            // Orders the word loads before the recheck: a word a later
            // publication wrote is one whose odd mark the recheck sees.
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some(words);
            }
        }
        None
    }
}
