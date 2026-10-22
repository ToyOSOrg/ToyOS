//! The futex words' watches: one hashed bucket per word, keyed by physical
//! address so every process mapping a word shares it.

use crate::watch::Watch;
use crate::DirectMap;

// A collision only lengthens the walk: a futex wake is bounded by the waiter's token, not by which bucket it landed in.
const FUTEX_BUCKETS: usize = 64;
static FUTEX_WATCH: [Watch; FUTEX_BUCKETS] = [const { Watch::new() }; FUTEX_BUCKETS];

/// The watch for a futex word.
pub fn watch_of(addr: DirectMap) -> &'static Watch {
    &FUTEX_WATCH[(addr.phys() >> 2) as usize % FUTEX_BUCKETS]
}

/// End every futex wait on a word in one of `frames`, ascending 4 KiB frames
/// no leaf maps any more: called after the clear and its shootdown and before
/// the frames can be reissued, so no waiter arms on one again and no later
/// post for a word in a reused frame reaches a waiter from before.
///
/// One call takes each of the 64 bucket locks once, whatever `frames` holds.
pub fn revoke_frames(frames: &[u64]) -> usize {
    revoke_sorted(frames, |&phys| phys)
}

/// [`revoke_frames`] for `sorted`, ascending by the frame `phys` answers.
pub fn revoke_sorted<T>(sorted: &[T], phys: impl Fn(&T) -> u64 + Copy) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    revoke(|token| sorted.binary_search_by_key(&(token & !(crate::mm::PAGE_SIZE - 1)), phys).is_ok())
}

/// [`revoke_frames`] for the frames of `[phys, phys + len)`.
pub fn revoke_run(phys: u64, len: u64) -> usize {
    revoke(|token| token.wrapping_sub(phys) < len)
}

fn revoke(ended: impl Fn(u64) -> bool + Copy) -> usize {
    // A frame spans buckets by construction, so every bucket is walked.
    FUTEX_WATCH.iter().map(|watch| watch.revoke(ended)).sum()
}
