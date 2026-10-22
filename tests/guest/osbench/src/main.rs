//! `osbench`: the same scheduler and system workloads, built from one source
//! for ToyOS and for Linux, so one machine's two systems can be compared.
//!
//! `osbench <group>` runs a group: `system`, `sched`, `fair` or `all`. A
//! ToyOS job list passes no arguments, so there the group is the name the
//! program was started under, `osbench_<group>`. Every other command is one
//! this program gives its own children.
//!
//! Every result is one line of `key=value` words (`report`): a distribution
//! with its sample count, minimum, p50, p90, p99, p99.9, maximum and mean, and
//! the conditions it was taken under. Each benchmark's own comment states its
//! warm-up and repetitions. Benchmarks report and never judge: a group exits
//! non-zero only when one of them could not measure.
//!
//! File I/O is not measured: ToyOS's storage on the benches is a USB stick and
//! Linux's an NVMe disk, so a comparison would be of the devices.
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
//! With the `toyos` profile every guest binary is built with, osbench's own
//! code is generated alike for both; std's platform layer and the C library
//! beneath it (musl's on Linux) are each system's own.

#[cfg(target_os = "toyos")]
mod arch;
mod fair;
mod pingpong;
mod place;
mod report;
mod sys;
mod system;
mod wake;

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
    system::pipe_throughput(64);
    system::pipe_throughput(64 * 1024);
    system::anon_memory();
    system::map_unmap_small();
    system::bandwidth();
}

fn sched() {
    system::chunk_rate();
    wake::periodic("wake.periodic.idle");
    wake::periodic_loaded("wake.periodic.loaded");
    #[cfg(target_os = "linux")]
    {
        // An ordinary Linux thread's sleep is padded by its timer slack, 50 µs
        // by default; these show the wake without it. ToyOS has none.
        let slack = sys::timer_slack_ns();
        sys::set_timer_slack_ns(1);
        wake::periodic("wake.periodic.idle.slack1ns");
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

fn group(name: &str, run: fn()) {
    report::group(name, "start");
    conditions();
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
        "system" => group("system", system),
        "sched" => group("sched", sched),
        "fair" => group("fair", fair),
        "all" => {
            group("system", system);
            group("sched", sched);
            group("fair", fair);
        }
        "nop" => return,
        "spin" => return wake::spin(),
        "fair-job" => return fair::job(rest),
        "fair-helper" => return fair::helper(rest),
        other => {
            eprintln!("osbench: no command {other:?}; the groups are system, sched, fair and all");
            std::process::exit(2);
        }
    }
    if report::any_refused() {
        std::process::exit(1);
    }
}
