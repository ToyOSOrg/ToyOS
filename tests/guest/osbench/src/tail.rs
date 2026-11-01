//! The tail: where a millisecond goes. Investigation output as much as
//! numbers, so beside each distribution it prints every outlier with its
//! time, in the clock the system's own log uses, and the CPU it ran on.
//!
//! - `tail.wake.periodic.60s`: round 1's periodic wake (500 µs, absolute
//!   targets), for 60 s, and every wake later than [`OUTLIER`].
//! - `tail.stolen.10s`: a spinner per CPU reading the clock for 10 s; a gap
//!   between two reads is time the thread did not run, an interrupt's or
//!   another thread's or the firmware's. Every gap past [`GAP`] is counted
//!   and every one past [`OUTLIER`] printed.
//! - ToyOS only, where the boot grants the observation rights (`diag`): the
//!   wake again for 30 s, and at each late one the scheduler's diary around
//!   it, every CPU's firmware interrupts (SMI) and clock (APERF/MPERF) either
//!   side of the run, and each process's CPU time over it. Linux has no
//!   unprivileged equivalent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::report::{note, result};
use crate::{ns_since, sys};

const PERIOD: Duration = Duration::from_micros(500);
/// A wake this late is printed whole.
const OUTLIER: u64 = 1_000_000;
/// A clock gap this long is counted as time taken from a spinner.
const GAP: u64 = 20_000;
/// The most outliers one run prints.
const PRINTED: usize = 200;

/// A late wake: when it was due, how late, and the CPU before and after, as
/// [`sys::current_cpu`] names them.
struct Late {
    target_ns: u64,
    late_ns: u64,
    slept_on: u32,
    woke_on: u32,
}

fn log_s(monotonic_ns: u64) -> String {
    let at = sys::log_time_ns(monotonic_ns);
    format!("{}.{:06}", at / 1_000_000_000, at % 1_000_000_000 / 1000)
}

pub fn suite() {
    #[cfg(target_os = "toyos")]
    let diag = sys::diag::open();
    #[cfg(target_os = "toyos")]
    let before = diag.as_ref().map(crate::around::read);

    let late = periodic("tail.wake.periodic.60s", Duration::from_secs(60), &mut |_| {});
    print_outliers("tail.wake.periodic.60s", &late);

    #[cfg(target_os = "toyos")]
    if let (Some(d), Some(before)) = (diag.as_ref(), before) {
        counters_over("tail.wake.periodic.60s", before, d);
    }

    stolen(Duration::from_secs(10));

    #[cfg(target_os = "toyos")]
    match diag {
        Some(d) if d.has_diary() => diary_run(d),
        Some(_) => note("tail.diary", &[("absent", "the capability carries no TRACE".into())]),
        None => note("tail.diary", &[("absent", "this boot endows no capability".into())]),
    }
}

/// The periodic wake for `span`, its distribution reported as `bench`, and
/// every outlier; `on_late` runs after each outlier is recorded, and the
/// targets start again after it, so one stall is one outlier and what
/// `on_late` spends delays no later wake. Targets and wakes are read on
/// [`sys::monotonic_ns`], the clock `Instant` reads on both systems. A run
/// that has not taken its samples in twice its span ends there, with the
/// samples it has.
fn periodic(bench: &str, span: Duration, on_late: &mut dyn FnMut(&Late)) -> Vec<Late> {
    const WARMUP: u64 = 200;
    let period = PERIOD.as_nanos() as u64;
    let samples = (span.as_nanos() as u64 / period) as usize;
    let mut lateness = Vec::with_capacity(samples);
    let mut outliers = Vec::new();
    let began = sys::monotonic_ns();
    let give_up = began + 2 * span.as_nanos() as u64;
    let mut start = began;
    let mut i = 0u64;
    while lateness.len() < samples && sys::monotonic_ns() < give_up {
        i += 1;
        let target_ns = start + period * i;
        let slept_on = sys::current_cpu();
        if let Some(left) = target_ns.checked_sub(sys::monotonic_ns()) {
            thread::sleep(Duration::from_nanos(left));
        }
        let woke = sys::monotonic_ns();
        let late_ns = woke.saturating_sub(target_ns);
        if i <= WARMUP && start == began {
            continue;
        }
        lateness.push(late_ns);
        if late_ns > OUTLIER {
            let late = Late { target_ns, late_ns, slept_on, woke_on: sys::current_cpu() };
            on_late(&late);
            outliers.push(late);
            start = sys::monotonic_ns();
            i = 0;
        }
    }
    let over = outliers.len();
    result(
        bench,
        "ns",
        &mut lateness,
        &[
            ("period_ns", period.to_string()),
            ("late_over_1ms", over.to_string()),
            ("ran_ms", ((sys::monotonic_ns() - began) / 1_000_000).to_string()),
        ],
    );
    outliers
}

