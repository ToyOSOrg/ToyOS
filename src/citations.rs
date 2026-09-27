//! Every path the tracker and the `CLAUDE.md` files cite is a path this tree
//! holds.
//!
//! An issue names the site it is about by path, and so does every `CLAUDE.md`;
//! a path is the claim a reader checks first. Moving or deleting a file leaves
//! every citation of it pointing at nothing, so this reds on one, naming the
//! citing file, its line and the missing path. A move carries its citations in
//! the same merge.
//!
//! **What counts as a citation.** A whitespace-, backtick- or bracket-delimited
//! token that begins with a directory `git` tracks at the root of this tree —
//! `kernel/…`, `src/…`, `issues/…` — with a `:line` or `:first-last` suffix
//! allowed and trailing sentence punctuation dropped. It resolves if `git`
//! tracks it or tracks a file under it. A path `git` does not track is one a
//! clean checkout does not have, however it looks in this one.
//!
//! **A root this tree no longer holds is still read**, or deleting a whole one
//! would silence every citation into it: a token shaped like a file or a
//! directory (`name/…/file.ext`, `name/…/`, the name lowercase) whose first
//! component is no tracked root, no directory below one and no [`FOREIGN`]
//! name is a citation of a root that is gone, and reds.
//!
//! **What is not one**, each because another namespace spells the same text:
//!
//! - a token carrying a pattern — `*`, `{`, `<`, `…`, `$` — which names many
//!   paths or none;
//! - `path:line:column`, which is a Rust panic location, relative to whichever
//!   crate panicked, quoted from a capture;
//! - anything through a `target` directory, which is build output;
//! - `rust/…`, the std fork: `src/forkcheck.rs` governs it, and a linked
//!   worktree's `rust/` is empty until its first build;
//! - `.cargo/…`: `.cargo/config.toml` is the name cargo reads in every
//!   directory, so a mention of one is a kind of file.
//!
//! So a path in another repository is written with that repository's name in
//! front of it (`mio/src/sys/toyos/waker.rs`), a path that no longer exists is
//! written as the revision that held it (`<rev>^:src/durations.rs`), and a path
//! that does not exist yet is written relative to its crate (`arch/api.rs`).
//!
//! **The holes it leaves.** A crate-relative citation (`sched/dump.rs`) is not
//! read, even of a real file, and neither is a gone root whose name is also a
//! directory below one. A `<rev>^:<path>` citation is trusted, never resolved:
//! resolving one needs the history, which the host job's depth-one checkout
//! does not have.
//!
//! Only its own tests read this, so it is not compiled into the build system.

use std::collections::BTreeSet;
use std::path::Path;

/// Roots whose citations this does not read, each for the reason the module
/// header gives.
const NOT_READ: &[&str] = &["rust", ".cargo"];

/// First components the tracker writes that name something outside this tree:
/// QEMU's `hw/` and `chardev/`, the std fork's `library/`, `pal/` and `base/`,
/// the forked or quoted crates, a game's repository, and the host directories a
/// capture was read from.
const FOREIGN: &[&str] = &[
    "base", "chardev", "debug", "gbae", "hw", "library", "memmap2", "mio", "pal", "scratchpad",
    "smoltcp-0.12.0", "softbuffer", "t14-run122", "t14-run76",
];

/// Characters that make a token a pattern rather than a path.
const PATTERN: &[char] = &['*', '{', '}', '<', '>', '…', '$'];

/// Characters that end a token.
fn delimits(c: char) -> bool {
    c.is_whitespace() || matches!(c, '`' | '(' | ')' | '[' | ']' | '"' | '|' | ',' | ';')
}

/// Every file `git` tracks, repository-relative, and every directory above one.
pub fn tracked(root: &Path) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    for file in crate::sysroot::tracked_files(root, &[]) {
        let mut at = 0;
        while let Some(slash) = file[at..].find('/') {
            all.insert(file[..at + slash].to_string());
            at += slash + 1;
        }
        all.insert(file);
    }
    all
}

/// What a first component can mean: the directories `tracked` holds at the
/// root, and the names of those below it.
pub struct Names {
    roots: BTreeSet<String>,
    below: BTreeSet<String>,
}

pub fn names(tracked: &BTreeSet<String>) -> Names {
    let (mut roots, mut below) = (BTreeSet::new(), BTreeSet::new());
    for path in tracked {
        let mut dirs = path.split('/').rev().skip(1).collect::<Vec<_>>();
        if let Some(root) = dirs.pop() {
            roots.insert(root.to_string());
        }
        below.extend(dirs.into_iter().map(String::from));
    }
    Names { roots, below }
}

