//! Every CPU's counters across a span of an idle machine and then across every
//! CPU spinning, printed as `inspect kernel.*` renders them, for the `counters`
//! metal row to judge (`tests/toyos.rs`). It asserts nothing: frequency, busy
//! fraction and firmware interrupts are the hardware's to say.
//!
//! Three reads: `idle0` and `idle1` either side of [`IDLE`], and `spin` after
//! [`SPIN`] iterations on a thread per CPU begun at `idle1`. Each read's
//! records follow a line with the clock after it and what the read took, a
//! whole round each, none joining another's. **Nothing is printed until all
//! three are taken**: a printed line reaches the stick within the second,
//! through the `/log` fileserver on that fileserver's CPU.
//!
//! **`idle0` waits for the log to be quiet** ([`settle`]): a job starts while
//! logkeeper is still writing the boot so far and the job's own launch lines
//! to the stick, and a second begun then measures that write.
//!
//! **Then `loaded`: how late the round's kick reaches each CPU** while a thread
//! per CPU spawns a program that exits at once, which is the load Linux's
//! timer reading of this machine was taken under. A round's reader kicks every
//! other CPU and then stamps its own block, and each CPU stamps its block in
//! its kick handler, so a CPU's stamp less the round's earliest is how late
//! its kick handler ran, short by however long the earliest came after the
//! kicks. A round across which a CPU's SMI count moved is dropped, since an
//! SMI stops every CPU, and so is one with a CPU stale.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall::{self, SyscallError};
use toyos_logstream::{program_line, Lines};

/// The idle span: long enough that a CPU's busy fraction is its idle one and
/// not the reads'.
const IDLE: Duration = Duration::from_secs(1);

/// Iterations of a dependent multiply-add per thread, about four cycles each:
/// over four seconds of a CPU at the T14's highest frequency, so `idle0` and
/// `spin` span more than twice its firmware's 2.2 s interrupt period whatever
/// the frequency.
const SPIN: u64 = 4_000_000_000;

/// How long `loaded` samples rounds for: a share of the job list's bound
/// (`toyos_tco::JOB_BOUND_MS`) the reads before it leave.
const LOADED: Duration = Duration::from_secs(20);

/// How long [`settle`] waits for each of its lines: two of logkeeper's rounds
/// at its write budget (`userland/logkeeper/src/policy.rs`, 5 s).
const SETTLE_BOUND: Duration = Duration::from_secs(10);

/// What this binary's own children are asked to do: exit at once.
const EXIT_AT_ONCE: &str = "exit-at-once";

/// Every CPU's records of one round.
fn round(cap: &SysCap) -> Vec<Record> {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect()
}

/// The nearest-rank `q` of an already-sorted sample.
fn rank(sorted: &[u64], q: f64) -> u64 {
    sorted[((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len()) - 1]
}

/// The `loaded` phase, in the module header's words.
fn loaded(cap: &SysCap) {
    let cpus = syscall::cpu_count() as usize;
    let stop = AtomicBool::new(false);
    let mut late: Vec<Vec<u64>> = vec![Vec::new(); cpus];
    let (mut smi, mut stale) = (0u64, 0u64);
    let (first, last) = std::thread::scope(|s| {
        for _ in 0..cpus {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let status = Command::new("/system/bin/test_rs_counters_metal")
                        .arg(EXIT_AT_ONCE)
                        .status()
                        .expect("this binary spawns itself");
                    assert!(status.success(), "a child that exits at once exited {status:?}");
                }
            });
        }
        let begun = Instant::now();
        let mut before = round(cap);
        let first = (toyos_abi::clock::nanos_since_boot(), before.iter().filter_map(|r| r.get(Counter::Stamp)).max());
        let mut last = first;
        while begun.elapsed() < LOADED {
            let records = round(cap);
            last = (toyos_abi::clock::nanos_since_boot(), records.iter().filter_map(|r| r.get(Counter::Stamp)).max());
            let moved = records.iter().zip(&before).any(|(r, b)| r.get(Counter::Smi) != b.get(Counter::Smi));
            if records.iter().any(|r| r.stale) {
                stale += 1;
            } else if moved {
                smi += 1;
            } else {
                let stamps: Vec<u64> = records.iter().map(|r| r.get(Counter::Stamp).expect("a fresh record is stamped")).collect();
                let earliest = *stamps.iter().min().expect("a machine has a cpu");
                for (cpu, stamp) in stamps.iter().enumerate() {
                    late[cpu].push(stamp - earliest);
                }
            }
            before = records;
        }
        stop.store(true, Ordering::Relaxed);
        (first, last)
    });
    assert!(!late[0].is_empty(), "every round was dropped: {smi} across an SMI, {stale} with a cpu stale");
    let (Some(from), Some(to)) = (first.1, last.1) else { panic!("a round carried no stamp") };
    // Stamp ticks per microsecond, off the same rounds.
    let per_us = (to - from) as f64 / ((last.0 - first.0) as f64 / 1_000.0);
    println!(
        "counters_metal loaded: {} rounds kept, {smi} dropped across an SMI, {stale} with a cpu stale, \
         at {per_us:.0} stamp ticks per us",
        late[0].len()
    );
    for (cpu, sample) in late.iter_mut().enumerate() {
        sample.sort_unstable();
        let us = |ticks: u64| ticks as f64 / per_us;
        println!(
            "counters_metal loaded: cpu{cpu} kick late p50={:.1}us p99={:.1}us max={:.1}us",
            us(rank(sample, 0.5)),
            us(rank(sample, 0.99)),
            us(rank(sample, 1.0)),
        );
    }
}

