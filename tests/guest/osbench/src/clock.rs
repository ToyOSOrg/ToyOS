//! The clock a CPU is running at, read the same way on both systems:
//! `arch::add_chain`'s adds wait a cycle each, so cycles are the number of
//! adds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::report::result;
use crate::ns_since;

/// What the last [`settle`] settled at, for the next result line to print.
static SETTLED: AtomicU64 = AtomicU64::new(0);

/// Turns per probe: 100,000 cycles, 25 to 50 µs on either bench.
const TURNS: u64 = 1_000;
const ADDS: u64 = crate::arch::ADDS_PER_TURN;

fn burst() -> u64 {
    let t = Instant::now();
    crate::arch::add_chain(TURNS);
    ns_since(t).max(1)
}

/// The clock the calling CPU runs at now, in MHz: the fastest of five probes,
/// so an interrupt inside one does not read as a slow clock.
pub fn mhz() -> u64 {
    let ns = (0..5).map(|_| burst()).min().expect("five probes");
    ADDS * TURNS * 1000 / ns
}

/// A steady-state warm-up of the calling CPU, for a benchmark to run after:
/// probes back to back for at least 10 ms and until three in a row agree
/// within 1%, or for 200 ms. The clock it reached is printed beside the next
/// result as `mhz_settled`, and `mhz` beside it is the clock just after.
pub fn settle() {
    let start = Instant::now();
    let mut last = [0u64; 3];
    loop {
        last.rotate_left(1);
        last[2] = ADDS * TURNS * 1000 / burst();
        let (lo, hi) = (last.iter().min().copied().unwrap_or(0), last.iter().max().copied().unwrap_or(0));
        let steady = lo > 0 && (hi - lo) * 100 <= hi;
        let spent = start.elapsed();
        if (steady && spent >= Duration::from_millis(10)) || spent >= Duration::from_millis(200) {
            SETTLED.store(last[2], Ordering::Relaxed);
            return;
        }
    }
}

/// The clock the last [`settle`] reached, once: zero when none has run since.
pub fn take_settled() -> u64 {
    SETTLED.swap(0, Ordering::Relaxed)
}

/// `cpu.clock`: 200 probes back to back after one, MHz. The first probes of a
/// CPU leaving idle show its ramp; the rest, the clock it settles at.
pub fn distribution() {
    burst();
    let mut mhz: Vec<u64> = (0..200).map(|_| ADDS * TURNS * 1000 / burst()).collect();
    result("cpu.clock", "MHz", &mut mhz, &[("adds_per_probe", (ADDS * TURNS).to_string())]);
}
