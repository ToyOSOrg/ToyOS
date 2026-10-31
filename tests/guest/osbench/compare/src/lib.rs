//! osbench runs of ToyOS and Linux on one machine, compared.
//!
//! A run is one file of osbench's lines, a Linux run's stdout or a ToyOS
//! boot's log, from whatever prefix each line carries. A benchmark's value in
//! a run is its p50. Per machine and benchmark, each system's runs give the
//! median of their values and their run-to-run range, `(max - min) / median`:
//! Linux's range on a path is that path's noise floor. ToyOS's median over
//! Linux's is the ratio, and the exact two-sided Mann-Whitney test over the
//! two sets of runs says whether the difference is resolved at all: with four
//! runs each, complete separation gives p = 2/70. Each p is one row's,
//! uncorrected for the table's other rows. Better or worse is judged only
//! for a unit whose better direction is known ([`better`]); for any other
//! the verdict says which way ToyOS differs.
//!
//! Each row's clocks are told beside its verdict, since no program reads a
//! power or heat limit on both systems:
//!
//! - in how many runs the clock settled after the row was under nine tenths
//!   of the one settled before it ([`clock_fell`]), which a limit reached
//!   inside the row most likely explains;
//! - where the two systems' median clocks settled before the row lie more
//!   than a tenth apart ([`clocks_apart`]), which also catches a row that
//!   started already limited, as a fall cannot;
//! - in how many runs a settle ran out its time (`unsettled`), whose clocks
//!   say nothing either way.
//!
//! A row whose lines name their medium (`medium=`, the memory or the kind of
//! disk under a filesystem) is compared only where every run's medium is one:
//! a row written to an NVMe disk under one system and a USB stick under the
//! other has a verdict of what the two media are, not of the systems.
//!
//! A system's runs on one machine are one configuration ([`SETUP`]): runs that
//! disagree on it are refused, since their median would mix two, and each
//! machine's table states what each system's runs were taken under. What a
//! system chooses afresh at each boot ([`PER_RUN`]) is part of that system as
//! booted: it is stated run by run and never refused.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The test level a difference is resolved at.
pub const RESOLVED: f64 = 0.05;
/// The most runs the exact test enumerates the splits of.
pub const MAX_RUNS: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Os {
    Linux,
    Toyos,
}