/// Whether `first/rest` is shaped like a file or a directory rather than prose
/// (`and/or`, `A/B`, `44100/2ch`).
fn path_shaped(first: &str, rest: &str) -> bool {
    let lowercase = |s: &str| s.starts_with(|c: char| c.is_ascii_lowercase());
    let extension = rest.rsplit('/').next().and_then(|last| last.rsplit_once('.')).is_some_and(|(stem, ext)| {
        !stem.is_empty() && lowercase(ext) && ext.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    lowercase(first)
        && first.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && (rest.ends_with('/') || extension)
}

/// Every path `line` cites, with its trailing `/` kept off.
pub fn cited(line: &str, names: &Names) -> Vec<String> {
    let mut found = Vec::new();
    for token in line.split(delimits) {
        if token.contains(PATTERN) {
            continue;
        }
        let token = token.trim_end_matches(['.', '!', '?']);
        let token = token.strip_suffix("'s").unwrap_or(token).trim_end_matches('\'');
        let (path, suffix) = match token.split_once(':') {
            Some((path, suffix)) => (path, Some(suffix)),
            None => (token, None),
        };
        if let Some(suffix) = suffix {
            let numbers: Vec<&str> = suffix.trim_end_matches(':').split(':').collect();
            let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-');
            if numbers.len() >= 2 && numbers.iter().all(|n| numeric(n)) {
                continue;
            }
        }
        let Some((first, rest)) = path.split_once('/') else { continue };
        if NOT_READ.contains(&first) || path.split('/').any(|segment| segment == "target") {
            continue;
        }
        let gone = !names.below.contains(first) && !FOREIGN.contains(&first) && path_shaped(first, rest);
        if names.roots.contains(first) || gone {
            found.push(path.trim_end_matches('/').to_string());
        }
    }
    found
}

/// Every `file:line: cites path` in `text` that `tracked` does not hold.
pub fn dead(file: &str, text: &str, names: &Names, tracked: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for path in cited(line, names) {
            if !tracked.contains(&path) {
                out.push(format!("{file}:{}: cites {path}, which this tree does not hold", n + 1));
            }
        }
    }
    out
}

/// The files whose citations are read: every tracked `.md` under `issues/`, and
/// every tracked `CLAUDE.md`.
pub fn citing(tracked: &BTreeSet<String>) -> Vec<String> {
    tracked
        .iter()
        .filter(|p| {
            (p.starts_with("issues/") && p.ends_with(".md"))
                || p.rsplit('/').next() == Some("CLAUDE.md")
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// **The gate.** Every path an issue or a `CLAUDE.md` cites resolves.
    #[test]
    fn every_path_the_tracker_and_the_claude_files_cite_exists() {
        let root = repo_root();
        let tracked = tracked(&root);
        let names = names(&tracked);
        let files = citing(&tracked);
        assert!(
            files.iter().any(|f| f == "issues/README.md") && files.iter().any(|f| f == "CLAUDE.md"),
            "the walk reached neither the tracker nor the root CLAUDE.md, so it reads nothing: {files:?}"
        );
        assert!(["kernel", "src", "issues"].iter().all(|r| names.roots.contains(*r)), "{:?}", names.roots);
        let mut complaints = Vec::new();
        let mut read = 0;
        for file in &files {
            let text = std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("read {file}: {e}"));
            read += text.lines().map(|l| cited(l, &names).len()).sum::<usize>();
            complaints.extend(dead(file, &text, &names, &tracked));
        }
        assert!(read > 500, "only {read} citations read across {} files; the scan is reading nothing", files.len());
        assert!(
            complaints.is_empty(),
            "{} citation(s) name a path this tree does not hold. Point each at where the file went, \
             or write a deleted one as the revision that held it (`<rev>^:<path>`):\n{}",
            complaints.len(),
            complaints.join("\n"),
        );
    }

    /// Teeth: a dead citation reds, naming the file, the line and the path; a
    /// live one, a directory and a line-suffixed one do not; and a root the
    /// tree no longer holds is read rather than forgotten.
    #[test]
    fn a_dead_citation_is_named_and_a_live_one_is_not() {
        let tracked = set(&["kernel", "kernel/src", "kernel/src/vfs.rs", "issues", "issues/README.md"]);
        let names = names(&tracked);
        let text = "See `kernel/src/vfs.rs:32` and kernel/src/.\nThen `kernel/src/gone.rs`, and issues/README.md.\n\
                    `toyos-cc/src/lower.rs:9` and toyos-cc/tests/, not toyos-cc/, and/or `src/vfs.rs` in A/B.\n";
        assert_eq!(dead("issues/x/y.md", text, &names, &tracked), [
            "issues/x/y.md:2: cites kernel/src/gone.rs, which this tree does not hold",
            "issues/x/y.md:3: cites toyos-cc/src/lower.rs, which this tree does not hold",
            "issues/x/y.md:3: cites toyos-cc/tests, which this tree does not hold",
        ]);
    }

    /// What another namespace spells is not read as ours.
    #[test]
    fn a_pattern_a_panic_location_build_output_and_a_foreign_path_are_not_citations() {
        let names = names(&set(&["kernel/src/arch/tlb.rs", "src/lib.rs", "tests/a", "rust/x", ".cargo/c"]));
        for line in [
            "PANIC: panicked at src/arch/tlb.rs:171:42:",
            "LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=38",
            "`kernel/src/arch/idt/*.rs` and `tests/<name>case/system.toml`",
            "`kernel/src/{a,b}.rs`, `kernel/src/…`",
            "`kernel/target` 318 MB, `userland/target`",
            "the fork's `rust/src/bootstrap/` and a `.cargo/config.toml`",
            "`mio/src/sys/toyos/waker.rs` and `d5c2d9c9^:src/durations.rs`",
            "https://github.com/ToyOSOrg/ToyOS/blob/main/src/lib.rs",
            "`arch/api.rs`, 44100/2ch/i16, read/write and A/B",
        ] {
            assert!(cited(line, &names).is_empty(), "{line:?} read as {:?}", cited(line, &names));
        }
        assert_eq!(cited("`src/redlist.rs`'s row, `kernel/src/a.rs:1-9`.", &names), ["src/redlist.rs", "kernel/src/a.rs"]);
        assert_eq!(cited("(src/x.rs) [kernel/y/](kernel/y/)", &names), ["src/x.rs", "kernel/y", "kernel/y"]);
    }
}
