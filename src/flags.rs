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
    /// The next word, whatever it looks like: `--kernel-param --help` arms the
    /// actuator named `--help`. A line that ends without one is refused.
    Next,
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
    pub MERGE_HEALTH = "--merge-health", None;
    pub SINCE = "--since", Next;
    pub DAYS = "--days", Next;
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
    pub KERNEL_PARAM = "--kernel-param", Next;
    pub KERNEL_FEATURE = "--kernel-feature", Next;
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

/// Pure over `args` (as `std::env::args().collect()` produces them, `argv[0]`
/// included) and the declaration.
pub fn check(args: &[String]) -> Outcome {
    let line = CARGO_RUN.walk(args.get(1..).unwrap_or_default());
    if let Some(word) = line.unknown {
        return Outcome::Refuse(unknown(word));
    }
    for seen in &line.seen {
        if let Given::Inline(_) = seen.given {
            return Outcome::Refuse(inline(seen.word, seen.flag));
        }
        if matches!(seen.given, Given::Nothing) && seen.flag.value == Value::Next {
            return Outcome::Refuse(format!(
                "Error: {0} was given no value: {0} <value>.",
                seen.flag.name
            ));
        }
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

/// A value written onto the flag would be dropped in silence by a reader that
/// takes the next word, so each kind of flag is refused with the shape it does
/// accept.
fn inline(word: &str, flag: &Flag) -> String {
    let shape = match flag.value {
        Value::None => format!("{} takes no value", flag.name),
        Value::Next | Value::Optional => {
            format!("{0} takes its value as the next word, {0} <value>", flag.name)
        }
        Value::Rest => {
            format!("{0} takes its subcommand as the next word, {0} <subcommand>", flag.name)
        }
    };
    format!("Error: {word:?}: {shape}.")
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

/// One walk of a command line against one vocabulary.
pub(crate) struct Walk<'a> {
    pub(crate) seen: Vec<Seen<'a>>,
    /// Words that are nobody's value.
    pub(crate) positionals: Vec<&'a str>,
    /// The first word the declaration does not account for; the walk stops
    /// there, because what follows it belongs to a flag nobody declared.
    pub(crate) unknown: Option<&'a str>,
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
                (None, Value::Next) => take(args, &mut at),
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

    /// The one value `want` carried; a second use is refused rather than
    /// resolved.
    pub fn value<'a>(&self, args: &'a [String], want: &Flag) -> Option<&'a str> {
        let found = self.values(args, want);
        assert!(found.len() < 2, "{} takes one value; this asks for {found:?}", want.name);
        found.first().copied()
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
            .map(|f| match f.value {
                Value::None => format!("  {}", f.name),
                Value::Next => format!("  {} <value>", f.name),
                Value::Optional => format!("  {} [<value>]", f.name),
                Value::Rest => format!("  {} <subcommand>", f.name),
            })
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

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("toyos-build").chain(words.iter().copied()).map(String::from).collect()
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

    /// A value written onto the flag is read by nothing, so the refusal says
    /// what that flag does take.
    #[test]
    fn an_inline_value_is_refused_with_the_shape_the_flag_accepts() {
        let debug = refusal(&["--debug=1"]);
        assert!(debug.contains("--debug takes no value"), "{debug}");
        let smp = refusal(&["--smp=4"]);
        assert!(smp.contains("--smp takes its value as the next word, --smp <value>"), "{smp}");
        let worktree = refusal(&["--worktree=add"]);
        assert!(worktree.contains("--worktree <subcommand>"), "{worktree}");
    }

    /// A missing value reached the dispatch as a panic, after the prerequisites
    /// and a `set_current_dir`.
    #[test]
    fn a_flag_left_without_its_value_is_refused() {
        for words in [vec!["--smp"], vec!["--boot-config"], vec!["--kernel-param"]] {
            let message = refusal(&words);
            assert!(message.contains(words[0]), "{words:?}: {message}");
        }
        assert!(matches!(checked(&["--known-red"]), Outcome::Proceed), "--known-red answers alone");
        assert!(matches!(checked(&["--known-red", "audio_tone"]), Outcome::Proceed));
        assert!(refusal(&["--known-red", "--frobnicate"]).contains("--frobnicate"));
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
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
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

    /// A flag this tree's own workflows, sources and scripts pass that the
    /// declaration lacks would refuse a run nothing is watching.
    #[test]
    fn every_flag_the_tree_passes_to_cargo_run_is_declared() {
        let declared: BTreeSet<&str> = CARGO_RUN.0.iter().map(|f| f.name).collect();
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
        let end =
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        let token = &rest[..end];
        (token.starts_with("--") && token.len() > 2).then(|| token.to_string())
    }

    #[test]
    fn the_scan_reads_a_flag_through_a_variable_and_out_of_punctuation() {
        let text = "\
            base_arg=\"--tier-base $TIER_BASE\"\n\
            ARGS=$ARGS --build-only\n\
            run: cargo run -- --diag-boot $base_arg $ARGS `--clippy`\n";
        assert_eq!(
            flags_passed_to_cargo_run(text),
            ["--build-only", "--clippy", "--diag-boot", "--tier-base"]
                .map(String::from)
                .into_iter()
                .collect::<BTreeSet<String>>()
        );
    }
}
