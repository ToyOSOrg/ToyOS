//! Scout, never landed: an ordinary wake's post-to-dispatch latency, by the
//! target CPU's state, read off a `scout-wake` kernel (`kernel/src/scout_wake.rs`).
//!
//! The main thread and one partner ping-pong through `park`/`unpark`, each
//! round two wakes, beside a number of threads that spin and never block.
//! Each phase arms the kernel's record for this process, ping-pongs for its
//! span, and has the kernel log the phase's quantiles before anything stops.
//!
//! Phases 0 to 3 run the spinners in this process, so the pair and the
//! spinners share one fair-share pot. Phases 4 and 5 run them in a second
//! process — this binary started with `spin <n>` — so the pair's pot is its
//! own, and a woken thread is owed the CPU ahead of a spinner.
//!
//! Phases 6 to 10 **stage** where every thread runs, so that two kernels are
//! compared on one arrangement rather than on wherever placement and the
//! balance path left them. Each staged thread asks the kernel which CPU it
//! first ran on and is kept only where it was wanted, or let go and started
//! again; the thread that stages a phase spins while it waits, so no CPU is
//! idle for a candidate to be handed to. Phases 6 to 8 put the pair alone on
//! two CPUs of different cores, three times over; phase 9 puts it on the same
//! two beside one spinner on every CPU (the staging thread is the spinner on
//! its own), and phase 10 puts both on the first of them beside the same
//! spinners. The kernel's `moved=` line says whether the arrangement held.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

const ARM: u64 = 0x5C00_0001;
const DUMP: u64 = 0x5C00_0002;
const WHERE: u64 = 0x5C00_0003;

const SELF: &str = "/system/bin/test_rs_scout_wake";

/// Where a phase's spinners run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spinners {
    Here,
    Apart,
}

fn spin_threads(count: usize, stop: &Arc<AtomicBool>) -> Vec<thread::JoinHandle<()>> {
    (0..count)
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
        .collect()
}

/// The second process: `count` spinners until its stdin closes.
fn spin_apart(count: usize) {
    let stop = Arc::new(AtomicBool::new(false));
    let spinners = spin_threads(count, &stop);
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    stop.store(true, Ordering::Release);
    for s in spinners {
        s.join().unwrap();
    }
}

/// One phase: its id in the kernel's lines, its spinners, its span.
fn phase(id: u64, spinners: usize, at: Spinners, span: Duration) {
    let stop = Arc::new(AtomicBool::new(false));
    let turn = Arc::new(AtomicU32::new(0));
    let (spin, mut apart): (Vec<_>, Option<Child>) = match at {
        Spinners::Here => (spin_threads(spinners, &stop), None),
        Spinners::Apart => (
            Vec::new(),
            Some(
                Command::new(SELF)
                    .arg("spin")
                    .arg(spinners.to_string())
                    .stdin(Stdio::piped())
                    .spawn()
                    .expect("spawn the spinners' process"),
            ),
        ),
    };
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
    // Every thread is up before the record is armed.
    thread::sleep(Duration::from_millis(50));
    let pid = toyos_abi::syscall::getpid().0 as u64;
    assert_eq!(toyos_abi::syscall::debug_with(ARM, pid), 0, "this kernel carries scout-wake");
    let begun = Instant::now();
    let mut rounds = 0u64;
    while begun.elapsed() < span {
        turn.store(1, Ordering::Release);
        partner.thread().unpark();
        while turn.load(Ordering::Acquire) != 0 {
            thread::park();
        }
        rounds += 1;
    }
    let took = begun.elapsed();
    assert_eq!(toyos_abi::syscall::debug_with(DUMP, id), 0);
    println!(
        "scout-wake: phase={id} spinners={spinners} apart={} cpus={} rounds={rounds} in {} ms",
        at == Spinners::Apart,
        toyos_abi::syscall::cpu_count(),
        took.as_millis()
    );
    stop.store(true, Ordering::Release);
    partner.thread().unpark();
    partner.join().unwrap();
    for s in spin {
        s.join().unwrap();
    }
    if let Some(child) = apart.as_mut() {
        drop(child.stdin.take());
        let status = child.wait().expect("wait for the spinners' process");
        assert!(status.success(), "the spinners' process ended {status:?}");
    }
}

/// The CPU the calling thread runs on, as the scout kernel answers it.
fn cpu() -> usize {
    toyos_abi::syscall::debug_with(WHERE, 0) as usize
}

