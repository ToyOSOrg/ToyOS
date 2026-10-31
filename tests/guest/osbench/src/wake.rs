//! How long a woken thread waits to run: on a timer, onto a busy CPU, and in
//! a storm.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use crate::place::{self, Placed};
use crate::report::{refuse, result};
use crate::{ns_since, sys};

/// What a spin-wait on another thread's progress may take before the run is
/// refused: every wait here is microseconds on a working machine.
const STUCK: Duration = Duration::from_secs(5);

pub fn spin_until(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > STUCK {
            return Err(format!("{what} did not happen within {STUCK:?}"));
        }
        std::hint::spin_loop();
    }
    Ok(())
}

/// The period the wake group's periodic wakes keep.
pub const PERIOD: Duration = Duration::from_micros(500);

/// A cyclictest-style periodic wake: sleep to absolute targets every
/// `period` and record how late each wake ran, ns, 5,000 after 100. A late
/// wake shortens the next sleep rather than moving every later target.
/// Ordinary priority on both systems, as an unprivileged program gets it.
pub fn periodic(bench: &str, period: Duration) {
    const WARMUP: u32 = 100;
    const SAMPLES: u32 = 5_000;
    let start = Instant::now();
    let mut late = Vec::with_capacity(SAMPLES as usize);
    for i in 1..=WARMUP + SAMPLES {
        let target = start + period * i;
        if let Some(left) = target.checked_duration_since(Instant::now()) {
            thread::sleep(left);
        }
        if i > WARMUP {
            late.push(ns_since(target));
        }
    }
    result(bench, "ns", &mut late, &[("period_ns", period.as_nanos().to_string())]);
}

/// The periodic wake with every CPU kept busy by another process's
/// spinners, one per CPU.
pub fn periodic_loaded(bench: &str) {
    let mut load = match Spinners::start() {
        Ok(load) => load,
        Err(why) => return refuse(bench, why),
    };
    periodic(bench, PERIOD);
    if let Err(why) = load.stop() {
        refuse(bench, why);
    }
}

/// `osbench spin`: one spinning thread per CPU, which says `spinning` once
/// all of them have started, until it is killed or [`SPIN_BOUND`] has
/// passed, so a coordinator that died leaves no load behind.
pub fn spin() {
    let end = Instant::now() + SPIN_BOUND;
    let started = Arc::new(Barrier::new(crate::cpus() + 1));
    for _ in 0..crate::cpus() {
        let started = started.clone();
        thread::spawn(move || {
            started.wait();
            while Instant::now() < end {
                std::hint::black_box(crate::chunk());
            }
        });
    }
    started.wait();
    println!("spinning");
    thread::sleep(end.saturating_duration_since(Instant::now()));
}

/// Ten times what one loaded periodic run takes.
const SPIN_BOUND: Duration = Duration::from_secs(30);

struct Spinners(Child);

impl Spinners {
    fn start() -> Result<Spinners, String> {
        let exe = std::env::current_exe().map_err(|e| format!("no path to this program: {e}"))?;
        let mut child = Command::new(exe)
            .arg("spin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("the spinners would not start: {e}"))?;
        let stdout = child.stdout.take().expect("stdout was asked for piped");
        let (said, heard) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = said.send(BufReader::new(stdout).read_line(&mut line).map(|_| line));
        });
        let mut spinners = Spinners(child);
        match heard.recv_timeout(STUCK) {
            Ok(Ok(line)) if line.trim() == "spinning" => Ok(spinners),
            outcome => {
                let _ = spinners.stop();
                Err(format!("the spinners never said they were spinning: {outcome:?}"))
            }
        }
    }

    fn stop(&mut self) -> Result<(), String> {
        self.0.kill().map_err(|e| format!("the spinners would not die: {e}"))?;
        self.0.wait().map_err(|e| format!("the spinners were not reaped: {e}"))?;
        Ok(())
    }
}

const PARKED: u8 = 0;
const RUN: u8 = 1;
const DONE: u8 = 2;
const STOP: u8 = 3;

/// A partner: whether it has parked, where it ran when woken, and how far it
/// has got.
struct Seat {
    parked: AtomicBool,
    ran_on: AtomicU32,
    phase: AtomicU8,
}

