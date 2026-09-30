//! The suite's command line, checked against the flags it actually has.
//!
//! `tests/toyos.rs` takes the first word that is nobody's value as the run's
//! filter, so a flag this table does not declare would hand its own value to
//! that filter and report a one-test run as a pass. The table is the harness's
//! whole vocabulary, and [`SUITE`] is the only way to read a word off its argv.

use crate::flags::declare_flags;
use std::path::PathBuf;

/// One machine's slice of the suite.
///
/// A shard is a *host*, never a lane. `--jobs` divides one machine's cores
/// between guests that contend for them; this divides the work between machines
/// that share nothing, which is the only lever CI has and the one the dev host
/// does not have at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shard {
    /// One-based, as it is written on the command line and in a job matrix.
    pub index: usize,
    pub count: usize,
}

impl Shard {
    /// Drop everything another shard owns out of `pools`, keeping the order of
    /// what is left: the items are dealt in turn, so the `n`th of the pools'
    /// concatenation, counting from zero, is shard `n % count + 1`'s.
    ///
    /// **A rule over positions and nothing else**, so every process that builds
    /// the same lists takes the same partition of them, and every item lands in
    /// exactly one shard, which is the property a verdict depends on. One count
    /// runs across the pools, so a later pool's first item goes to the shard
    /// after the one the earlier pool's last went to and the counts stay within
    /// one of each other.
    pub fn keep<T>(self, pools: &mut [&mut Vec<T>]) {
        let mut dealt = 0;
        for pool in pools.iter_mut() {
            pool.retain(|_| {
                let mine = dealt % self.count == self.index - 1;
                dealt += 1;
                mine
            });
        }
    }
}

/// `--shard <index>/<count>`, or `None` for the whole suite.
///
/// `Err` is a refusal to print and exit on, like [`parse`]'s: a shard number
/// outside its range would take no tests and report the run green.
pub fn parse_shard(args: &[String]) -> Result<Option<Shard>, String> {
    let Some(spec) = SUITE.value(args, &SHARD) else {
        return Ok(None);
    };
    let (index, count) = spec
        .split_once('/')
        .ok_or_else(|| format!("--shard {spec}: not <index>/<count>, e.g. 2/4"))?;
    let index: usize = index
        .parse()
        .map_err(|_| format!("--shard {spec}: {index:?} is not a shard number"))?;
    let count: usize = count
        .parse()
        .map_err(|_| format!("--shard {spec}: {count:?} is not a shard count"))?;
    if !(1..=count).contains(&index) {
        return Err(format!(
            "--shard {spec}: shards are numbered 1 through {count}, and a run outside \
             that range would take no tests and report itself green"
        ));
    }
    Ok(Some(Shard { index, count }))
}

/// Refuse a shard that owns nothing after the ordinary suite's filter
/// and task grouping have all been applied.
///
/// A valid shard number is not enough to establish that the selected suite has
/// at least that many bins. The check therefore belongs after `Shard::keep`,
/// where `total` is the number of verdicts this process can actually produce.
pub fn validate_ordinary_shard(
    shard: Option<Shard>,
    filter: Option<&str>,
    total: usize,
) -> Result<(), String> {
    let Some(shard) = shard else { return Ok(()) };
    if total > 0 {
        return Ok(());
    }
    Err(format!(
        "--shard {}/{} with filter {filter:?} owns no ordinary tests after selection; \
         refusing a false-green shard run",
        shard.index, shard.count,
    ))
}

declare_flags!(pub SUITE = {
    pub DEBUG = "--debug", None;
    pub LIST = "--list", None;
    pub NOCAPTURE = "--nocapture", None;
    pub JOBS = "--jobs", Next;
    pub JOBS_SHORT = "-j", Next;
    pub SHARD = "--shard", Next;
    /// The metal profile: the registrations that run on the T14, batched into
    /// images and judged off the log the stick came back with.
    pub METAL = "--metal", None;
    /// Where those images and their readbacks live. **Naming it means the
    /// machine is not touched**: the run builds the images and writes down what
    /// to run on them, or judges readbacks a driver already left there.
    pub METAL_READBACK = "--metal-readback", Next;
    /// The owner `guest_dies_with_its_harness` kills: the image it names,
    /// booted and held until stdin ends. Alone on its line.
    pub HOLD = "--hold", Next;
});

