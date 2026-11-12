//! The futex words' watches: one hashed bucket per word, keyed by physical
//! address so every process mapping a word shares it.

use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use crate::watch::Watch;

// A collision only lengthens the walk: a futex wake is bounded by the waiter's token, not by which bucket it landed in.
const FUTEX_BUCKETS: usize = 64;
static FUTEX_WATCH: [Watch; FUTEX_BUCKETS] = [const { Watch::new() }; FUTEX_BUCKETS];

/// The watch for the futex word at `phys`.
pub fn watch_of(phys: u64) -> &'static Watch {
    &FUTEX_WATCH[(phys >> 2) as usize % FUTEX_BUCKETS]
}

/// One address space's threads inside a futex wait, each counted by its
/// `mm::space::FutexWait`.
#[derive(Default)]
pub struct Waits(AtomicUsize);

impl Waits {
    pub fn raise(&self) {
        self.0.fetch_add(1, Relaxed);
    }

    pub fn lower(&self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

/// End every futex wait on a word in one of `frames`, ascending 4 KiB frames
/// of the space whose waits `waits` counts, which no leaf maps any more:
/// called under that space's lock, after the clear and its shootdown and
/// before the frames can be reissued, so no waiter arms on one again and no
/// later post for a word in a reused frame reaches a waiter from before.
///
/// With none of the space's threads in a futex wait it walks nothing;
/// otherwise it takes each of the 64 bucket locks once, whatever `frames`
/// holds.
pub fn revoke_frames(waits: &Waits, frames: &[u64]) -> usize {
    // A private frame is mapped in its one space, so only a wait of that
    // space's threads can be keyed on it, and only on a frame its syscall's
    // translation found, which takes the space's lock after the wait is
    // counted (`mm::space::FutexWait`). A wait whose translation held the lock
    // before the clear this runs under is counted by then, and the lock orders
    // that count before this load: the buckets are walked, and the walk's
    // bucket locks order the shootdown's count before any later check of the
    // wait (`Translation::holds`). A wait whose translation came after finds
    // the frame gone and waits on no frame of these. The lock orders both, so
    // `Relaxed` is enough.
    if waits.0.load(Relaxed) == 0 {
        return 0;
    }
    revoke_sorted(frames, |&phys| phys)
}

/// [`revoke_frames`] for `sorted`, ascending by the frame `phys` answers, of
/// a shared object: every bucket is walked, since a wait in any space that
/// mapped the object can be keyed on its frames.
pub fn revoke_sorted<T>(sorted: &[T], phys: impl Fn(&T) -> u64 + Copy) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    revoke(|token| sorted.binary_search_by_key(&(token & !(crate::mm::PAGE_SIZE - 1)), phys).is_ok())
}

/// [`revoke_sorted`] for the frames of `[phys, phys + len)`.
pub fn revoke_run(phys: u64, len: u64) -> usize {
    revoke(|token| token.wrapping_sub(phys) < len)
}

fn revoke(ended: impl Fn(u64) -> bool + Copy) -> usize {
    // A frame spans buckets by construction, so every bucket is walked.
    FUTEX_WATCH.iter().map(|watch| watch.revoke(ended)).sum()
}
