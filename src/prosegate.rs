//! The comment law, and the ratchet that holds the tree to it.
//!
//! **Three kinds of comment survive**: the one-clause invariant at the edit
//! site, the boundary contract, and the refusal-reason at a surprising
//! decision — over a module doc that is the contract and nothing else.
//! Investigation history, chronology, the provenance of a measurement, what an
//! earlier implementation did, and narration of what the code plainly says
//! belong in the commit message and in `issues/`. Both are exempt from this
//! gate, along with every other file that is not `.rs`: a commit message is
//! addressed to whoever reads that change, and a source comment to whoever
//! reads the code today.
//!
//! **The ratchet.** [`LEDGER`] records, for every `.rs` file the tree holds,
//! the comment lines and the dated comment lines it is permitted. A file
//! measured above either number is refused; a file measured below either is
//! green, and the shrinkage prints so a sweep knows which rows to rewrite.
//! Raising an entry is a line in the diff somebody wrote on purpose, which is
//! the whole mechanism: no comment is forbidden and no comment is free.
//!
//! **Slack is tolerated deliberately.** Requiring a file to sit at its exact
//! count would red every unrelated change that deletes code, and a gate that
//! reds on innocent work is one people route around. Only a deliberate
//! re-record lowers an entry.
//!
//! **Chronology is banned outright rather than ratcheted**, and it carries a
//! second lock: [`DATED_TOTAL`] is the ledger's whole dated column, declared
//! here, and a ledger that does not sum to it exactly is refused. So a file's
//! dated entry rises only in a change that edits this file too — and a file
//! absent from the ledger is admitted at a dated column of zero, which is what
//! makes a date in a new file two deliberate edits away rather than none.
//!
//! **No density target, and that is the design.** A share of comment lines is
//! a diagnostic and never a definition of quality: a file that is 70% comment
//! may be a register map that has to be, and one at 5% may be unreadable. A
//! threshold makes the ratio the goal, and the three kinds that survive delete
//! as easily as the kinds that do not. This gate has no opinion about how much
//! prose a file carries; it has one about prose being added to it.
//!
//! **The methodology, stated once and applied to every file.** A comment line
//! is one whose first non-whitespace characters are `//`, `/*` or `*`; a dated
//! comment line is such a line also carrying a `20NN-NN`. It counts a block
//! comment's `*`-led continuation lines and its `*/` closer, and it does not
//! count a comment trailing code on the same line — so a file's real prose is
//! higher than its entry, which costs a ceiling nothing. Its one false
//! positive is a statement opening with a dereference;
//! `rg -n '^[[:space:]]*\*[^ /]' $(git ls-files '*.rs')` names every line in
//! the tree it miscounts.
//!
//! **Every `.rs` file the repository holds is ledgered.** [`UNLEDGERED`] is
//! the whole of the exclusion — the compiler fork and build output — so there
//! is no first-party list to drift out of date: a file with no comments sits
//! at `0 0`, and a file that leaves the tree is a red until its row does.
//! `userland/libc`'s relaxed rules are about the code it may contain and not
//! about the narration committed beside it, so it is ledgered like the rest.
//!
//! **Two citation laws the sweeps settled.** A `§` in a source comment names
//! its document or it is a dead pointer — a mark reads authoritative whether
//! or not anything backs it, which is how one survived the corpus it cited.
//! And a citation living in a string literal or an identifier is code, not
//! prose: a comments-only sweep flags it and never edits it, because resolving
//! it changes program output or a public name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The ledger, relative to the repository root.
const LEDGER: &str = "src/prose-ledger";

/// The only directories the walk does not enter.
const UNLEDGERED: &[&str] = &["rust", "target", ".git"];

/// [`LEDGER`]'s dated column, summed.
///
/// Declared apart from the ledger so admitting one dated comment line costs an
/// edit here as well as there. It only goes down: a sweep that removes
/// chronology lowers this and the rows together.
const DATED_TOTAL: usize = 230;

/// The sentence a raised entry has to be worth.
const RAISING: &str =
    "raising the ledger is the deliberate act; the same PR edits it or the prose goes";

/// One file's prose: measured from its text, or permitted by its ledger row.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Prose {
    comments: usize,
    dated: usize,
}

