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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Measured { unit: String, p50: u64 },
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
/// one name in a group, as `fair.flat`'s jobs, `<name>#k`. A run with no
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
            Outcome::Measured { unit: unit.to_string(), p50 }
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
    let mut not: BTreeMap<Os, Vec<String>> = BTreeMap::new();
    for run in runs {
        match run.benches.get(name) {
            Some(Outcome::Measured { unit: u, p50 }) => {
                if unit.get_or_insert(u.as_str()) != u {
                    return "| | | | | | | | | units differ between runs".into();
                }
                values.entry(run.os).or_default().push(*p50);
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
    let (ratio, p, verdict) = match (values.get(&Os::Linux), values.get(&Os::Toyos)) {
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
    for (os, reasons) in &not {
        let mut distinct = reasons.clone();
        distinct.sort();
        distinct.dedup();
        let who = if *os == Os::Linux { "Linux" } else { "ToyOS" };
        let sep = if verdict.is_empty() { "" } else { "; " };
        verdict = format!("{verdict}{sep}{who} {} run(s) {}", reasons.len(), distinct.join(" / "));
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
        assert_eq!(r.benches["lan.wired.tcp.rtt.64"], Outcome::Measured { unit: "ns".into(), p50: 812 });
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
        let jobs = "osbench v=2 os=linux bench=fair.flat unit=permille n=5 min=1 p50=63 max=1 mean=1 job=build\n".repeat(2);
        assert_eq!(parse(&jobs).unwrap().benches.keys().collect::<Vec<_>>(), ["fair.flat#1", "fair.flat#2"]);
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
        let power = |os: &str, v: u64| run(os, &[&format!("bench=energy.idle_60s.package unit=mW n=1 min=1 p50={v} max=9 mean=5")]);
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
