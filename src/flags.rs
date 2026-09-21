//! The vocabulary of `cargo run --`, and the one walk every command line in
//! this crate is read with; `src/testargs.rs` declares the harness's argv
//! against the same [`Flag`].
//!
//! [`check`] runs before `main` builds, locks or launches anything, and an
//! argument that is neither a declared flag nor a declared flag's value is
//! refused by name. Every reader takes the [`Flag`] itself and not its
//! spelling, so a flag no declaration carries cannot be read off a command line
//! at all, and one deleted from a declaration stops compiling at every site
//! that read it.

pub struct Flag {
    pub name: &'static str,
    pub(crate) value: Value,
}

/// What follows a flag on the command line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value {
    /// Nothing; the next word is the next argument.
    None,
    /// The next word, whatever it looks like, and the flag is written once. A
    /// line that ends without the word is refused, and so is a second use.
    Next,
    /// The next word, and the flag may be written again: `--kernel-param --help
    /// --kernel-param slow` arms two actuators, and every value is read.
    Each,
    /// The next word unless that word is itself a flag: `--known-red` alone
    /// answers about every row, and `--known-red <test>` about one.
    Optional,
    /// Every word after it — the flag names a subcommand that owns the rest of
    /// the line, as `--worktree add <path>` does.
    Rest,
}

/// One command line's whole vocabulary, and the only way to read one.
pub struct Vocabulary(pub(crate) &'static [&'static Flag]);

