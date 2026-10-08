//! The suite's command line, checked against the flags it actually has.
//!
//! `tests/toyos.rs` takes every word that is nobody's value as one of the
//! run's filters, so a flag this table does not declare would hand its own
//! value to them and report a one-test run as a pass. The table is the
//! harness's whole vocabulary, and [`SUITE`] is the only way to read a word
//! off its argv.

use crate::flags::declare_flags;
use std::path::PathBuf;

declare_flags!(pub SUITE = {
    pub DEBUG = "--debug", None;
    pub LIST = "--list", None;
    pub NOCAPTURE = "--nocapture", None;
    pub JOBS = "--jobs", Next;
    pub JOBS_SHORT = "-j", Next;
    /// The metal profile: the registrations that run on the T14, batched into
    /// images and judged off the log the stick came back with.
    pub METAL = "--metal", None;
    /// Where those images and their readbacks live. **Naming it means the
    /// machine is not touched**: the run builds the images and writes down what
    /// to run on them, or judges readbacks a driver already left there.
    pub METAL_READBACK = "--metal-readback", Next;
});

/// How a filter word names one of the metal profile's boots whole, ahead of
/// the boot's name as `--metal --list` prints it.
pub const BOOT: &str = "boot:";

/// The run's filters and `--metal`'s mode, all decided by [`parse`]: an unknown
/// flag refuses the line before any is read.
pub struct Parsed<'a> {
    /// A run takes every name any of these is part of, and with none, and no
    /// `boots`, every name there is.
    pub filters: Vec<&'a str>,
    /// The [`BOOT`] words, without it: boots `--metal` takes whole.
    pub boots: Vec<&'a str>,
    pub metal: Option<MetalMode>,
}

/// Validate the harness's argv and return the run's filters and metal mode.
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

    let (boots, filters): (Vec<&str>, Vec<&str>) =
        line.positionals.into_iter().partition(|word| word.starts_with(BOOT));
    let boots: Vec<&str> = boots.into_iter().map(|word| &word[BOOT.len()..]).collect();

    let has = |want| SUITE.present(args, want);
    if let (Some(boot), false) = (boots.first(), has(&METAL)) {
        return Err(format!(
            "{BOOT}{boot} names a boot of the metal profile, and a run without --metal has \
             none, so it would be dropped in silence; add --metal"
        ));
    }
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
        for flag in [&JOBS, &JOBS_SHORT] {
            if has(flag) {
                return Err(format!(
                    "{} beside --metal: the metal profile reads no {}, so it would be dropped \
                     in silence, and a following --list would be taken as its value",
                    flag.name, flag.name
                ));
            }
        }
        for word in filters.iter().chain(&boots) {
            if word.trim().is_empty() {
                return Err("--metal with an empty filter: name a registration or drop the word"
                    .to_string());
            }
            if let Some(flag) = SUITE.0.iter().find(|f| f.name.trim_start_matches('-') == *word) {
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

    Ok(Parsed { filters, boots, metal })
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

    /// The run's filters, as one text.
    fn parse_owned(args: &[&str]) -> Result<Option<String>, String> {
        parse(&owned(args)).map(|p| (!p.filters.is_empty()).then(|| p.filters.join(" ")))
    }

    fn metal_owned(args: &[&str]) -> Result<Option<MetalMode>, String> {
        parse(&owned(args)).map(|p| p.metal)
    }

    #[test]
    fn a_deleted_flag_is_refused_rather_than_becoming_the_filter() {
        for (flag, value) in
            [("--skip", "x"), ("--host-slots", "0"), ("--host-builds", "0"), ("--shard", "2/12")]
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
    fn every_word_that_is_nobodys_value_is_a_filter() {
        assert_eq!(parse_owned(&["futex", "--jobs", "4", "dlopen"]).unwrap().as_deref(), Some("futex dlopen"));
    }

    /// A boot word is `--metal`'s alone, and never one of the filters.
    #[test]
    fn a_boot_word_names_a_metal_boot_and_nothing_else() {
        let line = owned(&["--metal", "boot:shared-2", "control_regs", "boot:ccorpus"]);
        let parsed = parse(&line).unwrap();
        assert_eq!((parsed.filters, parsed.boots), (vec!["control_regs"], vec!["shared-2", "ccorpus"]));
        let refusal = parse_owned(&["boot:shared"]).unwrap_err();
        assert!(refusal.contains("add --metal"), "{refusal}");
        let refusal = metal_owned(&["--metal", "boot:"]).unwrap_err();
        assert!(refusal.contains("empty filter"), "{refusal}");
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
            vec!["--debug"],
            vec!["--metal"],
            vec!["--metal", "--metal-readback", "target/metal"],
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
        for word in ["list", "metal", "jobs", "debug", "metal-readback"] {
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
            &["--list", "--metal", "-j", "4"],
            &["--list", "--metal", "--jobs", "4"],
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
}
