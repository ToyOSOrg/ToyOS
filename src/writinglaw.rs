//! The writing law: what a landing may add in prose is priced by what it adds
//! in code.
//!
//! Over every `.rs` file a branch changes against its merge-base with the base
//! ref, Δc is the net comment lines and Δk the net code lines, both by
//! `prosegate::measure`'s one methodology; the rule is `4·Δc ≤ max(Δk, 0)` —
//! at most one comment line per four code lines, and a branch that removes
//! code adds no prose. Over every `CLAUDE.md` it changes, Δw is the net words
//! and the rule is `Δw ≤ 0`. Both sides are read from the committed trees,
//! never the working tree, so `--pr` and CI's `abi-split` job judge the same
//! bytes.

use std::path::Path;

use crate::pr::git;
use crate::prosegate::{measure, UNLEDGERED};

/// One changed `.rs` file's movement between the merge-base and HEAD.
struct Delta {
    path: String,
    comments: i64,
    code: i64,
}

/// The law over the branch at `root` against `base`: the one-line verdict, or
/// the refusal.
pub fn judge(root: &Path, base: &str) -> Result<String, String> {
    let merge_base = git(root, &["merge-base", base, "HEAD"])?;
    let changed = git(root, &["diff", "--name-only", "--no-renames", "-z", &merge_base, "HEAD"])?;

    let mut sources: Vec<Delta> = Vec::new();
    let mut guides: Vec<(String, i64)> = Vec::new();
    for path in changed.split('\0').filter(|p| !p.is_empty() && !excluded(p)) {
        if path.ends_with(".rs") {
            let at_head = measure(&text(root, "HEAD", path)?);
            let at_base = measure(&text(root, &merge_base, path)?);
            sources.push(Delta {
                path: path.to_string(),
                comments: delta(at_head.comments, at_base.comments),
                code: delta(at_head.code, at_base.code),
            });
        } else if Path::new(path).file_name().is_some_and(|n| n == "CLAUDE.md") {
            let words = |commit: &str| -> Result<usize, String> {
                Ok(text(root, commit, path)?.split_whitespace().count())
            };
            guides.push((path.to_string(), delta(words("HEAD")?, words(&merge_base)?)));
        }
    }

    let dc: i64 = sources.iter().map(|d| d.comments).sum();
    let dk: i64 = sources.iter().map(|d| d.code).sum();
    let allowance = dk.max(0) / 4;
    let dw: i64 = guides.iter().map(|(_, words)| *words).sum();
    if dc <= allowance && dw <= 0 {
        return Ok(format!(
            "{dc:+} comment lines against {dk:+} code lines (allowance {allowance}); CLAUDE.md \
             {dw:+} words."
        ));
    }

    let mut lines = Vec::new();
    if dc > allowance {
        lines.push(format!(
            "[prose] this branch adds {dc:+} comment lines against {dk:+} code lines, and the \
             writing law allows one comment line per four net new code lines: allowance \
             {allowance}."
        ));
        sources.retain(|d| d.comments != 0);
        sources.sort_by(|a, b| b.comments.cmp(&a.comments).then_with(|| a.path.cmp(&b.path)));
        lines.push("[prose]   Δcomments  Δcode  path".to_string());
        for d in &sources {
            lines.push(format!(
                "[prose]   {:>9}  {:>5}  {}",
                format!("{:+}", d.comments),
                format!("{:+}", d.code),
                d.path
            ));
        }
        lines.push(format!(
            "[prose] cut {} comment line(s) — first at the sites this branch touched; \
             `cargo test --lib prosegate` prints the ledger rows to lower.",
            dc - allowance
        ));
    }
    if dw > 0 {
        for (path, words) in guides.iter().filter(|(_, words)| *words > 0) {
            lines.push(format!("[prose] {path} grew by {words} word(s)."));
        }
        lines.push(
            "[prose] A CLAUDE.md never grows: a bullet in is a bullet out, and the story goes in \
             the commit message."
                .to_string(),
        );
    }
    Err(lines.join("\n"))
}

/// Under a directory the ledger does not enter either.
fn excluded(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|c| UNLEDGERED.iter().any(|dir| c.as_os_str().to_str() == Some(*dir)))
}

/// `path`'s text at `commit`: empty where that commit does not hold it.
fn text(root: &Path, commit: &str, path: &str) -> Result<String, String> {
    if git(root, &["ls-tree", commit, "--", path])?.is_empty() {
        return Ok(String::new());
    }
    git(root, &["show", &format!("{commit}:{path}")])
}