/// The declaration: one line per flag, expanding to the constant every reader
/// names it by and to the table [`check`] holds a command line against, so the
/// two cannot disagree.
macro_rules! declare_flags {
    ($tvis:vis $table:ident = { $($(#[$about:meta])* $vis:vis $konst:ident = $name:literal, $value:ident;)* }) => {
        $($(#[$about])* $vis const $konst: $crate::flags::Flag = $crate::flags::Flag {
            name: $name,
            value: $crate::flags::Value::$value,
        };)*
        $tvis const $table: $crate::flags::Vocabulary =
            $crate::flags::Vocabulary(&[$(&$konst),*]);
    };
}
pub(crate) use declare_flags;

declare_flags!(pub CARGO_RUN = {
    pub HELP = "--help", None;
    pub LAND = "--land", None;
    pub PR = "--pr", None;
    pub GATES_AFTER_MERGE = "--gates-after-merge", None;
    pub SYNC = "--sync", None;
    pub ABI_SPLIT_CHECK = "--abi-split-check", None;
    pub SDK_VERSION_CHECK = "--sdk-version-check", None;
    pub BASE = "--base", Next;
    pub SDK_VERSIONS = "--sdk-versions", None;
    pub MERGE_DURATIONS = "--merge-durations", Next;
    pub TIER_BASE = "--tier-base", Next;
    pub CLIPPY = "--clippy", None;
    pub KNOWN_RED = "--known-red", Optional;
    pub ABI_CALLERS = "--abi-callers", Next;
    pub DEBUG = "--debug", None;
    pub BUILD_ONLY = "--build-only", None;
    pub DUMP_AUDIO = "--dump-audio", None;
    pub REBUILD_TOOLCHAIN = "--rebuild-toolchain", None;
    pub CLAIM_SYSROOT = "--claim-sysroot", None;
    pub HOST_BUILDS = "--host-builds", Next;
    pub SMP = "--smp", Next;
    pub GOP = "--gop", None;
    pub METAL_SIM = "--metal-sim", None;
    pub MUTE = "--mute", None;
    pub KERNEL_PARAM = "--kernel-param", Each;
    pub KERNEL_FEATURE = "--kernel-feature", Each;
    pub DIAG_BOOT = "--diag-boot", None;
    pub CONSOLE_BOOT = "--console-boot", None;
    pub BOOT_CONFIG = "--boot-config", Next;
    pub REGEN_FONT = "--regen-font", None;
    pub REGEN_WALLPAPER = "--regen-wallpaper", None;
    pub REGEN_SOUNDFONT = "--regen-soundfont", Next;
    pub WORKTREE = "--worktree", Rest;
    pub CHECK_FORKS = "--check-forks", None;
});

/// What became of a command line, checked before anything else in `main` runs.
pub enum Outcome {
    Proceed,
    /// `--help` was there: print this and exit 0.
    Help(String),
    /// An argument the declaration does not account for: print this to stderr
    /// and exit 2.
    Refuse(String),
}

/// Pure over the words after the program name — the line every reader of it
/// walks, `std::env::args().skip(1)` as `tests/toyos.rs` collects the harness's.
pub fn check(args: &[String]) -> Outcome {
    let line = CARGO_RUN.walk(args);
    if let Some(word) = line.unknown {
        return Outcome::Refuse(unknown(word));
    }
    if let Some(refusal) = line.malformed() {
        return Outcome::Refuse(format!("Error: {refusal}"));
    }
    if let Some(word) = line.positionals.first() {
        return Outcome::Refuse(unknown(word));
    }
    if line.seen.iter().any(|seen| seen.flag.name == HELP.name) {
        return Outcome::Help(format!("cargo run -- accepts:\n{}", CARGO_RUN.usage()));
    }
    Outcome::Proceed
}

fn unknown(word: &str) -> String {
    format!("Error: unknown argument {word:?}.\ncargo run -- accepts:\n{}", CARGO_RUN.usage())
}

/// What a flag accepts after it, as a refusal and a usage line both spell it.
fn shape(value: Value) -> &'static str {
    match value {
        Value::None => "",
        Value::Next | Value::Each => " <value>",
        Value::Optional => " [<value>]",
        Value::Rest => " <subcommand>",
    }
}

/// What a command line wrote after a flag.
#[derive(Clone, Copy)]
pub(crate) enum Given<'a> {
    Nothing,
    /// The next word: `--smp 4`.
    Next(&'a str),
    /// Written onto the flag itself: `--smp=4`.
    Inline(&'a str),
    /// Every word after a [`Value::Rest`] flag.
    Rest(&'a [String]),
}

impl<'a> Given<'a> {
    fn value(self) -> Option<&'a str> {
        match self {
            Given::Next(value) | Given::Inline(value) => Some(value),
            Given::Nothing | Given::Rest(_) => None,
        }
    }
}

pub(crate) struct Seen<'a> {
    pub(crate) flag: &'static Flag,
    /// The word as it was typed, which is what a refusal names.
    pub(crate) word: &'a str,
    pub(crate) given: Given<'a>,
}

pub(crate) struct Walk<'a> {
    pub(crate) seen: Vec<Seen<'a>>,
    /// Words that are nobody's value.
    pub(crate) positionals: Vec<&'a str>,
    /// The first word the declaration does not account for; the walk stops
    /// there, because what follows it belongs to a flag nobody declared.
    pub(crate) unknown: Option<&'a str>,
}

impl Walk<'_> {
    /// A flag whose value no reader of this line would get — every shape that
    /// reaches a reader as a silent default, and the whole of what either
    /// command line asks. A flag with nothing after it — the end of the line,
    /// an empty `--smp=`, or a `--worktree` owning no words — answers `None` to
    /// [`Vocabulary::value`] and `&[]` to [`Vocabulary::rest`]; a flag written
    /// twice has every use but one dropped; and an inline value is dropped by
    /// exactly two readers, `Value::None` having none and `rest` taking only the
    /// words after the flag, [`Given::value`] handing it to all the rest.
    pub(crate) fn malformed(&self) -> Option<String> {
        for (at, seen) in self.seen.iter().enumerate() {
            let name = seen.flag.name;
            let shape = shape(seen.flag.value);
            let nothing_after = match seen.given {
                Given::Next(_) => false,
                Given::Inline(value) => value.is_empty() && seen.flag.value != Value::None,
                Given::Nothing => matches!(seen.flag.value, Value::Next | Value::Each),
                Given::Rest(rest) => rest.is_empty(),
            };
            if nothing_after {
                return Some(format!("{name} was given no value: {name}{shape}."));
            }
            if matches!(seen.given, Given::Inline(_))
                && matches!(seen.flag.value, Value::None | Value::Rest)
            {
                let takes = match seen.flag.value {
                    Value::Rest => format!("takes its words after it, {name}{shape}"),
                    _ => "takes no value".to_string(),
                };
                return Some(format!("{:?}: {name} {takes}.", seen.word));
            }
            if seen.flag.value != Value::Each
                && self.seen[..at].iter().any(|earlier| earlier.flag.name == name)
            {
                return Some(format!(
                    "{name} was given twice, and a second use is read by nothing."
                ));
            }
        }
        None
    }
}

