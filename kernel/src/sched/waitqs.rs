//! Wait subjects: every waitable object owns a `Watch`; a waiter parks on its own thread's queue, not a shared list.

use crate::completion::{self, Outcome, Subject, Watch};
use crate::DirectMap;

// A collision only lengthens the walk: matching is by the waiter's token, not by which bucket it landed in.
const FUTEX_BUCKETS: usize = 64;
static FUTEX_WATCH: [Watch; FUTEX_BUCKETS] = [const { Watch::new() }; FUTEX_BUCKETS];

/// Device wait subjects; a reader arms directly on one and parks on its own thread's queue.
/// Only these two exist — an empty Mouse, NIC or framebuffer read answers `NotFound`, never parks.
pub static KEYBOARD_WATCH: Watch = Watch::new();
pub static AUDIO_WATCH: Watch = Watch::new();

/// Wakes every waiter armed on `watch`.
pub fn wake_device(watch: &'static Watch) {
    completion::post(Subject::of(watch), Outcome::Ready);
}

/// The watch for a futex word, keyed by physical address so it is shared across every process mapping the word.
pub fn futex_watch(addr: DirectMap) -> &'static Watch {
    &FUTEX_WATCH[(addr.phys() >> 2) as usize % FUTEX_BUCKETS]
}

/// Call only after `[phys, phys + len)` is unmapped, under the address-space lock, so no waiter arms on memory already freed.
pub fn revoke_futex_range(phys: u64, len: u64) -> usize {
    // A range spans buckets by construction, so every bucket is walked; an unarmed one costs one relaxed load.
    FUTEX_WATCH
        .iter()
        .map(|watch| completion::revoke_range(Subject::of(watch), phys, len))
        .sum()
}