fn delta(head: usize, base: usize) -> i64 {
    i64::try_from(head).expect("line count") - i64::try_from(base).expect("line count")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::tests::{commit, repo, sh};

    /// `comments` comment lines, then `code` code lines.
    fn body(comments: usize, code: usize) -> String {
        let mut text = String::new();
        for n in 0..comments {
            text.push_str(&format!("// comment {n}\n"));
        }
        for n in 0..code {
            text.push_str(&format!("fn f{n}() {{}}\n"));
        }
        text
    }

    /// Everything committed so far becomes main's; the branch starts here.
    fn main_is_here(wt: &Path) {
        sh(wt, &["branch", "-f", "main", "HEAD"]);
    }

    #[test]
    fn one_comment_line_per_four_code_lines_and_not_one_more() {
        let (_origin, wt) = repo("law-ratio");
        commit(&wt, "kernel/src/a.rs", &body(5, 20), "five against twenty");
        let line = judge(&wt, "main").expect("one per four is the law's own ratio");
        assert_eq!(
            line,
            "+5 comment lines against +20 code lines (allowance 5); CLAUDE.md +0 words."
        );

        commit(&wt, "kernel/src/a.rs", &body(6, 20), "one over");
        let refusal = judge(&wt, "main").expect_err("six against twenty is one over");
        assert!(refusal.contains("+6 comment lines against +20 code lines"), "{refusal}");
        assert!(refusal.contains("allowance 5"), "{refusal}");
        assert!(refusal.contains("cut 1 comment line"), "{refusal}");
        let row = refusal.lines().find(|l| l.contains("kernel/src/a.rs")).expect("a table row");
        assert!(row.contains("+6") && row.contains("+20"), "{row}");
    }

    #[test]
    fn prose_alone_buys_nothing_and_neither_does_removed_code() {
        let (_origin, wt) = repo("law-nofund");
        commit(&wt, "kernel/src/a.rs", &body(3, 0), "prose only");
        let refusal = judge(&wt, "main").expect_err("no new code allows no prose");
        assert!(refusal.contains("+3 comment lines against +0 code lines"), "{refusal}");

        // The branch shrinks the code and narrates the shrinking.
        let (_origin, wt) = repo("law-negative");
        commit(&wt, "kernel/src/a.rs", &body(0, 50), "the code");
        main_is_here(&wt);
        commit(&wt, "kernel/src/a.rs", &body(3, 10), "shrink and narrate");
        let refusal = judge(&wt, "main").expect_err("removed code funds no prose");
        assert!(refusal.contains("+3 comment lines against -40 code lines"), "{refusal}");
        assert!(refusal.contains("allowance 0"), "{refusal}");
    }

    #[test]
    fn a_sweep_that_only_cuts_prose_passes() {
        let (_origin, wt) = repo("law-sweep");
        commit(&wt, "kernel/src/a.rs", &body(100, 0), "the prose");
        main_is_here(&wt);
        commit(&wt, "kernel/src/a.rs", "", "the sweep");
        let line = judge(&wt, "main").expect("a sweep adds nothing");
        assert!(line.starts_with("-100 comment lines against +0 code lines"), "{line}");
    }

    #[test]
    fn a_new_file_counts_fully_and_a_deleted_one_counts_negatively() {
        let (_origin, wt) = repo("law-files");
        commit(&wt, "kernel/src/new.rs", &body(3, 4), "a new file");
        let refusal = judge(&wt, "main").expect_err("three against four is over the ratio");
        assert!(refusal.contains("+3 comment lines against +4 code lines"), "{refusal}");
        assert!(refusal.contains("kernel/src/new.rs"), "{refusal}");

        main_is_here(&wt);
        sh(&wt, &["rm", "-q", "kernel/src/new.rs"]);
        commit(&wt, "kernel/src/other.rs", &body(1, 4), "the file goes");
        let line = judge(&wt, "main").expect("the deletion funds the new file's prose");
        assert!(line.starts_with("-2 comment lines against +0 code lines"), "{line}");
    }

    #[test]
    fn a_claude_md_never_grows() {
        let (_origin, wt) = repo("law-guide");
        commit(&wt, "src/CLAUDE.md", "# Build\n\nOne rule here.\n", "the guide");
        main_is_here(&wt);
        commit(&wt, "src/CLAUDE.md", "# Build\n\nOne longer rule here.\n", "one word in");
        let refusal = judge(&wt, "main").expect_err("a CLAUDE.md never grows");
        assert!(refusal.contains("src/CLAUDE.md grew by 1 word"), "{refusal}");
        assert!(refusal.contains("never grows"), "{refusal}");

        commit(&wt, "src/CLAUDE.md", "# Build\n\nOne rule.\n", "a word out");
        let line = judge(&wt, "main").expect("a shrinking CLAUDE.md passes");
        assert!(line.ends_with("CLAUDE.md -1 words."), "{line}");
    }

    #[test]
    fn the_fork_is_not_this_repositorys_prose() {
        let (_origin, wt) = repo("law-fork");
        commit(&wt, "rust/library/std/src/x.rs", &body(10, 0), "the fork moves");
        let line = judge(&wt, "main").expect("rust/ is outside the law");
        assert!(line.starts_with("+0 comment lines against +0 code lines"), "{line}");
    }

    /// The table is sorted by Δc descending, and a file whose prose did not
    /// move is not in it.
    #[test]
    fn the_table_lists_the_heaviest_writer_first() {
        let (_origin, wt) = repo("law-table");
        commit(&wt, "kernel/src/small.rs", &body(2, 0), "two");
        commit(&wt, "kernel/src/big.rs", &body(9, 0), "nine");
        commit(&wt, "kernel/src/code.rs", &body(0, 3), "code only");
        let refusal = judge(&wt, "main").expect_err("eleven against three");
        let rows: Vec<&str> = refusal.lines().filter(|l| l.contains("kernel/src/")).collect();
        assert_eq!(rows.len(), 2, "{refusal}");
        assert!(rows[0].contains("big.rs") && rows[1].contains("small.rs"), "{refusal}");
    }
}
