//! Merging what a sharded run measured into the profile the next one reads.
//!
//! A runner is a fresh clone and has no `target/test-durations`, so
//! `longest_first` prices every test the same and `Shard::keep`'s LPT
//! degenerates to round-robin. `tests/test-durations` is the answer and it is
//! committed, because the machines that need it are the ones that have run
//! nothing. **The shard splitter is its only reader, and a name it does not
//! price costs the splitter a default** — a new test needs no row to land.
//!
//! **Its numbers come from a runner and not from here**, deliberately:
//! cross-arch TCG on an M4 Pro and KVM on four Azure cores do not agree about
//! which tests are long, and the file exists for the checkout that has measured
//! nothing.
//!
//! Why a command and not a `cat`: the shards are a *partition*, and that is the
//! property the merged file's usefulness rests on. A repeated name means two
//! shards claimed one test or one shard ran the same label twice — three shards
//! of `nvme_` once ran one test twice and one nowhere, and all three reported
//! green. A concatenation cannot see it; this refuses it by name, and it is the
//! only thing here that refuses.
//!
//! **The tier report is warnings.** Each shard line carries the tier its test
//! ran in, and every measurement on the wrong side of
//! [`crate::tiers::FAST_CEILING_MS`] is printed as a `::warning::` — a Fast
//! name over the line, a Nightly name under it. Nothing about a price fails a
//! run; moving a test is editing its registration's `Tier`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::flags;
use crate::tiers::Tier;

/// The file name a sharded run leaves its own measurement in.
const SHARD_PREFIX: &str = "test-durations.shard-";

/// `--merge-durations <dir>`: every shard file under `dir`, into
/// `tests/test-durations`.
///
/// `dir` is where `gh run download` put the artifacts, so the files sit one
/// level down in a directory per shard; the walk is recursive for that reason
/// and for no other.
pub fn dispatch(root: &Path, args: &[String]) {
    let dir = flags::CARGO_RUN
        .value(args, &flags::MERGE_DURATIONS)
        .unwrap_or_else(|| unreachable!("dispatched on the flag, whose value `check` required"));
    let dir = Path::new(dir);

    let mut files = Vec::new();
    collect(dir, &mut files);
    assert!(
        !files.is_empty(),
        "no {SHARD_PREFIX}* under {}: a sharded run uploads one per shard",
        dir.display()
    );
    let count = whole_run(&files);

    let mut merged: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut tiers: BTreeMap<String, Tier> = BTreeMap::new();
    for file in &files {
        let who = file.file_name().expect("a file has a name").to_string_lossy().into_owned();
        let text = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        for (name, ms, tier) in shard_rows(file, &text) {
            insert_measurement(&mut merged, name, ms, &who);
            tiers.insert(name.to_string(), tier);
        }
    }

    let out = root.join("tests/test-durations");
    let before = read_profile(&out);
    report(&merged, &before, count);

    let said =
        crate::tiers::off_the_line(merged.iter().map(|(n, (ms, _))| (n.as_str(), *ms, tiers[n])));
    println!(
        "[durations] {} measurement(s) on the wrong side of the {} ms line for their tier",
        said.len(),
        crate::tiers::FAST_CEILING_MS
    );
    for line in &said {
        println!("::warning::{line}");
    }

    let profile = merged_profile(&merged, &before);
    let body: String = profile.iter().map(|(n, ms)| format!("{n} {ms}\n")).collect();
    fs::write(&out, body).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
    println!(
        "{}: {} measured test(s) from {} shard file(s), {} timing row(s) written",
        out.display(),
        merged.len(),
        files.len(),
        profile.len(),
    );
}

/// Add one execution label to a whole-run profile.
///
/// A duplicate is never a second sample. Across files it means two shards
/// disagreed about ownership; within one file it means a shard ran one label
/// twice. Keeping either duration would let the other verdict disappear.
fn insert_measurement(
    merged: &mut BTreeMap<String, (u64, String)>,
    name: &str,
    ms: u64,
    who: &str,
) {
    if let Some((_, first)) = merged.insert(name.to_string(), (ms, who.to_string())) {
        panic!(
            "{name} was measured twice, first in {first} and again in {who}. Every execution \
             label must occur exactly once: two shards may disagree about ownership, or one \
             shard may have run the same test twice"
        );
    }
}

/// The profile a completed sharded run leaves behind: what it measured, over
/// what was committed. A row this run did not measure is kept — the fast tier
/// does not run the nightly one, so absence is not evidence a row is stale.
fn merged_profile(
    measured: &BTreeMap<String, (u64, String)>,
    before: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    let mut after = before.clone();
    after.extend(measured.iter().map(|(name, (ms, _))| (name.clone(), *ms)));
    after
}