/// One read: the clock after it, what it took, and every CPU's records.
struct Read {
    at: u64,
    took: Duration,
    records: Vec<Record>,
}

fn read(cap: &SysCap) -> Read {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let asked = Instant::now();
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    let took = asked.elapsed();
    let at = toyos_abi::clock::nanos_since_boot();
    Read { at, took, records: raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect() }
}

fn print(phase: &str, read: &Read) {
    println!("counters_metal {phase}: at {} ns, the read took {} ns", read.at, read.took.as_nanos());
    for (path, value) in toyos_inspect::kernel::render(&read.records).expect("one record per cpu") {
        println!("counters_metal {phase}: {}", toyos_inspect::line(&path, &value));
    }
}

/// Return once logkeeper has written, and made durable, everything stamped
/// before this call.
///
/// A reader of the `log` port is handed each round only after it is on the
/// stick, so this prints a line and reads the log until that line comes back.
/// **Twice**: the round that writes the first may itself put a record in the
/// log — the stick's first sync is one — and the second writes it. The
/// `counters` row reds a kernel record stamped inside the idle second.
fn settle() {
    let pipe = logkeeper_api::read().unwrap_or_else(|why| panic!("test-runner's `log` port: {why}")).pipe;
    let poller = Poller::new(1);
    let mut lines = Lines::new();
    let mut chunk = vec![0u8; 64 * 1024];
    for round in ["first", "second"] {
        let said = format!("counters_metal settle: the log holds this {round} line");
        println!("{said}");
        let by = Instant::now() + SETTLE_BOUND;
        let mut held = false;
        while !held {
            match pipe.read_nonblock(&mut chunk) {
                Ok(0) => panic!("logkeeper closed the log before it held {said:?}"),
                Ok(n) => lines.push(&chunk[..n], |line, _| {
                    let line = std::str::from_utf8(line)
                        .unwrap_or_else(|e| panic!("logkeeper served a line that is not UTF-8 ({e}): {line:?}"));
                    held |= program_line(line).is_some_and(|line| line.text == said);
                }),
                Err(SyscallError::WouldBlock) => {
                    let left = by.checked_duration_since(Instant::now()).unwrap_or_else(|| {
                        panic!("the log did not hold {said:?} within {SETTLE_BOUND:?}")
                    });
                    poller.watch(&pipe, READABLE, 0);
                    poller.wait(1, left.as_nanos() as u64, |_| {});
                }
                Err(e) => panic!("the log's pipe refused a read: {e:?}"),
            }
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(EXIT_AT_ONCE) {
        return;
    }
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    settle();
    let idle0 = read(&cap);
    std::thread::sleep(IDLE);
    let idle1 = read(&cap);
    std::thread::scope(|s| {
        for _ in 0..syscall::cpu_count() {
            s.spawn(|| {
                let mut x = 1u64;
                for _ in 0..SPIN {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            });
        }
    });
    let spin = read(&cap);
    for (phase, read) in [("idle0", &idle0), ("idle1", &idle1), ("spin", &spin)] {
        print(phase, read);
    }
    loaded(&cap);
}
