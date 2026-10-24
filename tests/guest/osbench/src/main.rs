//! `osbench`: the same system workloads, built from one source for ToyOS
//! and for Linux, so one machine's two systems can be compared.
//!
//! `osbench <group>` runs a group, or `all` of them in [`GROUPS`]' order. A
//! ToyOS job list passes no arguments, so there the group is the name the
//! program was started under, `osbench_<group>`. Every other command is one
//! this program gives its own children.
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
//! benchmark: native IPC, persistent storage's medium, the tail group's
//! diagnostics. Everything else is the same source on both.
//!
//! The Linux build is static (`x86_64-unknown-linux-musl`, linked by the
//! toolchain's own LLD per `.cargo/config.toml`), so a bench runs it without a
//! toolchain:
//!
//! ```text
//! rustup target add --toolchain nightly-2026-07-22 x86_64-unknown-linux-musl
//! cargo +nightly-2026-07-22 build -p osbench --target x86_64-unknown-linux-musl --profile toyos
//! ```
//!
//! That nightly is the build's stage-0 compiler (`build/sysroot.rs`'s
//! `STAGE0_CARGO`, which this follows), the upstream one nearest ToyOS's own.
//! On Linux `nvme` and `energy` read what only root may (the raw namespace,
//! the package's energy counter) and say so unprivileged; `lan` needs an
//! `osbench-peer` (`lan/`) running on another machine on the link.
//! With the `toyos` profile every guest binary is built with, osbench's own
//! code is generated alike for both; std's platform layer and the C library
//! beneath it (musl's on Linux) are each system's own.

mod arch;
mod boot;
mod clock;
mod crunch;
mod energy;
mod fair;
mod files;
mod identity;
mod ipc;
mod lan;
mod measure;
mod mem;
mod net;
mod nvme;
mod pingpong;
mod place;
mod procs;
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
    for target in files::memory_targets() {
        files::suite(&target);
    }
}

/// Every group, in the order `all` and the ToyOS job list run them: `boot`
/// first, since it reads how long the boot took to reach it; `energy` while
/// the machine is still quiet; `crunch`, which heats it, after every other
/// measurement; and `persist` last, since it writes the volume a ToyOS
/// boot's log is on.
const GROUPS: [(&str, fn()); 18] = [
    ("boot", boot::suite),
    ("identity", identity::suite),
    ("energy", energy::suite),
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
    ("lan", lan::suite),
    ("crunch", crunch::suite),
    ("persist", storage::suite),
];

/// A group between its first and last lines, after the machine's conditions
/// and the clock its CPU starts at.
fn group(name: &str, run: fn()) {
    report::group(name, "start");
    conditions();
    clock::distribution();
    let t = Instant::now();
    run();
    report::group(name, &format!("done secs={}", t.elapsed().as_secs()));
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
        "all" => GROUPS.iter().for_each(|(name, run)| group(name, *run)),
        "nop" => return,
        "spin" => return wake::spin(),
        "fair-job" => return fair::job(rest),
        "fair-helper" => return fair::helper(rest),
        "ipc-serve" => return ipc::serve(),
        "cc" => return work::cc(rest),
        named => match GROUPS.iter().find(|(name, _)| *name == named) {
            Some((name, run)) => group(name, *run),
            None => {
                let names: Vec<&str> = GROUPS.iter().map(|(name, _)| *name).collect();
                eprintln!("osbench: no command {named:?}; the groups are {} and all", names.join(", "));
                std::process::exit(2);
            }
        },
    }
    if report::any_refused() {
        std::process::exit(1);
    }
}
