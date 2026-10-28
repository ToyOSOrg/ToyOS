//! What the machine's CPUs did across one measured row, printed beside it:
//! the CPUs the row's threads ran on, on both systems; and on ToyOS, where
//! the boot grants the counters, each CPU's busy fraction, the clock it ran
//! at, its core's C6 and C7 residency where the CPU counts them, and the
//! idle periods the kernel chose a deep state for. A row that moves with the
//! machine's idle states, or with where its threads ran, says so here.
//!
//! **Reading the counters kicks every CPU** (`toyos_abi::counters`), so they
//! are read around rows of seconds, where one wake is nothing, and never
//! around `cpu.clock`'s few milliseconds, which [`ran_during`] names from the
//! roster instead.

use std::time::Instant;

use crate::report::note;

/// The counters as a row began.
pub struct Around {
    began: Instant,
    #[cfg(target_os = "toyos")]
    before: Option<Vec<crate::sys::diag::CpuCounts>>,
}

/// Read the counters before a row.
pub fn begin() -> Around {
    Around {
        began: Instant::now(),
        #[cfg(target_os = "toyos")]
        before: crate::sys::diag::shared().and_then(|d| d.counters().ok()),
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
            match d.counters() {
                Ok(after) => cpus(bench, &before, &after, crate::ns_since(self.began)),
                Err(why) => note(&format!("{bench}.cpus"), &[("failed", why)]),
            }
        }
        #[cfg(not(target_os = "toyos"))]
        let _ = self.began;
    }
}

/// One line a CPU: what it did between `before` and `after`, `ns` apart.
#[cfg(target_os = "toyos")]
pub fn cpus(bench: &str, before: &[crate::sys::diag::CpuCounts], after: &[crate::sys::diag::CpuCounts], ns: u64) {
    let delta = |x: Option<u64>, y: Option<u64>| x.zip(y).map(|(x, y)| y.wrapping_sub(x));
    let shown = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
    let permille = |part: Option<u64>, whole: Option<u64>| shown(part.zip(whole).map(|(p, w)| p * 1000 / w.max(1)));
    for b in after {
        let Some(a) = before.iter().find(|a| a.cpu == b.cpu) else { continue };
        let stamp = delta(a.stamp, b.stamp);
        let (aperf, mperf) = (delta(a.aperf, b.aperf), delta(a.mperf, b.mperf));
        // MPERF counts at the TSC's rate while the CPU runs, so APERF/MPERF
        // of the TSC's rate is the clock it ran at.
        let tsc_mhz = stamp.map(|s| s * 1000 / ns.max(1));
        let run_mhz = aperf.zip(mperf).zip(tsc_mhz).map(|((a, m), t)| a * t / m.max(1));
        note(
            &format!("{bench}.cpus"),
            &[
                ("cpu", b.cpu.to_string()),
                ("apic", b.apic.to_string()),
                ("busy_permille", permille(mperf, stamp)),
                ("run_mhz", shown(run_mhz)),
                ("core_c6_permille", permille(delta(a.core_c6, b.core_c6), stamp)),
                ("core_c7_permille", permille(delta(a.core_c7, b.core_c7), stamp)),
                // Counted when a period ends, so one begun before the row counts whole: past 1000 is that.
                ("deep_idles", shown(delta(a.deep_idles, b.deep_idles))),
                ("deep_permille", permille(delta(a.deep_idle_nanos, b.deep_idle_nanos), Some(ns))),
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
