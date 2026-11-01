//! The clock a CPU is running at, read the same way on both systems:
//! `arch::add_chain`'s adds wait a cycle each, so cycles are the number of
//! adds. A probe times a short chain and a long one and takes the
//! difference, so the cost of reading the time, which both timings carry, is
//! no part of the clock it reads: 1.2 µs a read on a Linux timing through the
//! HPET, a twentieth of a 100,000-cycle chain.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use osbench_rules::settle::{probe_mhz, Plateau};

use crate::report::result;
use crate::ns_since;

/// What the last [`settle`] reached, for the next result line to print.
static SETTLED: Mutex<Option<Settled>> = Mutex::new(None);

/// The clock a settle reached, and whether it held: `capped` is a settle
/// that ran out its [`SETTLE_LIMIT`] and took the reading it had.
#[derive(Clone, Copy)]
pub struct Settled {
    pub mhz: u64,
    pub capped: bool,
}

/// Turns in a probe's short chain, 100,000 cycles; its long one is
/// [`LONG`] times that, so the difference is 300,000 cycles: 70 µs at the
/// AMD bench's 4.24 GHz, 230 µs at 1.3 GHz.
const TURNS: u64 = 1_000;
const LONG: u64 = 4;
const ADDS: u64 = crate::arch::ADDS_PER_TURN;
/// The cycles a probe's difference spans.
const CYCLES: u64 = ADDS * TURNS * (LONG - 1);

fn chain(turns: u64) -> u64 {
    let t = Instant::now();
    crate::arch::add_chain(turns);
    ns_since(t)
}

/// One probe for `cpu.clock`'s distribution, in MHz: `None` where the short
/// chain was preempted past the long one.
fn burst() -> Option<u64> {
    let short = chain(TURNS);
    probe_mhz(CYCLES, short, chain(TURNS * LONG))
}

/// The clock the calling CPU runs at now, in MHz: the fastest of five of
/// each chain, taken in turn, so an interrupt inside one reads as neither a
/// slow clock nor, in the short chain, a fast one. A clock that rose so far
/// between the two that the fastest long chain took no longer than the
/// fastest short one is read again.
pub fn mhz() -> u64 {
    loop {
        let (mut short, mut long) = (u64::MAX, u64::MAX);
        for _ in 0..5 {
            short = short.min(chain(TURNS));
            long = long.min(chain(TURNS * LONG));
        }
        if let Some(mhz) = probe_mhz(CYCLES, short, long) {
            return mhz;
        }
    }
}

/// The longest a settle spins before it takes the clock it has.
const SETTLE_LIMIT: Duration = Duration::from_millis(500);

/// A steady-state warm-up of the calling CPU, for a benchmark to run after:
/// [`mhz`] back to back until `osbench_rules::settle` calls the clock
/// settled, or for [`SETTLE_LIMIT`]. The clock it reached is printed beside
/// the next result as `mhz_settled`, and `mhz` beside it is the clock just
/// after.
pub fn settle() {
    let start = Instant::now();
    let at = || u64::try_from(start.elapsed().as_nanos()).expect("a settle under 584 years");
    let mut plateau = Plateau::new(at(), mhz());
    loop {
        let reading = mhz();
        let held = plateau.settled_with(at(), reading);
        if held || start.elapsed() >= SETTLE_LIMIT {
            *SETTLED.lock().expect("no settle panics holding it") = Some(Settled { mhz: reading, capped: !held });
            return;
        }
    }
}

/// The clock the last [`settle`] reached, once: `None` when none has run
/// since.
pub fn take_settled() -> Option<Settled> {
    SETTLED.lock().expect("no settle panics holding it").take()
}

/// `cpu.clock`: 200 probes back to back after one, MHz. The first probes of a
/// CPU leaving idle show its ramp; the rest, the clock it settles at.
pub fn distribution() {
    burst();
    let mut mhz: Vec<u64> = (0..200).filter_map(|_| burst()).collect();
    result("cpu.clock", "MHz", &mut mhz, &[("cycles_per_probe", CYCLES.to_string())]);
}
