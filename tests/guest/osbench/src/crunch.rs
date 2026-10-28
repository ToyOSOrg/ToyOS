//! A short CPU crunch of real work, the same code on both systems: deflate
//! of a fixed corpus (`miniz_oxide`), SHA-256 (`toyos-crypto`), a numeric
//! kernel (a dense `f64` matrix product) and a compile-like task (`syn`
//! parsing generated Rust). One thread, then one per CPU, each workload
//! [`SLICE`] in its phase, with an idle cool-down after each phase. A parse
//! is some 589,000 allocations and frees, so its all-core figure is the
//! system allocator's under contention as much as the CPU's: the Linux build's
//! static musl and ToyOS std's dlmalloc each serialise on one lock.
//!
//! **A laptop is not let overheat.** Where the system reads the package's
//! temperature, a phase stops once it reaches `osbench_rules::heat`'s limit,
//! [`osbench_rules::heat::MARGIN_MC`] below the CPU's TjMax, and each result
//! names the limit's source, and a reading that fails stops it too. Where
//! the system cannot read it (ToyOS), or its sensor never answers, only the
//! one-thread phase runs and every all-core result is `unmeasured`, there
//! being nothing to stop at.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use osbench_rules::heat::{self, Limit, Source};

use crate::report::{condition, result, unmeasured};
use crate::{ns_since, sys};

/// Each workload's span in a phase: four make a phase of about 16 s.
const SLICE: Duration = Duration::from_secs(4);
/// How often a phase reads the temperature.
const POLL: Duration = Duration::from_millis(100);
/// The longest cool-down waits for the package to cool.
const COOL_BOUND: Duration = Duration::from_secs(90);

const MIB: usize = 1 << 20;
const N: usize = 160;

/// What every workload works on, made once before the phases.
struct Inputs {
    corpus: Vec<u8>,
    a: Vec<f64>,
    b: Vec<f64>,
    rust: String,
}

impl Inputs {
    fn new() -> Self {
        let corpus = (0..(MIB / crate::work::FILE_BYTES) as u64).flat_map(crate::work::source).collect();
        let a = (0..N * N).map(|i| (i % 17) as f64 * 0.25).collect();
        let b = (0..N * N).map(|i| (i % 13) as f64 * 0.5).collect();
        Self { corpus, a, b, rust: rust_corpus() }
    }
}

/// A fixed program of Rust: functions with loops, branches and matches,
/// and a struct with an impl every tenth.
fn rust_corpus() -> String {
    let mut out = String::new();
    for i in 0..400u64 {
        let (c, k) = (i * 7919 % 1000, i * 104_729 % 997 + 3);
        if i % 10 == 0 {
            out.push_str(&format!(
                "pub struct S{i} {{ pub a: u64, b: Vec<u32>, c: Option<Box<S{i}>> }}\n\
                 impl S{i} {{ pub fn get(&self) -> u64 {{ self.a + self.b.len() as u64 }} }}\n"
            ));
        }
        let tail = if i == 0 { "x".to_string() } else { format!("f{}(x, &b[1..])", i - 1) };
        out.push_str(&format!(
            "fn f{i}(a: u64, b: &[u32]) -> u64 {{\n    let mut x = a ^ {c};\n    \
             for (j, v) in b.iter().enumerate() {{\n        if *v as u64 > x {{ x = x.wrapping_mul({k}).wrapping_add(j as u64); }}\n        \
             else {{ x ^= (*v as u64) << (j % 7); }}\n    }}\n    \
             match x % 3 {{ 0 => x, 1 => x / 2 + {c}, _ => {tail} }}\n}}\n"
        ));
    }
    out
}

struct Workload {
    name: &'static str,
    unit: &'static str,
    run: fn(&Inputs) -> u64,
}

