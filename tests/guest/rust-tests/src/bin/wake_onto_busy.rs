//! How long a wake takes to run when it lands on a CPU a spinner holds, beside
//! CPUs with nothing to run.
//!
//! **Every timed wake lands on the busy CPU**, because a woken thread that is
//! handed to an idle CPU parks there afterwards and its next wake finds that
//! CPU idle. So each round wakes a partner it has never woken: all of them were
//! parked on one CPU before a spinner was kept there, and each, once woken,
//! says where it ran and parks again where it is, which leaves that CPU idle
//! sooner than an exit would. A kernel that leaves the wake on the busy CPU
//! until that CPU's own quantum ends waits up to a quantum every round; one
//! that drains it at once and hands it to an idle CPU waits an interrupt or
//! two.
//!
//! **The arrangement is staged and checked before anything is timed**, because
//! spawn placement alone decides nothing a row can rest on. Each staged thread
//! reads the CPU it first runs on and is kept only where it was wanted, or let
//! go and started again: the partners on one CPU other than the waker's and
//! the one device interrupts target, then a spinner there, and again whenever
//! the kernel moves the spinner to an idle CPU during the run. The waker spins
//! rather than parks from the first placement to the last round, so its own
//! CPU is never idle for a wake to be handed to and it is never woken itself.
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
use toyos_abi::syscall;

#[path = "../arch/cpu.rs"]
mod cpu;

/// Rounds timed, one partner each, and the span that ends the run first if
/// the machine is slow enough to need it — a wake left behind the spinner
/// waits up to a quantum, ten milliseconds.
const ROUNDS: usize = 200;
const SPAN_NS: u64 = 5_000_000_000;

const BUCKETS: usize = 32_768;

/// How many threads one placement may start, and how long it may take: a
/// candidate lands where spawn placement puts it, which rotates over the CPUs,
/// so a machine of `n` needs about `n` of them.
const TRIES: usize = 256;
const STAGE_NS: u64 = 5_000_000_000;

/// How many times a run keeps a spinner on the busy CPU again after the
/// kernel moved one off it, before the run is refused.
const RESTAGES: usize = 8;

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
/// `kernel/src/arch/x86_64/apic.rs`). The partners are never staged there: a
/// pass run for an interrupt would drain their wakes early on any kernel.
const DEVICE_IRQ_CPU: u32 = 0;

const UNSEEN: u32 = u32::MAX;
const UNDECIDED: u8 = 0;
const KEPT: u8 = 1;
const LET_GO: u8 = 2;
const RUN: u8 = 3;
const DONE: u8 = 4;
const STOP: u8 = 5;

