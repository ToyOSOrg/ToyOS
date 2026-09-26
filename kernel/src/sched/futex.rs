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

/// Call only after `[phys, phys + len)` is unmapped, under the address-space lock, so no waiter arms on memory already freed.
pub fn revoke_range(phys: u64, len: u64) -> usize {
    // A range spans buckets by construction, so every bucket is walked; an unarmed one costs one lock round trip.
    FUTEX_WATCH.iter().map(|watch| watch.revoke_range(phys, len)).sum()
}