/// The run's filter and `--metal`'s mode, both decided by [`parse`]: an unknown
/// flag refuses the line before either is read.
pub struct Parsed<'a> {
    pub filter: Option<&'a str>,
    pub metal: Option<MetalMode>,
}

/// Validate the harness's argv and return the run's filter and metal mode.
///
/// `Err` is a refusal to print and exit on. It is asked before the sysroot lock
/// and before anything is compiled, so a stale command line costs a message
/// rather than a queue behind it.
pub fn parse(args: &[String]) -> Result<Parsed<'_>, String> {
    let line = SUITE.walk(args);
    if let Some(word) = line.unknown {
        return Err(format!(
            "{word}: the suite has no such flag, and an unknown flag's value becomes the \
             run's filter — so this would have measured whatever one test it named.\n\
             Flags it has:\n{}",
            SUITE.usage()
        ));
    }
    if let Some(refusal) = line.malformed() {
        return Err(refusal);
    }
    let flags = line.seen.len();

    let mut filter: Option<&str> = None;
    for word in line.positionals {
        if let Some(first) = filter {
            return Err(format!(
                "{first:?} and {word:?}: the suite takes one filter, and the second word \
                 would have been dropped in silence.\n\
                 A filter is a substring, so `{first}` and `{word}` are one run only if one \
                 substring matches both."
            ));
        }
        filter = Some(word);
    }

    let has = |want| SUITE.present(args, want);
    if has(&JOBS) && has(&JOBS_SHORT) {
        return Err(
            "--jobs and -j are two spellings of one width, and the run would read one of \
             them and drop the other in silence; write one"
                .to_string(),
        );
    }
    if has(&METAL_READBACK) && !has(&METAL) {
        return Err(
            "--metal-readback says where the metal profile's images and readbacks live and \
             decides nothing on its own; add --metal"
                .to_string(),
        );
    }
    if has(&METAL) {
        for flag in [&SHARD, &JOBS, &JOBS_SHORT] {
            if has(flag) {
                return Err(format!(
                    "{} beside --metal: the metal profile reads no {}, so it would be dropped \
                     in silence, and a following --list would be taken as its value",
                    flag.name, flag.name
                ));
            }
        }
        if let Some(word) = filter {
            if word.trim().is_empty() {
                return Err("--metal with an empty filter: name a registration or drop the word"
                    .to_string());
            }
            if let Some(flag) = SUITE.0.iter().find(|f| f.name.trim_start_matches('-') == word) {
                return Err(format!(
                    "{word:?} beside --metal is {}'s name without its dashes, and would be \
                     read as a filter that selects whatever contains it",
                    flag.name
                ));
            }
        }
        if has(&LIST) && has(&METAL_READBACK) {
            return Err("--metal --list prints the plan and reads no readback directory, so \
                        --metal-readback would be dropped in silence"
                .to_string());
        }
        if SUITE.value(args, &METAL_READBACK).is_some_and(|dir| dir.starts_with('-')) {
            return Err(
                "--metal-readback takes a directory, and a word that starts with - is a flag \
                 that lost its place"
                    .to_string(),
            );
        }
    }
    if has(&HOLD) && (flags != 1 || filter.is_some()) {
        return Err(
            "--hold boots the image it names and holds it, and reads nothing else on the line; \
             every other word would be dropped in silence"
                .to_string(),
        );
    }

    let metal = has(&METAL).then(|| {
        if has(&LIST) {
            MetalMode::List
        } else {
            match SUITE.value(args, &METAL_READBACK) {
                Some(dir) => MetalMode::Offline(PathBuf::from(dir)),
                None => MetalMode::Drive,
            }
        }
    });

    Ok(Parsed { filter, metal })
}

