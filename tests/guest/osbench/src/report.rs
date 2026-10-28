//! The one output format: every line starts `osbench v=2 os=<os>` and is
//! otherwise `key=value` words, so a run's stdout is its data.

use std::sync::atomic::{AtomicBool, Ordering};

static REFUSED: AtomicBool = AtomicBool::new(false);

pub const OS: &str = if cfg!(target_os = "toyos") { "toyos" } else { "linux" };

/// A value as one word: whitespace becomes `_`.
pub fn word(value: impl std::fmt::Display) -> String {
    value.to_string().split_whitespace().collect::<Vec<_>>().join("_")
}

fn emit(body: &str) {
    println!("osbench v=2 os={OS} {body}");
}

/// A fact about the machine or the run that its numbers depend on.
pub fn condition(key: &str, value: impl std::fmt::Display) {
    emit(&format!("cond {key}={}", word(value)));
}

/// A benchmark that did not measure: its line says why, and the process exits
/// non-zero once its group has run.
pub fn refuse(bench: &str, why: impl std::fmt::Display) {
    REFUSED.store(true, Ordering::Relaxed);
    emit(&format!("bench={bench} refused={}", word(why)));
}

/// A benchmark that ran and kept no sample, with why: a result, which does
/// not fail its group, where the reason is the system's and not a fault.
pub fn unmeasured(bench: &str, why: &str, extra: &[(&str, String)]) {
    let mut line = format!("bench={bench} unmeasured={}", word(why));
    for (key, value) in extra {
        line.push_str(&format!(" {key}={}", word(value)));
    }
    emit(&line);
}

/// A facility this system does not offer, which is a result and not a
/// failure: its line says what was asked and what the system answered.
pub fn unsupported(bench: &str, why: impl std::fmt::Display) {
    emit(&format!("bench={bench} unsupported={}", word(why)));
}

/// An investigation's record, not a distribution: `<what> key=value...`.
pub fn note(what: &str, fields: &[(&str, String)]) {
    let mut line = format!("note={what}");
    for (key, value) in fields {
        line.push_str(&format!(" {key}={}", word(value)));
    }
    emit(&line);
}

pub fn any_refused() -> bool {
    REFUSED.load(Ordering::Relaxed)
}

/// One measured distribution, by nearest rank. A percentile the sample count
/// cannot resolve prints `-`: p99 needs 100 samples and p99.9 a thousand.
/// `mhz` is the reporting CPU's clock read just after the measurement
/// (`clock::mhz`). Where the benchmark's warm-up settled the clock first,
/// `mhz_settled` is the clock it reached, and `mhz_after` the clock a settle
/// just after the measurement reaches: a CPU that only idled meanwhile is
/// back at its clock, and one a power or heat limit holds is not.
/// `unsettled` names a settle that ran out its limit and took the clock it
/// had.
pub fn result(bench: &str, unit: &str, samples: &mut [u64], extra: &[(&str, String)]) {
    if samples.is_empty() {
        refuse(bench, "no samples");
        return;
    }
    samples.sort_unstable();
    let n = samples.len();
    let rank = |permille: usize| samples[(permille * n).div_ceil(1000) - 1];
    let resolved = |permille: usize, needs: usize| if n >= needs { rank(permille).to_string() } else { "-".into() };
    let mean = samples.iter().map(|&s| u128::from(s)).sum::<u128>() / n as u128;
    let mut line = format!(
        "bench={bench} unit={unit} n={n} min={} p50={} p90={} p99={} p999={} max={} mean={mean}",
        samples[0],
        rank(500),
        resolved(900, 10),
        resolved(990, 100),
        resolved(999, 1000),
        samples[n - 1],
    );
    for (key, value) in extra {
        line.push_str(&format!(" {key}={}", word(value)));
    }
    let after = crate::clock::mhz();
    if let Some(before) = crate::clock::take_settled() {
        crate::clock::settle();
        let settled_after = crate::clock::take_settled().expect("the settle just run");
        line.push_str(&format!(" mhz_settled={} mhz_after={}", before.mhz, settled_after.mhz));
        let unsettled: Vec<&str> =
            [("before", before.capped), ("after", settled_after.capped)].iter().filter(|(_, c)| *c).map(|(w, _)| *w).collect();
        if !unsettled.is_empty() {
            line.push_str(&format!(" unsettled={}", unsettled.join("+")));
        }
    }
    line.push_str(&format!(" mhz={after}"));
    emit(&line);
}

/// A group's first and last line: the judge reads the last as "it ran".
pub fn group(name: &str, phase: &str) {
    emit(&format!("group={name} {phase}"));
}
