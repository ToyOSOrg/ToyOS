//! How long a ping-pong round trip takes when every CPU but one runs a thread
//! that never blocks — so a wake lands on a busy CPU and is served by the CPU
//! the waker leaves idle.
//!
//! Two threads hand a turn back and forth through `park`/`unpark`, each round
//! two wakes, beside `cpus − 1` spinners of the same process. The round trip is
//! timed in this program, on the kernel's clock, and histogrammed at 1 µs.
//!
//! **Boundary contract: this program's exit code is its p99, in microseconds**,
//! as `cyclictest`'s is and for its reason — on the T14 the kernel's
//! `exit:` record is the one word about a program that survives on its own. A
//! non-negative code is the measured p99; a negative one is a [`Refusal`] and
//! no number in that run means anything. Every percentile is on stdout too.

use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use toyos_abi::clock::nanos_since_boot;
use toyos_abi::syscall;

/// Rounds measured, and the span that ends the run first if the machine is
/// slow enough to need it — a wake that waits out a quantum is twenty
/// milliseconds a round.
const ROUNDS: usize = 20_000;
const SPAN_NS: u64 = 2_000_000_000;

/// Discarded before the histogram opens: the first rounds pay for the
/// threads' own start.
const WARMUP: usize = 20;

const BUCKETS: usize = 32_768;

#[derive(Clone, Copy)]
#[repr(i32)]
enum Refusal {
    /// The p99 is past the histogram's last bucket: a floor, not a measurement.
    PastTheHistogram = -3,
    /// Too few rounds for a 99th percentile to be one.
    TooFewRounds = -4,
}

fn refuse(why: Refusal, said: &str) -> ! {
    println!("wake_onto_busy: {said}");
    exit(why as i32);
}

fn main() {
    let cpus = syscall::cpu_count() as usize;
    let stop = Arc::new(AtomicBool::new(false));
    let turn = Arc::new(AtomicU32::new(0));
    let spinners: Vec<_> = (0..cpus.saturating_sub(1))
        .map(|_| {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..1000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })
        })
        .collect();
    let main = thread::current();
    let partner = {
        let (stop, turn) = (stop.clone(), turn.clone());
        thread::spawn(move || loop {
            while turn.load(Ordering::Acquire) != 1 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
            turn.store(0, Ordering::Release);
            main.unpark();
        })
    };
    // Every spinner is up before anything is timed.
    thread::sleep(Duration::from_millis(50));

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst_ns = 0u64;
    let mut rounds = 0usize;
    let begun = nanos_since_boot();
    for round in 0..WARMUP + ROUNDS {
        let sent = nanos_since_boot();
        turn.store(1, Ordering::Release);
        partner.thread().unpark();
        while turn.load(Ordering::Acquire) != 0 {
            thread::park();
        }
        let took = nanos_since_boot() - sent;
        if round < WARMUP {
            continue;
        }
        rounds += 1;
        worst_ns = worst_ns.max(took);
        match usize::try_from(took / 1_000).ok().filter(|us| *us < BUCKETS) {
            Some(us) => histogram[us] += 1,
            None => overflow += 1,
        }
        if nanos_since_boot() - begun > SPAN_NS {
            break;
        }
    }
    stop.store(true, Ordering::Release);
    partner.thread().unpark();
    partner.join().expect("the partner panicked");
    for spinner in spinners {
        spinner.join().expect("a spinner panicked");
    }

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
    let p99 = percentile(rounds * 99 / 100);
    println!(
        "wake_onto_busy: {rounds} rounds beside {} spinners on {cpus} cpus: p50={}us p90={}us \
         p99={}us max={}us, {overflow} past the {BUCKETS}us histogram",
        cpus.saturating_sub(1),
        shown(percentile(rounds / 2)),
        shown(percentile(rounds * 9 / 10)),
        shown(p99),
        worst_ns / 1_000,
    );
    if rounds < 100 {
        refuse(Refusal::TooFewRounds, &format!("{rounds} rounds have no 99th percentile"));
    }
    let Some(p99) = p99 else {
        refuse(Refusal::PastTheHistogram, "the p99 is past the histogram, so it is a floor");
    };
    exit(p99 as i32);
}
