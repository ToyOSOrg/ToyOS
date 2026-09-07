//! What this machine's wake latency is, as a distribution.
//!
//! The design is `issues/diagnostics/no-cyclictest.md`'s: enter the real-time
//! band, arm a timer, sleep, and histogram `actual − programmed` at 1 µs
//! resolution over enough samples to have percentiles rather than a maximum.
//! Nothing else in the tree measures this — soundd's `max_wake_lat_ns` is a
//! maximum over a ~2 s window measured against a DLL's prediction of a DMA
//! completion, so it folds in the device model and needs a sound card, and
//! `toyos-sched`'s invariant I4 bounds the same quantity inside a simulator
//! that can never see a real IPI.
//!
//! **The target is absolute and is never re-based on where the wake landed.**
//! `target[i] = start + (i + 1) * PERIOD`, so a late wake shortens the next
//! sleep instead of shifting every later target by its own lateness; a run that
//! re-based would report one late wake as one sample rather than as the drift
//! it is.
//!
//! **Boundary contract: this program's exit code is its p99, in microseconds.**
//! On the machine this instrument exists for there is no serial port, a
//! userland `println!` reaches `Backend::None` and is dropped, and the only
//! word a program gets onto the log partition is the kernel's own
//! `exit: <name> pid=N code=N cpu=Nms` record. So the headline number leaves
//! through the exit code: `0..=[REPORTED_CEILING_US]` is a measured p99 in
//! microseconds, [`REPORTED_CEILING_US`] means "at or above that", and
//! [`RUN_FAILED`] means the measurement did not happen and no number in this
//! run means anything. Every percentile is on stdout as well, for the host that
//! has a console to read it on.

use std::process::exit;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::syscall;

/// How often a wake is asked for. Ten thousand of these is two seconds of a
/// boot, which is what buys a 99th percentile at all: a hundredth of the
/// samples is a hundred of them, so the figure is a percentile and not the
/// second-largest reading.
const PERIOD_NS: u64 = 200_000;
const SAMPLES: usize = 10_000;

/// Discarded before the histogram opens: the first wakes of a fresh process pay
/// for its own demand paging and for the timer's first arm, which is a
/// property of starting rather than of waking.
const WARMUP: usize = 100;

/// The widest lateness the histogram has a bucket for; anything past it lands
/// in the last one and is reported as "at or above".
const BUCKETS: usize = 4096;

/// The largest p99 the exit code can carry, and the value it carries for
/// anything wider.
const REPORTED_CEILING_US: u64 = 254;

/// The exit code that is not a measurement.
const RUN_FAILED: i32 = 255;

fn main() {
    // **The band is the privilege and it is asked for by name.** A refusal is
    // loud rather than silently measuring the ordinary band: the two are
    // different machines, and a number that did not say which it came from
    // would be compared against a ceiling taken on the other.
    let cap: Option<SysCap> = Endowments::get().take(SYSCAP_LABEL);
    let Some(cap) = cap else {
        println!("cyclictest: no capability was endowed, so there is no real-time band to enter");
        exit(RUN_FAILED);
    };
    if let Err(e) = cap.enter_rt() {
        println!("cyclictest: the real-time band was refused: {e:?}");
        exit(RUN_FAILED);
    }

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst = 0u64;

    for _ in 0..WARMUP {
        syscall::nanosleep(PERIOD_NS);
    }
    // The origin for the whole run, taken after the warm-up so the warm-up's
    // own drift is in no later target.
    let start = syscall::clock_nanos();

    for i in 0..SAMPLES {
        let target = start + (i as u64 + 1) * PERIOD_NS;
        let now = syscall::clock_nanos();
        // A target already passed is a wake this loop owes nothing for: the
        // lateness is recorded and the next sleep is skipped rather than
        // negative.
        if let Some(remaining) = target.checked_sub(now).filter(|left| *left > 0) {
            syscall::nanosleep(remaining);
        }
        let late_us = syscall::clock_nanos().saturating_sub(target) / 1_000;
        worst = worst.max(late_us);
        match usize::try_from(late_us).ok().filter(|us| *us < BUCKETS) {
            Some(bucket) => histogram[bucket] += 1,
            None => overflow += 1,
        }
    }

    let percentile = |want: usize| -> u64 {
        let mut seen = 0usize;
        for (us, count) in histogram.iter().enumerate() {
            seen += *count as usize;
            if seen >= want {
                return us as u64;
            }
        }
        // Every remaining sample is in the overflow, which has no bucket to name.
        BUCKETS as u64
    };

    let p50 = percentile(SAMPLES / 2);
    let p90 = percentile(SAMPLES * 9 / 10);
    let p99 = percentile(SAMPLES * 99 / 100);
    let p999 = percentile(SAMPLES * 999 / 1000);
    println!(
        "cyclictest: {SAMPLES} wakes at {}us: p50={p50}us p90={p90}us p99={p99}us \
         p99.9={p999}us max={worst}us, {overflow} past the {BUCKETS}us histogram",
        PERIOD_NS / 1_000,
    );

    exit(p99.min(REPORTED_CEILING_US) as i32);
}
