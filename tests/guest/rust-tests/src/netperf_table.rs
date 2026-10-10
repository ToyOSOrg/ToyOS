//! MEASUREMENT ONLY, never lands: the per-MB table of `netperf.rs`'s lines,
//! std alone, for the job on Linux and for the harness's judge.

use std::fmt::Write as _;

pub const PREFIX: &str = "https_download: ";

/// A `key=value` word's value in `line`.
pub fn field(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

/// Copies between the socket and the job, in the job: ureq's read into
/// rustls's deframer, rustls's plaintext into ureq's buffer, ureq's body into
/// the job's chunk.
const APP_COPIES: f64 = 3.0;

/// The per-MB table of every `perf run` line in `log`, each row tagged `M`
/// (a counter's difference over the bytes) or `D` (a bench, a subtraction or
/// a model).
pub fn table(log: &str) -> String {
    let said = |key: &str| -> Vec<&str> {
        log.lines().filter_map(|l| l.find(PREFIX).map(|at| &l[at + PREFIX.len()..])).filter(|l| l.starts_with(key)).collect()
    };
    // A download's stream ends after the bench, wherever its line lands
    // against the job's own.
    let benched = log.lines().position(|l| l.contains("https_download: bench ")).unwrap_or(0);
    let profs: Vec<&str> = log
        .lines()
        .skip(benched)
        .filter_map(|l| l.find("netstack: prof ").map(|at| &l[at + "netstack: prof ".len()..]))
        .filter(|l| field(l, "pipe_w_bytes").unwrap_or(0.0) > 1e6)
        .collect();
    let mut out = String::new();
    let Some(idle) = said("perf idle").first().copied() else {
        return "netperf: no idle line, no table\n".into();
    };
    let bench = said("bench").first().copied().unwrap_or("");
    let g = |line: &str, key: &str| field(line, key).unwrap_or(f64::NAN);
    let (idle_s, tsc_hz, cpus) = (g(idle, "secs"), g(idle, "tsc_hz"), g(idle, "cpus"));
    let _ = writeln!(out, "netperf: idle {idle_s:.3} s on {cpus} cpus, tsc {:.3} GHz", tsc_hz / 1e9);
    let _ = writeln!(
        out,
        "netperf: idle rates: C0 {:.2} ms/s, {:.2} Mcyc/s aperf, eff {:.3} GHz",
        g(idle, "mperf") / tsc_hz * 1e3 / idle_s,
        g(idle, "aperf") / 1e6 / idle_s,
        g(idle, "aperf") / g(idle, "mperf") * tsc_hz / 1e9
    );
    let _ = writeln!(out, "netperf: {bench}");
    for (k, run) in said("perf run").iter().enumerate() {
        let (bytes, secs) = (g(run, "bytes"), g(run, "secs"));
        let mb = bytes / 1e6;
        let ghz = g(run, "aperf") / g(run, "mperf") * tsc_hz / 1e9;
        let row = |out: &mut String, tag: char, what: &str, value: f64, unit: &str| {
            let _ = writeln!(out, "  {tag}  {what:<48} {value:>12.3} {unit}");
        };
        let _ = writeln!(out, "netperf: {run}");
        let _ = writeln!(out, "netperf: run {} per MB ({mb:.2} MB in {secs:.3} s, {:.1} Mb/s)", g(run, "run"), mb * 8.0 / secs);
        row(&mut out, 'M', "system C0 (sum mperf / tsc_hz)", g(run, "mperf") / tsc_hz * 1e3 / mb, "ms");
        row(&mut out, 'M', "system cycles (sum aperf)", g(run, "aperf") / 1e6 / mb, "Mcyc");
        row(&mut out, 'M', "system effective clock (aperf / mperf)", ghz, "GHz");
        let idle_aperf = g(idle, "aperf") / idle_s * secs;
        let idle_mperf = g(idle, "mperf") / idle_s * secs;
        row(&mut out, 'D', "system C0 less the idle window's rate", (g(run, "mperf") - idle_mperf) / tsc_hz * 1e3 / mb, "ms");
        let busy_mcyc = (g(run, "aperf") - idle_aperf) / 1e6 / mb;
        row(&mut out, 'D', "system cycles less the idle window's rate", busy_mcyc, "Mcyc");
        let app_ms = g(run, "app.cpu_ns") / 1e6 / mb;
        row(&mut out, 'M', "app cpu (its own accounting)", app_ms, "ms");
        let app_mcyc = app_ms * ghz;
        row(&mut out, 'D', "app cycles (cpu x effective clock)", app_mcyc, "Mcyc");
        if !g(run, "app.syscalls").is_nan() {
            row(&mut out, 'M', "app syscalls", g(run, "app.syscalls") / mb, "/MB");
            row(&mut out, 'M', "app time in syscalls (its waits included)", g(run, "app.syscall_ns") / 1e6 / mb, "ms");
            row(&mut out, 'M', "app runqueue wait", g(run, "app.runq_ns") / 1e6 / mb, "ms");
        }
        let tls = g(bench, "aead_aperf_per_b");
        let sha = g(bench, "sha_aperf_per_b");
        let copy = g(bench, "memcpy_aperf_per_b") * APP_COPIES;
        row(&mut out, 'D', "app TLS decrypt (aead bench x bytes)", tls, "Mcyc");
        row(&mut out, 'D', "app SHA-256 (bench x bytes)", sha, "Mcyc");
        row(&mut out, 'D', "app copies (3 x memcpy bench x bytes)", copy, "Mcyc");
        let sys_mcyc = g(run, "app.syscall_ns") / 1e6 / mb * ghz;
        if !sys_mcyc.is_nan() {
            row(&mut out, 'D', "app syscalls (time x effective clock)", sys_mcyc, "Mcyc");
            row(&mut out, 'D', "app rest (cycles less the four above)", app_mcyc - tls - sha - copy - sys_mcyc, "Mcyc");
        } else {
            row(&mut out, 'D', "app rest (cycles less the three above)", app_mcyc - tls - sha - copy, "Mcyc");
        }
        if let Some(prof) = profs.get(k) {
            let ns_ms = g(prof, "cpu_ns") / 1e6 / mb;
            row(&mut out, 'M', "netstack cpu (its own accounting)", ns_ms, "ms");
            row(&mut out, 'D', "netstack cycles (cpu x effective clock)", ns_ms * ghz, "Mcyc");
            row(&mut out, 'M', "netstack time in syscalls (its waits included)", g(prof, "syscall_ns") / 1e6 / mb, "ms");
            for w in prof.split_whitespace() {
                let Some((name, v)) = w.split_once('=') else { continue };
                let Ok(v) = v.parse::<f64>() else { continue };
                if name.starts_with("cy_") {
                    row(&mut out, 'M', &format!("netstack {name} (tsc)"), v / tsc_hz * 1e3 / mb, "ms");
                } else if !matches!(name, "cpu_ns" | "syscall_ns" | "runq_ns" | "wall_ns") {
                    row(&mut out, 'M', &format!("netstack {name}"), v / mb, "/MB");
                }
            }
            let kernel = busy_mcyc - app_mcyc - ns_ms * ghz;
            row(&mut out, 'D', "kernel outside both tasks (busy less app, netstack)", kernel, "Mcyc");
        } else {
            row(&mut out, 'D', "kernel and every other task (busy less app)", busy_mcyc - app_mcyc, "Mcyc");
        }
        for w in run.split_whitespace() {
            let Some((name, v)) = w.split_once('=') else { continue };
            let Ok(v) = v.parse::<f64>() else { continue };
            if let Some(name) = name.strip_prefix("k.") {
                if name.ends_with("_cycles") {
                    row(&mut out, 'M', &format!("kernel {name} (tsc)"), v / tsc_hz * 1e3 / mb, "ms");
                } else {
                    row(&mut out, 'M', &format!("kernel {name}"), v / mb, "/MB");
                }
            } else if let Some(name) = name.strip_prefix("lx.") {
                if name.ends_with("_ms") {
                    row(&mut out, 'M', &format!("linux {name}"), v / mb, "ms");
                } else {
                    row(&mut out, 'M', &format!("linux {name}"), v / mb, "/MB");
                }
            }
        }
    }
    out
}
