//! What the machine's CPUs did across one measured row, printed beside it:
//! the CPUs the row's threads ran on, on both systems; and on ToyOS, where
//! the boot grants the counters, each CPU's APERF/MPERF ratio, its core's
//! C6 and C7 residency where the CPU counts them, and the deep idle periods
//! that ended, over the readings' own window. A row that moves with the
//! machine's idle states, or with where its threads ran, says so here.
//!
//! **Reading the counters kicks every CPU** (`toyos_abi::counters`), so they
//! are read around rows of seconds, where one wake is nothing, and never
//! around `cpu.clock`'s few milliseconds, which [`ran_during`] names from the
//! roster instead.

use crate::report::note;

/// The counters as a row began.
pub struct Around {
    #[cfg(target_os = "toyos")]
    before: Option<Reading>,
}

/// Read the counters before a row.
pub fn begin() -> Around {
    Around {
        #[cfg(target_os = "toyos")]
        before: crate::sys::diag::shared().and_then(|d| read(d).ok()),
    }
}

impl Around {
    /// Note what the CPUs did since [`begin`], for `bench`, whose threads ran
    /// on `placed`: each thread's CPU as it began and as it ended, by
    /// [`crate::sys::current_cpu`]'s naming.
    pub fn end(self, bench: &str, placed: &[(u32, u32)]) {
        let placed: Vec<String> = placed.iter().map(|(from, to)| format!("{from}>{to}")).collect();
        note(&format!("{bench}.placement"), &[(crate::sys::CPU_NAMING, placed.join(","))]);
        #[cfg(target_os = "toyos")]
        if let (Some(before), Some(d)) = (self.before, crate::sys::diag::shared()) {
            match read(d) {
                Ok(after) => cpus(bench, &before, &after),
                Err(why) => note(&format!("{bench}.cpus"), &[("failed", why)]),
            }
        }
    }
}

/// One counters read, with when its call began and what it cost.
#[cfg(target_os = "toyos")]
pub struct Reading {
    pub cpus: Vec<crate::sys::diag::CpuCounts>,
    called: std::time::Instant,
    cost_ns: u64,
}

#[cfg(target_os = "toyos")]
pub fn read(d: &crate::sys::diag::Diag) -> Result<Reading, String> {
    let called = std::time::Instant::now();
    let cpus = d.counters()?;
    Ok(Reading { cpus, called, cost_ns: crate::ns_since(called) })
}

/// One line a CPU: what it did between `before` and `after`. Every figure
/// is of the readings' own window, off the CPU's counter in each, and a CPU
/// whose readings are not the row's is refused by name
/// ([`osbench_rules::window`]). Core residency counts at the TSC's rate (SDM
/// Vol. 4 Table 2-20), the window's own counter; APERF/MPERF is printed as
/// the ratio, the one figure of the pair either vendor defines. Deep idle is
/// counted as the periods that ended in the window: the kernel credits a
/// period's time when it ends, so its time is no fraction of the window.
#[cfg(target_os = "toyos")]
pub fn cpus(bench: &str, before: &Reading, after: &Reading) {
    use osbench_rules::window;
    let page = toyos_abi::clock::page();
    let to_ns = |ticks: u64| toyos_abi::clock::ticks_to_nanos(ticks, page.period_fs);
    let span = window::Span {
        row_ns: u64::try_from(after.called.duration_since(before.called).as_nanos()).expect("a row under 584 years")
            + after.cost_ns,
        reads_ns: before.cost_ns + after.cost_ns,
    };
    let delta = |x: Option<u64>, y: Option<u64>| x.zip(y).map(|(x, y)| y.wrapping_sub(x));
    let shown = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
    for b in &after.cpus {
        let Some(a) = before.cpus.iter().find(|a| a.cpu == b.cpu) else { continue };
        let (ticks, window_ns) = match window::of(a.stale || b.stale, a.stamp, b.stamp, span, to_ns) {
            Ok(window) => window,
            Err(why) => {
                note(
                    &format!("{bench}.cpus"),
                    &[("cpu", b.cpu.to_string()), ("refused", why.to_string()), ("row_us", (span.row_ns / 1000).to_string())],
                );
                continue;
            }
        };
        let permille = |part: Option<u64>| shown(part.map(|p| window::permille(p, ticks)));
        let ratio = delta(a.aperf, b.aperf).zip(delta(a.mperf, b.mperf)).and_then(|(a, m)| window::aperf_mperf_permille(a, m));
        note(
            &format!("{bench}.cpus"),
            &[
                ("cpu", b.cpu.to_string()),
                ("apic", b.apic.to_string()),
                ("window_us", (window_ns / 1000).to_string()),
                ("row_us", (span.row_ns / 1000).to_string()),
                ("reads_us", (span.reads_ns / 1000).to_string()),
                ("aperf_mperf_permille", shown(ratio)),
                ("core_c6_permille", permille(delta(a.core_c6, b.core_c6))),
                ("core_c7_permille", permille(delta(a.core_c7, b.core_c7))),
                ("deep_ended", shown(delta(a.deep_idles, b.deep_idles))),
                ("kicks", shown(delta(a.kicks, b.kicks))),
                ("smi", shown(delta(a.smi, b.smi))),
            ],
        );
    }
}

/// Run `measure`, and note each process that ran on any CPU meanwhile, with
/// its CPU time: on ToyOS from the roster, read on this CPU before and after,
/// which wakes no other CPU and so leaves a short measurement as it was. A
/// clock held below its one-core bin is then named by what ran beside it.
pub fn ran_during(bench: &str, measure: impl FnOnce()) {
    #[cfg(target_os = "toyos")]
    let before = crate::sys::diag::shared().map(|d| d.roster());
    measure();
    #[cfg(target_os = "toyos")]
    if let (Some(before), Some(d)) = (before, crate::sys::diag::shared()) {
        let mut by_process: std::collections::BTreeMap<(u32, String), u64> = std::collections::BTreeMap::new();
        for (&(pid, tid), (name, after)) in &d.roster() {
            let was = before.get(&(pid, tid)).map_or(0, |(_, ns)| *ns);
            *by_process.entry((pid, name.clone())).or_default() += after.saturating_sub(was);
        }
        for ((pid, name), ns) in by_process.into_iter().filter(|(_, ns)| *ns > 0) {
            note(&format!("{bench}.ran"), &[("pid", pid.to_string()), ("name", name), ("us", (ns / 1000).to_string())]);
        }
    }
    #[cfg(not(target_os = "toyos"))]
    let _ = bench;
}