/// One file's [`Prose`], by the methodology in this module's header.
fn measure(text: &str) -> Prose {
    let mut prose = Prose::default();
    for line in text.lines() {
        let line = line.trim_start();
        if !(line.starts_with("//") || line.starts_with("/*") || line.starts_with('*')) {
            continue;
        }
        prose.comments += 1;
        prose.dated += usize::from(dated(line));
    }
    prose
}

/// Whether `line` carries a `20NN-NN`.
fn dated(line: &str) -> bool {
    line.as_bytes().windows(7).any(|w| {
        w[0] == b'2'
            && w[1] == b'0'
            && w[2].is_ascii_digit()
            && w[3].is_ascii_digit()
            && w[4] == b'-'
            && w[5].is_ascii_digit()
            && w[6].is_ascii_digit()
    })
}

/// `path` relative to the repository root, with forward slashes.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Every `.rs` file the tree holds, as `(relative path, text)`.
fn sources(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skipped = path.file_name().is_some_and(|name| {
                UNLEDGERED.iter().any(|dir| name.to_str() == Some(*dir))
            });
            if !skipped {
                walk(root, &path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            out.push((rel(root, &path), text));
        }
    }
}

/// The ledger's rows, or the first line that is not one.
///
/// Byte order is refused rather than sorted away: rows land where a reader and
/// a merge both expect them, and an appended row that sorts elsewhere is a
/// mistake worth naming at the line it is on.
fn read_ledger(text: &str) -> Result<BTreeMap<String, Prose>, String> {
    let mut rows = BTreeMap::new();
    let mut previous = String::new();
    for (n, line) in text.lines().enumerate() {
        let at = format!("{LEDGER}:{}", n + 1);
        let mut fields = line.split_whitespace();
        let row = (|| {
            let path = fields.next()?;
            let comments = fields.next()?.parse().ok()?;
            let dated = fields.next()?.parse().ok()?;
            fields.next().is_none().then(|| (path.to_string(), Prose { comments, dated }))
        })();
        let Some((path, prose)) = row else {
            return Err(format!(
                "{at}: not `<path> <comment lines> <dated comment lines>`: {line:?}"
            ));
        };
        if path <= previous {
            return Err(format!("{at}: `{path}` sorts before `{previous}`; the ledger is ordered"));
        }
        previous.clone_from(&path);
        rows.insert(path, prose);
    }
    Ok(rows)
}

/// Every refusal the tree earns against the ledger, one line each.
///
/// Takes both sides and the declared total rather than reading any of them, so
/// the negative control stages a tree that is not on disk.
fn refusals(
    permitted: &BTreeMap<String, Prose>,
    measured: &BTreeMap<String, Prose>,
    declared_total: usize,
) -> Vec<String> {
    let mut bad = Vec::new();

    let summed: usize = permitted.values().map(|p| p.dated).sum();
    if summed != declared_total {
        bad.push(format!(
            "{LEDGER}'s dated column sums to {summed} where `DATED_TOTAL` in src/prosegate.rs is \
             {declared_total}. Chronology is admitted by two deliberate edits or by none: a date \
             in a source comment belongs in the commit message and in issues/"
        ));
    }

    for (path, found) in measured {
        let Some(allowed) = permitted.get(path) else {
            bad.push(format!(
                "{path} is not in {LEDGER}: add the row `{path} {} 0`. A file enters at what it \
                 measures and at a dated column of zero",
                found.comments
            ));
            if found.dated > 0 {
                bad.push(format!(
                    "{path} is new to {LEDGER} and carries {} dated comment line(s). Chronology \
                     is the one kind banned outright: move it to the commit message",
                    found.dated
                ));
            }
            continue;
        };
        if found.comments > allowed.comments {
            bad.push(format!(
                "{path}: {} comment lines, {LEDGER} permits {} — {RAISING}",
                found.comments, allowed.comments
            ));
        }
        if found.dated > allowed.dated {
            bad.push(format!(
                "{path}: {} dated comment lines, {LEDGER} permits {}. A date in a source comment \
                 is chronology and belongs in the commit message and in issues/ — {RAISING}",
                found.dated, allowed.dated
            ));
        }
    }

    for path in permitted.keys() {
        if !measured.contains_key(path) {
            bad.push(format!(
                "{LEDGER} has a row for {path}, which is not in the tree. The row goes in the \
                 merge that deletes the file"
            ));
        }
    }

    bad
}

