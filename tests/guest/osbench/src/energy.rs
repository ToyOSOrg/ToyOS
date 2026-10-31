//! The package's power, idle and under defined loads, read off the same
//! registers on both systems (`sys::power`): its energy counter and unit
//! (SDM Vol. 3B §17.10, PPR 55922's RAPL), its C-state residency where Intel
//! documents it, and each CPU's APERF/MPERF, core residency and, on AMD, core
//! energy. ToyOS reads them through `SYS_COUNTERS` under `Rights::TRACE`, and
//! Linux through `/dev/cpu/<n>/msr` as root.
//!
//! **First the machine is let go quiet** ([`osbench_rules::energy::quiet`]),
//! for at most [`QUIET_BOUND`]: a ToyOS boot starts the group while its
//! services are still starting, the ACPI server's idle states among them.
//!
//! Then each load runs for its span, and the package is read once a second:
//! each row is that second's power, its residency and, under a spin, its
//! energy per [`crate::chunk`]. Each CPU's counters are read at the load's
//! two ends, so Linux reads the package on the reader's own CPU inside a
//! second; ToyOS's every read kicks every CPU. No figure is better higher or lower by itself: a load that ran
//! more work may draw more.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use osbench_rules::energy::{self, RANGE};

use crate::report::{note, result};
use crate::sys::{power, CpuPower, Package, Refused};

/// The longest the group waits for the machine to go quiet.
const QUIET_BOUND: Duration = Duration::from_secs(120);

/// The residency rows, in [`Package::residency`]'s order.
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
    if let Err(why) = power::package() {
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
    let first = power::cpus()?;
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
    let last = power::cpus()?;
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
    if let Some(mw) = unit.and_then(|unit| cores_mw(&first, &last, unit, samples.span_ns)) {
        fields.push(("cores_mw", mw.to_string()));
    }
    for (name, steps) in [("pp0_mw", samples.plane_steps[0]), ("pp1_mw", samples.plane_steps[1])] {
        if let (Some(steps), Some(unit)) = (steps, unit) {
            fields.push((name, energy::milliwatts(energy::microjoules(steps, unit), samples.span_ns).to_string()));
        }
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
    cpus(bench, &first, &last);
    Ok(())
}

#[derive(Default)]
struct Samples {
    mw: Vec<u64>,
    residency: [Vec<u64>; 4],
    nj_per_chunk: Vec<u64>,
    power_unit: Option<u64>,
    span_ns: u64,
    /// Each plane's steps over the row, where both ends read it.
    plane_steps: [Option<u64>; 2],
}

/// A reading of the package each second for `seconds`, into `out`, with the
/// energy per chunk where `per_chunk`.
fn sample(seconds: u64, counts: &[Slot], per_chunk: bool, out: &mut Samples) -> Result<(), Refused> {
    let chunks = || counts.iter().map(|c| c.0.load(Relaxed)).sum::<u64>();
    let mut before: Package = power::package()?;
    let (start, mut at, mut done) = (Instant::now(), Instant::now(), chunks());
    out.power_unit = before.power_unit;
    for second in 1..=seconds {
        // The span is the measurement: each read is due a second after the start, not after the last read.
        std::thread::sleep((start + Duration::from_secs(second)).saturating_duration_since(Instant::now()));
        let after = power::package()?;
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
        for (i, steps) in out.plane_steps.iter_mut().enumerate() {
            if let (Some(p0), Some(p1)) = (before.planes[i], after.planes[i]) {
                *steps = Some(steps.unwrap_or(0) + energy::span(p0, p1, RANGE).unwrap_or(0));
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

/// A measurement scout, ToyOS-only and not for landing, on an image without
/// netstack, so the T14's I219 is left as firmware handed it over. Each step
/// is 30 s of idle; the toggles (`SYS_DEBUG` in a test-actuators kernel) are
/// cumulative and never undone:
///
/// 1. the PCIe power registers of every function, logged;
/// 2. the base, and one thread waking every 1 ms, 10 ms, 100 ms and 1 s;
/// 3. the I219 (00:1f.6) parked in D3hot;
/// 4. the NVMe's (04:00.0) LTR Mechanism Enable cleared;
/// 5. pipe A's planes disabled, then its transcoder;
/// 6. the registers again.
#[cfg(target_os = "toyos")]
pub fn scout() {
    use toyos_abi::syscall::{debug_action as da, debug_with};
    if let Err(why) = power::package() {
        return why.report("energy.scout.0_base.package");
    }
    wait_quiet();
    note("energy.scout", &[("functions", debug_with(da::PCI_POWER_REPORT, 0).to_string())]);
    let step = |bench: &str, load: Load| {
        let bench = format!("energy.scout.{bench}");
        let mut measured = Ok(());
        crate::around::ran_during(&bench, || measured = row(&bench, 30, load));
        if let Err(why) = measured {
            why.report(&format!("{bench}.package"));
        }
    };
    step("0_base", Load::Idle);
    for (name, ms) in [("1ms", 1), ("10ms", 10), ("100ms", 100), ("1s", 1000)] {
        step(&format!("0_wake_{name}"), Load::Wake(Duration::from_millis(ms)));
    }
    let toggles: [(&str, u64, u64); 4] = [
        ("1_i219_d3hot", da::PCI_PARK_D3HOT, 0x00fe),
        ("2_nvme_ltr_off", da::PCI_LTR_OFF, 0x0400),
        ("3_planes_off", da::DISPLAY_OFF, 1),
        ("4_transcoder_off", da::DISPLAY_OFF, 2),
    ];
    for (name, action, arg) in toggles {
        let answered = debug_with(action, arg);
        note("energy.scout", &[("step", name.to_string()), ("answered", answered.to_string())]);
        step(name, Load::Idle);
    }
    debug_with(da::PCI_POWER_REPORT, 0);
}

/// A read-only measurement scout, ToyOS-only and not for landing: the display
/// engine's state as the firmware's GOP left it, read by the kernel
/// (`SYS_DEBUG` `DISPLAY_READ`) once quiet, then a 30 s idle row, then read
/// again, so a register that moves at idle shows.
#[cfg(target_os = "toyos")]
pub fn display_scout() {
    use toyos_abi::syscall::{debug_action as da, debug_with};
    if let Err(why) = power::package() {
        return why.report("energy.display_scout.idle.package");
    }
    wait_quiet();
    note("energy.display_scout", &[("read", "0".to_string()), ("answered", debug_with(da::DISPLAY_READ, 0).to_string())]);
    let bench = "energy.display_scout.idle";
    let mut measured = Ok(());
    crate::around::ran_during(bench, || measured = row(bench, 30, Load::Idle));
    if let Err(why) = measured {
        why.report(&format!("{bench}.package"));
    }
    note("energy.display_scout", &[("read", "1".to_string()), ("answered", debug_with(da::DISPLAY_READ, 1).to_string())]);
}