impl Vocabulary {
    /// Flag then value, because a declared flag's value is that flag's and is
    /// never a flag itself.
    pub(crate) fn walk<'a>(&self, args: &'a [String]) -> Walk<'a> {
        let (mut seen, mut positionals) = (Vec::new(), Vec::new());
        let mut at = 0;
        while at < args.len() {
            let word = args[at].as_str();
            at += 1;
            let (name, inline) = match word.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (word, None),
            };
            let Some(flag) = self.0.iter().copied().find(|f| f.name == name) else {
                if word.starts_with('-') {
                    return Walk { seen, positionals, unknown: Some(word) };
                }
                positionals.push(word);
                continue;
            };
            let given = match (inline, flag.value) {
                (Some(value), _) => Given::Inline(value),
                (None, Value::None) => Given::Nothing,
                (None, Value::Next | Value::Each) => take(args, &mut at),
                (None, Value::Optional) => match args.get(at) {
                    Some(next) if next.starts_with('-') => Given::Nothing,
                    _ => take(args, &mut at),
                },
                (None, Value::Rest) => {
                    let rest = &args[at..];
                    at = args.len();
                    Given::Rest(rest)
                }
            };
            seen.push(Seen { flag, word, given });
        }
        Walk { seen, positionals, unknown: None }
    }

    /// Whether the line names `want`.
    pub fn present(&self, args: &[String], want: &Flag) -> bool {
        self.walk(args).seen.iter().any(|seen| seen.flag.name == want.name)
    }

    /// Every value a repeatable `<flag> <value>` carried, in the order given.
    pub fn values<'a>(&self, args: &'a [String], want: &Flag) -> Vec<&'a str> {
        self.walk(args)
            .seen
            .iter()
            .filter(|seen| seen.flag.name == want.name)
            .filter_map(|seen| seen.given.value())
            .collect()
    }

    /// The one value `want` carried; [`Walk::malformed`] has already refused a
    /// line that wrote it twice.
    pub fn value<'a>(&self, args: &'a [String], want: &Flag) -> Option<&'a str> {
        self.values(args, want).first().copied()
    }

    /// The words a [`Value::Rest`] flag owns.
    pub fn rest<'a>(&self, args: &'a [String], want: &Flag) -> &'a [String] {
        self.walk(args)
            .seen
            .iter()
            .find_map(|seen| match seen.given {
                Given::Rest(rest) if seen.flag.name == want.name => Some(rest),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The declaration as a list to print, one flag a line.
    pub(crate) fn usage(&self) -> String {
        self.0
            .iter()
            .map(|f| format!("  {}{}", f.name, shape(f.value)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn take<'a>(args: &'a [String], at: &mut usize) -> Given<'a> {
    match args.get(*at) {
        Some(value) => {
            *at += 1;
            Given::Next(value)
        }
        None => Given::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().copied().map(String::from).collect()
    }

    fn checked(words: &[&str]) -> Outcome {
        check(&argv(words))
    }

    fn refusal(words: &[&str]) -> String {
        match checked(words) {
            Outcome::Refuse(message) => message,
            _ => panic!("{words:?} must be refused"),
        }
    }

    #[test]
    fn an_argument_the_declaration_does_not_expect_is_refused_by_name() {
        for words in [
            vec!["--frobnicate"],
            vec!["-build-only"],
            vec!["-h"],
            vec!["buildonly"],
            vec!["--build-only", "target/bootable.img"],
        ] {
            let bad = words.last().expect("a command line to refuse");
            let message = refusal(&words);
            assert!(message.contains(bad), "{words:?}: {message}");
        }
    }

    #[test]
    fn an_inline_value_is_refused_exactly_where_it_would_be_dropped() {
        let debug = refusal(&["--debug=1"]);
        assert!(debug.contains("--debug takes no value"), "{debug}");
        let worktree = refusal(&["--worktree=add"]);
        assert!(worktree.contains("--worktree <subcommand>"), "{worktree}");
        let line = argv(&["--smp=4", "--known-red=audio_tone", "--kernel-param=slow"]);
        assert!(matches!(check(&line), Outcome::Proceed));
        assert_eq!(CARGO_RUN.value(&line, &SMP), Some("4"));
        assert_eq!(CARGO_RUN.value(&line, &KNOWN_RED), Some("audio_tone"));
        assert_eq!(CARGO_RUN.values(&line, &KERNEL_PARAM), ["slow"]);
    }

    #[test]
    fn a_flag_left_without_its_value_is_refused() {
        for flag in
            CARGO_RUN.0.iter().filter(|f| !matches!(f.value, Value::None | Value::Optional))
        {
            for word in [flag.name.to_string(), format!("{}=", flag.name)] {
                let message = refusal(&[word.as_str()]);
                assert!(message.contains(flag.name), "{word}: {message}");
                assert!(message.contains("no value"), "{word}: {message}");
            }
        }
        assert!(refusal(&["--known-red="]).contains("no value"), "an optional value is a value");
        assert!(matches!(checked(&["--known-red"]), Outcome::Proceed), "--known-red answers alone");
        assert!(matches!(checked(&["--known-red", "audio_tone"]), Outcome::Proceed));
        assert!(refusal(&["--known-red", "--frobnicate"]).contains("--frobnicate"));
    }

    #[test]
    fn a_flag_that_reads_one_value_is_refused_when_it_is_written_twice() {
        for words in [
            vec!["--smp", "1", "--smp", "2"],
            vec!["--boot-config", "diag", "--boot-config", "console"],
            vec!["--build-only", "--build-only"],
        ] {
            let message = refusal(&words);
            assert!(message.contains(words[0]), "{words:?}: {message}");
            assert!(message.contains("twice"), "{words:?}: {message}");
        }
    }

    /// The only thing `values` does that `value` does not: a repeatable flag
    /// keeps every value, in the order given.
    #[test]
    fn a_repeatable_flag_keeps_every_value_it_was_given() {
        let line =
            argv(&["--kernel-param", "slow", "--kernel-feature", "x", "--kernel-param", "a"]);
        assert!(matches!(check(&line), Outcome::Proceed));
        assert_eq!(CARGO_RUN.values(&line, &KERNEL_PARAM), ["slow", "a"]);
        assert_eq!(CARGO_RUN.values(&line, &KERNEL_FEATURE), ["x"]);
    }

    /// The value of a flag that takes one is that flag's, whatever it spells —
    /// for the checker and for every reader of the same line.
    #[test]
    fn a_declared_flags_value_is_never_read_as_a_flag() {
        for words in [
            vec!["--kernel-param", "--help"],
            vec!["--boot-config", "--help"],
            vec!["--worktree", "add", "--help"],
            vec!["--boot-config", "diag", "--build-only"],
        ] {
            assert!(matches!(checked(&words), Outcome::Proceed), "{words:?} must proceed");
        }
        let line = argv(&["--kernel-param", "--help"]);
        assert_eq!(CARGO_RUN.values(&line, &KERNEL_PARAM), ["--help"]);
        assert!(!CARGO_RUN.present(&line, &HELP), "the actuator's name is not the flag");
        let worktree = argv(&["--worktree", "add", "/tmp/wt"]);
        assert_eq!(CARGO_RUN.rest(&worktree, &WORKTREE), ["add", "/tmp/wt"]);
        assert_eq!(CARGO_RUN.value(&argv(&["--boot-config", "diag"]), &BOOT_CONFIG), Some("diag"));
        assert_eq!(CARGO_RUN.value(&argv(&["--build-only"]), &BOOT_CONFIG), None);
    }

    #[test]
    fn help_is_the_whole_declared_list() {
        let Outcome::Help(message) = checked(&["--help"]) else {
            panic!("--help must be accepted");
        };
        for flag in CARGO_RUN.0 {
            assert!(message.contains(flag.name), "{} is missing from --help: {message}", flag.name);
        }
    }

    /// Every file in this repository that can carry a command: the whole tree
    /// and not a list of directories, so a script or workflow arriving anywhere
    /// is read on its first commit.
    fn scanned_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        files_under(root, &mut files);
        files.sort();
        files
    }

    fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("readable dir entry").path();
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let dotted = name.starts_with('.') && name != ".github";
            if path.is_dir() {
                if name == "target" || name == "rust" || dotted {
                    continue;
                }
                files_under(&path, out);
                continue;
            }
            let carries = path.extension().is_none_or(|e| {
                matches!(&*e.to_string_lossy(), "rs" | "sh" | "yml" | "yaml" | "toml")
            });
            if carries && !dotted {
                out.push(path);
            }
        }
    }

    fn read(file: &Path) -> String {
        std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
    }

    /// A flag this tree's own workflows, sources and scripts pass that the
    /// declaration lacks would refuse a run nothing is watching.
    #[test]
    fn every_flag_the_tree_passes_to_cargo_run_is_declared() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let files = scanned_files(&root);
        // A walk that reached less than it claims would pass by reading
        // nothing, so the reach is asserted before the flags are.
        for named in [
            ".github/workflows/ci.yml",
            "diag/flash.sh",
            "userland/doom/build.rs",
            "toyos-symbols/tests/real.rs",
            "kernel/Cargo.toml",
            "tests/toyos.rs",
            "NOTICE",
        ] {
            assert!(files.contains(&root.join(named)), "the scan does not reach {named}");
        }
        for file in &files {
            let under = file.strip_prefix(&root).expect("a file under the root");
            let dotted =
                under.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
            assert!(!dotted || under.starts_with(".github"), "the scan reaches {under:?}");
        }

        let declared: BTreeSet<&str> = CARGO_RUN.0.iter().map(|f| f.name).collect();
        let mut seen = BTreeSet::new();
        for file in files {
            for flag in flags_passed_to_cargo_run(&read(&file)) {
                assert!(
                    declared.contains(flag.as_str()),
                    "{} passes {flag} to `cargo run --`, and src/flags.rs does not declare it",
                    file.display()
                );
                seen.insert(flag);
            }
        }
        assert!(seen.contains("--build-only"), "the scan read no command line at all: {seen:?}");
    }

    /// Every `--flag` a text hands to *this* binary's `cargo run --`, directly
    /// on the line or one shell variable away. A `cargo run` naming another
    /// package, binary or example runs a command line this table does not own.
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
            let selected = &line[at..at + sep];
            if ["-p ", "--package", "--bin", "--example", "--manifest-path"]
                .iter()
                .any(|other| selected.contains(other))
            {
                continue;
            }
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
        let end =
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        let token = &rest[..end];
        (token.starts_with("--") && token.len() > 2).then(|| token.to_string())
    }

    /// The forms the scan has to read, the ones it must not attribute to this
    /// binary — `-p toyos-sched-sim` and `--example imgstat` run command lines
    /// this table knows nothing about — and the two it does not reach at all:
    /// the continued line yields nothing, and `--debug=1` yields `--debug`.
    #[test]
    fn the_scan_reads_this_binarys_command_lines_and_no_others() {
        let text = "\
            base_arg=\"--tier-base $TIER_BASE\"\n\
            ARGS=$ARGS --build-only\n\
            run: cargo run -- --diag-boot $base_arg $ARGS `--clippy`\n\
            //! cargo run -- --console-boot\n\
            cargo run --release -p toyos-sched-sim -- --fuzz-sweep\n\
            cargo run --example imgstat -- --histogram\n\
            cargo run --\n\
            --rebuild-toolchain\n\
            cargo run -- --debug=1\n";
        assert_eq!(
            flags_passed_to_cargo_run(text),
            ["--build-only", "--clippy", "--console-boot", "--debug", "--diag-boot", "--tier-base"]
                .map(String::from)
                .into_iter()
                .collect::<BTreeSet<String>>()
        );
    }
}