const WORKLOADS: [Workload; 4] = [
    Workload {
        name: "deflate",
        unit: "the 1 MiB corpus at level 6",
        run: |i| miniz_oxide::deflate::compress_to_vec(&i.corpus, 6).len() as u64,
    },
    Workload {
        name: "sha256",
        unit: "1 MiB",
        run: |i| u64::from(toyos_crypto::hash::Sha256::digest(&i.corpus).expect("1 MiB is under SHA-256's bound")[0]),
    },
    Workload { name: "matmul", unit: "a 160x160 f64 product", run: matmul },
    Workload {
        name: "parse",
        unit: "the generated Rust program",
        run: |i| syn::parse_file(&i.rust).expect("the generated program parses").items.len() as u64,
    },
];

fn matmul(i: &Inputs) -> u64 {
    let mut c = vec![0f64; N * N];
    for r in 0..N {
        for k in 0..N {
            let a = i.a[r * N + k];
            for (out, b) in c[r * N..][..N].iter_mut().zip(&i.b[k * N..][..N]) {
                *out += a * b;
            }
        }
    }
    c.iter().sum::<f64>() as u64
}

/// The package's temperature and where its phases stop, where both are known.
struct Heat {
    sensor: sys::Sensor,
    limit: Limit,
}

impl Heat {
    fn read(&self) -> Option<i64> {
        (self.sensor.read)()
    }
}

pub fn suite() {
    let heat = match sys::package_sensor() {
        Ok(sensor) => {
            let limit = heat::limit(sensor.crit_mc, &sys::cpu_model());
            condition("crunch.sensor", &sensor.what);
            condition("crunch.stop_c", limit.stop_mc / 1000);
            condition("crunch.tjmax_c", limit.tjmax_mc.map_or("unknown".into(), |t| (t / 1000).to_string()));
            let source = match limit.source {
                Source::Sensor => "the sensor's critical temperature".to_string(),
                Source::Published(said) => said.to_string(),
                Source::Assumed => "assumed: nothing names this CPU's TjMax".to_string(),
            };
            condition("crunch.limit_from", source);
            Some(Heat { sensor, limit })
        }
        Err(why) => {
            condition("crunch.temperature", format!("unread: {why}"));
            None
        }
    };
    let inputs = Arc::new(Inputs::new());
    // The all-core phase runs only on a sensor that has answered once.
    let Some((heat, start_mc)) = heat.and_then(|heat| heat.read().map(|start| (heat, start))) else {
        run_phase("single", 1, &inputs, None);
        for workload in &WORKLOADS {
            unmeasured(&format!("crunch.all.{}", workload.name), "no package temperature to stop the all-core phase at", &[]);
        }
        return;
    };
    condition("crunch.start_c", start_mc as f64 / 1000.0);
    for (phase, threads) in [("single", 1), ("all", crate::cpus())] {
        run_phase(phase, threads, &inputs, Some(&heat));
        cool(phase, start_mc, &heat);
    }
}

/// Idle until the package reads within `heat::COOLED_WITHIN_MC` of where the
/// group started, or [`COOL_BOUND`].
fn cool(after: &str, start_mc: i64, heat: &Heat) {
    let t = Instant::now();
    let cooled = || heat.read().is_some_and(|now| heat::cooled(start_mc, now));
    while t.elapsed() < COOL_BOUND && !cooled() {
        thread::sleep(Duration::from_secs(1));
    }
    let now = heat.read().map_or("unread".into(), |n| (n as f64 / 1000.0).to_string());
    condition(&format!("crunch.cooled_after_{after}"), format!("{}ms to {now}C", t.elapsed().as_millis()));
}

/// Why a watched phase stopped early.
const RUNNING: u8 = 0;
const HOT: u8 = 1;
const UNREAD: u8 = 2;

