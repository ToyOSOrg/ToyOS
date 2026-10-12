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

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const ARM: u64 = 0x5C00_0001;
const DUMP: u64 = 0x5C00_0002;

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
}
