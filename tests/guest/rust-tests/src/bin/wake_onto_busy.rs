//! How long a ping-pong round trip takes when the partner parks on a CPU that a
//! spinner of the same program holds, beside CPUs with nothing to run — so the
//! partner's wake lands on a busy CPU while an idle one could serve it.
//!
//! **The arrangement is staged and checked before anything is timed**, because
//! spawn placement alone decides nothing a row can rest on. Each staged thread
//! reads the CPU it first runs on and is kept only where it was wanted, or let
//! go and started again: the partner on a CPU other than the waker's, then a
//! spinner on the partner's. A kernel that leaves the partner's wake on the
//! busy CPU waits out the spinner's quantum every round; one that hands it to
//! an idle CPU pays that once.
//!
//! Two threads hand a turn back and forth through `park`/`unpark`, each round
//! two wakes, timed in this program on the kernel's clock and histogrammed at
//! 1 µs.
//!
//! **Boundary contract: this program's exit code is its p99, in microseconds**,
//! as `cyclictest`'s is and for its reason — on the T14 the kernel's
//! `exit:` record is the one word about a program that survives on its own. A
//! non-negative code is the measured p99; a negative one is a [`Refusal`] and
//! no number in that run means anything. Every percentile is on stdout too.

use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use toyos_abi::clock::nanos_since_boot;
use toyos_abi::syscall;

#[path = "../arch/cpu.rs"]
mod cpu;

/// Rounds measured, and the span that ends the run first if the machine is
/// slow enough to need it — a wake that waits out a quantum is ten
/// milliseconds a round.
const ROUNDS: usize = 20_000;
const SPAN_NS: u64 = 2_000_000_000;

/// Discarded before the histogram opens: the first rounds pay for the
/// threads' own start.
const WARMUP: usize = 20;

const BUCKETS: usize = 32_768;

/// How many threads staging may start, and how long it may take, before the
/// run is refused: a candidate lands where spawn placement puts it, which
/// rotates over the CPUs, so a machine of `n` needs about `n` of them.
const TRIES: usize = 64;
const STAGE_NS: u64 = 5_000_000_000;

#[derive(Clone, Copy)]
#[repr(i32)]
enum Refusal {
    /// The p99 is past the histogram's last bucket: a floor, not a measurement.
    PastTheHistogram = -3,
    /// Too few rounds for a 99th percentile to be one.
    TooFewRounds = -4,
    /// The arrangement this row times was not reached, or did not hold.
    NotStaged = -5,
}

fn refuse(why: Refusal, said: &str) -> ! {
    println!("wake_onto_busy: {said}");
    exit(why as i32);
}

const UNSEEN: u32 = u32::MAX;
const UNDECIDED: u8 = 0;
const KEPT: u8 = 1;
const LET_GO: u8 = 2;

/// A thread started to be placed: the CPU it first ran on, and whether it
/// stays.
struct Seat {
    on: AtomicU32,
    verdict: AtomicU8,
}

/// Start threads running `body` until one first runs on a CPU `wanted`
/// accepts, letting every other go; answer it and that CPU. Each candidate
/// says where it is and parks until it is judged, so the CPU it is kept on is
/// the one it parked on.
fn place(
    waker: &Thread,
    wanted: impl Fn(u32) -> bool,
    body: impl FnOnce() + Clone + Send + 'static,
) -> (JoinHandle<()>, u32) {
    let deadline = nanos_since_boot() + STAGE_NS;
    for _ in 0..TRIES {
        let seat = Arc::new(Seat { on: AtomicU32::new(UNSEEN), verdict: AtomicU8::new(UNDECIDED) });
        let candidate = {
            let (seat, waker, body) = (seat.clone(), waker.clone(), body.clone());
            thread::spawn(move || {
                seat.on.store(cpu::apic_id(), Ordering::Release);
                waker.unpark();
                loop {
                    match seat.verdict.load(Ordering::Acquire) {
                        UNDECIDED => thread::park(),
                        KEPT => break,
                        _ => return,
                    }
                }
                body();
            })
        };
        let on = loop {
            let on = seat.on.load(Ordering::Acquire);
            if on != UNSEEN {
                break on;
            }
            let now = nanos_since_boot();
            if now >= deadline {
                refuse(Refusal::NotStaged, "a candidate never said which CPU it ran on");
            }
            thread::park_timeout(Duration::from_nanos(deadline - now));
        };
        let keep = wanted(on);
        seat.verdict.store(if keep { KEPT } else { LET_GO }, Ordering::Release);
        candidate.thread().unpark();
        if keep {
            return (candidate, on);
        }
        candidate.join().expect("a candidate let go panicked");
    }
    refuse(Refusal::NotStaged, &format!("{TRIES} threads started and none where it was wanted"));
}

fn main() {
    let cpus = syscall::cpu_count() as usize;
    if cpus < 2 {
        refuse(Refusal::NotStaged, "one CPU has no idle CPU beside it");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let turn = Arc::new(AtomicU32::new(0));
    let main = thread::current();

    let (partner, partner_on) = place(&main, |on| on != cpu::apic_id(), {
        let (stop, turn, main) = (stop.clone(), turn.clone(), main.clone());
        move || loop {
            while turn.load(Ordering::Acquire) != 1 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
            turn.store(0, Ordering::Release);
            main.unpark();
        }
    });
    let spinner_at = Arc::new(AtomicU32::new(UNSEEN));
    let (spinner, _) = place(&main, |on| on == partner_on, {
        let (stop, at) = (stop.clone(), spinner_at.clone());
        move || {
            let mut x = 1u64;
            loop {
                at.store(cpu::apic_id(), Ordering::Relaxed);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                for _ in 0..1000 {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            }
        }
    });
    // The arrangement is read once the kept spinner has said where it spins.
    let deadline = nanos_since_boot() + STAGE_NS;
    while spinner_at.load(Ordering::Relaxed) == UNSEEN {
        if nanos_since_boot() >= deadline {
            refuse(Refusal::NotStaged, "the kept spinner never said where it spins");
        }
        std::hint::spin_loop();
    }
    let (waker_on, at) = (cpu::apic_id(), spinner_at.load(Ordering::Relaxed));
    if waker_on == partner_on || at != partner_on {
        refuse(
            Refusal::NotStaged,
            &format!(
                "the waker is on CPU {waker_on}, the partner parked on {partner_on} and the \
                 spinner on {at}"
            ),
        );
    }

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
    // The spinner is still where it was staged.
    let held = spinner_at.load(Ordering::Relaxed);
    stop.store(true, Ordering::Release);
    partner.thread().unpark();
    partner.join().expect("the partner panicked");
    spinner.join().expect("the spinner panicked");
    if held != partner_on {
        refuse(Refusal::NotStaged, &format!("the spinner left CPU {partner_on} for {held}"));
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
        "wake_onto_busy: {rounds} rounds on {cpus} cpus, waker on CPU {waker_on}, partner parked \
         beside a spinner on CPU {partner_on}: p50={}us p90={}us p99={}us max={}us, {overflow} past \
         the {BUCKETS}us histogram",
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
