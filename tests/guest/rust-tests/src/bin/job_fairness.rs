//! **Independent jobs share the machine equally, whatever their helper counts.**
//!
//! Three jobs, each made with the test estate's `jobs` right: a build of one
//! busy helper process per CPU, a build of one process with a busy thread per
//! CPU, and a helper-heavy job of four busy helper processes per CPU. Each can
//! fill a third of the machine, so each is owed a third (owner decision J1).
//! What each receives is the CPU time its processes ran over a window the
//! machine is saturated throughout.
//!
//! The tolerance is a tenth of the machine either way. The host model gives
//! this workload thirds to the thousandth on both benches' shapes
//! (`two_builds_and_a_helper_heavy_job_split_the_machine_in_thirds`), and a
//! share per helper process — what the scheduler had before jobs — gives the
//! heavy job two thirds.
//!
//! Its verdict is its exit code; it prints the shares it measured.

use std::io::Read;
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::job::JOB_LABEL;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{self, ProcessStats};
use toyos_abi::RawHandle;

const ME: &str = "/system/bin/test_rs_job_fairness";
/// Long enough for every helper to have been placed and balanced.
const WARMUP: Duration = Duration::from_millis(500);
const WINDOW: Duration = Duration::from_secs(3);
const TOLERANCE: f64 = 0.1;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("spin") => {
            let threads = std::env::args().nth(2).and_then(|n| n.parse().ok()).expect("a thread count");
            spin(threads)
        }
        _ => run(),
    }
}

/// Wait for the start — stdin closing — then keep `threads` threads busy until
/// killed.
fn spin(threads: usize) -> ! {
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    for _ in 1..threads {
        thread::spawn(|| loop {
            core::hint::spin_loop();
        });
    }
    loop {
        core::hint::spin_loop();
    }
}

fn run() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows the estate's capability");
    let cpus = syscall::cpu_count() as usize;
    let shapes = [("build", cpus, 1), ("threaded", 1, cpus), ("heavy", 4 * cpus, 1)];
    let mut jobs: Vec<(&str, Vec<Child>)> = Vec::new();
    for (name, helpers, threads) in shapes {
        let job = cap.new_job().expect("the estate's capability makes a job");
        let children = (0..helpers)
            .map(|_| {
                let copy = job.duplicate().expect("a second handle to the job");
                Command::new(ME)
                    .args(["spin", &threads.to_string()])
                    .stdin(Stdio::piped())
                    .endow(JOB_LABEL, copy.into())
                    .spawn()
                    .expect("spawn a helper into its job")
            })
            .collect();
        jobs.push((name, children));
    }
    for (_, children) in &mut jobs {
        children.iter_mut().for_each(|child| drop(child.stdin.take()));
    }

    thread::sleep(WARMUP);
    let before = cpu_ns(&jobs);
    thread::sleep(WINDOW);
    let after = cpu_ns(&jobs);
    for (_, children) in &mut jobs {
        for child in children.iter_mut() {
            child.kill().expect("kill a helper");
            child.wait().expect("reap a helper");
        }
    }

    let used: Vec<u64> = before.iter().zip(&after).map(|(b, a)| a - b).collect();
    let total: u64 = used.iter().sum();
    let shares: Vec<f64> = used.iter().map(|&ns| ns as f64 / total as f64).collect();
    let capacity = cpus as u64 * WINDOW.as_nanos() as u64;
    println!(
        "job_fairness: cpus={cpus} window={}ms ran={}ms of {}ms shares {}",
        WINDOW.as_millis(),
        total / 1_000_000,
        capacity / 1_000_000,
        jobs.iter().zip(&shares).map(|((name, _), s)| format!("{name}={s:.3}")).collect::<Vec<_>>().join(" "),
    );
    // A window the machine was not saturated through measures placement and
    // idling, not shares.
    assert!(total * 10 >= capacity * 9, "the helpers ran {total} ns of the window's {capacity} ns");
    for ((name, _), share) in jobs.iter().zip(&shares) {
        assert!(
            (share - 1.0 / 3.0).abs() <= TOLERANCE,
            "{name} received {share:.3} of the machine against a third",
        );
    }
}

/// CPU time each job's processes have received so far.
fn cpu_ns(jobs: &[(&str, Vec<Child>)]) -> Vec<u64> {
    jobs.iter()
        .map(|(_, children)| {
            children
                .iter()
                .map(|child| {
                    let mut stats = ProcessStats::default();
                    syscall::process_stats(RawHandle(child.as_raw_handle()), &mut stats).expect("a helper's accounting");
                    stats.cpu_ns
                })
                .sum()
        })
        .collect()
}
