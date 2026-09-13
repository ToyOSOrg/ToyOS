//! The complete `--` vocabulary `cargo run` accepts, checked once in
//! [`check`] before `main` builds, locks, or launches anything.
//!
//! A `--` argument [`check`] does not recognize is refused by name; `--help`
//! prints the same list [`check`] refuses against and exits clean. Nothing in
//! this tree may consume a `--` word from `main`'s argument vector that is not
//! declared in [`FLAGS`].

/// One flag `cargo run --` accepts: its name, and whether the argument right
/// after it is that flag's value rather than another flag.
pub struct Flag {
    pub name: &'static str,
    pub takes_value: bool,
}

/// Every flag the build system accepts, declared once. `main.rs` dispatches on
/// each of these by name, `build.rs`'s `plan_for` reads the two kernel flags
/// out of the same argument vector, and this module's own test holds the set
/// against every flag this tree's workflows and scripts actually pass.
pub const FLAGS: &[Flag] = &[
    Flag { name: "--help", takes_value: false },
    Flag { name: "--land", takes_value: false },
    Flag { name: "--pr", takes_value: false },
    Flag { name: "--gates-after-merge", takes_value: false },
    Flag { name: "--sync", takes_value: false },
    Flag { name: "--abi-split-check", takes_value: false },
    Flag { name: "--sdk-version-check", takes_value: false },
    Flag { name: "--base", takes_value: true },
    Flag { name: "--sdk-versions", takes_value: false },
    Flag { name: "--merge-durations", takes_value: true },
    Flag { name: "--tier-base", takes_value: true },
    Flag { name: "--clippy", takes_value: false },
    Flag { name: "--known-red", takes_value: true },
    Flag { name: "--merge-health", takes_value: false },
    Flag { name: "--since", takes_value: true },
    Flag { name: "--days", takes_value: true },
    Flag { name: "--abi-callers", takes_value: false },
    Flag { name: "--debug", takes_value: false },
    Flag { name: "--build-only", takes_value: false },
    Flag { name: "--dump-audio", takes_value: false },
    Flag { name: "--rebuild-toolchain", takes_value: false },
    Flag { name: "--claim-sysroot", takes_value: false },
    Flag { name: "--host-builds", takes_value: true },
    Flag { name: "--smp", takes_value: true },
    Flag { name: "--gop", takes_value: false },
    Flag { name: "--metal-sim", takes_value: false },
    Flag { name: "--mute", takes_value: false },
    Flag { name: "--kernel-param", takes_value: true },
    Flag { name: "--kernel-feature", takes_value: true },
    Flag { name: "--diag-boot", takes_value: false },
    Flag { name: "--console-boot", takes_value: false },
    Flag { name: "--boot-config", takes_value: true },
    Flag { name: "--regen-font", takes_value: false },
    Flag { name: "--regen-wallpaper", takes_value: false },
    Flag { name: "--regen-soundfont", takes_value: true },
    Flag { name: "--worktree", takes_value: true },
    Flag { name: "--check-forks", takes_value: false },
];

/// What became of a command line, checked before anything else in `main` runs.
pub enum Outcome {
    /// Nothing unrecognized: `main` proceeds.
    Proceed,
    /// `--help` was there: print this and exit 0.
    Help(String),
    /// A `--` word [`FLAGS`] does not declare: print this to stderr and exit 2.
    Refuse(String),
}

/// Pure over `args` (as `std::env::args().collect()` produces them, `argv[0]`
/// included) and [`FLAGS`], so the refusal is a value a test can assert on
/// rather than something only a human running the binary ever sees.
pub fn check(args: &[String]) -> Outcome {
    let rest = args.iter().skip(1);
    if rest.clone().any(|a| a == "--help") {
        return Outcome::Help(usage());
    }
    let mut skip_next = false;
    for arg in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        match FLAGS.iter().find(|f| f.name == arg) {
            Some(flag) => skip_next = flag.takes_value,
            None if arg.starts_with("--") => {
                return Outcome::Refuse(format!("Error: unknown flag {arg:?}.\n{}", usage()));
            }
            None => {}
        }
    }
    Outcome::Proceed
}