/// Each outlier as a note, and the spacing between them: a source that
/// strikes on a period shows as gaps that agree.
fn print_outliers(bench: &str, late: &[Late]) {
    let (slept, woke) = (format!("slept_{}", sys::CPU_NAMING), format!("woke_{}", sys::CPU_NAMING));
    for (i, l) in late.iter().take(PRINTED).enumerate() {
        note(
            &format!("{bench}.outlier"),
            &[
                ("i", i.to_string()),
                ("log_s", log_s(l.target_ns)),
                ("late_us", (l.late_ns / 1000).to_string()),
                (&slept, l.slept_on.to_string()),
                (&woke, l.woke_on.to_string()),
            ],
        );
    }
    let mut gaps: Vec<u64> = late.windows(2).map(|w| w[1].target_ns.saturating_sub(w[0].target_ns) / 1000).collect();
    if !gaps.is_empty() {
        result(&format!("{bench}.outlier_spacing"), "us", &mut gaps, &[]);
    }
}

/// A spinner per CPU reading the clock for `span`: every gap past [`GAP`]
/// between two of one thread's reads, ns, with the outliers printed.
fn stolen(span: Duration) {
    const BENCH: &str = "tail.stolen.10s";
    let threads = crate::cpus();
    let start = Arc::new(Barrier::new(threads));
    let stop = Arc::new(AtomicBool::new(false));
    let found: Arc<Mutex<Vec<(u64, u64, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let spinners: Vec<_> = (0..threads)
        .map(|_| {
            let (start, stop, found) = (start.clone(), stop.clone(), found.clone());
            thread::spawn(move || {
                let mut mine = Vec::new();
                start.wait();
                let mut last = sys::monotonic_ns();
                while !stop.load(Ordering::Relaxed) {
                    let now = sys::monotonic_ns();
                    let gap = now.saturating_sub(last);
                    if gap > GAP {
                        mine.push((last, gap, sys::current_cpu()));
                    }
                    last = now;
                }
                found.lock().expect("no spinner panics holding it").extend(mine);
            })
        })
        .collect();
    let t = Instant::now();
    thread::sleep(span);
    stop.store(true, Ordering::Relaxed);
    let ran = ns_since(t);
    for s in spinners {
        s.join().expect("a spinner panicked");
    }
    let mut gaps = std::mem::take(&mut *found.lock().expect("joined"));
    gaps.sort_unstable_by_key(|g| g.0);
    let stolen: u64 = gaps.iter().map(|g| g.1).sum();
    let permille = stolen * 1000 / (ran * threads as u64).max(1);
    let mut lengths: Vec<u64> = gaps.iter().map(|g| g.1).collect();
    let extra = [
        ("threads", threads.to_string()),
        ("gap_floor_ns", GAP.to_string()),
        ("stolen_permille", permille.to_string()),
        ("over_1ms", gaps.iter().filter(|g| g.1 > OUTLIER).count().to_string()),
    ];
    if lengths.is_empty() {
        lengths.push(0);
    }
    result(BENCH, "ns", &mut lengths, &extra);
    for (i, (at, gap, cpu)) in gaps.iter().filter(|g| g.1 > OUTLIER).take(PRINTED).enumerate() {
        note(
            &format!("{BENCH}.outlier"),
            &[
                ("i", i.to_string()),
                ("log_s", log_s(*at)),
                ("gap_us", (gap / 1000).to_string()),
                (sys::CPU_NAMING, cpu.to_string()),
            ],
        );
    }
}

/// What each CPU did between `before` and now ([`crate::around`]).
#[cfg(target_os = "toyos")]
fn counters_over(bench: &str, before: Result<crate::around::Reading, String>, d: &sys::diag::Diag) {
    match (before, crate::around::read(d)) {
        (Ok(before), Ok(after)) => crate::around::cpus(&format!("{bench}.counters"), &before, &after),
        (Err(why), _) | (_, Err(why)) => note(&format!("{bench}.counters"), &[("failed", why)]),
    }
}

/// The wake for 30 s with the diary read at each outlier: every record from
/// 2 ms before the missed target to the wake, as `offset_us:event:pid/tid`,
/// by the kernel's index of the CPU it was written on, the CPUs the sleeper
/// slept and woke on first; then each process's CPU time over the run and the
/// counters across it.
#[cfg(target_os = "toyos")]
fn diary_run(mut d: sys::diag::Diag) {
    const BENCH: &str = "tail.diag.periodic.30s";
    const SHOWN: usize = 16;
    let before = crate::around::read(&d);
    let names_before = d.roster();
    d.diary();
    let mut shown = 0usize;
    let mut lost_total = 0u64;
    // Printed after the run: a line logged inside it is work the log's
    // writers do while the wake is measured.
    let mut notes: Vec<(String, Vec<(&str, String)>)> = Vec::new();
    let late = {
        let d = &mut d;
        periodic(BENCH, Duration::from_secs(30), &mut |l: &Late| {
            if shown >= SHOWN {
                return;
            }
            let (records, lost, failed) = d.diary();
            lost_total += lost;
            let (from, to) = (l.target_ns.saturating_sub(2_000_000), l.target_ns + l.late_ns);
            let mut by_cpu: std::collections::BTreeMap<u16, Vec<String>> = std::collections::BTreeMap::new();
            for (at, entry) in records.iter().filter(|(at, _)| (from..=to).contains(at)) {
                let offset = *at as i64 - l.target_ns as i64;
                by_cpu.entry(entry.cpu).or_default().push(format!("{}:{}", offset / 1000, sys::diag::describe(entry)));
            }
            notes.push((
                format!("{BENCH}.outlier"),
                vec![
                    ("i", shown.to_string()),
                    ("log_s", log_s(l.target_ns)),
                    ("late_us", (l.late_ns / 1000).to_string()),
                    ("slept_cpu", l.slept_on.to_string()),
                    ("woke_cpu", l.woke_on.to_string()),
                    ("records", records.len().to_string()),
                    ("lost", lost.to_string()),
                    ("read_failed", failed.unwrap_or_else(|| "no".into())),
                ],
            ));
            for (cpu, events) in by_cpu {
                let first = [l.slept_on, l.woke_on].contains(&u32::from(cpu));
                let keep = if first { 120 } else { 24 };
                let dropped = events.len().saturating_sub(keep);
                let tail: Vec<String> = events.into_iter().rev().take(keep).rev().collect();
                notes.push((
                    format!("{BENCH}.diary"),
                    vec![
                        ("i", shown.to_string()),
                        ("cpu", cpu.to_string()),
                        ("older_not_shown", dropped.to_string()),
                        ("events", tail.join(",")),
                    ],
                ));
            }
            shown += 1;
        })
    };
    for (what, fields) in &notes {
        note(what, fields);
    }
    note(&format!("{BENCH}.summary"), &[("outliers", late.len().to_string()), ("diary_lost", lost_total.to_string())]);
    let names_after = d.roster();
    let mut by_process: std::collections::BTreeMap<(u32, String), u64> = std::collections::BTreeMap::new();
    for (&(pid, tid), (name, cpu_after)) in &names_after {
        let cpu_before = names_before.get(&(pid, tid)).map_or(0, |(_, ns)| *ns);
        *by_process.entry((pid, name.clone())).or_default() += cpu_after.saturating_sub(cpu_before);
    }
    for ((pid, name), ns) in by_process.into_iter().filter(|(_, ns)| *ns > 0) {
        note(&format!("{BENCH}.cpu_time"), &[("pid", pid.to_string()), ("name", name), ("us", (ns / 1000).to_string())]);
    }
    counters_over(BENCH, before, &d);
}
