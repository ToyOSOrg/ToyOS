//! The live-object census, waited on rather than sampled against a clock.
//!
//! **A release is not finished when the syscall that caused it returns.** The
//! last handle's drop queues the object on the object layer's zero-handle
//! queue, and `object::drain_zero_handles` clears its pending flag before it
//! runs the hooks, so the release can land on another CPU after the caller's
//! own drain site found the queue empty
//! (`issues/kernel/deferred-release-outlives-its-syscall.md`). So a reading is
//! a wait for an event, and a leak is the event that never comes: the harness
//! ceiling reds it.

use std::time::Duration;

use toyos::census::Census;

/// Between two readings. A pace and never a verdict: the zero-handle queue is
/// drained on the idle loop among other places, and a reader that never
/// sleeps keeps its CPU from ever reaching it.
const PACE: Duration = Duration::from_millis(10);

/// The census once two readings a pace apart agree.
pub fn settled() -> Census {
    let mut last = Census::now();
    loop {
        std::thread::sleep(PACE);
        let next = Census::now();
        if next == last {
            return next;
        }
        last = next;
    }
}

/// The census once no kind has grown since `before`.
pub fn released_to(before: &Census) -> Census {
    loop {
        let now = Census::now();
        if now.grown_since(before).next().is_none() {
            return now;
        }
        std::thread::sleep(PACE);
    }
}
