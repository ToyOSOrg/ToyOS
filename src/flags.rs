//! The flag table a command line is checked against, and the `cargo run --`
//! vocabulary itself; `src/testargs.rs` declares the harness's argv against the
//! same [`Flag`].
//!
//! [`check`] runs before `main` builds, locks or launches anything, and an
//! argument that is neither a declared flag nor a declared flag's value is
//! refused by name — so a typo of any shape boots no guest.

use crate::durations::TIER_BASE_FLAG;
use crate::pr::ACCEPTS_MERGE;

/// One flag a command line accepts: its name, and what comes after it.
pub(crate) struct Flag {
    pub(crate) name: &'static str,
    pub(crate) value: Value,
}

/// What follows a flag on the command line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value {
    /// Nothing; the next word is the next argument.
    None,
    /// The next word, whatever it looks like: `--kernel-param --help` arms the
    /// actuator named `--help`. A flag left without one is refused where its
    /// value is read, by name.
    Next,
    /// Every word after it — the flag names a subcommand that owns the rest of
    /// the line, as `--worktree add <path>` does.
    Rest,
}

pub(crate) const fn flag(name: &'static str, value: Value) -> Flag {
    Flag { name, value }
}

/// Every flag `cargo run --` accepts; `main.rs` and `build.rs`'s `plan_for`
/// read them out of the same argument vector.
const FLAGS: &[Flag] = &[
    flag("--help", Value::None),
    flag("--land", Value::None),
    flag("--pr", Value::None),
    flag(ACCEPTS_MERGE, Value::None),
    flag("--sync", Value::None),
    flag("--abi-split-check", Value::None),
    flag("--sdk-version-check", Value::None),
    flag("--base", Value::Next),
    flag("--sdk-versions", Value::None),
    flag("--merge-durations", Value::Next),
    flag(TIER_BASE_FLAG, Value::Next),
    flag("--clippy", Value::None),
    flag("--known-red", Value::Next),
    flag("--merge-health", Value::None),
    flag("--since", Value::Next),
    flag("--days", Value::Next),
    flag("--abi-callers", Value::Next),
    flag("--debug", Value::None),
    flag("--build-only", Value::None),
    flag("--dump-audio", Value::None),
    flag("--rebuild-toolchain", Value::None),
    flag("--claim-sysroot", Value::None),
    flag("--host-builds", Value::Next),
    flag("--smp", Value::Next),
    flag("--gop", Value::None),
    flag("--metal-sim", Value::None),
    flag("--mute", Value::None),
    flag("--kernel-param", Value::Next),
    flag("--kernel-feature", Value::Next),
    flag("--diag-boot", Value::None),
    flag("--console-boot", Value::None),
    flag("--boot-config", Value::Next),
    flag("--regen-font", Value::None),
    flag("--regen-wallpaper", Value::None),
    flag("--regen-soundfont", Value::Next),
    flag("--worktree", Value::Rest),
    flag("--check-forks", Value::None),
];

/// What became of a command line, checked before anything else in `main` runs.
pub enum Outcome {
    Proceed,
    /// `--help` was there: print this and exit 0.
    Help(String),
    /// An argument the declaration does not account for: print this to stderr
    /// and exit 2.
    Refuse(String),
}

/// Pure over `args` (as `std::env::args().collect()` produces them, `argv[0]`
/// included) and the declaration, so the refusal is a value a test can assert
/// on rather than something only a human running the binary ever sees.
///
/// The list is read positionally, flag then value, because a declared flag's
/// value is that flag's and is never a flag itself.
pub fn check(args: &[String]) -> Outcome {
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        if arg == "--help" {
            return Outcome::Help(accepted());
        }
        let Some(flag) = FLAGS.iter().find(|f| f.name == arg) else {
            return Outcome::Refuse(refusal(arg));
        };
        match flag.value {
            Value::None => {}
            Value::Next => {
                rest.next();
            }
            Value::Rest => return Outcome::Proceed,
        }
    }
    Outcome::Proceed
}

fn refusal(arg: &str) -> String {
    // Every value here is a separate word, so an inline spelling would be
    // dropped in silence by the dispatch below rather than read.
    if let Some((name, _)) = arg.split_once('=') {
        if FLAGS.iter().any(|f| f.name == name) {
            return format!("Error: {arg:?}: {name} takes its value as the next word, {name} <value>.");
        }
    }
    format!("Error: unknown argument {arg:?}.\n{}", accepted())
}

fn accepted() -> String {
    format!("cargo run -- accepts:\n{}", usage(FLAGS))
}