impl Os {
    pub fn name(self) -> &'static str {
        match self {
            Os::Linux => "Linux",
            Os::Toyos => "ToyOS",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `clock_fell` as [`clock_fell`] judges the line's two clocks, unless
    /// either is `unsettled`; `settled` the clock settled before it; `medium`
    /// as the line names it.
    Measured {
        unit: String,
        p50: u64,
        clock_fell: bool,
        settled: Option<u64>,
        unsettled: bool,
        medium: Option<String>,
    },
    /// Unmeasured, unsupported or refused, with the run's reason.
    Not { kind: String, why: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub os: Os,
    pub conds: BTreeMap<String, String>,
    pub benches: BTreeMap<String, Outcome>,
}

impl Run {
    /// The machine a run is of: its CPU model.
    pub fn machine(&self) -> Option<&str> {
        self.conds.get("cpu_model").map(String::as_str)
    }
}

/// One run's lines. A benchmark a run reports in more than one group, as
/// every group's `cpu.clock`, is named `<bench>@<group>`, and the `k`th of
/// one name in a group `<name>#k`. A run with no
/// osbench line, two systems' lines or one group started twice is refused.
pub fn parse(text: &str) -> Result<Run, String> {
    let mut os = None;
    let mut conds = BTreeMap::new();
    let mut group = String::new();
    let mut started = std::collections::BTreeSet::new();
    let mut read: Vec<(String, String, Outcome)> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("osbench v=2 os=") else { continue };
        let mut words = line[at..].split_whitespace().skip(2);
        let line_os = match words.next() {
            Some("os=linux") => Os::Linux,
            Some("os=toyos") => Os::Toyos,
            other => return Err(format!("a line of no system osbench knows: {other:?}")),
        };
        if *os.get_or_insert(line_os) != line_os {
            return Err("a run holds two systems' lines".into());
        }
        let first = words.next().unwrap_or_default();
        if first == "cond" {
            if let Some((key, value)) = words.next().and_then(|w| w.split_once('=')) {
                conds.entry(key.to_string()).or_insert_with(|| value.to_string());
            }
            continue;
        }
        if let Some(name) = first.strip_prefix("group=") {
            if words.next() == Some("start") {
                if started.contains(name) {
                    return Err(format!("group {name} starts twice: a file holds one run"));
                }
                started.insert(name.to_string());
            }
            group = name.to_string();
            continue;
        }
        let Some(bench) = first.strip_prefix("bench=") else { continue };
        let fields: BTreeMap<&str, &str> = words.filter_map(|w| w.split_once('=')).collect();
        let outcome = if let Some(unit) = fields.get("unit") {
            let p50 = fields.get("p50").and_then(|v| v.parse().ok()).ok_or_else(|| format!("{bench}: no p50"))?;
            let mhz = |key: &str| fields.get(key).and_then(|v| v.parse().ok());
            let unsettled = fields.contains_key("unsettled");
            Outcome::Measured {
                unit: unit.to_string(),
                p50,
                clock_fell: !unsettled && clock_fell(mhz("mhz_settled"), mhz("mhz_after")),
                settled: mhz("mhz_settled"),
                unsettled,
                medium: fields.get("medium").map(|m| m.to_string()),
            }
        } else if let Some((kind, why)) =
            ["unmeasured", "unsupported", "refused"].iter().find_map(|k| fields.get(k).map(|why| (*k, *why)))
        {
            Outcome::Not { kind: kind.to_string(), why: why.to_string() }
        } else {
            return Err(format!("{bench}: neither a distribution nor a reason"));
        };
        read.push((group.clone(), bench.to_string(), outcome));
    }
    let mut benches = BTreeMap::new();
    for (i, (group, bench, outcome)) in read.iter().enumerate() {
        let same = |(g, b, _): &&(String, String, Outcome)| b == bench && (g == group || group.is_empty());
        let mut name = bench.clone();
        if read.iter().any(|(g, b, _)| b == bench && g != group) {
            name = format!("{name}@{group}");
        }
        if read.iter().filter(same).count() > 1 {
            name = format!("{name}#{}", read[..=i].iter().filter(same).count());
        }
        benches.insert(name, outcome.clone());
    }
    Ok(Run { os: os.ok_or("no osbench line")?, conds, benches })
}

/// Whether the clock fell across a measurement: the clock settled after it
/// under nine tenths of the clock settled before it. A line that settled no
/// clock says nothing either way.
pub fn clock_fell(settled: Option<u64>, after: Option<u64>) -> bool {
    matches!((settled, after), (Some(settled), Some(after)) if after * 10 < settled * 9)
}

/// Whether two systems' median settled clocks lie more than a tenth of the
/// higher apart.
pub fn clocks_apart(a: f64, b: f64) -> bool {
    (a - b).abs() * 10.0 > a.max(b)
}

pub fn median(values: &[u64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    assert!(n > 0, "a median of no values");
    if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        f64::midpoint(v[n / 2 - 1] as f64, v[n / 2] as f64)
    }
}

/// `(max - min) / median`, the spread of a set of runs; `None` about a
/// median of 0.
pub fn range(values: &[u64]) -> Option<f64> {
    let (min, max) = (values.iter().min().expect("values"), values.iter().max().expect("values"));
    let median = median(values);
    (median > 0.0).then(|| (max - min) as f64 / median)
}

/// The exact two-sided Mann-Whitney test of `a` against `b`: `p`, and
/// `least`, the smallest p these pooled values could give whichever runs
/// were whose. Ties count half a win. Over every split of the pooled values
/// into sets of the two sizes, `p` is the share of splits whose |2U - nm| is
/// at least the observed one: the distance from the centre, not twice the
/// one-sided p. Since the splits hold the observed ties, a tie can make a
/// split's mirror image less extreme. Then fewer splits reach the farthest
/// distance than the 2 of C(n + m, n) that untied values give, and `least`
/// is lower.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exact {
    pub p: f64,
    pub least: f64,
}

