//! What this machine's wake latency is, as a distribution.
//!
//! Enter the real-time band, arm a timer, sleep, and histogram
//! `actual − programmed` at 1 µs resolution over enough samples to have
//! percentiles rather than a maximum.
//! Nothing else in the tree measures this — soundserver's `max_wake_lat_ns` is a
//! maximum over a ~2 s window measured against a DLL's prediction of a DMA
//! completion, so it folds in the device model and needs a sound card, and
//! `kernel-sim`'s invariant I4 bounds the same quantity inside a simulator
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
//! userland `println!` reaches the log partition as a line under the runner's
//! name among every other job's, and the one word about a program the kernel
//! writes itself is its `exit: <name> pid=N code=N cpu=Nms` record, which
//! carries the whole `i32`.
//! So the headline number leaves through the exit code: a non-negative code is
//! a measured p99 in microseconds; a negative one is a [`Refusal`] and no
//! number in that run means anything.
//! `userland/metalprobe` spells the same contract for the device suite, and the
//! sign is what separates the two halves of it there as here. Every percentile
//! is on stdout as well, for the host that has a console to read it on.
//!
//! **Beside the distribution, when its worst wake was and each CPU's SMI count
//! either side of the run**, so a reader can say whether that wake waited out
//! the firmware: an SMI stops every CPU at once, and its count moves on all of
//! them alike.

use std::process::exit;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
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

const BUCKETS: usize = 4096;

/// Why this run measured nothing, as the exit code carries it. Negative,
/// because a non-negative code is a microsecond figure and a run that failed
/// must not be readable as a fast one.
#[derive(Clone, Copy)]
#[repr(i32)]
enum Refusal {
    NoCapability = -1,
    BandRefused = -2,
    /// The p99 is past the histogram's last bucket: a floor, not a measurement.
    PastTheHistogram = -3,
    CountersRefused = -4,
}

fn refuse(why: Refusal, said: &str) -> ! {
    println!("cyclictest: {said}");
    exit(why as i32);
}

/// When the counters were read, and each CPU's SMI count, `-` where its CPU
/// counts none.
fn smis(cap: &SysCap) -> (u64, String) {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let read = match cap.counters(&mut raw) {
        Ok(read) => read,
        Err(e) => refuse(Refusal::CountersRefused, &format!("the counters read was refused: {e:?}")),
    };
    let at = toyos_abi::clock::nanos_since_boot();
    let counts = raw[..read]
        .iter()
        .map(|r| match Record::decode(r).expect("the kernel wrote a record that decodes").get(Counter::Smi) {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    (at, counts)
}

fn main() {
    // **The band is the privilege and it is asked for by name.** A refusal is
    // loud rather than silently measuring the ordinary band: the two are
    // different machines, and a number that did not say which it came from
    // would be compared against a ceiling taken on the other.
    let cap: Option<SysCap> = Endowments::get().take(SYSCAP_LABEL);
    let Some(cap) = cap else {
        refuse(
            Refusal::NoCapability,
            "no capability was endowed, so there is no real-time band to enter",
        );
    };
    if let Err(e) = cap.enter_rt() {
        refuse(Refusal::BandRefused, &format!("the real-time band was refused: {e:?}"));
    }

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst = 0u64;
    let mut worst_at = 0u64;

    for _ in 0..WARMUP {
        syscall::nanosleep(PERIOD_NS);
    }
    let before = smis(&cap);
    // The origin for the whole run, taken after the warm-up so the warm-up's
    // own drift is in no later target.
    let start = toyos_abi::clock::nanos_since_boot();

    for i in 0..SAMPLES {
        let target = start + (i as u64 + 1) * PERIOD_NS;
        let now = toyos_abi::clock::nanos_since_boot();
        // A target already passed is a wake this loop owes nothing for: the
        // lateness is recorded and the next sleep is skipped rather than
        // negative.
        if let Some(remaining) = target.checked_sub(now).filter(|left| *left > 0) {
            syscall::nanosleep(remaining);
        }
        let woke = toyos_abi::clock::nanos_since_boot();
        let late_us = woke.saturating_sub(target) / 1_000;
        if late_us > worst {
            (worst, worst_at) = (late_us, woke);
        }
        match usize::try_from(late_us).ok().filter(|us| *us < BUCKETS) {
            Some(bucket) => histogram[bucket] += 1,
            None => overflow += 1,
        }
    }

    let after = smis(&cap);
    println!(
        "cyclictest: the worst wake was at {worst_at} ns; smi per cpu {} at {} ns and {} at {} ns",
        before.1, before.0, after.1, after.0
    );

    // `None` where the sample wanted is in the overflow, which has no bucket to
    // name.
    let percentile = |want: usize| -> Option<u64> {
        let mut seen = 0usize;
        for (us, count) in histogram.iter().enumerate() {
            seen += *count as usize;
            if seen >= want {
                return Some(us as u64);
            }
        }
        None
    };
    let shown = |p: Option<u64>| p.map_or_else(|| format!(">{}", BUCKETS - 1), |us| us.to_string());

    let p50 = percentile(SAMPLES / 2);
    let p90 = percentile(SAMPLES * 9 / 10);
    let p99 = percentile(SAMPLES * 99 / 100);
    let p999 = percentile(SAMPLES * 999 / 1000);
    println!(
        "cyclictest: {SAMPLES} wakes at {}us: p50={}us p90={}us p99={}us p99.9={}us \
         max={worst}us, {overflow} past the {BUCKETS}us histogram",
        PERIOD_NS / 1_000,
        shown(p50),
        shown(p90),
        shown(p99),
        shown(p999),
    );
    let Some(p99) = p99 else {
        refuse(Refusal::PastTheHistogram, "the p99 is past the histogram, so it is a floor");
    };

    exit(p99 as i32);
}
