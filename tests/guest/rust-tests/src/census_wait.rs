//! The live-object census, waited on rather than sampled against a clock.
//!
//! **A release is not finished when the syscall that caused it returns.** The
//! last handle's drop queues the object on the object layer's zero-handle
//! queue, and `object::drain_zero_handles` clears its pending flag before it
//! runs the hooks, so the release can land on another CPU after the caller's
//! own drain site found the queue empty
//! (`issues/deferred-release-outlives-its-syscall.md`). So a reading is
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

/// Paces between two lines naming what a wait is still waiting on.
const SAY_EVERY: u32 = 100;

/// The census once no kind has grown since `before`. A wait past
/// [`SAY_EVERY`] paces names the kinds still grown, again whenever they
/// change, so a wait the harness ceiling ends says what it was waiting on.
pub fn released_to(before: &Census) -> Census {
    let mut paces = 0u32;
    let mut said = String::new();
    loop {
        let now = Census::now();
        let grown: Vec<String> = now.grown_since(before).map(|(kind, was, is)| format!("{kind:?} {was} -> {is}")).collect();
        if grown.is_empty() {
            return now;
        }
        paces += 1;
        if paces % SAY_EVERY == 0 {
            let line = grown.join(", ");
            if line != said {
                println!("census_wait: after {paces} paces, still grown since the first reading: {line}");
                said = line;
            }
        }
        std::thread::sleep(PACE);
    }
}