/// What `--metal`'s own flags resolve a run to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MetalMode {
    /// `--metal --list`: print what would run, and touch nothing.
    List,
    Offline(PathBuf),
    /// `--metal` alone: flash and drive the machine.
    Drive,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags::Value;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    fn parse_owned(args: &[&str]) -> Result<Option<String>, String> {
        parse(&owned(args)).map(|p| p.filter.map(ToString::to_string))
    }

    fn metal_owned(args: &[&str]) -> Result<Option<MetalMode>, String> {
        parse(&owned(args)).map(|p| p.metal)
    }

    #[test]
    fn a_deleted_flag_is_refused_rather_than_becoming_the_filter() {
        for (flag, value) in
            [("--skip", "desktop_window_child"), ("--host-slots", "0"), ("--host-builds", "0")]
        {
            let refusal = parse_owned(&[flag, value]).unwrap_err();
            assert!(refusal.starts_with(&format!("{flag}:")), "{refusal}");
            assert!(refusal.contains("--jobs <value>"), "{refusal}");
        }
    }

    #[test]
    fn a_flags_value_is_not_the_filter() {
        assert_eq!(parse_owned(&["--jobs", "4"]).unwrap(), None);
        assert_eq!(parse_owned(&["-j", "4"]).unwrap(), None);
    }

    #[test]
    fn the_filter_is_the_word_that_is_nobodys_value() {
        assert_eq!(parse_owned(&["process_stats"]).unwrap().as_deref(), Some("process_stats"));
        assert_eq!(
            parse_owned(&["--jobs", "4", "audio_tone", "--nocapture"]).unwrap().as_deref(),
            Some("audio_tone")
        );
        assert_eq!(
            parse_owned(&["--jobs=4", "futex"]).unwrap().as_deref(),
            Some("futex")
        );
    }

    #[test]
    fn two_filters_are_refused_because_only_one_would_run() {
        let refusal = parse_owned(&["futex", "dlopen"]).unwrap_err();
        assert!(refusal.contains("\"futex\"") && refusal.contains("\"dlopen\""), "{refusal}");
    }

    fn shard_of(args: &[&str]) -> Result<Option<Shard>, String> {
        parse_shard(&owned(args))
    }

    #[test]
    fn a_shard_is_index_and_count() {
        assert_eq!(shard_of(&["--shard", "2/4"]).unwrap(), Some(Shard { index: 2, count: 4 }));
        assert_eq!(shard_of(&["--shard=1/1"]).unwrap(), Some(Shard { index: 1, count: 1 }));
        assert_eq!(shard_of(&[]).unwrap(), None);
    }

    /// The failure with no symptom: a shard nobody owns runs nothing, and a run
    /// that ran nothing exits 0.
    #[test]
    fn a_shard_outside_its_range_is_refused() {
        for spec in ["0/4", "5/4", "2/0"] {
            let refusal = shard_of(&["--shard", spec]).unwrap_err();
            assert!(refusal.contains("green"), "{spec}: {refusal}");
        }
        assert!(shard_of(&["--shard", "half"]).is_err());
        assert!(shard_of(&["--shard", "x/4"]).is_err());
    }

    #[test]
    fn an_empty_selected_shard_is_a_named_false_green() {
        let shard = Some(Shard { index: 8, count: 12 });
        let refusal = validate_ordinary_shard(shard, Some("one_test"), 0).unwrap_err();
        assert!(refusal.contains("--shard 8/12"), "{refusal}");
        assert!(refusal.contains("filter Some(\"one_test\")"), "{refusal}");
        assert!(refusal.contains("false-green"), "{refusal}");

        assert!(validate_ordinary_shard(shard, None, 1).is_ok());
        assert!(validate_ordinary_shard(None, Some("nothing"), 0).is_ok());
    }

    /// The property every verdict rests on: the shards are a partition. Not one
    /// test may be dropped by all of them, and none may be run by two.
    #[test]
    fn every_item_lands_in_exactly_one_shard() {
        let (first, second): (Vec<u32>, Vec<u32>) = ((0..97).collect(), (97..105).collect());
        for count in 1..=8 {
            let mut seen: Vec<u32> = Vec::new();
            let mut sizes = Vec::new();
            for index in 1..=count {
                let (mut a, mut b) = (first.clone(), second.clone());
                Shard { index, count }.keep(&mut [&mut a, &mut b]);
                sizes.push(a.len() + b.len());
                seen.extend(a.into_iter().chain(b));
            }
            seen.sort_unstable();
            assert_eq!(seen, (0..105).collect::<Vec<u32>>(), "count {count}");
            let (fewest, most) = (sizes.iter().min(), sizes.iter().max());
            assert!(most.zip(fewest).is_some_and(|(m, f)| m - f <= 1), "count {count}: {sizes:?}");
        }
    }

    /// **One deal across the pools.** Two pools of `[0, 1, 2]` and `[3, 4]` over
    /// two shards: the deal goes on from where the first pool stopped, so the
    /// second pool's first item is shard 2's, where a deal restarted per pool
    /// would hand shard 1 both pools' first items.
    #[test]
    fn a_later_pool_is_dealt_on_from_where_the_earlier_stopped() {
        let taken = |index| {
            let (mut a, mut b) = (vec![0, 1, 2], vec![3, 4]);
            Shard { index, count: 2 }.keep(&mut [&mut a, &mut b]);
            (a, b)
        };
        assert_eq!(taken(1), (vec![0, 2], vec![4]));
        assert_eq!(taken(2), (vec![1], vec![3]));
    }

    /// Every `None` here is a default the run then takes in silence: `--jobs`
    /// the built-in width.
    #[test]
    fn a_flag_left_without_its_value_is_refused_by_name() {
        for flag in SUITE.0.iter().filter(|f| f.value != Value::None) {
            for word in [flag.name.to_string(), format!("{}=", flag.name)] {
                let refusal = parse_owned(&[word.as_str()]).unwrap_err();
                assert!(refusal.contains(flag.name), "{word}: {refusal}");
                assert!(refusal.contains("no value"), "{word}: {refusal}");
            }
        }
    }

    #[test]
    fn a_readback_flag_with_no_directory_never_reaches_the_metal_driver() {
        let refusal = parse_owned(&["--metal", "--metal-readback"]).unwrap_err();
        assert!(refusal.contains("--metal-readback"), "{refusal}");
        assert!(refusal.contains("no value"), "{refusal}");
        assert_eq!(SUITE.value(&owned(&["--metal", "--metal-readback"]), &METAL_READBACK), None);
    }

    /// A second use is read by nothing, so the run would take the first value
    /// and say nothing about the one it dropped.
    #[test]
    fn a_flag_written_twice_is_refused_by_name() {
        for argv in [vec!["--jobs", "1", "--jobs", "4"], vec!["--list", "--list"]] {
            let refusal = parse_owned(&argv).unwrap_err();
            assert!(refusal.contains(argv[0]), "{argv:?}: {refusal}");
            assert!(refusal.contains("twice"), "{argv:?}: {refusal}");
        }
        // Two spellings of the width are the same drop under two names.
        let refusal = parse_owned(&["--jobs", "1", "-j", "4"]).unwrap_err();
        assert!(refusal.contains("--jobs and -j"), "{refusal}");
    }

    #[test]
    fn an_inline_value_on_a_flag_that_has_none_is_refused() {
        let refusal = parse_owned(&["--nocapture=1"]).unwrap_err();
        assert!(refusal.contains("--nocapture"), "{refusal}");
    }

    #[test]
    fn the_documented_command_lines_parse() {
        for argv in [
            vec![],
            vec!["--nocapture"],
            vec!["process_stats"],
            vec!["process_stats", "--nocapture"],
            vec!["--list"],
            vec!["--jobs", "4"],
            vec!["--shard", "2/4"],
            vec!["--shard", "2/12", "--jobs", "1"],
            vec!["--debug"],
            vec!["--metal"],
            vec!["--metal", "--metal-readback", "target/metal"],
            vec!["--hold", "boot.img"],
            vec!["--hold=boot.img"],
        ] {
            assert!(parse_owned(&argv).is_ok(), "{argv:?}");
        }
    }

    /// A readback directory with no `--metal` beside it selects no tier at all,
    /// so the run it describes would be an ordinary suite that wrote images
    /// nobody looked at.
    #[test]
    fn a_readback_directory_alone_selects_no_tier() {
        let refusal = parse_owned(&["--metal-readback", "target/metal"]).unwrap_err();
        assert!(refusal.contains("add --metal"), "{refusal}");
    }

    #[test]
    fn metal_and_list_never_drives() {
        assert_eq!(metal_owned(&["--metal", "--list"]).unwrap(), Some(MetalMode::List));
    }

    #[test]
    fn metal_list_beside_a_readback_directory_is_refused() {
        for argv in [
            &["--metal", "--list", "--metal-readback", "x"][..],
            &["--metal", "--metal-readback", "x", "--list"],
        ] {
            let refusal = metal_owned(argv).expect_err(&format!("{argv:?} was accepted"));
            assert!(refusal.contains("would be dropped in silence"), "{argv:?}: {refusal}");
        }
    }

    #[test]
    fn metal_refuses_a_filter_that_is_a_flags_name_or_empty() {
        for word in ["list", "metal", "jobs", "shard", "debug", "metal-readback"] {
            let refusal = metal_owned(&["--metal", word]).expect_err(word);
            assert!(refusal.contains("without its dashes"), "{word}: {refusal}");
        }
        for word in ["", "  "] {
            let refusal = metal_owned(&["--metal", word]).expect_err("empty filter");
            assert!(refusal.contains("empty filter"), "{word:?}: {refusal}");
        }
        assert!(metal_owned(&["--metal", "abuse_listener_hijack"]).is_ok());
    }

    #[test]
    fn metal_refuses_the_flags_it_reads_nothing_of_by_name() {
        for argv in [
            &["--metal", "-j", "--list"][..],
            &["--metal", "--jobs", "--list"],
            &["--metal", "--shard", "--list"],
            &["--list", "--metal", "--shard", "2/4"],
            &["--list", "--metal", "-j", "4"],
            &["--list", "--metal", "--jobs", "4"],
            &["--metal", "--shard", "2/4"],
            &["--metal", "-j", "4"],
        ] {
            let refusal = metal_owned(argv).expect_err(&format!("{argv:?} was accepted"));
            assert!(refusal.contains("beside --metal"), "{argv:?}: {refusal}");
        }
        for argv in [
            &["--metal", "--metal-readback", "--list"][..],
            &["--metal-readback", "--list", "--metal"],
        ] {
            let refusal = metal_owned(argv).expect_err(&format!("{argv:?} was accepted"));
            assert!(refusal.contains("--metal-readback takes a directory"), "{argv:?}: {refusal}");
        }
    }

    #[test]
    fn metal_resolves_to_a_mode_that_reaches_the_machine_only_when_asked() {
        assert_eq!(metal_owned(&[]).unwrap(), None);
        assert_eq!(metal_owned(&["--metal"]).unwrap(), Some(MetalMode::Drive));
        assert_eq!(
            metal_owned(&["--metal", "--metal-readback", "target/metal"]).unwrap(),
            Some(MetalMode::Offline(PathBuf::from("target/metal")))
        );
    }

    #[test]
    fn an_unknown_flag_is_refused_before_metal_decides_a_mode() {
        let refusal = parse_owned(&["--metal", "--bogus", "--list"]).unwrap_err();
        assert!(refusal.contains("--bogus"), "{refusal}");
    }

    #[test]
    fn hold_is_alone_on_its_line() {
        for argv in [
            &["--hold", "boot.img", "boot"][..],
            &["--hold", "boot.img", "--list"],
            &["-j", "2", "--hold", "boot.img"],
            &["--hold"],
        ] {
            let refusal = parse_owned(argv).unwrap_err();
            assert!(refusal.contains("--hold"), "{argv:?}: {refusal}");
        }
    }
}
