//! The live-object census, waited on rather than sampled against a clock.
//!
//! **A release is not finished when the syscall that caused it returns.** The
//! last handle's drop queues the object on the object layer's zero-handle
//! queue, and `object::drain_zero_handles` clears its pending flag before it
//! runs the hooks, so the release can land on another CPU after the caller's
//! own drain site found the queue empty
//! (`issues/deferred-release-outlives-its-syscall.md`). So a reading is
//! a wait for an event, and a leak is the event that never comes: the wait
//! panics past [`BOUND`] naming what it waited on, so a shared boot's later
//! members still run.

use std::time::{Duration, Instant};

use toyos::census::Census;

/// Between two readings. A pace and never a verdict: the zero-handle queue is
/// drained on the idle loop among other places, and a reader that never
/// sleeps keeps its CPU from ever reaching it.
const PACE: Duration = Duration::from_millis(10);

/// How long a wait takes before it is a failure: a liveness ceiling far above
/// a release, whose deferred drain lands within milliseconds of the event.
const BOUND: Duration = Duration::from_secs(10);

/// The census once two readings a pace apart agree.
pub fn settled() -> Census {
    let deadline = Instant::now() + BOUND;
    let mut last = Census::now();
    loop {
        std::thread::sleep(PACE);
        let next = Census::now();
        if next == last {
            return next;
        }
        assert!(Instant::now() < deadline, "census_wait: no two readings agreed within {BOUND:?}: {last}, then {next}");
        last = next;
    }
}

/// The census once no kind has grown since `before`, or a panic past
/// [`BOUND`] naming the kinds still grown.
pub fn released_to(before: &Census) -> Census {
    let deadline = Instant::now() + BOUND;
    loop {
        let now = Census::now();
        let grown: Vec<String> = now.grown_since(before).map(|(kind, was, is)| format!("{kind:?} {was} -> {is}")).collect();
        if grown.is_empty() {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "census_wait: still grown since the first reading after {BOUND:?}: {}",
            grown.join(", ")
        );
        std::thread::sleep(PACE);
    }
}