/// The shard count these files are all of, refusing anything that is not a
/// whole run.
///
/// The information was always there: a shard writes
/// `test-durations.shard-<i>-of-<n>`, so the file names say both how many
/// shards there were and which one each is.
fn whole_run(files: &[std::path::PathBuf]) -> usize {
    let mut seen: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut counts: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for file in files {
        let name = file.file_name().expect("a file has a name").to_string_lossy().into_owned();
        let spec = name.strip_prefix(SHARD_PREFIX).unwrap_or_else(|| {
            panic!("{name} was collected as a shard file and does not start with {SHARD_PREFIX}")
        });
        let (index, count) = spec.split_once("-of-").unwrap_or_else(|| {
            panic!("{name}: a shard file is named {SHARD_PREFIX}<index>-of-<count>")
        });
        let (index, count) = match (index.parse::<usize>(), count.parse::<usize>()) {
            (Ok(i), Ok(n)) if i >= 1 && i <= n => (i, n),
            _ => panic!("{name}: {index:?}/{count:?} is not a shard of a run"),
        };
        counts.insert(count);
        seen.entry(index).or_default().push(name);
    }

    assert!(
        counts.len() == 1,
        "these files are from more than one sharded run — shard counts {:?}. A profile merged \
         across two runs is a partition of neither.",
        counts
    );
    let count = *counts.iter().next().expect("one count");

    let twice: Vec<String> = seen
        .values()
        .filter(|f| f.len() > 1)
        .map(|f| f.join(" and "))
        .collect();
    assert!(twice.is_empty(), "one shard left two files: {}", twice.join("; "));

    let missing: Vec<String> =
        (1..=count).filter(|i| !seen.contains_key(i)).map(|i| i.to_string()).collect();
    assert!(
        missing.is_empty(),
        "shard(s) {} of {count} left no measurement, so this is not a whole run. Merging what is \
         here would write a profile missing everything those shards own, and every later run \
         would price those names at the longest this one knew — which is the imbalance the \
         profile exists to remove. Re-run the shards that did not finish.",
        missing.join(", ")
    );
    count
}

/// One profile row: `<label> <ms>`. The label may contain spaces
/// (`audio_tone_load (smp=1)`), so a row is read from the right.
pub fn parse_profile_line(line: &str) -> Option<(&str, u64)> {
    let (name, ms) = line.rsplit_once(' ')?;
    ms.parse().ok().map(|ms| (name, ms))
}

/// One shard-file row: `<label> <ms> <tier>`.
fn parse_shard_line(line: &str) -> Option<(&str, u64, Tier)> {
    let (rest, tier) = line.rsplit_once(' ')?;
    let (name, ms) = parse_profile_line(rest)?;
    Some((name, ms, Tier::from_token(tier)?))
}

/// Every row of one shard file. A row that does not parse is refused by file and
/// line: skipping it keeps the committed price for a name this run measured.
fn shard_rows<'a>(file: &Path, text: &'a str) -> Vec<(&'a str, u64, Tier)> {
    text.lines()
        .enumerate()
        .map(|(at, line)| {
            parse_shard_line(line).unwrap_or_else(|| {
                panic!(
                    "{}:{}: {line:?} is not a shard row, which is `<label> <ms> <tier>`",
                    file.display(),
                    at + 1
                )
            })
        })
        .collect()
}

fn read_profile(path: &Path) -> BTreeMap<String, u64> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(parse_profile_line)
        .map(|(n, ms)| (n.to_string(), ms))
        .collect()
}