fn park_until(seat: &Seat, until: &[u8]) -> u8 {
    loop {
        let phase = seat.phase.load(Ordering::Acquire);
        if until.contains(&phase) {
            return phase;
        }
        thread::park();
    }
}

/// The CPU device interrupts reach on ToyOS, the kernel's cpu0
/// (`kernel/src/arch/x86_64/apic.rs`), left out on both systems so an
/// interrupt's pass never drains a wake early.
pub const IRQ_CPU: u32 = 0;
const ROUNDS: usize = 200;

/// A wake onto a CPU a spinner holds, beside idle CPUs: each round, from a
/// waker spinning on a CPU of its own, wakes a fresh partner that parked on
/// the busy one and times it to running, ns. Both systems pin the waker, each
/// partner until it has parked, and the spinner. Either kernel may run
/// the wake where it is or move it to an idle CPU, and `ran_on_busy` counts
/// how often it stayed.
pub fn onto_busy() {
    const BENCH: &str = "wake.onto_busy";
    if crate::cpus() < 3 {
        return refuse(BENCH, "the waker, the busy CPU and the interrupts' CPU need three");
    }
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let waker = {
        let outcome = outcome.clone();
        place::spawn_on(|on| on != IRQ_CPU, move || *outcome.lock().unwrap() = Some(rounds_onto_busy()))
    };
    let waker = match waker {
        Ok(waker) => waker,
        Err(why) => return refuse(BENCH, format!("the waker was not placed: {why}")),
    };
    waker.thread.join().expect("the waker panicked");
    let measured = outcome.lock().unwrap().take().expect("the waker said nothing");
    match measured {
        Ok((mut ns, extra)) => result(BENCH, "ns", &mut ns, &extra),
        Err(why) => refuse(BENCH, why),
    }
}