fn usage() -> String {
    let mut lines = vec!["cargo run -- accepts:".to_string()];
    for flag in FLAGS {
        lines.push(if flag.takes_value {
            format!("  {} <value>", flag.name)
        } else {
            format!("  {}", flag.name)
        });
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn an_unknown_flag_is_refused_by_name() {
        let args = ["toyos-build", "--nonsense-flag"].map(String::from);
        match check(&args) {
            Outcome::Refuse(message) => {
                assert!(message.contains("--nonsense-flag"), "{message}");
                assert!(message.contains("--build-only"), "{message}");
            }
            _ => panic!("an undeclared flag must be refused"),
        }
    }

    #[test]
    fn help_is_declared_and_accepted() {
        let args = ["toyos-build", "--help"].map(String::from);
        match check(&args) {
            Outcome::Help(message) => assert!(message.contains("--help")),
            other => panic!("--help must be accepted, got a {}", match other {
                Outcome::Proceed => "Proceed",
                Outcome::Refuse(_) => "Refuse",
                Outcome::Help(_) => unreachable!(),
            }),
        }
    }

    #[test]
    fn a_declared_value_is_not_scanned_as_the_next_flag() {
        // `--boot-config` takes the next word as its directory. Without the
        // skip, a directory that happens to look like a flag would be refused
        // even though it was never typed as one.
        let args = ["toyos-build", "--boot-config", "--not-a-real-flag", "--build-only"]
            .map(String::from);
        assert!(matches!(check(&args), Outcome::Proceed));
    }

    #[test]
    fn every_declared_flag_round_trips_through_usage() {
        let text = usage();
        for flag in FLAGS {
            assert!(text.contains(flag.name), "{} is declared but missing from --help: {text}", flag.name);
        }
    }

    /// A flag this tree's own workflows and scripts pass that [`FLAGS`] does
    /// not declare would refuse a run nothing is watching, so every one of
    /// them has to appear here too.
    #[test]
    fn every_flag_the_tree_passes_to_cargo_run_is_declared() {
        let root = repo_root();
        let mut files = Vec::new();

        let workflows = root.join(".github/workflows");
        for entry in std::fs::read_dir(&workflows)
            .unwrap_or_else(|e| panic!("{}: {e}", workflows.display()))
        {
            let path = entry.expect("readable dir entry").path();
            if path.extension().is_some_and(|e| e == "yml") {
                files.push(path);
            }
        }

        let src = root.join("src");
        for entry in std::fs::read_dir(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display())) {
            let path = entry.expect("readable dir entry").path();
            if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }

        files.push(root.join("tests/toyos.rs"));

        let declared: BTreeSet<&str> = FLAGS.iter().map(|f| f.name).collect();
        for file in files {
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            for flag in flags_passed_to_cargo_run(&text) {
                assert!(
                    declared.contains(flag.as_str()),
                    "{} passes {flag} to `cargo run --`, and src/flags.rs does not declare it",
                    file.display()
                );
            }
        }
    }

    /// Every `--flag` a text hands to `cargo run --`, directly on the line or
    /// one shell variable away — crude, and on purpose, like every other scan
    /// of this repository's own workflows.
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
            collect_flags(tail, &vars, &mut found);
        }
        found
    }

    /// One level of `$VAR` indirection: a word that names a variable this
    /// text assigned is expanded and scanned in its place.
    fn collect_flags(text: &str, vars: &BTreeMap<String, String>, found: &mut BTreeSet<String>) {
        for word in text.split_whitespace() {
            if let Some(rest) = word.strip_prefix('$') {
                let name: String =
                    rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if let Some(value) = vars.get(&name) {
                    collect_flags(value, vars, found);
                }
                continue;
            }
            if let Some(flag) = leading_flag(word) {
                found.insert(flag);
            }
        }
    }

    /// The `--flag` a word starts with, stopping at the first character that
    /// is neither alphanumeric nor `-` — a trailing backtick, sentence
    /// punctuation, or an escaped `\n` a doc comment's string literal carries
    /// belongs to the prose around the flag, never to the flag itself.
    fn leading_flag(word: &str) -> Option<String> {
        let start = word.find(|c: char| c.is_ascii_alphanumeric() || c == '-')?;
        let rest = &word[start..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        let token = &rest[..end];
        (token.starts_with("--") && token.len() > 2).then(|| token.to_string())
    }
}