/// A thread started to be placed: the CPU it first ran on, the CPU it ran on
/// when it was woken to run, and how far it has got.
struct Seat {
    on: AtomicU32,
    ran_on: AtomicU32,
    phase: AtomicU8,
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
/// accepts, letting every other go; answer it, its seat and that CPU. Each
/// candidate says where it is and parks until it is judged, so the CPU it is
/// kept on is the one it parked on.
fn place(
    wanted: impl Fn(u32) -> bool,
    body: impl FnOnce(&Seat) + Clone + Send + 'static,
) -> Result<(JoinHandle<()>, Arc<Seat>, u32), String> {
    let deadline = nanos_since_boot() + STAGE_NS;
    for _ in 0..TRIES {
        let seat = Arc::new(Seat {
            on: AtomicU32::new(UNSEEN),
            ran_on: AtomicU32::new(UNSEEN),
            phase: AtomicU8::new(UNDECIDED),
        });
        let candidate = {
            let (seat, body) = (seat.clone(), body.clone());
            thread::spawn(move || {
                seat.on.store(cpu::x2apic_id(), Ordering::Release);
                loop {
                    match seat.phase.load(Ordering::Acquire) {
                        UNDECIDED => thread::park(),
                        KEPT => break,
                        _ => return,
                    }
                }
                body(&seat);
            })
        };
        let seen = || Some(seat.on.load(Ordering::Acquire)).filter(|on| *on != UNSEEN);
        let on = spin_for(deadline, seen, "a candidate")?;
        let keep = wanted(on);
        seat.phase.store(if keep { KEPT } else { LET_GO }, Ordering::Release);
        candidate.thread().unpark();
        if keep {
            return Ok((candidate, seat, on));
        }
        candidate.join().expect("a candidate let go panicked");
    }
    Err(format!("{TRIES} threads started and none where it was wanted"))
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

/// A partner's whole life once kept: park on the CPU it was kept on until its
/// round, then say where it ran and park there until the run ends.
fn partner(seat: &Seat) {
    if park_until(seat, &[RUN, STOP]) == STOP {
        return;
    }
    seat.ran_on.store(cpu::x2apic_id(), Ordering::Relaxed);
    seat.phase.store(DONE, Ordering::Release);
    park_until(seat, &[STOP]);
}

/// A thread spinning where it was kept, saying in `at` where it spins.
struct Spinner {
    thread: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    at: Arc<AtomicU32>,
}

impl Spinner {
    /// Keep a spinner on `busy`, once it says it spins there.
    fn on(busy: u32) -> Result<Spinner, String> {
        let (stop, at) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(UNSEEN)));
        let (thread, _, _) = place(|on| on == busy, {
            let (stop, at) = (stop.clone(), at.clone());
            move |_: &Seat| {
                let mut x = 1u64;
                loop {
                    at.store(cpu::x2apic_id(), Ordering::Relaxed);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    for _ in 0..1000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            }
        })?;
        let spinner = Spinner { thread, stop, at };
        let seen = || Some(spinner.at()).filter(|at| *at != UNSEEN);
        match spin_for(nanos_since_boot() + STAGE_NS, seen, "the kept spinner") {
            Ok(at) if at == busy => Ok(spinner),
            Ok(at) => {
                spinner.end();
                Err(format!("a spinner kept on CPU {busy} spins on {at}"))
            }
            Err(why) => {
                spinner.end();
                Err(why)
            }
        }
    }

    fn at(&self) -> u32 {
        self.at.load(Ordering::Relaxed)
    }

    /// Stop it, and answer where it last said it was.
    fn end(self) -> u32 {
        self.stop.store(true, Ordering::Release);
        self.thread.join().expect("a spinner panicked");
        self.at.load(Ordering::Relaxed)
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

/// Stage `ROUNDS` partners on one CPU other than the waker's and the
/// interrupts', then a spinner there; answer them and that CPU.
fn stage(waker_on: u32) -> Result<(Vec<(JoinHandle<()>, Arc<Seat>)>, u32, Spinner), String> {
    let mut partners = Vec::with_capacity(ROUNDS);
    let mut busy = UNSEEN;
    while partners.len() < ROUNDS {
        let wanted = |on| on != waker_on && on != DEVICE_IRQ_CPU && (busy == UNSEEN || on == busy);
        match place(wanted, partner) {
            Ok((thread, seat, on)) => {
                busy = on;
                partners.push((thread, seat));
            }
            Err(why) => {
                end_partners(partners);
                return Err(why);
            }
        }
    }
    match Spinner::on(busy) {
        Ok(spinner) => Ok((partners, busy, spinner)),
        Err(why) => {
            end_partners(partners);
            Err(why)
        }
    }
}

fn main() {
    let cpus = syscall::cpu_count() as usize;
    if cpus < 3 {
        refuse(Refusal::NotStaged, "the waker, the busy CPU and the interrupts' CPU need three");
    }
    let waker_on = cpu::x2apic_id();
    let (partners, busy, spinner) = match stage(waker_on) {
        Ok(staged) => staged,
        Err(why) => refuse(Refusal::NotStaged, &format!("not staged: {why}")),
    };
    let mut spinner = Some(spinner);

    let mut histogram = vec![0u32; BUCKETS];
    let mut overflow = 0u32;
    let mut worst_ns = 0u64;
    let mut rounds = 0usize;
    let mut off_busy = 0usize;
    let mut restaged = 0usize;
    let begun = nanos_since_boot();
    for (partner, seat) in &partners {
        // A kernel that balances may hand the spinner to an idle CPU; then
        // the partners' CPU is idle, and a spinner is kept there again.
        if spinner.as_ref().is_some_and(|spinner| spinner.at() != busy) {
            let left_for = spinner.take().map_or(UNSEEN, Spinner::end);
            if restaged == RESTAGES {
                refuse(Refusal::NotStaged, &format!("the spinner left CPU {busy} {RESTAGES} times"));
            }
            restaged += 1;
            match Spinner::on(busy) {
                Ok(again) => spinner = Some(again),
                Err(why) => refuse(
                    Refusal::NotStaged,
                    &format!("the spinner left CPU {busy} for {left_for}, and none was kept there again: {why}"),
                ),
            }
        }
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
    let now_on = cpu::x2apic_id();
    spinner.map(Spinner::end);
    end_partners(partners);
    if now_on != waker_on {
        refuse(Refusal::NotStaged, &format!("the waker left CPU {waker_on} for {now_on}"));
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
        "wake_onto_busy: {rounds} wakes on {cpus} cpus onto CPU {busy} beside its spinner, waker on \
         CPU {waker_on}, {off_busy} run off CPU {busy}, the spinner kept there again {restaged} \
         time(s): p50={}us p90={}us p99={}us max={}us, {overflow} past the {BUCKETS}us histogram",
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