/// A declared vocabulary as a list to print, one flag a line.
pub(crate) fn usage(flags: &[Flag]) -> String {
    flags
        .iter()
        .map(|f| match f.value {
            Value::None => format!("  {}", f.name),
            Value::Next => format!("  {} <value>", f.name),
            Value::Rest => format!("  {} <subcommand>", f.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn checked(words: &[&str]) -> Outcome {
        let args: Vec<String> =
            std::iter::once("toyos-build").chain(words.iter().copied()).map(String::from).collect();
        check(&args)
    }

    /// A mistyped flag of any shape, and a word that is nobody's value: each
    /// would otherwise have built everything and put a guest on the screen.
    #[test]
    fn an_argument_the_declaration_does_not_expect_is_refused_by_name() {
        for words in [
            vec!["--frobnicate"],
            vec!["-build-only"],
            vec!["-h"],
            vec!["buildonly"],
            vec!["--smp=4"],
            vec!["--build-only", "target/bootable.img"],
        ] {
            let bad = words.last().expect("a command line to refuse");
            match checked(&words) {
                Outcome::Refuse(message) => {
                    assert!(message.contains(bad), "{words:?}: {message}");
                }
                _ => panic!("{words:?} must be refused"),
            }
        }
    }

    /// The value of a flag that takes one is that flag's, whatever it spells.
    #[test]
    fn a_declared_flags_value_is_never_read_as_a_flag() {
        for words in [
            vec!["--kernel-param", "--help"],
            vec!["--boot-config", "--help"],
            vec!["--known-red", "--frobnicate"],
            vec!["--worktree", "add", "--help"],
            vec!["--boot-config", "diag", "--build-only"],
        ] {
            assert!(matches!(checked(&words), Outcome::Proceed), "{words:?} must proceed");
        }
    }

    #[test]
    fn help_is_the_whole_declared_list() {
        let Outcome::Help(message) = checked(&["--help"]) else {
            panic!("--help must be accepted");
        };
        for flag in FLAGS {
            assert!(message.contains(flag.name), "{} is missing from --help: {message}", flag.name);
        }
    }

    /// One command line per declared flag, held equal to [`FLAGS`] in both
    /// directions: a flag deleted from the declaration reds here instead of
    /// making a working command exit 2, and one added reds until it is
    /// exercised.
    const EXAMPLES: &[&[&str]] = &[
        &["--help"],
        &["--land"],
        &["--pr"],
        &["--gates-after-merge"],
        &["--sync"],
        &["--abi-split-check"],
        &["--sdk-version-check"],
        &["--base", "origin/main"],
        &["--sdk-versions"],
        &["--merge-durations", "/tmp/durations"],
        &["--tier-base", "e3b0c442"],
        &["--clippy"],
        &["--known-red", "audio_tone"],
        &["--merge-health"],
        &["--since", "2026-09-01T00:00:00Z"],
        &["--days", "7"],
        &["--abi-callers", "stack_info"],
        &["--debug"],
        &["--build-only"],
        &["--dump-audio"],
        &["--rebuild-toolchain"],
        &["--claim-sysroot"],
        &["--host-builds", "0"],
        &["--smp", "1"],
        &["--gop"],
        &["--metal-sim"],
        &["--mute"],
        &["--kernel-param", "control-regs-bench"],
        &["--kernel-feature", "boot-actuators"],
        &["--diag-boot"],
        &["--console-boot"],
        &["--boot-config", "diag"],
        &["--regen-font"],
        &["--regen-wallpaper"],
        &["--regen-soundfont", "bank.sf2"],
        &["--worktree", "add", "/tmp/wt"],
        &["--check-forks"],
    ];

    #[test]
    fn every_declared_flag_is_exercised_by_a_command_line() {
        let declared: BTreeSet<&str> = FLAGS.iter().map(|f| f.name).collect();
        let exercised: BTreeSet<&str> =
            EXAMPLES.iter().map(|words| *words.first().expect("a flag to exercise")).collect();
        assert_eq!(declared, exercised, "the declaration and the command lines that exercise it");
        for words in EXAMPLES {
            assert!(
                matches!(checked(words), Outcome::Proceed | Outcome::Help(_)),
                "{words:?} must be accepted"
            );
        }
    }

    /// Every file a `cargo run --` command line is written in and this
    /// repository runs. Documentation carries no gates here.
    fn scanned_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        files_with(&root.join(".github/workflows"), "yml", &mut files);
        files_with(&root.join("src"), "rs", &mut files);
        files_with(&root.join("tests/common"), "rs", &mut files);
        files.push(root.join("tests/toyos.rs"));
        files_with(&root.join("diag"), "sh", &mut files);
        files
    }

    fn files_with(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
        for entry in
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                files_with(&path, extension, out);
            } else if path.extension().is_some_and(|e| e == extension) {
                out.push(path);
            }
        }
    }

    fn read(file: &Path) -> String {
        std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
    }

    /// A flag this tree's own workflows, sources and scripts pass that [`FLAGS`]
    /// does not declare would refuse a run nothing is watching.
    #[test]
    fn every_flag_the_tree_passes_to_cargo_run_is_declared() {
        let declared: BTreeSet<&str> = FLAGS.iter().map(|f| f.name).collect();
        for file in scanned_files(&repo_root()) {
            for flag in flags_passed_to_cargo_run(&read(&file)) {
                assert!(
                    declared.contains(flag.as_str()),
                    "{} passes {flag} to `cargo run --`, and src/flags.rs does not declare it",
                    file.display()
                );
            }
        }
    }

    /// A `--` word a source compares an argument against is a flag that source
    /// consumes, and one the declaration lacks is refused before ever reaching
    /// it.
    #[test]
    fn every_flag_a_source_compares_an_argument_against_is_declared() {
        let root = repo_root();
        let build: BTreeSet<&str> = FLAGS.iter().map(|f| f.name).collect();
        let harness: BTreeSet<&str> =
            crate::testargs::FLAGS.iter().map(|f| f.name).collect();
        for file in scanned_files(&root).iter().filter(|f| f.extension().is_some_and(|e| e == "rs"))
        {
            // `tests/` and `src/testargs.rs` read the harness's argv, and
            // `testargs::FLAGS` is that vocabulary.
            let harness_side =
                file.starts_with(root.join("tests")) || file.ends_with("testargs.rs");
            let declared = if harness_side { &harness } else { &build };
            for flag in compared_flags(&read(file)) {
                assert!(
                    declared.contains(flag.as_str()),
                    "{} reads {flag} off a command line its vocabulary does not declare",
                    file.display()
                );
            }
        }
    }

    /// Every `--flag` a text hands to `cargo run --`, directly on the line or
    /// one shell variable away.
    fn flags_passed_to_cargo_run(text: &str) -> BTreeSet<String> {
        let mut vars: BTreeMap<String, String> = BTreeMap::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if let Some((name, value)) = trimmed.split_once('=') {
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    vars.insert(name.to_string(), value.to_string());
                }
            }
        }

        let mut found = BTreeSet::new();
        for line in text.lines() {
            let Some(at) = line.find("cargo run") else { continue };
            let Some(sep) = line[at..].find("-- ") else { continue };
            let tail = &line[at + sep + "-- ".len()..];
            collect_flags(tail, &vars, &mut BTreeSet::new(), &mut found);
        }
        found
    }

    fn collect_flags(
        text: &str,
        vars: &BTreeMap<String, String>,
        expanding: &mut BTreeSet<String>,
        found: &mut BTreeSet<String>,
    ) {
        for word in text.split_whitespace() {
            if let Some(rest) = word.strip_prefix('$') {
                let name: String =
                    rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                // A variable naming itself (`ARGS=$ARGS --build-only`) expands
                // once: the walk is bounded by the names the text assigns.
                if expanding.insert(name.clone()) {
                    if let Some(value) = vars.get(&name) {
                        collect_flags(value, vars, expanding, found);
                    }
                    expanding.remove(&name);
                }
                continue;
            }
            if let Some(flag) = leading_flag(word) {
                found.insert(flag);
            }
        }
    }

    /// The `--flag` a word starts with: the quotes and back-ticks around it are
    /// the prose's, never the flag's.
    fn leading_flag(word: &str) -> Option<String> {
        let start = word.find(|c: char| c.is_ascii_alphanumeric() || c == '-')?;
        let rest = &word[start..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        let token = &rest[..end];
        (token.starts_with("--") && token.len() > 2).then(|| token.to_string())
    }

    /// Every `--` word a source compares an argument against.
    fn compared_flags(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for (at, needle) in text.match_indices("== \"") {
            let rest = &text[at + needle.len()..];
            let Some(end) = rest.find('"') else { continue };
            let word = &rest[..end];
            if word.starts_with("--") {
                found.insert(word.to_string());
            }
        }
        found
    }

    /// Teeth for the two scans above: without them a walk that quietly found
    /// nothing would hold the declaration against nothing at all.
    #[test]
    fn the_scan_reads_a_flag_through_a_variable_and_out_of_punctuation() {
        let text = "\
            base_arg=\"--tier-base $TIER_BASE\"\n\
            ARGS=$ARGS --build-only\n\
            run: cargo run -- --diag-boot $base_arg $ARGS `--clippy`\n\
            let debug = args.iter().any(|a| a == \"--debug\");\n";
        assert_eq!(
            flags_passed_to_cargo_run(text),
            ["--build-only", "--clippy", "--diag-boot", "--tier-base"]
                .map(String::from)
                .into_iter()
                .collect::<BTreeSet<String>>()
        );
        assert_eq!(
            compared_flags(text),
            ["--debug"].map(String::from).into_iter().collect::<BTreeSet<String>>()
        );
    }
}
