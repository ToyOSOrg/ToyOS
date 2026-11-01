//! How long a wake takes to run when it lands on a CPU a spinner holds, beside
//! CPUs with nothing to run.
//!
//! **Every timed wake lands on the busy CPU**, because a woken thread that is
//! handed to an idle CPU parks there afterwards and its next wake finds that
//! CPU idle. So each round wakes a partner it has never woken: all of them are
//! pinned to one CPU, other than the waker's and the one device interrupts
//! target, and park there beside a spinner pinned there too; each is unpinned
//! just before its wake, says where it ran, and parks again where it is. A
//! kernel that leaves the wake on the busy CPU until that CPU's own quantum
//! ends waits up to a quantum every round; one that drains it at once and
//! hands it to an idle CPU waits an interrupt or two. The waker is pinned and
//! spins rather than parks, so its own CPU is never idle for a wake to be
//! handed to and it is never woken itself.
//!
//! **Boundary contract: this program's exit code is its p99, in microseconds**,
//! as `cyclictest`'s is and for its reason — on the T14 the kernel's
//! `exit:` record is the one word about a program that survives on its own. A
//! non-negative code is the measured p99; a negative one is a [`Refusal`] and
//! no number in that run means anything. Every percentile is on stdout too.

use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use toyos_abi::clock::nanos_since_boot;
use toyos_abi::syscall::{self, CPU_MASK_WORDS};

/// Rounds timed, one partner each, and the span that ends the run first if
/// the machine is slow enough to need it — a wake left behind the spinner
/// waits up to a quantum, ten milliseconds.
const ROUNDS: usize = 200;
const SPAN_NS: u64 = 5_000_000_000;

const BUCKETS: usize = 32_768;

/// How long a thread may take to say it has started or parked.
const START_NS: u64 = 5_000_000_000;

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

/// The CPU every device interrupt targets, the kernel's cpu0 (`MSG_DEST` in
/// `kernel/src/arch/x86_64/apic.rs`). The partners are never pinned there: a
/// pass run for an interrupt would drain their wakes early on any kernel.
const DEVICE_IRQ_CPU: u32 = 0;

const UNSEEN: u32 = u32::MAX;
const PARKED: u8 = 0;
const RUN: u8 = 1;
const DONE: u8 = 2;
const STOP: u8 = 3;

fn mask_of(cpus: impl IntoIterator<Item = u32>) -> [u64; CPU_MASK_WORDS] {
    let mut mask = [0u64; CPU_MASK_WORDS];
    for cpu in cpus {
        mask[cpu as usize / 64] |= 1 << (cpu % 64);
    }
    mask
}

fn pin(tid: u64, cpu: u32) {
    syscall::thread_set_affinity(tid, &mask_of([cpu])).unwrap_or_else(|e| panic!("pin {tid} to {cpu}: {e:?}"));
}

/// A partner pinned to the busy CPU: its thread id, the CPU it ran on when it
/// was woken to run, and how far it has got.
struct Seat {
    tid: AtomicU32,
    ran_on: AtomicU32,
    phase: AtomicU8,
}

/// Wait, spinning, for `done`, or say what never happened.
fn spin_for(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let deadline = nanos_since_boot() + START_NS;
    while !done() {
        if nanos_since_boot() >= deadline {
            return Err(format!("{what} never happened"));
        }
        std::hint::spin_loop();
    }
    Ok(())
}

/// Park until the phase is one of `until`, and answer it.
fn park_until(seat: &Seat, until: &[u8]) -> u8 {
    loop {
        let phase = seat.phase.load(Ordering::Acquire);
        if until.contains(&phase) {
            return phase;
        }
        thread::park();
    }
}

/// A partner's whole life: pin itself to `busy`, say so, park there until its
/// round, then say where it ran and park there until the run ends.
fn partner(busy: u32) -> (JoinHandle<()>, Arc<Seat>) {
    let seat = Arc::new(Seat { tid: AtomicU32::new(UNSEEN), ran_on: AtomicU32::new(UNSEEN), phase: AtomicU8::new(PARKED) });
    let thread = {
        let seat = seat.clone();
        thread::spawn(move || {
            pin(0, busy);
            assert_eq!(syscall::current_cpu(), busy, "a partner pinned to CPU {busy}");
            seat.tid.store(toyos_abi::current_tid().0, Ordering::Release);
            if park_until(&seat, &[RUN, STOP]) == STOP {
                return;
            }
            seat.ran_on.store(syscall::current_cpu(), Ordering::Relaxed);
            seat.phase.store(DONE, Ordering::Release);
            park_until(&seat, &[STOP]);
        })
    };
    (thread, seat)
}

