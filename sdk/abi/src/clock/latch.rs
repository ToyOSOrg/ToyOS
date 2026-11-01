//! A value one writer replaces while any reader, an NMI on the writer's own
//! CPU among them, reads it with no lock and no wait for the writer: two
//! copies, and a sequence whose parity names the one to read.
//!
//! The writer moves readers to the second copy, rewrites the first, moves them
//! back and rewrites the second, so a reader always has a copy nobody is
//! writing; it retries only when the sequence moved while it read. Every
//! payload word is atomic, so a torn read is words thrown away, not a data
//! race. No `crate::` references: `kernel-loom` compiles this file under
//! `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicU64, Ordering};

#[repr(C)]
pub struct Latch<const N: usize> {
    seq: AtomicU64,
    copies: [[AtomicU64; N]; 2],
}

impl<const N: usize> Latch<N> {
    /// The kernel lays a latch out in place on its page, so only a test builds one.
    #[cfg(all(test, not(feature = "loom")))]
    pub(super) const fn new() -> Self {
        Self { seq: AtomicU64::new(0), copies: [const { [const { AtomicU64::new(0) }; N] }; 2] }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { seq: AtomicU64::new(0), copies: core::array::from_fn(|_| core::array::from_fn(|_| AtomicU64::new(0))) }
    }

    /// Replace the value. One writer at a time: the caller's own exclusion is
    /// what makes the load-then-store of `seq` sound.
    pub fn publish(&self, words: [u64; N]) {
        let seq = self.seq.load(Ordering::Relaxed);
        // `Release`, so a reader sent to the other copy sees what the last
        // publication left in it.
        self.seq.store(seq.wrapping_add(1), Ordering::Release);
        // Orders the move before the rewrite: a reader that sees a new word
        // sees the sequence moved, and throws the read away.
        #[cfg(not(feature = "clock-latch-fence-off"))]
        fence(Ordering::Release);
        Self::store(&self.copies[(seq & 1) as usize], words);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
        fence(Ordering::Release);
        Self::store(&self.copies[(seq.wrapping_add(1) & 1) as usize], words);
    }

    /// The value one publication left, whole.
    pub fn read(&self) -> [u64; N] {
        self.read_with(|| ()).0
    }

    /// The value one publication left, whole, and what `inside` read while
    /// that publication was in force: a publication that lands between the
    /// value's loads and the recheck after `inside` sends the read round
    /// again. `inside` orders whatever it reads that is not memory, such as a
    /// counter, after the loads before it and before the recheck after it.
    pub fn read_with<T>(&self, mut inside: impl FnMut() -> T) -> ([u64; N], T) {
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            let copy = &self.copies[(seq & 1) as usize];
            let words = core::array::from_fn(|i| copy[i].load(Ordering::Relaxed));
            let read = inside();
            // Orders the word loads, and `inside`'s, before the recheck: a
            // word a later publication wrote is one whose move the recheck
            // sees.
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == seq {
                return (words, read);
            }
        }
    }

    fn store(copy: &[AtomicU64; N], words: [u64; N]) {
        for (slot, word) in copy.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
    }
}