const UNSEEN: usize = usize::MAX;
const UNDECIDED: u8 = 0;
const KEPT: u8 = 1;
const LET_GO: u8 = 2;

/// How many threads one placement may start before the phase is given up.
const TRIES: usize = 64;

/// A thread started to be placed: the CPU it first ran on, and whether it stays.
struct Seat {
    on: AtomicUsize,
    verdict: AtomicU8,
}

/// Start threads running `body` until one first runs on a CPU `wanted`
/// accepts, letting every other go; answer it and that CPU, or `None` once
/// [`TRIES`] were spent. Each candidate parks until it is judged, so the CPU
/// it is kept on is the one it parked on. The caller spins while it waits, so
/// its own CPU is never idle for a candidate to be handed to.
fn place(
    wanted: impl Fn(usize) -> bool,
    body: impl FnOnce() + Clone + Send + 'static,
) -> Option<(JoinHandle<()>, usize)> {
    for _ in 0..TRIES {
        let seat = Arc::new(Seat { on: AtomicUsize::new(UNSEEN), verdict: AtomicU8::new(UNDECIDED) });
        let candidate = {
            let (seat, body) = (seat.clone(), body.clone());
            thread::spawn(move || {
                seat.on.store(cpu(), Ordering::Release);
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
        let begun = Instant::now();
        let on = loop {
            let on = seat.on.load(Ordering::Acquire);
            if on != UNSEEN {
                break on;
            }
            assert!(begun.elapsed() < Duration::from_secs(5), "a candidate never said where it ran");
            std::hint::spin_loop();
        };
        let keep = wanted(on);
        seat.verdict.store(if keep { KEPT } else { LET_GO }, Ordering::Release);
        candidate.thread().unpark();
        if keep {
            return Some((candidate, on));
        }
        candidate.join().unwrap();
    }
    None
}

/// A staged spinner: spins until `stop`, saying where it runs every few
/// hundred microseconds — a system call each time, so not every round.
fn staged_spinner(stop: Arc<AtomicBool>, at: Arc<AtomicUsize>) -> impl FnOnce() + Clone + Send + 'static {
    move || {
        let mut x = 1u64;
        loop {
            at.store(cpu(), Ordering::Relaxed);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            for _ in 0..1 << 18 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }
    }
}

/// The pair's CPUs: 1 and 2, which are different cores on both benches (the
/// T14's SMT siblings are four ids apart, the AMD bench's adjacent pairs from
/// 0), or 5 and 6, also different cores on both, where this thread is on 1 or
/// 2. A machine of fewer than seven takes the first two that are not this one.
fn pair_cpus(here: usize, cpus: usize) -> (usize, usize) {
    if cpus >= 7 {
        return if here == 1 || here == 2 { (5, 6) } else { (1, 2) };
    }
    let mut others = (0..cpus).filter(|&c| c != here);
    (others.next().expect("a second CPU"), others.next().unwrap_or(here))
}

/// One staged phase. With `spinners`, a spinner on every CPU but this
/// thread's, and this thread spins on its own for the span. Then the waker on
/// the first of [`pair_cpus`] and its partner on the second, or on the first
/// too when `together`. The waker arms the record, runs the ping-pong for
/// `span` and dumps; a phase that could not be staged says so and measures
/// nothing.
fn staged(id: u64, spinners: bool, together: bool, span: Duration) {
    let cpus = toyos_abi::syscall::cpu_count() as usize;
    let here = cpu();
    let (waker_on, partner_on) = match pair_cpus(here, cpus) {
        (one, _) if together => (one, one),
        pair => pair,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let turn = Arc::new(AtomicU32::new(0));
    let go = Arc::new(AtomicBool::new(false));
    let partner_thread: Arc<OnceLock<Thread>> = Arc::new(OnceLock::new());
    let mut threads = Vec::new();
    let mut spinning = Vec::new();
    let mut refused = None;
    if spinners {
        for want in (0..cpus).filter(|&c| c != here) {
            let at = Arc::new(AtomicUsize::new(UNSEEN));
            let Some((spinner, _)) = place(|on| on == want, staged_spinner(stop.clone(), at.clone())) else {
                refused = Some(format!("no spinner landed on CPU {want}"));
                break;
            };
            threads.push(spinner);
            spinning.push((want, at));
        }
    }
    let waker = refused.is_none().then(|| {
        let (stop, done, turn, go, partner_thread) =
            (stop.clone(), done.clone(), turn.clone(), go.clone(), partner_thread.clone());
        place(|on| on == waker_on, move || {
            while !go.load(Ordering::Acquire) {
                thread::park();
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            let partner = partner_thread.get().expect("the partner is placed before the go").clone();
            let pid = toyos_abi::syscall::getpid().0 as u64;
            assert_eq!(toyos_abi::syscall::debug_with(ARM, pid), 0, "this kernel carries scout-wake");
            let begun = Instant::now();
            let mut rounds = 0u64;
            while begun.elapsed() < span {
                turn.store(1, Ordering::Release);
                partner.unpark();
                while turn.load(Ordering::Acquire) != 0 {
                    thread::park();
                }
                rounds += 1;
            }
            let took = begun.elapsed();
            assert_eq!(toyos_abi::syscall::debug_with(DUMP, id), 0);
            println!(
                "scout-wake: phase={id} staged waker={waker_on} partner={partner_on} spinners={} \
                 coordinator={here} cpus={} rounds={rounds} in {} ms",
                if spinners { cpus } else { 0 },
                toyos_abi::syscall::cpu_count(),
                took.as_millis()
            );
            done.store(true, Ordering::Release);
            stop.store(true, Ordering::Release);
            partner.unpark();
        })
    });
    let waker = match waker {
        Some(Some((waker, _))) => Some(waker),
        Some(None) => {
            refused = Some(format!("no waker landed on CPU {waker_on}"));
            None
        }
        None => None,
    };
    let partner = waker.as_ref().and_then(|waker| {
        let (stop, turn, waker) = (stop.clone(), turn.clone(), waker.thread().clone());
        let placed = place(|on| on == partner_on, move || loop {
            while turn.load(Ordering::Acquire) != 1 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
            turn.store(0, Ordering::Release);
            waker.unpark();
        });
        if placed.is_none() {
            refused = Some(format!("no partner landed on CPU {partner_on}"));
        }
        placed.map(|(partner, _)| partner)
    });
    // Every spinner, and this thread, is still where it was staged.
    if refused.is_none() {
        let begun = Instant::now();
        for (want, at) in &spinning {
            while at.load(Ordering::Relaxed) == UNSEEN {
                assert!(begun.elapsed() < Duration::from_secs(5), "a kept spinner never said where it spins");
                std::hint::spin_loop();
            }
            let on = at.load(Ordering::Relaxed);
            if on != *want {
                refused = Some(format!("the spinner kept on CPU {want} is on {on}"));
            }
        }
        if cpu() != here {
            refused = Some(format!("the coordinator left CPU {here} for {}", cpu()));
        }
    }
    match (&waker, &partner, &refused) {
        (Some(waker), Some(partner), None) => {
            partner_thread.set(partner.thread().clone()).expect("set once");
            go.store(true, Ordering::Release);
            waker.thread().unpark();
            if spinners {
                // This CPU's spinner, for the span.
                let mut x = 1u64;
                while !done.load(Ordering::Acquire) {
                    for _ in 0..1000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            }
        }
        _ => {
            println!("scout-wake: phase={id} not staged: {}", refused.as_deref().unwrap_or("?"));
            stop.store(true, Ordering::Release);
            if let Some(partner) = &partner {
                partner.thread().unpark();
            }
            if let Some(waker) = &waker {
                go.store(true, Ordering::Release);
                waker.thread().unpark();
            }
        }
    }
    for t in waker.into_iter().chain(partner).chain(threads) {
        t.join().unwrap();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("spin") {
        spin_apart(args[2].parse().expect("a spinner count"));
        return;
    }
    let cpus = toyos_abi::syscall::cpu_count() as usize;
    phase(0, 0, Spinners::Here, Duration::from_secs(3));
    phase(1, cpus - 1, Spinners::Here, Duration::from_secs(5));
    phase(2, cpus, Spinners::Here, Duration::from_secs(5));
    phase(3, 2 * cpus, Spinners::Here, Duration::from_secs(5));
    phase(4, cpus, Spinners::Apart, Duration::from_secs(5));
    phase(5, 2 * cpus, Spinners::Apart, Duration::from_secs(5));
    // Shorter than the natural phases: the boot's whole job list has a minute.
    for id in 6..=8 {
        staged(id, false, false, Duration::from_secs(2));
    }
    staged(9, true, false, Duration::from_secs(4));
    staged(10, true, true, Duration::from_secs(4));
}