/// A thread spinning pinned to `busy` until stopped.
struct Spinner {
    thread: JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl Spinner {
    fn on(busy: u32) -> Result<Spinner, String> {
        let (stop, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let thread = {
            let (stop, started) = (stop.clone(), started.clone());
            thread::spawn(move || {
                pin(0, busy);
                assert_eq!(syscall::current_cpu(), busy, "a spinner pinned to CPU {busy}");
                started.store(true, Ordering::Release);
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..1000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })
        };
        let spinner = Spinner { thread, stop };
        match spin_for("the spinner's start", || started.load(Ordering::Acquire)) {
            Ok(()) => Ok(spinner),
            Err(why) => {
                spinner.end();
                Err(why)
            }
        }
    }

    fn end(self) {
        self.stop.store(true, Ordering::Release);
        self.thread.join().expect("a spinner panicked");
    }
}

/// End every partner, woken or not.
fn end_partners(partners: Vec<(JoinHandle<()>, Arc<Seat>)>) {
    for (partner, seat) in partners {
        seat.phase.store(STOP, Ordering::Release);
        partner.thread().unpark();
        partner.join().expect("a partner panicked");
    }
}

fn main() {
    let cpus = syscall::cpu_count();
    if cpus < 3 {
        refuse(Refusal::NotStaged, "the waker, the busy CPU and the interrupts' CPU need three");
    }
    let waker_on = (0..cpus).find(|&cpu| cpu != DEVICE_IRQ_CPU).expect("three CPUs");
    let busy = (0..cpus).find(|&cpu| cpu != DEVICE_IRQ_CPU && cpu != waker_on).expect("three CPUs");
    pin(0, waker_on);
    let everywhere = mask_of(0..cpus);
    let partners: Vec<_> = (0..ROUNDS).map(|_| partner(busy)).collect();
    for (_, seat) in &partners {
        if let Err(why) = spin_for("a partner's pin", || seat.tid.load(Ordering::Acquire) != UNSEEN) {
            end_partners(partners);
            refuse(Refusal::NotStaged, &why);
        }
    }
    let spinner = match Spinner::on(busy) {
        Ok(spinner) => spinner,
        Err(why) => {
            end_partners(partners);
            refuse(Refusal::NotStaged, &why);
        }
    };

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst_ns = 0u64;
    let mut rounds = 0usize;
    let mut off_busy = 0usize;
    let begun = nanos_since_boot();
    for (partner, seat) in &partners {
        syscall::thread_set_affinity(u64::from(seat.tid.load(Ordering::Acquire)), &everywhere)
            .expect("a partner's own process unpins it");
        let sent = nanos_since_boot();
        seat.phase.store(RUN, Ordering::Release);
        partner.thread().unpark();
        while seat.phase.load(Ordering::Acquire) != DONE {
            std::hint::spin_loop();
        }
        let took = nanos_since_boot() - sent;
        rounds += 1;
        off_busy += usize::from(seat.ran_on.load(Ordering::Relaxed) != busy);
        worst_ns = worst_ns.max(took);
        match usize::try_from(took / 1_000).ok().filter(|us| *us < BUCKETS) {
            Some(us) => histogram[us] += 1,
            None => overflow += 1,
        }
        if nanos_since_boot() - begun > SPAN_NS {
            break;
        }
    }
    let now_on = syscall::current_cpu();
    spinner.end();
    end_partners(partners);
    assert_eq!(now_on, waker_on, "the waker pinned to CPU {waker_on} ran on {now_on}");

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
        "wake_onto_busy: {rounds} wakes on {cpus} cpus onto CPU {busy} beside its spinner, waker on \
         CPU {waker_on}, {off_busy} run off CPU {busy}: p50={}us p90={}us p99={}us max={}us, {overflow} \
         past the {BUCKETS}us histogram",
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
