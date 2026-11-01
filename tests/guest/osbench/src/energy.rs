//! The package's power, idle and under defined loads, read off the same
//! registers on both systems (`sys::power`): its energy counter and unit
//! (SDM Vol. 3B §17.10, PPR 55922's RAPL), its C-state residency where Intel
//! documents it, and each CPU's APERF/MPERF, core residency and, on AMD, core
//! energy. ToyOS reads them through `SYS_COUNTERS` under `Rights::TRACE`, and
//! Linux through `/dev/cpu/<n>/msr` as root.
//!
//! **First the machine is let go quiet** ([`osbench_rules::energy::quiet`]),
//! for at most [`QUIET_BOUND`]: a ToyOS boot starts the group while its
//! services are still starting. The gate measures CPU time alone; whether
//! the cores had reached their deepest idle state is each row's core C6/C7
//! note.
//!
//! Then each load runs for its span, read once a second by a round of every
//! CPU's counters and the package's (`power::round`), which reaches every CPU
//! on both systems: each row is that second's power, its residency and,
//! under a spin, its energy per [`crate::chunk`]. No figure is better higher
//! or lower by itself: a load that ran more work may draw more.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use osbench_rules::energy::{self, RANGE};

use crate::report::{note, result};
use crate::sys::{power, CpuPower, Refused, Round};

/// The longest the group waits for the machine to go quiet.
const QUIET_BOUND: Duration = Duration::from_secs(120);

/// The residency rows, in [`crate::sys::Package::residency`]'s order.
const STATES: [&str; 4] = ["pc2", "pc3", "pc6", "pc7"];

#[derive(Clone, Copy)]
enum Load {
    Idle,
    /// One thread sleeping this long and waking, over and over.
    Wake(Duration),
    /// This many threads each running [`crate::chunk`] back to back.
    Spin(usize),
}

pub fn suite() {
    #[cfg(target_os = "linux")]
    power::conditions();
    let loads = [
        ("energy.idle", 60, Load::Idle),
        ("energy.wake_10ms", 30, Load::Wake(Duration::from_millis(10))),
        ("energy.spin.t1", 30, Load::Spin(1)),
        ("energy.spin.all", 30, Load::Spin(crate::cpus())),
    ];
    if let Err(why) = power::round() {
        for (bench, ..) in loads {
            why.clone().report(&format!("{bench}.package"));
        }
        return;
    }
    wait_quiet();
    for (bench, seconds, load) in loads {
        let mut measured = Ok(());
        crate::around::ran_during(bench, || measured = row(bench, seconds, load));
        if let Err(why) = measured {
            why.report(&format!("{bench}.package"));
        }
    }
}

/// Seconds of the machine's CPU time, each read a second apart, until the
/// last few were each quiet or [`QUIET_BOUND`] has passed.
fn wait_quiet() {
    let start = Instant::now();
    let mut seconds = Vec::new();
    let mut before = match power::busy_ns() {
        Ok(ns) => ns,
        Err(why) => return note("energy.quiet", &[("unread", why.why())]),
    };
    while !energy::quiet(&seconds) && start.elapsed() < QUIET_BOUND {
        std::thread::sleep(Duration::from_secs(1));
        let now = match power::busy_ns() {
            Ok(now) => now,
            Err(why) => return note("energy.quiet", &[("unread", why.why())]),
        };
        // A ToyOS process that ended leaves the roster with its time.
        seconds.push(now.saturating_sub(before));
        before = now;
    }
    let last_us: Vec<String> = seconds.iter().rev().take(energy::QUIET_SECONDS).rev().map(|ns| (ns / 1000).to_string()).collect();
    note(
        "energy.quiet",
        &[
            ("quiet", energy::quiet(&seconds).to_string()),
            ("waited_s", start.elapsed().as_secs().to_string()),
            ("last_busy_us", last_us.join(",")),
        ],
    );
}

/// One thread's chunk count, on a line of its own.
#[repr(align(128))]
#[derive(Default)]
struct Slot(AtomicU64);

/// Run `load` for `seconds`, reading the package each second, and report.
fn row(bench: &str, seconds: u64, load: Load) -> Result<(), Refused> {
    if let Load::Spin(_) = load {
        crate::clock::settle();
    }
    let threads = match load {
        Load::Idle => 0,
        Load::Wake(_) => 1,
        Load::Spin(n) => n,
    };
    let stop = AtomicBool::new(false);
    let counts: Vec<Slot> = (0..threads).map(|_| Slot::default()).collect();
    let mut samples = Samples::default();
    let sampled = std::thread::scope(|s| {
        for slot in &counts {
            let stop = &stop;
            s.spawn(move || {
                while !stop.load(Relaxed) {
                    match load {
                        Load::Wake(period) => std::thread::sleep(period),
                        _ => {
                            crate::chunk();
                        }
                    }
                    slot.0.fetch_add(1, Relaxed);
                }
            });
        }
        let sampled = sample(seconds, &counts, matches!(load, Load::Spin(_)), &mut samples);
        stop.store(true, Relaxed);
        sampled
    });
    sampled?;
    let (first, last) = (&samples.first_cpus, &samples.last_cpus);
    let unit = samples.power_unit;
    let mut fields = vec![("seconds", seconds.to_string())];
    let done: u64 = counts.iter().map(|c| c.0.load(Relaxed)).sum();
    match load {
        Load::Idle => {}
        Load::Wake(_) => fields.push(("wakes", done.to_string())),
        Load::Spin(n) => {
            fields.push(("threads", n.to_string()));
            fields.push(("chunks_per_s", (done / seconds.max(1)).to_string()));
        }
    }
    if let Some(mw) = unit.and_then(|unit| cores_mw(first, last, unit, samples.span_ns)) {
        fields.push(("cores_mw", mw.to_string()));
    }
    let series: Vec<String> = samples.mw.iter().map(u64::to_string).collect();
    note(&format!("{bench}.series"), &[("mw", series.join(","))]);
    result(&format!("{bench}.package"), "mW", &mut samples.mw, &fields);
    for (state, permille) in STATES.iter().zip(&mut samples.residency) {
        if !permille.is_empty() {
            result(&format!("{bench}.{state}"), "permille", permille, &[]);
        }
    }
    if !samples.nj_per_chunk.is_empty() {
        result(&format!("{bench}.per_chunk"), "nJ", &mut samples.nj_per_chunk, &[]);
    }
    cpus(bench, first, last);
    Ok(())
}