/// What the run this merges was actually partitioned into, and what the profile
/// it partitioned on had to say about it.
///
/// **Both halves are measurements and neither is a model.** The spread is the
/// shard files' own totals; the ideal is their sum over the shard count. Nobody
/// has to be told what a better partition would have produced, because the run
/// that produced these files already answered it.
fn report(
    merged: &BTreeMap<String, (u64, String)>,
    before: &BTreeMap<String, u64>,
    shards: usize,
) {
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    for (ms, who) in merged.values() {
        *totals.entry(who.as_str()).or_default() += ms;
    }
    let (low, high) = (
        totals.values().min().copied().unwrap_or(0),
        totals.values().max().copied().unwrap_or(0),
    );
    let ideal = merged.values().map(|(ms, _)| ms).sum::<u64>() / shards.max(1) as u64;
    println!(
        "[durations] the shards measured {:.1}s to {:.1}s of tests; an even split is {:.1}s, \
         so this partition cost {:.1}s of critical path",
        low as f64 / 1000.0,
        high as f64 / 1000.0,
        ideal as f64 / 1000.0,
        (high.saturating_sub(ideal)) as f64 / 1000.0,
    );

    let unpriced: Vec<&str> =
        merged.keys().filter(|n| !before.contains_key(*n)).map(String::as_str).collect();
    if !unpriced.is_empty() {
        println!(
            "[durations] {} name(s) the profile did not price, each costed at the longest it \
             knew: {}",
            unpriced.len(),
            unpriced.join(", ")
        );
    }
    let gone: Vec<&str> =
        before.keys().filter(|n| !merged.contains_key(*n)).map(String::as_str).collect();
    if !gone.is_empty() {
        println!(
            "[durations] {} name(s) the profile prices and no shard ran: {}",
            gone.len(),
            gone.join(", ")
        );
    }
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(SHARD_PREFIX))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn shards(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from("/tmp").join(format!("{SHARD_PREFIX}{n}"))).collect()
    }

    /// The splitter reads every committed row, so one it cannot read is a
    /// price silently replaced by the default.
    #[test]
    fn every_committed_row_parses() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test-durations");
        let text = fs::read_to_string(&path).expect("the committed profile exists");
        for line in text.lines() {
            assert!(parse_profile_line(line).is_some(), "unparseable profile row: {line:?}");
        }
    }

    #[test]
    fn a_row_is_read_from_the_right() {
        assert_eq!(parse_profile_line("foo 123"), Some(("foo", 123)));
        assert_eq!(
            parse_profile_line("audio_tone_load (smp=1) 456"),
            Some(("audio_tone_load (smp=1)", 456))
        );
        assert_eq!(parse_profile_line("bare-name"), None);
        assert_eq!(
            parse_shard_line("audio_tone_load (smp=1) 456 nightly"),
            Some(("audio_tone_load (smp=1)", 456, Tier::Nightly))
        );
        assert_eq!(parse_shard_line("foo 123"), None);
    }

    #[test]
    fn a_shard_row_that_does_not_parse_is_refused_by_file_and_line() {
        let file = PathBuf::from("/tmp/durations-shard-3/test-durations.shard-3-of-12");
        assert_eq!(
            shard_rows(&file, "foo 120 fast\nbar (smp=2) 45000 nightly\n"),
            vec![("foo", 120, Tier::Fast), ("bar (smp=2)", 45_000, Tier::Nightly)]
        );
        let err = std::panic::catch_unwind(|| shard_rows(&file, "foo 120 fast\nbar 45000\n"))
            .expect_err("a two-column row is not a shard row");
        let refusal = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(refusal.contains("test-durations.shard-3-of-12:2:"), "{refusal}");
        assert!(refusal.contains("\"bar 45000\""), "{refusal}");
    }

    #[test]
    fn a_whole_run_is_every_shard_of_one_run_exactly_once() {
        assert_eq!(whole_run(&shards(&["1-of-3", "2-of-3", "3-of-3"])), 3);
        assert_eq!(whole_run(&shards(&["1-of-1"])), 1);
    }

    /// Teeth, and the middle one is the defect this was written for: eleven
    /// files of a twelve-way run merged to a profile missing a twelfth of the
    /// suite, and said so in a line among others while writing it anyway.
    #[test]
    fn a_partial_or_mixed_set_is_refused_by_name() {
        let refusal = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let err = std::panic::catch_unwind(|| whole_run(&shards(&refs)))
                .expect_err("this set is not a whole run");
            err.downcast_ref::<String>().cloned().unwrap_or_default()
        };

        assert!(refusal(&["1-of-3", "3-of-3"]).contains("shard(s) 2 of 3 left no measurement"));
        assert!(refusal(&["1-of-2", "2-of-2", "1-of-3"]).contains("more than one sharded run"));
        assert!(refusal(&["1-of-2", "1-of-2", "2-of-2"]).contains("one shard left two files"));
        assert!(refusal(&["4-of-3"]).contains("is not a shard of a run"));
        assert!(refusal(&["one-of-three"]).contains("is not a shard of a run"));
        assert!(refusal(&["7"]).contains("<index>-of-<count>"));
    }


    #[test]
    fn a_merge_keeps_what_it_did_not_measure_and_replaces_what_it_did() {
        let measured = BTreeMap::from([("ran".to_string(), (120, "shard-1".to_string()))]);
        let before = BTreeMap::from([("ran".to_string(), 999), ("held_back".to_string(), 45_000)]);
        let after = merged_profile(&measured, &before);
        assert_eq!(after.get("ran"), Some(&120));
        assert_eq!(after.get("held_back"), Some(&45_000));
    }

    #[test]
    fn one_shard_may_not_report_the_same_execution_label_twice() {
        let mut merged = BTreeMap::new();
        insert_measurement(&mut merged, "foo", 11_001, "test-durations.shard-1-of-12");
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            insert_measurement(&mut merged, "foo", 1, "test-durations.shard-1-of-12");
        }))
        .expect_err("the later short timing overwrote an over-ceiling execution");
        let refusal = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(refusal.contains("foo was measured twice"), "{refusal}");
        assert!(refusal.contains("one shard may have run"), "{refusal}");
    }
}