type Measured = Result<(Vec<u64>, Vec<(&'static str, String)>), String>;

fn rounds_onto_busy() -> Measured {
    let waker_on = sys::current_cpu();
    let busy = sys::allowed_cpus()
        .into_iter()
        .map(|cpu| cpu as u32)
        .find(|&on| on != waker_on && on != IRQ_CPU)
        .ok_or("no CPU but the waker's and the interrupts' to keep busy")?;
    let mut partners: Vec<(Placed, Arc<Seat>)> = Vec::with_capacity(ROUNDS);
    let end = |partners: Vec<(Placed, Arc<Seat>)>| {
        for (partner, seat) in partners {
            seat.phase.store(STOP, Ordering::Release);
            partner.thread.thread().unpark();
            partner.thread.join().expect("a partner panicked");
        }
    };
    while partners.len() < ROUNDS {
        let seat = Arc::new(Seat { parked: AtomicBool::new(false), ran_on: AtomicU32::new(u32::MAX), phase: AtomicU8::new(PARKED) });
        let body = {
            let seat = seat.clone();
            move || {
                seat.parked.store(true, Ordering::Release);
                if park_until(&seat, &[RUN, STOP]) == RUN {
                    seat.ran_on.store(sys::current_cpu(), Ordering::Relaxed);
                    seat.phase.store(DONE, Ordering::Release);
                    park_until(&seat, &[STOP]);
                }
            }
        };
        match place::spawn_on(|on| on == busy, body) {
            Ok(placed) => partners.push((placed, seat)),
            Err(why) => {
                let placed = partners.len();
                end(partners);
                return Err(format!("partner {placed} was not placed: {why}"));
            }
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let spinning = Arc::new(AtomicBool::new(false));
    let spinner = {
        let (stop, spinning) = (stop.clone(), spinning.clone());
        place::spawn_on(
            |on| on == busy,
            move || {
                spinning.store(true, Ordering::Relaxed);
                while !stop.load(Ordering::Relaxed) {
                    std::hint::black_box(crate::chunk());
                }
            },
        )
    };
    let spinner = match spinner {
        Ok(spinner) => spinner,
        Err(why) => {
            end(partners);
            return Err(format!("no spinner on CPU {busy}: {why}"));
        }
    };
    let mut failed = spin_until("the spinner's start", || spinning.load(Ordering::Relaxed)).err();

    // Each partner is unpinned once it has parked, before any round: a parked
    // thread stays where it parked until its wake places it, and the unpin's
    // kick of the busy CPU then lands outside every timed wake.
    for (partner, seat) in partners.iter().filter(|_| failed.is_none()) {
        if let Err(why) = spin_until("a partner's park", || seat.parked.load(Ordering::Acquire)) {
            failed = Some(why);
            break;
        }
        partner.release();
    }
    let mut ns = Vec::with_capacity(ROUNDS);
    let mut ran_on_busy = 0usize;
    for (partner, seat) in partners.iter().filter(|_| failed.is_none()) {
        let sent = Instant::now();
        seat.phase.store(RUN, Ordering::Release);
        partner.thread.thread().unpark();
        if let Err(why) = spin_until("a woken partner's run", || seat.phase.load(Ordering::Acquire) == DONE) {
            failed = Some(why);
            break;
        }
        ns.push(ns_since(sent));
        ran_on_busy += usize::from(seat.ran_on.load(Ordering::Relaxed) == busy);
    }
    stop.store(true, Ordering::Relaxed);
    spinner.thread.join().expect("the spinner panicked");
    end(partners);
    if let Some(why) = failed {
        return Err(why);
    }
    Ok((
        ns,
        vec![
            ("busy_cpu", busy.to_string()),
            ("waker_cpu", waker_on.to_string()),
            ("ran_on_busy", ran_on_busy.to_string()),
        ],
    ))
}

/// The storm's generation: each storm advances it, which is what ends a
/// waiter's wait on both systems (a ToyOS futex wait returns only once the
/// word has changed), and each woken waiter counts itself and waits on the
/// new value.
static WORD: AtomicU32 = AtomicU32::new(0);
static ARRIVED: AtomicU32 = AtomicU32::new(0);
static FINISHED: AtomicBool = AtomicBool::new(false);

/// A futex wake of `width` parked waiters: what the call costs the waker
/// (`call`) and how long until every woken waiter has run (`drain`), ns, 15
/// rounds per width. A round counts only if the wake found exactly `width`
/// parked, which is the proof they all were.
pub fn storm() {
    const WIDTHS: [u32; 4] = [1, 4, 16, 64];
    const ROUNDS: usize = 15;
    const ATTEMPTS: usize = 400;
    let mut waiters = Vec::new();
    for width in WIDTHS {
        while waiters.len() < width as usize {
            waiters.push(thread::spawn(|| {
                let mut seen = WORD.load(Ordering::Acquire);
                while !FINISHED.load(Ordering::Acquire) {
                    sys::futex_wait(&WORD, seen);
                    let now = WORD.load(Ordering::Acquire);
                    if now != seen {
                        seen = now;
                        ARRIVED.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }));
        }
        let (mut call, mut drain) = (Vec::new(), Vec::new());
        let mut failed = None;
        for _ in 0..ATTEMPTS {
            if call.len() == ROUNDS {
                break;
            }
            // Time for every waiter woken last round to park again.
            thread::sleep(Duration::from_millis(2));
            let before = ARRIVED.load(Ordering::Acquire);
            let t = Instant::now();
            WORD.fetch_add(1, Ordering::AcqRel);
            let woken = sys::futex_wake(&WORD, u32::MAX);
            let called = ns_since(t);
            if let Err(why) =
                spin_until("a storm's drain", || ARRIVED.load(Ordering::Acquire).wrapping_sub(before) >= woken)
            {
                failed = Some(why);
                break;
            }
            if woken == width {
                call.push(called);
                drain.push(ns_since(t));
            }
        }
        let bench = |what: &str| format!("wake.storm.{what}.w{width}");
        match failed {
            Some(why) => refuse(&bench("call"), why),
            None if call.len() < ROUNDS => {
                refuse(&bench("call"), format!("{} of {ATTEMPTS} wakes found all {width} parked", call.len()))
            }
            None => {
                result(&bench("call"), "ns", &mut call, &[]);
                result(&bench("drain"), "ns", &mut drain, &[]);
            }
        }
    }
    FINISHED.store(true, Ordering::Release);
    WORD.fetch_add(1, Ordering::AcqRel);
    while sys::futex_wake(&WORD, u32::MAX) > 0 {}
    for waiter in waiters {
        waiter.join().expect("a storm waiter panicked");
    }
}