#[derive(Default)]
struct Samples {
    mw: Vec<u64>,
    residency: [Vec<u64>; 4],
    nj_per_chunk: Vec<u64>,
    power_unit: Option<u64>,
    span_ns: u64,
    /// Each CPU's counters at the row's first and last rounds.
    first_cpus: Vec<CpuPower>,
    last_cpus: Vec<CpuPower>,
}

/// A reading of the package each second for `seconds`, into `out`, with the
/// energy per chunk where `per_chunk`.
fn sample(seconds: u64, counts: &[Slot], per_chunk: bool, out: &mut Samples) -> Result<(), Refused> {
    let chunks = || counts.iter().map(|c| c.0.load(Relaxed)).sum::<u64>();
    let Round { package: mut before, cpus } = power::round()?;
    out.first_cpus = cpus;
    let (start, mut at, mut done) = (Instant::now(), Instant::now(), chunks());
    out.power_unit = before.power_unit;
    for second in 1..=seconds {
        // The span is the measurement: each read is due a second after the start, not after the last read.
        std::thread::sleep((start + Duration::from_secs(second)).saturating_duration_since(Instant::now()));
        let Round { package: after, cpus } = power::round()?;
        out.last_cpus = cpus;
        let (now, done_now) = (Instant::now(), chunks());
        let ns = u64::try_from((now - at).as_nanos()).expect("a second under 584 years");
        if let (Some(e0), Some(e1), Some(unit)) = (before.energy, after.energy, after.power_unit) {
            let steps = energy::span(e0, e1, RANGE)
                .ok_or_else(|| Refused::Failed(format!("the energy counter read {e1:#x}, past 32 bits")))?;
            let uj = energy::microjoules(steps, unit);
            out.mw.push(energy::milliwatts(uj, ns));
            if per_chunk && done_now > done {
                out.nj_per_chunk.push(uj * 1000 / (done_now - done));
            }
        }
        let ticks = after.tsc.wrapping_sub(before.tsc);
        for (i, rows) in out.residency.iter_mut().enumerate() {
            if let (Some(r0), Some(r1)) = (before.residency[i], after.residency[i]) {
                rows.push(osbench_rules::window::permille(r1.wrapping_sub(r0), ticks));
            }
        }
        (before, at, done) = (after, now, done_now);
    }
    out.span_ns = u64::try_from(start.elapsed().as_nanos()).expect("a row under 584 years");
    Ok(())
}

/// The cores' power over the row where each core counts its own energy:
/// each core once, SMT siblings reading the same counter.
fn cores_mw(first: &[CpuPower], last: &[CpuPower], unit: u64, span_ns: u64) -> Option<u64> {
    let mut seen = std::collections::BTreeSet::new();
    let mut steps = 0;
    for b in last {
        let a = first.iter().find(|a| a.cpu == b.cpu)?;
        if seen.insert(b.core) {
            steps += energy::span(a.core_energy?, b.core_energy?, RANGE)?;
        }
    }
    (!seen.is_empty()).then(|| energy::milliwatts(energy::microjoules(steps, unit), span_ns))
}

/// One line a CPU: its APERF/MPERF ratio and its core's C6 and C7 residency
/// over the row, each against its own TSC, which the residency counts at
/// (SDM Vol. 4 Table 2-20).
fn cpus(bench: &str, first: &[CpuPower], last: &[CpuPower]) {
    let delta = |x: Option<u64>, y: Option<u64>| x.zip(y).map(|(x, y)| y.wrapping_sub(x));
    let shown = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
    for b in last {
        let Some(a) = first.iter().find(|a| a.cpu == b.cpu) else { continue };
        let ticks = b.tsc.wrapping_sub(a.tsc);
        let permille = |x, y| shown(delta(x, y).map(|d| osbench_rules::window::permille(d, ticks)));
        let ratio = delta(a.aperf, b.aperf)
            .zip(delta(a.mperf, b.mperf))
            .and_then(|(aperf, mperf)| osbench_rules::window::aperf_mperf_permille(aperf, mperf));
        note(
            &format!("{bench}.cpus"),
            &[
                ("cpu", b.cpu.to_string()),
                ("core", b.core.to_string()),
                ("aperf_mperf_permille", shown(ratio)),
                ("core_c6_permille", permille(a.core_c6, b.core_c6)),
                ("core_c7_permille", permille(a.core_c7, b.core_c7)),
            ],
        );
    }
}