/// `None` past [`MAX_RUNS`] values or with an empty side.
pub fn mann_whitney(a: &[u64], b: &[u64]) -> Option<Exact> {
    let pooled: Vec<u64> = a.iter().chain(b).copied().collect();
    let total = pooled.len();
    if a.is_empty() || b.is_empty() || total > MAX_RUNS {
        return None;
    }
    // Twice U: two per pair the first set wins, one per tie.
    let doubled_u = |first: &[u64], second: &[u64]| -> i64 {
        let mut u = 0;
        for x in first {
            for y in second {
                u += match x.cmp(y) {
                    std::cmp::Ordering::Greater => 2,
                    std::cmp::Ordering::Equal => 1,
                    std::cmp::Ordering::Less => 0,
                };
            }
        }
        u
    };
    let centre = (a.len() * b.len()) as i64;
    let seen = (doubled_u(a, b) - centre).abs();
    let mut distances = Vec::new();
    let (mut first, mut second) = (Vec::with_capacity(total), Vec::with_capacity(total));
    for mask in 0u32..(1 << total) {
        if mask.count_ones() as usize != a.len() {
            continue;
        }
        first.clear();
        second.clear();
        for (i, v) in pooled.iter().enumerate() {
            if mask & (1 << i) != 0 { first.push(*v) } else { second.push(*v) }
        }
        distances.push((doubled_u(&first, &second) - centre).abs());
    }
    let share = |at_least: i64| distances.iter().filter(|&&d| d >= at_least).count() as f64 / distances.len() as f64;
    let farthest = *distances.iter().max().expect("a split");
    Some(Exact { p: share(seen), least: share(farthest) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Better {
    Lower,
    Higher,
}

/// Which way `unit` is better, for the units whose direction is known: a
/// time and a rate. A clock, a power or a share is no better higher or lower.
pub fn better(unit: &str) -> Option<Better> {
    match unit {
        "ns" => Some(Better::Lower),
        "MB/s" => Some(Better::Higher),
        _ => None,
    }
}

/// The condition every run of a machine must agree on for a benchmark's
/// runs to be compared, and how many of its leading words must agree: the
/// peer's link kind and driver, and the internet link's kind, not what
/// varies by the run.
const PATHS: [(&str, &str, usize); 2] = [("lan.", "lan.peer_link", 2), ("internet.", "internet.link", 1)];

/// The conditions every run of one system on one machine must share: the
/// kernel, the CPU's frequency request, what serves each filesystem the `fs`
/// and `persist` rows use, and the panel, its light, the radios and the links
/// the `energy` rows draw beside.
pub const SETUP: [&str; 17] = [
    "kernel",
    "cpufreq_driver",
    "governor",
    "epp",
    "min_khz",
    "max_khz",
    "boost",
    "intel_no_turbo",
    "platform_profile",
    "smt_active",
    "fs.mem.served_by",
    "fs.disk.served_by",
    "persist.served_by",
    "energy.display",
    "energy.backlight",
    "energy.radios",
    "energy.links",
];

/// The conditions a system chooses at each boot, stated per run: the clock
/// every timestamp is read from, which Linux's watchdog can move off the TSC
/// in one boot and not the next.
pub const PER_RUN: [&str; 1] = ["clocksource"];

/// `os`'s [`PER_RUN`] conditions, each as `key=<run 1's>,<run 2's>...` in the
/// runs' order, `absent` for a run that names none.
fn per_run(runs: &[&Run], os: Os) -> Vec<String> {
    let runs: Vec<&&Run> = runs.iter().filter(|r| r.os == os).collect();
    PER_RUN
        .iter()
        .filter(|key| runs.iter().any(|r| r.conds.contains_key(**key)))
        .map(|key| {
            let said: Vec<&str> = runs.iter().map(|r| r.conds.get(*key).map_or("absent", String::as_str)).collect();
            format!("{key}={}", said.join(","))
        })
        .collect()
}

/// `os`'s setup on these runs, as `key=value` in [`SETUP`]'s order, or the
/// first key its runs disagree on.
fn setup(runs: &[&Run], os: Os) -> Result<Vec<String>, String> {
    let runs: Vec<&&Run> = runs.iter().filter(|r| r.os == os).collect();
    let mut said = Vec::new();
    for key in SETUP {
        let values: std::collections::BTreeSet<Option<&String>> = runs.iter().map(|r| r.conds.get(key)).collect();
        match values.iter().collect::<Vec<_>>()[..] {
            [] | [None] => {}
            [Some(value)] => said.push(format!("{key}={value}")),
            _ => {
                let seen: Vec<&str> = values.iter().map(|v| v.map_or("absent", String::as_str)).collect();
                return Err(format!("the {} runs disagree on {key}: {}", os.name(), seen.join(" / ")));
            }
        }
    }
    Ok(said)
}

fn path(said: &str, words: usize) -> String {
    said.split(['_', ' ']).take(words).collect::<Vec<_>>().join(" ")
}

/// Every machine's comparison, as Markdown: one section per machine, one row
/// per benchmark either system reported.
pub fn compare(runs: &[Run]) -> Result<String, String> {
    let mut machines: BTreeMap<&str, Vec<&Run>> = BTreeMap::new();
    for run in runs {
        machines.entry(run.machine().ok_or("a run that names no cpu_model")?).or_default().push(run);
    }
    let mut out = String::new();
    for (machine, runs) in machines {
        let count = |os| runs.iter().filter(|r| r.os == os).count();
        let paths: Vec<(&str, &str, std::collections::BTreeSet<String>)> = PATHS
            .iter()
            .map(|&(prefix, cond, words)| {
                (prefix, cond, runs.iter().filter_map(|r| r.conds.get(cond)).map(|said| path(said, words)).collect())
            })
            .collect();
        writeln!(out, "## {machine}\n").unwrap();
        write!(out, "Runs: Linux {}, ToyOS {}.", count(Os::Linux), count(Os::Toyos)).unwrap();
        for (_, cond, seen) in &paths {
            let seen = if seen.is_empty() { "none".to_string() } else { seen.iter().cloned().collect::<Vec<_>>().join(", ") };
            write!(out, " {cond}: {seen}.").unwrap();
        }
        writeln!(out, "\n").unwrap();
        for os in [Os::Linux, Os::Toyos] {
            let said = setup(&runs, os).map_err(|why| format!("{machine}: {why}"))?;
            if !said.is_empty() {
                writeln!(out, "{} setup: {}.\n", os.name(), said.join(", ")).unwrap();
            }
            let each = per_run(&runs, os);
            if !each.is_empty() {
                writeln!(out, "{} per run: {}.\n", os.name(), each.join(", ")).unwrap();
            }
        }
        writeln!(out, "| bench | unit | Linux p50s | Linux median | Linux range | ToyOS p50s | ToyOS median | ToyOS range | ToyOS/Linux | p | verdict |").unwrap();
        writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|").unwrap();
        let names: std::collections::BTreeSet<&String> = runs.iter().flat_map(|r| r.benches.keys()).collect();
        for name in names {
            let split = paths.iter().find(|(prefix, _, seen)| name.starts_with(prefix) && seen.len() > 1);
            let row = match split {
                Some((_, cond, _)) => Err(format!("the runs disagree on {cond}")),
                None => Ok(compare_one(&runs, name)),
            };
            match row {
                Ok(row) => writeln!(out, "| {name} | {row} |").unwrap(),
                Err(why) => writeln!(out, "| {name} | | | | | | | | | | {why} |").unwrap(),
            }
        }
        writeln!(out, "\nEach p is its row's alone, uncorrected for the table's other rows.\n").unwrap();
    }
    Ok(out)
}

/// One benchmark's cells after its name.
fn compare_one(runs: &[&Run], name: &str) -> String {
    let mut unit = None;
    let mut values: BTreeMap<Os, Vec<u64>> = BTreeMap::new();
    let mut fell: BTreeMap<Os, usize> = BTreeMap::new();
    let mut unsettled_runs: BTreeMap<Os, usize> = BTreeMap::new();
    let mut settled: BTreeMap<Os, Vec<u64>> = BTreeMap::new();
    let mut media: BTreeMap<Os, std::collections::BTreeSet<Option<String>>> = BTreeMap::new();
    let mut not: BTreeMap<Os, Vec<String>> = BTreeMap::new();
    for run in runs {
        match run.benches.get(name) {
            Some(Outcome::Measured { unit: u, p50, clock_fell, settled: mhz, unsettled, medium }) => {
                if unit.get_or_insert(u.as_str()) != u {
                    return "| | | | | | | | | units differ between runs".into();
                }
                values.entry(run.os).or_default().push(*p50);
                *fell.entry(run.os).or_default() += usize::from(*clock_fell);
                *unsettled_runs.entry(run.os).or_default() += usize::from(*unsettled);
                settled.entry(run.os).or_default().extend(mhz.filter(|_| !*unsettled));
                media.entry(run.os).or_default().insert(medium.clone());
            }
            Some(Outcome::Not { kind, why }) => not.entry(run.os).or_default().push(format!("{kind}: {why}")),
            None => not.entry(run.os).or_default().push("absent".into()),
        }
    }
    let side = |os: Os| -> [String; 3] {
        match values.get(&os) {
            Some(v) => [
                v.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
                format!("{:.0}", median(v)),
                range(v).map_or_else(|| "-".into(), |r| format!("{:.1}%", r * 100.0)),
            ],
            None => [String::new(), "-".into(), "-".into()],
        }
    };
    let [lp, lm, lr] = side(Os::Linux);
    let [tp, tm, tr] = side(Os::Toyos);
    // A side that names no medium is a medium of its own beside one that does.
    let all_media: std::collections::BTreeSet<&Option<String>> = media.values().flatten().collect();
    let differ = all_media.len() > 1 && all_media.iter().any(|m| m.is_some());
    let (ratio, p, verdict) = match (values.get(&Os::Linux), values.get(&Os::Toyos)) {
        (Some(_), Some(_)) if differ => {
            let named = |os: Os| {
                let said = media.get(&os).into_iter().flatten().map(|m| m.as_deref().unwrap_or("unnamed"));
                said.collect::<Vec<_>>().join("+")
            };
            ("-".into(), "-".into(), format!("the media differ: Linux {}, ToyOS {}", named(Os::Linux), named(Os::Toyos)))
        }
        (Some(l), Some(t)) => {
            let ratio = median(t) / median(l);
            let exact = mann_whitney(t, l);
            let verdict = match exact {
                None => "too many runs for the exact test".to_string(),
                Some(Exact { p, .. }) if p <= RESOLVED => {
                    let higher = median(t) > median(l);
                    match better(unit.unwrap_or_default()) {
                        Some(way) => format!("ToyOS {}", if higher == (way == Better::Higher) { "better" } else { "worse" }),
                        None => format!("ToyOS {}", if higher { "higher" } else { "lower" }),
                    }
                }
                Some(Exact { least, .. }) if least > RESOLVED => format!("too few runs to resolve (least p {least:.3})"),
                Some(_) => "not resolved".to_string(),
            };
            let p = exact.map(|e| e.p);
            (format!("{ratio:.2}"), p.map_or_else(|| "-".into(), |p| format!("{p:.3}")), verdict)
        }
        _ => ("-".into(), "-".into(), String::new()),
    };
    let mut verdict = verdict;
    let mut note = |said: String| {
        let sep = if verdict.is_empty() { "" } else { "; " };
        verdict = format!("{verdict}{sep}{said}");
    };
    if let (Some(l), Some(t)) = (settled.get(&Os::Linux).filter(|v| !v.is_empty()), settled.get(&Os::Toyos).filter(|v| !v.is_empty())) {
        let (l, t) = (median(l), median(t));
        if clocks_apart(l, t) {
            note(format!("settled clocks apart: Linux {l:.0} MHz, ToyOS {t:.0} MHz"));
        }
    }
    for (counts, said) in [(&fell, "the clock fell"), (&unsettled_runs, "the clock did not settle")] {
        for (os, n) in counts.iter().filter(|(_, n)| **n > 0) {
            let of = values.get(os).map_or(0, Vec::len);
            note(format!("{said} in {n} of {of} {} run(s)", os.name()));
        }
    }
    for (os, reasons) in &not {
        let mut distinct = reasons.clone();
        distinct.sort();
        distinct.dedup();
        let sep = if verdict.is_empty() { "" } else { "; " };
        verdict = format!("{verdict}{sep}{} {} run(s) {}", os.name(), reasons.len(), distinct.join(" / "));
    }
    format!("{} | {lp} | {lm} | {lr} | {tp} | {tm} | {tr} | {ratio} | {p} | {verdict}", unit.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(os: &str, lines: &[&str]) -> Run {
        let mut text = format!("osbench v=2 os={os} cond cpu_model=Bench_CPU\n");
        for line in lines {
            text.push_str(&format!("[12.5 cpu3 osbench] osbench v=2 os={os} {line}\n"));
        }
        parse(&text).unwrap()
    }

    #[test]
    fn a_run_reads_each_benchmarks_p50_or_its_reason() {
        let r = run(
            "toyos",
            &[
                "bench=lan.wired.tcp.rtt.64 unit=ns n=2000 min=1 p50=812 p90=900 p99=990 p999=- max=2000 mean=850 mhz=4000",
                "bench=lan.wifi.tcp.upload unsupported=ToyOS_has_no_Wi-Fi_driver",
                "cond lan.peer_link=wireless_driver=iwlwifi_signal_dbm=-52",
                "group=lan done secs=40",
            ],
        );
        assert_eq!(r.os, Os::Toyos);
        assert_eq!(r.machine(), Some("Bench_CPU"));
        assert_eq!(
            r.benches["lan.wired.tcp.rtt.64"],
            Outcome::Measured { unit: "ns".into(), p50: 812, clock_fell: false, settled: None, unsettled: false, medium: None }
        );
        assert_eq!(
            r.benches["lan.wifi.tcp.upload"],
            Outcome::Not { kind: "unsupported".into(), why: "ToyOS_has_no_Wi-Fi_driver".into() }
        );
        assert_eq!(r.conds["lan.peer_link"], "wireless_driver=iwlwifi_signal_dbm=-52");
    }

    #[test]
    fn repeated_names_are_told_apart_and_a_file_of_two_runs_is_refused() {
        let twice = "osbench v=2 os=linux group=lan start\n".repeat(2);
        assert!(parse(&twice).unwrap_err().contains("twice"));
        let groups = ["sched", "fair"].map(|g| format!("osbench v=2 os=linux group={g} start\nosbench v=2 os=linux bench=cpu.clock unit=MHz n=1 min=1 p50=4000 max=1 mean=1\n")).concat();
        let run = parse(&groups).unwrap();
        assert_eq!(run.benches.keys().collect::<Vec<_>>(), ["cpu.clock@fair", "cpu.clock@sched"]);
        let jobs = "osbench v=2 os=linux bench=wake.storm unit=ns n=5 min=1 p50=63 max=1 mean=1\n".repeat(2);
        assert_eq!(parse(&jobs).unwrap().benches.keys().collect::<Vec<_>>(), ["wake.storm#1", "wake.storm#2"]);
        let mixed = "osbench v=2 os=linux cond cpus=8\nosbench v=2 os=toyos cond cpus=8\n";
        assert!(parse(mixed).unwrap_err().contains("two systems"));
        assert!(parse("nothing here\n").is_err());
    }

    #[test]
    fn the_exact_test_matches_its_tables() {
        // Complete separation has two splits as extreme as itself, of C(n+m, n).
        let p = |a: &[u64], b: &[u64]| mann_whitney(a, b).unwrap().p;
        assert!((p(&[1, 2, 3, 4], &[5, 6, 7, 8]) - 2.0 / 70.0).abs() < 1e-12);
        assert!((p(&[8, 7, 6, 5], &[1, 2, 3, 4]) - 2.0 / 70.0).abs() < 1e-12);
        assert!((p(&[1, 2, 3], &[4, 5, 6]) - 0.1).abs() < 1e-12);
        assert!((p(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10]) - 2.0 / 252.0).abs() < 1e-12);
        // Interleaved sets are as central as a split can be.
        assert_eq!(p(&[1, 3, 5, 7], &[2, 4, 6, 8]), p(&[2, 4, 6, 8], &[1, 3, 5, 7]));
        assert!(p(&[1, 4, 5, 8], &[2, 3, 6, 7]) > 0.9);
        // Every value tied is no evidence of a difference.
        assert_eq!(p(&[5, 5, 5], &[5, 5, 5]), 1.0);
        // One tie across the sets: U = 0.5 of 16 has 4 of the 70 splits at least as far out.
        assert!((p(&[1, 2, 3, 5], &[5, 6, 7, 8]) - 4.0 / 70.0).abs() < 1e-12);
        // A tie is half a win: [2, 2] against [1, 2] is U = 3 of 4, and every
        // split is as far from the centre's 2, where ties counted whole would
        // put half of them nearer.
        assert_eq!(p(&[2, 2], &[1, 2]), 1.0);
        assert_eq!(mann_whitney(&[], &[1]), None);
        assert_eq!(mann_whitney(&[1; 11], &[2; 10]), None);
    }

    #[test]
    fn median_and_range_of_a_set_of_runs() {
        assert_eq!(median(&[3, 1, 2]), 2.0);
        assert_eq!(median(&[4, 1, 3, 2]), 2.5);
        assert!((range(&[90, 100, 110, 100]).unwrap() - 0.2).abs() < 1e-12);
        assert_eq!(range(&[7]), Some(0.0));
        assert_eq!(range(&[0, 0, 3]), None, "no spread relative to a median of 0");
    }

    #[test]
    fn the_least_p_is_the_farthest_splits_share() {
        let least = |a: &[u64], b: &[u64]| mann_whitney(a, b).unwrap().least;
        // Distinct values: complete separation either way, 2 of C(n + m, n).
        assert!((least(&[1, 2, 3, 4], &[5, 6, 7, 8]) - 2.0 / 70.0).abs() < 1e-12);
        assert!((least(&[9, 1, 5, 3], &[2, 8, 4, 6]) - 2.0 / 70.0).abs() < 1e-12);
        assert!((least(&[1, 2, 3, 4], &[5, 6, 7]) - 2.0 / 35.0).abs() < 1e-12);
        assert_eq!(least(&[1], &[2]), 1.0);
        assert_eq!(least(&[1], &[2, 3, 4]), 0.5);
        // Three tied runs of four: only the three lowest values set apart are
        // that far out, so one split of 35, the session's T14 upload.
        let upload = mann_whitney(&[41, 48, 31], &[113, 113, 114, 113]).unwrap();
        assert!((upload.p - 1.0 / 35.0).abs() < 1e-12 && (upload.least - 1.0 / 35.0).abs() < 1e-12, "{upload:?}");
    }

    fn rtt(os: &str, p50: u64) -> Run {
        run(os, &[&format!("bench=lan.wired.tcp.rtt.64 unit=ns n=2000 min=1 p50={p50} max=9 mean=5")])
    }

    #[test]
    fn separated_runs_resolve_a_difference_and_overlapping_ones_do_not() {
        let mut runs: Vec<Run> = [100, 104, 98, 102].iter().map(|v| rtt("linux", *v)).collect();
        runs.extend([150, 149, 155, 153].iter().map(|v| rtt("toyos", *v)));
        let table = compare(&runs).unwrap();
        let row = table.lines().find(|l| l.starts_with("| lan.wired.tcp.rtt.64")).unwrap();
        assert!(row.contains("| 101 | 5.9% |"), "{row}");
        assert!(row.contains("| 1.50 | 0.029 | ToyOS worse |"), "{row}");

        let mut runs: Vec<Run> = [100, 104, 98, 101].iter().map(|v| rtt("linux", *v)).collect();
        runs.extend([99, 103, 97, 102].iter().map(|v| rtt("toyos", *v)));
        let table = compare(&runs).unwrap();
        assert!(table.contains("| not resolved |"), "{table}");
    }

    #[test]
    fn counts_that_cannot_reach_the_level_say_so_and_not_that_nothing_differs() {
        let mut runs: Vec<Run> = [100, 104, 98, 102].iter().map(|v| rtt("linux", *v)).collect();
        runs.extend([150, 149, 155].iter().map(|v| rtt("toyos", *v)));
        let table = compare(&runs).unwrap();
        assert!(table.contains("| 0.057 | too few runs to resolve (least p 0.057) |"), "{table}");
        // Ties can let the same counts resolve: the test decides, not the count.
        let mbps = |os: &str, v: u64| run(os, &[&format!("bench=lan.wired.tcp.upload unit=MB/s n=5 min=1 p50={v} max=9 mean=5")]);
        let mut runs: Vec<Run> = [113, 113, 114, 113].iter().map(|v| mbps("linux", *v)).collect();
        runs.extend([41, 48, 31].iter().map(|v| mbps("toyos", *v)));
        assert!(compare(&runs).unwrap().contains("| 0.029 | ToyOS worse |"));
    }

    #[test]
    fn a_unit_of_no_known_direction_is_told_only_which_way_it_differs() {
        let power = |os: &str, v: u64| run(os, &[&format!("bench=energy.idle.package unit=mW n=1 min=1 p50={v} max=9 mean=5")]);
        let mut runs: Vec<Run> = [900, 910, 920, 930].iter().map(|v| power("linux", *v)).collect();
        runs.extend([1500, 1510, 1520, 1530].iter().map(|v| power("toyos", *v)));
        let table = compare(&runs).unwrap();
        assert!(table.contains("| ToyOS higher |"), "{table}");
        assert!(!table.contains("better") && !table.contains("worse"), "{table}");
    }

    #[test]
    fn a_throughputs_higher_value_is_the_better_one() {
        let mbps = |os: &str, v: u64| run(os, &[&format!("bench=lan.wired.tcp.upload unit=MB/s n=5 min=1 p50={v} max=9 mean=5")]);
        let mut runs: Vec<Run> = [40, 41, 42, 43].iter().map(|v| mbps("linux", *v)).collect();
        runs.extend([50, 51, 52, 53].iter().map(|v| mbps("toyos", *v)));
        assert!(compare(&runs).unwrap().contains("| ToyOS better |"));
    }

    #[test]
    fn a_system_that_cannot_measure_says_so_beside_the_others_figures() {
        let mut runs: Vec<Run> = [10, 11].iter().map(|v| run("linux", &[&format!("bench=lan.wifi.tcp.connect unit=ns n=50 min=1 p50={v} max=9 mean=5")])).collect();
        runs.push(run("toyos", &["bench=lan.wifi.tcp.connect unsupported=no_Wi-Fi"]));
        let table = compare(&runs).unwrap();
        assert!(table.contains("| ToyOS 1 run(s) unsupported: no_Wi-Fi |"), "{table}");
    }

    #[test]
    fn lan_runs_whose_peers_were_on_different_links_are_not_compared() {
        let peer = |os: &str, said: &str, v: u64| {
            run(os, &[&format!("cond lan.peer_link={said}"), &format!("bench=lan.wired.tcp.rtt.64 unit=ns n=1 min=1 p50={v} max=1 mean=1")])
        };
        let runs = [
            peer("linux", "wireless_driver=iwlwifi_signal_dbm=-52", 10),
            peer("toyos", "wireless_driver=iwlwifi_signal_dbm=-60", 12),
        ];
        assert!(!compare(&runs).unwrap().contains("different links"), "a signal is no other link");
        let runs = [peer("linux", "wireless_driver=iwlwifi_signal_dbm=-52", 10), peer("toyos", "wired_driver=e1000e_speed_mbps=1000", 12)];
        assert!(compare(&runs).unwrap().contains("| the runs disagree on lan.peer_link |"));
    }

    #[test]
    fn a_row_whose_clock_fell_in_a_run_says_so_per_system() {
        assert!(clock_fell(Some(4289), Some(1884)));
        assert!(!clock_fell(Some(4289), Some(3861)), "a tenth down is not a fall");
        assert!(clock_fell(Some(4289), Some(3860)));
        assert!(!clock_fell(None, Some(1396)), "no settled clock, no judgement");
        let deflate = |os: &str, p50: u64, after: u64| {
            run(os, &[&format!("bench=crunch.single.deflate unit=ns n=63 min=1 p50={p50} max=1 mean=1 mhz_settled=4289 mhz_after={after} mhz=1300")])
        };
        let mut runs: Vec<Run> = [(65, 4200), (66, 4210), (64, 4190), (65, 4200)].iter().map(|&(p, m)| deflate("linux", p, m)).collect();
        runs.extend([(63, 1884), (66, 1396), (63, 4200), (64, 4280)].iter().map(|&(p, m)| deflate("toyos", p, m)));
        let table = compare(&runs).unwrap();
        let row = table.lines().find(|l| l.starts_with("| crunch.single.deflate")).unwrap();
        assert!(row.ends_with("not resolved; the clock fell in 2 of 4 ToyOS run(s) |"), "{row}");
    }

    #[test]
    fn clocks_settled_apart_and_settles_cut_short_are_told() {
        assert!(clocks_apart(4289.0, 1884.0));
        assert!(!clocks_apart(4186.0, 3788.0), "9.5% apart is within a tenth");
        assert!(clocks_apart(4186.0, 3767.0));
        let at = |os: &str, v: u64, settled: u64, unsettled: &str| {
            run(os, &[&format!("bench=fs.mem.stat unit=ns n=2000 min=1 p50={v} max=1 mean=1 mhz_settled={settled} mhz_after=1000{unsettled} mhz=1")])
        };
        let mut runs: Vec<Run> = [10, 11, 12, 13].iter().map(|v| at("linux", *v, 4200, "")).collect();
        runs.extend([20, 21, 22].iter().map(|v| at("toyos", *v, 1884, "")));
        runs.push(at("toyos", 23, 4200, " unsettled=after"));
        let row = compare(&runs).unwrap().lines().find(|l| l.starts_with("| fs.mem.stat")).unwrap().to_string();
        assert!(row.contains("settled clocks apart: Linux 4200 MHz, ToyOS 1884 MHz"), "{row}");
        assert!(row.contains("the clock fell in 4 of 4 Linux run(s)"), "{row}");
        assert!(row.contains("the clock fell in 3 of 4 ToyOS run(s); the clock did not settle in 1 of 4 ToyOS run(s) |"), "{row}");
    }

    #[test]
    fn a_row_on_two_media_is_no_comparison_of_the_systems() {
        let fsync = |os: &str, v: u64, medium: &str| {
            run(os, &[&format!("bench=persist.fsync_clean unit=ns n=200 min=1 p50={v} max=1 mean=1 medium={medium}")])
        };
        let mut runs: Vec<Run> = [67, 68, 66, 67].iter().map(|v| fsync("linux", *v, "nvme")).collect();
        runs.extend([1687, 1690, 1700, 1695].iter().map(|v| fsync("toyos", *v, "usb")));
        let row = compare(&runs).unwrap().lines().find(|l| l.starts_with("| persist.fsync_clean")).unwrap().to_string();
        assert!(row.ends_with("| - | - | the media differ: Linux nvme, ToyOS usb |"), "{row}");
        let mut runs: Vec<Run> = [67, 68, 66, 67].iter().map(|v| fsync("linux", *v, "nvme")).collect();
        runs.extend([70, 71, 72, 73].iter().map(|v| fsync("toyos", *v, "nvme")));
        assert!(compare(&runs).unwrap().contains("| ToyOS worse |"), "one medium is a comparison");
        let mut runs: Vec<Run> = [67, 68, 66].iter().map(|v| fsync("linux", *v, "nvme")).collect();
        runs.push(fsync("linux", 69, "sata"));
        runs.extend([70, 71, 72, 73].iter().map(|v| fsync("toyos", *v, "nvme")));
        assert!(compare(&runs).unwrap().contains("the media differ: Linux nvme+sata, ToyOS nvme"), "nor one system's runs on two");
        let mut runs: Vec<Run> = [67, 68, 66, 67].iter().map(|v| fsync("linux", *v, "nvme")).collect();
        runs.extend([70, 71, 72, 73].iter().map(|v| run("toyos", &[&format!("bench=persist.fsync_clean unit=ns n=200 min=1 p50={v} max=1 mean=1")])));
        assert!(compare(&runs).unwrap().contains("the media differ: Linux nvme, ToyOS unnamed"), "nor a named medium and none");
    }

    #[test]
    fn a_systems_runs_on_two_setups_are_refused_and_one_setup_is_stated() {
        let under = |os: &str, governor: &str| {
            run(os, &[&format!("cond governor={governor}"), "cond clocksource=tsc", "bench=time.now unit=ps n=15 min=1 p50=14 max=1 mean=1"])
        };
        let mixed = [under("linux", "performance*16"), under("linux", "schedutil*16"), under("toyos", "absent")];
        let why = compare(&mixed).unwrap_err();
        assert!(why.contains("the Linux runs disagree on governor: performance*16 / schedutil*16"), "{why}");
        let one = [under("linux", "performance*16"), under("linux", "performance*16")];
        let table = compare(&one).unwrap();
        assert!(table.contains("Linux setup: governor=performance*16."), "{table}");
        let absent = [under("linux", "performance*16"), run("linux", &["bench=time.now unit=ps n=15 min=1 p50=14 max=1 mean=1"])];
        assert!(compare(&absent).unwrap_err().contains("disagree on governor: absent / performance*16"));
    }

    #[test]
    fn a_clocksource_each_boot_chose_is_stated_per_run_and_never_refused() {
        let booted = |clocksource: &str| {
            run("linux", &[&format!("cond clocksource={clocksource}"), "bench=time.now unit=ps n=15 min=1 p50=14 max=1 mean=1"])
        };
        let runs = [booted("hpet"), booted("tsc"), booted("hpet"), run("toyos", &["bench=time.now unit=ps n=15 min=1 p50=9 max=1 mean=1"])];
        let table = compare(&runs).unwrap();
        assert!(table.contains("Linux per run: clocksource=hpet,tsc,hpet."), "{table}");
        assert!(!table.contains("ToyOS per run"), "{table}");
    }

    #[test]
    fn internet_runs_that_left_by_different_kinds_of_link_are_not_compared() {
        let left = |os: &str, said: &str| {
            run(os, &[&format!("cond internet.link={said}"), "bench=internet.https.rtt unit=ns n=1 min=1 p50=9 max=1 mean=1"])
        };
        let same = [left("linux", "wired_driver=e1000e_speed_mbps=1000"), left("toyos", "wired_netstack's_one_Ethernet_card")];
        assert!(!compare(&same).unwrap().contains("disagree"), "both wired, by any driver");
        let split = [left("linux", "wireless_driver=iwlwifi_signal_dbm=-50"), left("toyos", "wired_netstack's_one_Ethernet_card")];
        assert!(compare(&split).unwrap().contains("| the runs disagree on internet.link |"));
    }
}
