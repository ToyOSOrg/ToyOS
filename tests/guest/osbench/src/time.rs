//! Time: what reading the clock costs, how close a sleep lands to what was
//! asked, how late a 1 ms timer fires, and what yielding costs.

use std::thread;
use std::time::{Duration, Instant};

use crate::report::result;
use crate::ns_since;

pub fn suite() {
    clock_read();
    sleeps("");
    // A timer at 1 kHz: the periodic wake at a 1 ms period.
    crate::wake::periodic("time.timer_1ms.late", Duration::from_millis(1));
    #[cfg(target_os = "linux")]
    {
        // As the periodic wake does: an ordinary Linux sleep is padded by up
        // to the thread's timer slack, 50 µs by default. ToyOS has none.
        let slack = crate::sys::timer_slack_ns();
        crate::sys::set_timer_slack_ns(1);
        sleeps(".slack1ns");
        crate::wake::periodic("time.timer_1ms.late.slack1ns", Duration::from_millis(1));
        crate::sys::set_timer_slack_ns(slack);
    }
    yields();
}

/// `Instant::now`, which Linux answers from its vDSO and ToyOS from the clock
/// page, both without entering the kernel: 15 rounds of 1,000,000 calls after
/// one, picoseconds per call.
fn clock_read() {
    const CALLS: u64 = 1_000_000;
    let round = || {
        let t = Instant::now();
        for _ in 0..CALLS {
            std::hint::black_box(Instant::now());
        }
        ns_since(t) * 1000 / CALLS
    };
    round();
    crate::clock::settle();
    let mut ps: Vec<u64> = (0..15).map(|_| round()).collect();
    result("time.now", "ps", &mut ps, &[("calls_per_sample", CALLS.to_string())]);
}

/// `thread::sleep` of each length, and how much later than asked it
/// returned, ns: 1,000 each of 1, 10 and 100 µs, 300 of 1 ms and 100 of
/// 10 ms, after 20 of each.
fn sleeps(suffix: &str) {
    for (name, asked, samples) in [
        ("1us", Duration::from_micros(1), 1000),
        ("10us", Duration::from_micros(10), 1000),
        ("100us", Duration::from_micros(100), 1000),
        ("1ms", Duration::from_millis(1), 300),
        ("10ms", Duration::from_millis(10), 100),
    ] {
        let once = || {
            let t = Instant::now();
            thread::sleep(asked);
            ns_since(t).saturating_sub(asked.as_nanos() as u64)
        };
        for _ in 0..20 {
            once();
        }
        let mut late: Vec<u64> = (0..samples).map(|_| once()).collect();
        result(&format!("time.sleep_late.{name}{suffix}"), "ns", &mut late, &[("asked_ns", asked.as_nanos().to_string())]);
    }
}

/// `thread::yield_now` with nothing else to run, `sched_yield` on Linux: 15
/// rounds of 100,000 calls after one, picoseconds per call. ToyOS has no
/// yield to measure: its std's `yield_now` is a spin-loop hint
/// (`sdk/std/sys/thread.rs`).
fn yields() {
    if cfg!(target_os = "toyos") {
        return crate::report::unsupported("time.yield", "no yield syscall: std's yield_now is a spin-loop hint");
    }
    const CALLS: u64 = 100_000;
    let round = || {
        let t = Instant::now();
        for _ in 0..CALLS {
            thread::yield_now();
        }
        ns_since(t) * 1000 / CALLS
    };
    round();
    crate::clock::settle();
    let mut ps: Vec<u64> = (0..15).map(|_| round()).collect();
    result("time.yield", "ps", &mut ps, &[("calls_per_sample", CALLS.to_string())]);
}
