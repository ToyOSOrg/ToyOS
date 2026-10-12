//! How long a ping-pong round trip takes when the partner parks on a CPU that a
//! spinner of the same program holds, beside CPUs with nothing to run — so the
//! partner's wake lands on a busy CPU while an idle one could serve it.
//!
//! **The arrangement is staged and checked before anything is timed**, because
//! spawn placement alone decides nothing a row can rest on. Each staged thread
//! reads the CPU it first runs on and is kept only where it was wanted, or let
//! go and started again: the partner on a CPU other than the waker's and the
//! one device interrupts target, then a spinner on the partner's. A kernel
//! that leaves the partner's wake on the busy CPU waits out the spinner's
//! quantum every round; one that hands it to an idle CPU pays that once.
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

/// How many threads one placement may start, and how long it may take: a
/// candidate lands where spawn placement puts it, which rotates over the CPUs,
/// so a machine of `n` needs about `n` of them. And how many times the whole
/// arrangement is staged again before the run is refused: a thread can be
/// moved while staging is still under way.
const TRIES: usize = 64;
const STAGE_NS: u64 = 5_000_000_000;
const ATTEMPTS: usize = 4;

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

/// The CPU every device interrupt targets (`MSG_DEST` in
/// `kernel/src/arch/x86_64/apic.rs`). The partner is never staged there: a
/// pass run for an interrupt's wake would serve its wake early on any kernel.
const DEVICE_IRQ_CPU: u32 = 0;

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

/// Wait, spinning, for `seen` to answer something, or say why not.
fn spin_for(deadline: u64, seen: impl Fn() -> Option<u32>, what: &str) -> Result<u32, String> {
    loop {
        if let Some(on) = seen() {
            return Ok(on);
        }
        if nanos_since_boot() >= deadline {
            return Err(format!("{what} never said which CPU it ran on"));
        }
        std::hint::spin_loop();
    }
}

/// Start threads running `body` until one first runs on a CPU `wanted`
/// accepts, letting every other go; answer it and that CPU. Each candidate
/// says where it is and parks until it is judged, so the CPU it is kept on is
/// the one it parked on.
///
/// **The caller spins while it waits rather than parking**, so its own CPU is
/// never idle: a kernel that hands work to idle CPUs would otherwise hand it
/// a candidate, or hand the caller, woken, to another CPU.
fn place(
    wanted: impl Fn(u32) -> bool,
    body: impl FnOnce() + Clone + Send + 'static,
) -> Result<(JoinHandle<()>, u32), String> {
    let deadline = nanos_since_boot() + STAGE_NS;
    for _ in 0..TRIES {
        let seat = Arc::new(Seat { on: AtomicU32::new(UNSEEN), verdict: AtomicU8::new(UNDECIDED) });
        let candidate = {
            let (seat, body) = (seat.clone(), body.clone());
            thread::spawn(move || {
                seat.on.store(cpu::apic_id(), Ordering::Release);
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
        let seen = || Some(seat.on.load(Ordering::Acquire)).filter(|on| *on != UNSEEN);
        let on = spin_for(deadline, seen, "a candidate")?;
        let keep = wanted(on);
        seat.verdict.store(if keep { KEPT } else { LET_GO }, Ordering::Release);
        candidate.thread().unpark();
        if keep {
            return Ok((candidate, on));
        }
        candidate.join().expect("a candidate let go panicked");
    }
    Err(format!("{TRIES} threads started and none where it was wanted"))
}

/// The arrangement a run times: the partner parked on `partner_on`, and a
/// spinner there that says where it spins in `spinner_at`.
struct Staged {
    stop: Arc<AtomicBool>,
    turn: Arc<AtomicU32>,
    partner: JoinHandle<()>,
    partner_on: u32,
    spinner: Option<JoinHandle<()>>,
    spinner_at: Arc<AtomicU32>,
}

impl Staged {
    /// Stop both threads, and answer where the spinner last said it was.
    fn end(self) -> u32 {
        self.stop.store(true, Ordering::Release);
        self.partner.thread().unpark();
        self.partner.join().expect("the partner panicked");
        if let Some(spinner) = self.spinner {
            spinner.join().expect("the spinner panicked");
        }
        self.spinner_at.load(Ordering::Relaxed)
    }
}

/// Stage the partner on a CPU other than this thread's and the interrupts',
/// then a spinner on the partner's, and check that this thread is still
/// elsewhere and the spinner still there.
fn stage(waker: &Thread) -> Result<Staged, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let turn = Arc::new(AtomicU32::new(0));
    let (partner, partner_on) = place(|on| on != cpu::apic_id() && on != DEVICE_IRQ_CPU, {
        let (stop, turn, waker) = (stop.clone(), turn.clone(), waker.clone());
        move || loop {
            while turn.load(Ordering::Acquire) != 1 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
            turn.store(0, Ordering::Release);
            waker.unpark();
        }
    })?;
    let spinner_at = Arc::new(AtomicU32::new(UNSEEN));
    let mut staged = Staged { stop, turn, partner, partner_on, spinner: None, spinner_at };
    let placed = place(|on| on == partner_on && cpu::apic_id() != partner_on, {
        let (stop, at) = (staged.stop.clone(), staged.spinner_at.clone());
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
    match placed {
        Ok((spinner, _)) => staged.spinner = Some(spinner),
        Err(why) => {
            staged.end();
            return Err(why);
        }
    }
    let seen = || Some(staged.spinner_at.load(Ordering::Relaxed)).filter(|at| *at != UNSEEN);
    let at = match spin_for(nanos_since_boot() + STAGE_NS, seen, "the kept spinner") {
        Ok(at) => at,
        Err(why) => {
            staged.end();
            return Err(why);
        }
    };
    let waker_on = cpu::apic_id();
    if waker_on == partner_on || at != partner_on {
        staged.end();
        return Err(format!(
            "the waker is on CPU {waker_on}, the partner parked on {partner_on} and the spinner on {at}"
        ));
    }
    Ok(staged)
}

fn main() {
    let cpus = syscall::cpu_count() as usize;
    if cpus < 3 {
        refuse(Refusal::NotStaged, "the waker, the partner and the interrupts' CPU need three");
    }
    let main = thread::current();
    let mut why = String::new();
    let Some(staged) = (0..ATTEMPTS).find_map(|_| stage(&main).map_err(|said| why = said).ok()) else {
        refuse(Refusal::NotStaged, &format!("not staged in {ATTEMPTS} attempts, the last: {why}"));
    };
    let (waker_on, partner_on) = (cpu::apic_id(), staged.partner_on);
    let (turn, partner) = (staged.turn.clone(), staged.partner.thread().clone());

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst_ns = 0u64;
    let mut rounds = 0usize;
    let begun = nanos_since_boot();
    for round in 0..WARMUP + ROUNDS {
        let sent = nanos_since_boot();
        turn.store(1, Ordering::Release);
        partner.unpark();
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
    let held = staged.end();
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