/// Each workload for [`SLICE`] on `threads` threads, every unit timed, with
/// the temperature watched throughout: the phase stops at the limit, and at
/// a reading that fails, since then nothing guards it.
fn run_phase(phase: &str, threads: usize, inputs: &Arc<Inputs>, heat: Option<&Heat>) {
    let stopped = Arc::new(AtomicU8::new(RUNNING));
    let hottest = Arc::new(AtomicI64::new(i64::MIN));
    let done = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        if let Some(heat) = heat {
            let (stopped, hottest, done) = (stopped.clone(), hottest.clone(), done.clone());
            scope.spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    match heat.read() {
                        Some(t) => {
                            hottest.fetch_max(t, Ordering::Relaxed);
                            if heat.limit.too_hot(t) {
                                let _ = stopped.compare_exchange(RUNNING, HOT, Ordering::Relaxed, Ordering::Relaxed);
                            }
                        }
                        None => {
                            let _ = stopped.compare_exchange(RUNNING, UNREAD, Ordering::Relaxed, Ordering::Relaxed);
                        }
                    }
                    thread::sleep(POLL);
                }
            });
        }
        for workload in &WORKLOADS {
            let bench = format!("crunch.{phase}.{}", workload.name);
            match stopped.load(Ordering::Relaxed) {
                RUNNING => {}
                HOT => {
                    unmeasured(&bench, "the phase stopped once the package reached its limit", &stop_fields(heat, &hottest));
                    continue;
                }
                _ => {
                    unmeasured(&bench, "the phase stopped once the sensor stopped answering", &stop_fields(heat, &hottest));
                    continue;
                }
            }
            // The clock before the slice, beside the one after it: a slice
            // long enough to meet the package's power limit says so.
            crate::clock::settle();
            let around = crate::around::begin();
            let (mut ns, elapsed, placed) = slice(workload, threads, inputs, &stopped);
            around.end(&bench, &placed);
            let units = ns.len() as u64;
            let mut fields = vec![
                ("work_unit", workload.unit.to_string()),
                ("threads", threads.to_string()),
                ("units_per_s", format!("{:.3}", units as f64 * 1e9 / elapsed.max(1) as f64)),
                ("stopped", ["no", "hot", "unread"][usize::from(stopped.load(Ordering::Relaxed))].to_string()),
            ];
            fields.extend(stop_fields(heat, &hottest));
            result(&bench, "ns", &mut ns, &fields);
        }
        done.store(true, Ordering::Relaxed);
    });
}

fn stop_fields(heat: Option<&Heat>, hottest: &AtomicI64) -> Vec<(&'static str, String)> {
    match heat {
        Some(heat) => vec![
            ("hottest_c", (hottest.load(Ordering::Relaxed) as f64 / 1000.0).to_string()),
            ("stop_c", (heat.limit.stop_mc / 1000).to_string()),
        ],
        None => vec![("temperature", "unread".into())],
    }
}

/// `workload`'s units on `threads` threads until [`SLICE`] has passed or the
/// phase is stopped: every unit's ns, the slice's own, and each thread's CPU
/// as it began and as it ended.
fn slice(workload: &Workload, threads: usize, inputs: &Arc<Inputs>, stopped: &AtomicU8) -> (Vec<u64>, u64, Vec<(u32, u32)>) {
    let go = Barrier::new(threads + 1);
    let start = std::sync::OnceLock::new();
    let samples = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (go, start) = (&go, &start);
                scope.spawn(move || {
                    go.wait();
                    let start: &Instant = start.get().expect("the slice's start is set before it goes");
                    let began_on = crate::sys::current_cpu();
                    let mut ns = Vec::new();
                    while start.elapsed() < SLICE && stopped.load(Ordering::Relaxed) == RUNNING {
                        let t = Instant::now();
                        std::hint::black_box((workload.run)(std::hint::black_box(inputs)));
                        ns.push(ns_since(t));
                    }
                    (ns, (began_on, crate::sys::current_cpu()))
                })
            })
            .collect();
        start.set(Instant::now()).expect("one start");
        go.wait();
        workers.into_iter().map(|w| w.join().expect("a crunch worker panicked")).collect::<Vec<_>>()
    });
    let elapsed = ns_since(*start.get().expect("set"));
    let placed = samples.iter().map(|(_, placed)| *placed).collect();
    (samples.into_iter().flat_map(|(ns, _)| ns).collect(), elapsed, placed)
}