/// The rows a re-record would rewrite, each already in the ledger's own form.
fn shrinkage(
    permitted: &BTreeMap<String, Prose>,
    measured: &BTreeMap<String, Prose>,
) -> Vec<String> {
    let mut rows = Vec::new();
    for (path, found) in measured {
        let Some(allowed) = permitted.get(path) else { continue };
        if found.comments < allowed.comments || found.dated < allowed.dated {
            rows.push(format!("{path} {} {}", found.comments, found.dated));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn tree() -> BTreeMap<String, Prose> {
        sources(&repo_root()).into_iter().map(|(path, text)| (path, measure(&text))).collect()
    }

    /// The ratchet over the tree as it stands.
    ///
    /// The file count is asserted for the reason every walk here asserts one: a
    /// walk that quietly found nothing waves the whole tree through and reads
    /// as green.
    #[test]
    fn no_file_carries_more_prose_than_the_ledger_permits() {
        let root = repo_root();
        let measured = tree();
        assert!(
            measured.len() > 600,
            "only {} .rs file(s) found — the walk is looking elsewhere",
            measured.len()
        );

        let text = std::fs::read_to_string(root.join(LEDGER))
            .unwrap_or_else(|e| panic!("reading {LEDGER}: {e}"));
        let permitted = read_ledger(&text).unwrap_or_else(|e| panic!("{e}"));

        let shrunk = shrinkage(&permitted, &measured);
        if !shrunk.is_empty() {
            println!(
                "{} file(s) are under their entry. A sweep books the win by replacing these rows \
                 in {LEDGER} and lowering `DATED_TOTAL` to match:\n{}",
                shrunk.len(),
                shrunk.join("\n")
            );
        }

        let bad = refusals(&permitted, &measured, DATED_TOTAL);
        assert!(
            bad.is_empty(),
            "the comment law is this module's header, and {LEDGER} is what the tree is permitted \
             against it:\n  {}",
            bad.join("\n  ")
        );
    }

    fn staged(rows: &[(&str, usize, usize)]) -> BTreeMap<String, Prose> {
        rows.iter()
            .map(|(path, comments, dated)| {
                (path.to_string(), Prose { comments: *comments, dated: *dated })
            })
            .collect()
    }

    fn says(
        permitted: &[(&str, usize, usize)],
        measured: &[(&str, usize, usize)],
        total: usize,
        needle: &str,
    ) {
        let bad = refusals(&staged(permitted), &staged(measured), total);
        assert!(
            bad.iter().any(|b| b.contains(needle)),
            "expected a refusal naming {needle:?}, got {bad:?}"
        );
    }

    /// The teeth, one case per red the ratchet exists to fire.
    ///
    /// Every arm here is a negative control: strike the comparison it rests on
    /// out of [`refusals`] and this test is what fails.
    #[test]
    fn the_ratchet_refuses_what_it_is_for() {
        let ledgered = &[("a.rs", 10, 0), ("b.rs", 5, 2)][..];

        // A file over its comment ceiling.
        says(ledgered, &[("a.rs", 11, 0), ("b.rs", 5, 2)], 2, "10 — raising the ledger");
        // A file over its dated ceiling, with its comment count untouched.
        says(ledgered, &[("a.rs", 10, 0), ("b.rs", 5, 3)], 2, "3 dated comment lines");
        // A new file, absent from the ledger.
        says(ledgered, &[("a.rs", 10, 0), ("b.rs", 5, 2), ("c.rs", 4, 0)], 2, "c.rs is not in");
        // A new file carrying chronology: two refusals, and this is the second.
        says(
            ledgered,
            &[("a.rs", 10, 0), ("b.rs", 5, 2), ("c.rs", 4, 1)],
            2,
            "carries 1 dated comment line",
        );
        // A ledgered ghost.
        says(ledgered, &[("a.rs", 10, 0)], 2, "which is not in the tree");
        // A dated column that does not sum to what is declared — the second
        // lock, and the one a raised dated entry has to get past.
        says(ledgered, &[("a.rs", 10, 0), ("b.rs", 5, 2)], 5, "sums to 2");

        // The positive controls: at the ceiling is green, under it is green,
        // and neither is refused for having been staged.
        let green = |measured: &[(&str, usize, usize)]| {
            let bad = refusals(&staged(ledgered), &staged(measured), 2);
            assert!(bad.is_empty(), "expected no refusal, got {bad:?}");
        };
        green(&[("a.rs", 10, 0), ("b.rs", 5, 2)]);
        green(&[("a.rs", 0, 0), ("b.rs", 1, 0)]);
    }

    /// The shrinkage report is what a sweep reads to book its win, so it names
    /// the rows that moved and only those, already in the ledger's own form.
    #[test]
    fn the_shrinkage_report_names_the_rows_a_sweep_rewrites() {
        let permitted = staged(&[("a.rs", 10, 2), ("b.rs", 5, 0), ("c.rs", 1, 0)]);
        let measured = staged(&[("a.rs", 4, 1), ("b.rs", 5, 0), ("c.rs", 0, 0)]);
        assert_eq!(shrinkage(&permitted, &measured), ["a.rs 4 1", "c.rs 0 0"]);
        assert!(shrinkage(&permitted, &permitted).is_empty());
    }

    /// What the one methodology counts, stated as cases because a well-formed
    /// tree exercises none of them.
    #[test]
    fn the_measure_counts_what_this_module_says_it_counts() {
        let counted = |text: &str| measure(text).comments;
        assert_eq!(counted("// a line comment\n"), 1);
        assert_eq!(counted("    /// a doc comment\n"), 1);
        assert_eq!(counted("//! a module doc\n"), 1);
        assert_eq!(counted("/* opened\n * continued\n */\n"), 3);
        assert_eq!(counted("let x = 1; // trailing prose is not counted\n"), 0);
        assert_eq!(counted("let x = 1;\n"), 0);
        assert_eq!(counted("let y = a / b;\n"), 0);
        // The stated false positive, pinned rather than assumed: a statement
        // opening with a dereference reads as a block comment's continuation.
        assert_eq!(counted("        *p += 1;\n"), 1);

        let chronology = |text: &str| measure(text).dated;
        assert_eq!(chronology("// owner ruling 2026-08-25\n"), 1);
        assert_eq!(chronology("// measured 1999-01-01\n"), 0);
        assert_eq!(chronology("// version 2026-8-25\n"), 0);
        assert_eq!(chronology("let t = \"2026-08-25\";\n"), 0);
        assert_eq!(measure("// two 2026-08 dates 2026-09 on one line\n").dated, 1);
    }

    /// The ledger is data, and a row that is not a row is refused at its line
    /// rather than skipped.
    #[test]
    fn the_ledger_is_read_strictly() {
        assert!(read_ledger("a.rs 1 0\nb.rs 2 3\n").is_ok());
        assert!(read_ledger("").is_ok());
        assert!(read_ledger("a.rs 1\n").is_err());
        assert!(read_ledger("a.rs 1 0 0\n").is_err());
        assert!(read_ledger("a.rs one 0\n").is_err());
        assert!(read_ledger("\n").is_err());
        assert!(read_ledger("b.rs 1 0\na.rs 1 0\n").is_err());
        assert!(read_ledger("a.rs 1 0\na.rs 2 0\n").is_err());
    }

    /// The walk reaches the tree it claims to, and the two exclusions hold: a
    /// gate reading a subset of the sources is a gate that permits the rest.
    #[test]
    fn the_walk_reaches_every_tree_and_only_those() {
        let found = tree();
        for expected in [
            "kernel/src/main.rs",
            "src/prosegate.rs",
            "bootloader/src/main.rs",
            "userland/libc/src/lib.rs",
            "tests/toyos.rs",
            "toyos-abi/src/lib.rs",
        ] {
            assert!(found.contains_key(expected), "the walk missed {expected}");
        }
        assert!(
            !found.keys().any(|p| p.starts_with("rust/") || p.contains("/target/")),
            "the walk entered a directory {UNLEDGERED:?} excludes"
        );
        // And it reads the files rather than merely listing them.
        assert!(found["src/prosegate.rs"].comments > 0);
    }
}
