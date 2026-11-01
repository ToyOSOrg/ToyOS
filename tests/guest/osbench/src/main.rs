//! `osbench`: the same system workloads, built from one source for ToyOS
//! and for Linux, so one machine's two systems can be compared.
//!
//! `osbench <group>` runs a group, or `all` of them in [`GROUPS`]' order, each
//! in a process of its own as a ToyOS job list runs them; the groups in
//! [`BY_NAME`] need another party and run only by name. A ToyOS job list
//! passes no arguments, so there the group is the name the program was started
//! under, `osbench_<group>`. Every other command is one this program gives its
//! own children.
//!
//! Every result is one line of `key=value` words (`report`): a distribution
//! with its sample count, minimum, p50, p90, p99, p99.9, maximum and mean,
//! the clock it was taken at, and the conditions it was taken under. Each
//! benchmark's own comment states its warm-up and repetitions. Benchmarks
//! report and never judge: a group exits non-zero only when one of them could
//! not measure, and a facility a system does not offer is a result
//! (`unsupported`), not a failure.
//!
//! Where a facility differs the code does too, in `sys` and said at the
//! benchmark: native IPC, the directories each filesystem is reached at, the
//! tail group's diagnostics. Everything else is the same source on both.
//!
//! On Linux `nvme` and `energy` read what only root may (the raw namespace,
//! the model-specific registers) and say so unprivileged. `README.md` is how
//! the Linux half is built and how a comparison is run.

mod arch;
mod around;
mod boot;
mod clock;
mod crunch;
mod energy;
mod fair;
mod files;
mod identity;
mod internet;
mod ipc;
#[cfg(target_arch = "x86_64")]
mod kcopyat;
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
mod kernel_copy;
mod lan;
mod measure;
mod mem;
mod net;
mod nvme;
mod pingpong;
mod place;
mod procs;
mod ran;
mod report;
mod storage;
mod sys;
mod system;
mod tail;
mod time;
mod wake;
mod work;

use std::time::Instant;

/// Iterations of the compute chunk every CPU-bound worker here runs: a few
/// microseconds on either bench.
pub const CHUNK_ITERATIONS: u64 = 25_000;

/// One unit of CPU-bound work: a multiply-add chain, the same instructions
/// on both systems.
pub fn chunk() -> u64 {
    let mut x = std::hint::black_box(0x2545_f491_4f6c_dd1d_u64);
    for _ in 0..CHUNK_ITERATIONS {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    }
    std::hint::black_box(x)
}

pub fn ns_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).expect("a span under 584 years")
}

pub fn cpus() -> usize {
    std::thread::available_parallelism().expect("the number of CPUs this process may use").get()
}

fn conditions() {
    report::condition("cpu_model", sys::cpu_model());
    report::condition("cpus", cpus());
    report::condition("threads_per_core", sys::threads_per_core());
    sys::conditions();
}

fn system() {
    system::chunk_rate();
    system::null_syscall();
    system::thread_spawn();
    system::process_spawn();
    for size in [64, 256, 1024, 4096, 16 * 1024, 64 * 1024] {
        system::pipe_throughput(size);
    }
    system::anon_memory();
    system::map_unmap_small();
    system::bandwidth();
}

fn sched() {
    system::chunk_rate();
    wake::periodic("wake.periodic.idle", wake::PERIOD);
    wake::periodic_loaded("wake.periodic.loaded");
    #[cfg(target_os = "linux")]
    {
        // An ordinary Linux thread's sleep is padded by its timer slack, 50 µs
        // by default; these show the wake without it. ToyOS has none.
        let slack = sys::timer_slack_ns();
        sys::set_timer_slack_ns(1);
        wake::periodic("wake.periodic.idle.slack1ns", wake::PERIOD);
        wake::periodic_loaded("wake.periodic.loaded.slack1ns");
        sys::set_timer_slack_ns(slack);
    }
    wake::onto_busy();
    wake::storm();
    for same in [true, false] {
        pingpong::pipe(same);
        pingpong::futex(same);
    }
}

fn fair() {
    system::chunk_rate();
    for grouping in fair::groupings() {
        fair::contend(grouping);
    }
}

fn files() {
    files::suite(&files::memory());
}

/// Every group the machine alone runs, in the order `all` and the ToyOS job
/// list run them: `boot` first, since it reads how long the boot took to
/// reach it; and `crunch`, which heats it, last: ToyOS reads no temperature
/// to cool down by, so a group after it would start at a clock its Linux run
/// does not.
const GROUPS: [(&str, fn()); 16] = [
    ("boot", boot::suite),
    ("identity", identity::suite),
    ("system", system),
    ("sched", sched),
    ("fair", fair),
    ("files", files),
    ("ipc", ipc::suite),
    ("net", net::suite),
    ("procs", procs::suite),
    ("mem", mem::suite),
    ("time", time::suite),
    ("work", work::suite),
    ("tail", tail::suite),
    ("nvme", nvme::suite),
    ("persist", storage::suite),
    ("crunch", crunch::suite),
];

/// The groups that run only by name: `lan` needs the `osbench-peer` on the
/// other bench, `internet` the public internet, and `energy` a machine
/// running nothing else, in a ToyOS boot of its own and as root on Linux.
#[cfg(target_arch = "x86_64")]
const BY_NAME: [(&str, fn()); 4] =
    [("lan", lan::suite), ("internet", internet::suite), ("energy", energy::suite), ("kcopyat", kcopyat::suite)];
#[cfg(not(target_arch = "x86_64"))]
const BY_NAME: [(&str, fn()); 3] = [("lan", lan::suite), ("internet", internet::suite), ("energy", energy::suite)];

/// A group between its first and last lines, after the machine's conditions
/// and the clock its CPU starts at.
fn group(name: &str, run: fn()) {
    report::group(name, "start");
    conditions();
    around::ran_during(&format!("{name}.cpu.clock"), clock::distribution);
    let t = Instant::now();
    run();
    report::group(name, &format!("done secs={}", t.elapsed().as_secs()));
}

/// Every group in [`GROUPS`], each a child of its own, so no group runs in a
/// process an earlier one shaped: exits 1 if any did not measure.
fn all() {
    let exe = std::env::current_exe().expect("a path to this program");
    let mut failed = Vec::new();
    for (name, _) in GROUPS {
        let status = std::process::Command::new(&exe).arg(name).status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => failed.push(format!("{name} ({status})")),
            Err(e) => failed.push(format!("{name} (would not start: {e})")),
        }
    }
    if !failed.is_empty() {
        eprintln!("osbench: groups that did not measure: {}", failed.join(", "));
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let invoked = args.first().and_then(|a| a.rsplit('/').next()).unwrap_or_default();
    let command = match args.get(1) {
        Some(command) => command.as_str(),
        None => invoked.strip_prefix("osbench_").unwrap_or("all"),
    };
    let rest = args.get(2..).unwrap_or_default();
    match command {
        "all" => return all(),
        "nop" => return,
        "spin" => return wake::spin(),
        "fair-job" => return fair::job(rest),
        "fair-helper" => return fair::helper(rest),
        "ipc-serve" => return ipc::serve(),
        "cc" => return work::cc(rest),
        named => match GROUPS.iter().chain(&BY_NAME).find(|(name, _)| *name == named) {
            Some((name, run)) => group(name, *run),
            None => {
                let names: Vec<&str> = GROUPS.iter().chain(&BY_NAME).map(|(name, _)| *name).collect();
                eprintln!("osbench: no command {named:?}; the groups are {} and all", names.join(", "));
                std::process::exit(2);
            }
        },
    }
    if report::any_refused() {
        std::process::exit(1);
    }
}
