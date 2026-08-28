//! In-guest gate on [`Operation`] nesting: `begin` narrows the current
//! deadline and never widens it, and `Drop` restores what it displaced.
//! [`run`] is called from a boot-phase CPU context and from the `iod` task,
//! the two places a deadline slot can be established.

use crate::clock;
use crate::scheduler::Operation;
use crate::time::{Deadline, Duration};

// INNER < OUTER < WIDER: level 2 must narrow, level 3 must change nothing.
const OUTER: u64 = 1_000_000_000;
const INNER: u64 = 250_000_000;
const WIDER: u64 = 4_000_000_000;

/// Establishes three nested operations at `site` and logs what each level observed.
pub fn run(site: &str) {
    let base = clock::now();
    log!(
        "sched-op: {site} outside established={}",
        Operation::established(),
    );

    let level = |until: u64| Deadline::at(base + Duration::from_nanos(until));
    let observed = || Operation::deadline().nanos() - base.nanos_since_boot();

    let outer = Operation::begin(level(OUTER));
    log!(
        "sched-op: {site} begin level=1 asked={OUTER} observed={}",
        observed(),
    );
    {
        let inner = Operation::begin(level(INNER));
        log!(
            "sched-op: {site} begin level=2 asked={INNER} observed={}",
            observed(),
        );
        {
            // Asking for more than the enclosing frame must not widen it.
            let _wider = Operation::begin(level(WIDER));
            log!(
                "sched-op: {site} begin level=3 asked={WIDER} observed={}",
                observed(),
            );
        }
        log!(
            "sched-op: {site} end level=3 observed={} established={}",
            observed(),
            Operation::established(),
        );
        drop(inner);
        log!(
            "sched-op: {site} end level=2 observed={} established={}",
            observed(),
            Operation::established(),
        );
    }
    drop(outer);
    log!(
        "sched-op: {site} end level=1 established={}",
        Operation::established(),
    );
}
