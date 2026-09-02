//! The tracker's own honesty gate: every issue file says what it is, in the
//! words `issues/README.md` closes the two deciding fields to.
//!
//! `ls` is the index and the frontmatter is the query, so the directory's whole
//! usefulness is that `rg -l '^status: open' issues/` means *unheld work* and
//! `rg -l '^kind: track' issues/` means *staged work*. A value outside the
//! closed list answers neither query and is invisible in both: one file carried
//! `kind: design-debt` — an *area* directory name typed into the kind field —
//! among 363 others, and nothing in the tree could see it, because until now
//! nothing read `kind:` programmatically at all.
//!
//! The citation gate below is the other direction: closing an issue deletes
//! the file, and the root `CLAUDE.md` law says the same merge takes every
//! mention — by bare name as well as by path, because the slug is the
//! identity. `src/redlist.rs` resolves only its own `source` rows; this reads
//! every tracked file, so a pointer at a deleted write-up cannot read as
//! checked. It enumerates `git ls-files` rather than walking the checkout
//! because `.github/` holds citations too and a dotfile directory is what a
//! checkout scan silently skipped when this was first measured by hand.
//!
//! **The two lists live here and not in a scan over the README**, because
//! documentation carries no gates in this tree (`src/CLAUDE.md`): a test over
//! prose is exactly the artifact an owner ruling deleted. The README says what
//! the fields *mean*; this says what a file may hold, and this is what reds.
//!
//! Cheap on purpose — 363 files read and parsed is milliseconds, so it runs in
//! `cargo test --lib` on every machine that builds the tree rather than in a
//! job somebody has to remember.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What is owed. `issues/README.md`'s `status` column, closed.
const STATUSES: &[&str] = &["open", "assigned", "expected-red", "owner", "none"];

/// What the entry is. `issues/README.md`'s `kind` column, closed.
///
/// Not the `Areas` list beside it: an area is a directory, and a directory name
/// in this field is the mistake this gate exists to name.
const KINDS: &[&str] = &["defect", "finding", "track", "question", "rejected"];

/// The kinds that answer "what is owed" by themselves, and the statuses they
/// may therefore be paired with.
///
/// `kind` says what the entry is and `status` says what is owed; two of the
/// kinds say both, so they may not be contradicted. This is what makes
/// `rg -l '^status: open'` mean unheld work rather than "every file nobody was
/// assigned" — the `question` and `rejected` files all said `open` once, and
/// the query over-reported by eleven with nothing able to tell.
const ALLOWED_STATUS: &[(&str, &[&str])] = &[
    ("defect", &["open", "assigned", "expected-red"]),
    ("finding", &["open", "assigned", "expected-red"]),
    ("track", &["open", "assigned"]),
    ("question", &["owner"]),
    ("rejected", &["none"]),
];

/// The one file in `issues/` that is not an issue.
const README: &str = "issues/README.md";

/// Every refusal the tracker's frontmatter earns, one line each.
///
/// Takes the files rather than reading them, so the negative control can stage
/// a bad one without writing into the tree.
fn refusals(files: &[(String, String)]) -> Vec<String> {
    let mut bad = Vec::new();
    for (path, text) in files {
        let Some(front) = frontmatter(text) else {
            bad.push(format!(
                "{path}: no `---` frontmatter block. Every issue carries one, and it is what \
                 every query over this directory reads"
            ));
            continue;
        };
        let status = field(front, "status");
        let kind = field(front, "kind");
        match (status, kind) {
            (None, _) => bad.push(format!("{path}: no `status:` field")),
            (_, None) => bad.push(format!("{path}: no `kind:` field")),
            (Some(status), Some(kind)) => {
                if !STATUSES.contains(&status) {
                    bad.push(format!(
                        "{path}: `status: {status}` is not one of {STATUSES:?}, so no query over \
                         this directory can see the file"
                    ));
                }
                if !KINDS.contains(&kind) {
                    bad.push(format!(
                        "{path}: `kind: {kind}` is not one of {KINDS:?}, so no query over this \
                         directory can see the file — an area is a directory, not a kind"
                    ));
                }
                if let Some((_, allowed)) = ALLOWED_STATUS.iter().find(|(k, _)| *k == kind) {
                    if STATUSES.contains(&status) && !allowed.contains(&status) {
                        bad.push(format!(
                            "{path}: `kind: {kind}` already says what is owed, and \
                             `status: {status}` says otherwise — it may only be {allowed:?}"
                        ));
                    }
                }
            }
        }
    }
    bad
}

/// The leading `---` block, or nothing if the file does not open with one.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// One field's value, trimmed. A field is `name: value` at the start of a line.
fn field<'a>(front: &'a str, name: &str) -> Option<&'a str> {
    front
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .map(str::trim)
}

/// Every issue file in the tracker, path relative to the repository root.
///
/// `issues/README.md` is the one exclusion and it is by exact path: a `README`
/// in an area directory would be prose nobody asked for, and this would say so.
fn tracker(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk(root, &root.join("issues"), &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} is the issue tracker: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if relative == README {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        out.push((relative, text));
    }
}

/// Whole files the citation gate does not read. An archive's rows name
/// deleted files because that is its job — a record of what a fix closed is
/// not a claim the files exist — and an entry here is a reviewed edit to the
/// gate, not a marker any file can quietly give itself.
const ARCHIVES: &[&str] = &["issues/build/defect-events.md"];

fn is_slug_byte(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

/// `issues/<area>/<slug>.md` claims in `s`, as (start, end, area, slug).
fn path_claims(s: &str) -> Vec<(usize, usize, String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = s[from..].find("issues/") {
        let at = from + pos;
        let rest = &s[at + "issues/".len()..];
        from = at + "issues/".len();
        let Some((area, tail)) = rest.split_once('/') else { continue };
        if area.is_empty() || !area.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            continue;
        }
        let end = tail.find(|c| !is_slug_byte(c)).unwrap_or(tail.len());
        let slug = &tail[..end];
        if slug.is_empty() || !tail[end..].starts_with(".md") {
            continue;
        }
        let close = at + "issues/".len() + area.len() + 1 + slug.len() + ".md".len();
        out.push((at, close, area.to_string(), slug.to_string()));
    }
    out
}

/// Whole-token dead-slug mentions in `s`, as (start, end, token).
fn slug_mentions(s: &str, dead_slugs: &BTreeSet<String>) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices().chain([(s.len(), ' ')]) {
        match (start, is_slug_byte(c)) {
            (None, true) => start = Some(i),
            (Some(from), false) => {
                let token = &s[from..i];
                if token.contains('-') && dead_slugs.contains(token) {
                    out.push((from, i, token.to_string()));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A line with its indentation and any comment leader removed, so a citation
/// an editor wrapped can be rejoined across the break.
fn stripped(line: &str) -> &str {
    let t = line.trim_start();
    for lead in ["//!", "///", "//", "#", "*"] {
        if let Some(rest) = t.strip_prefix(lead) {
            return rest.strip_prefix(' ').unwrap_or(rest);
        }
    }
    t
}

/// Which lines of a Markdown file sit in or on a ``` fence: recorded command
/// output has to reproduce verbatim, so what it says is history, not a claim.
fn fenced_lines(path: &str, lines: &[&str]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    if !path.ends_with(".md") {
        return out;
    }
    let mut inside = false;
    for (i, line) in lines.iter().enumerate() {
        let fence = line.trim_start().starts_with("```");
        if fence || inside {
            out[i] = true;
        }
        if fence {
            inside = !inside;
        }
    }
    out
}

/// Every citation that does not resolve, one line each.
///
/// Two shapes claim a file exists: `issues/<area>/<slug>.md` where `<area>`
/// is a directory the tracker holds (so a staged fixture under a synthetic
/// area is not a claim), and a bare hyphenated token equal to a slug the
/// tracker once held and no longer does. A slug without a hyphen is
/// indistinguishable from a word of prose and is out of scope by that rule.
/// Each line is scanned alone and then joined with its successor, keeping
/// only what spans the seam — a wrapped citation is still one claim.
fn citation_refusals(
    scanned: &[(String, String)],
    areas: &BTreeSet<String>,
    issue_files: &BTreeSet<String>,
    dead_slugs: &BTreeSet<String>,
) -> Vec<String> {
    let mut bad = Vec::new();
    let mut judge = |path: &str, n: usize, s: &str, seam: Option<usize>| {
        for (start, end, area, slug) in path_claims(s) {
            if seam.is_some_and(|at| start >= at || end <= at) {
                continue;
            }
            let cited = format!("issues/{area}/{slug}.md");
            // `areas` is the set of directories under `issues/` that hold a
            // file, so the tracker on disk is the authority here and
            // `issues/README.md`'s closed list is read by nothing.
            if !areas.contains(&area) {
                bad.push(format!(
                    "{path}:{n}: cites `{cited}`, and no directory under `issues/` is named \
                     `{area}` — a claim under an area the tracker does not hold resolves to \
                     nothing"
                ));
            } else if !issue_files.contains(&cited) {
                bad.push(format!(
                    "{path}:{n}: cites `{cited}` and no such file exists — the merge that \
                     deletes an issue takes every mention of it"
                ));
            }
        }
        for (start, end, token) in slug_mentions(s, dead_slugs) {
            if seam.is_some_and(|at| start >= at || end <= at) {
                continue;
            }
            bad.push(format!(
                "{path}:{n}: `{token}` is a deleted issue's slug — the close that removed \
                 the file owed this mention too"
            ));
        }
    };
    for (path, text) in scanned {
        if ARCHIVES.contains(&path.as_str()) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let fenced = fenced_lines(path, &lines);
        for (i, line) in lines.iter().enumerate() {
            if fenced[i] {
                continue;
            }
            judge(path, i + 1, line, None);
            if let Some(next) = lines.get(i + 1) {
                if !fenced[i + 1] {
                    let head = stripped(line).trim_end();
                    let joined = format!("{head}{}", stripped(next).trim_start());
                    judge(path, i + 1, &joined, Some(head.len()));
                }
            }
        }
    }
    bad
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// Every tracked file that is UTF-8 text, read from the working tree so an
/// uncommitted fix already counts.
fn tracked_files(root: &Path) -> Vec<(String, String)> {
    let list = git(root, &["ls-files", "-z"]);
    list.split('\0')
        .filter(|p| !p.is_empty())
        .filter_map(|p| Some((p.to_string(), std::fs::read_to_string(root.join(p)).ok()?)))
        .collect()
}

/// Slugs the tracker held and the working tree does not: deleted anywhere in
/// history, or present in `HEAD` and since `git rm`ed — the second set is what
/// makes `cargo test --lib` red *before* a close is pushed.
///
/// The deletion log takes no pathspec: the tracker has lived at more than one
/// path (`specs/issues/` until it moved to the root), and a set scoped to the
/// current one was blind to everything closed before the move. Any deleted
/// path whose tail is `issues/<area>/<slug>.md` was a tracker file wherever
/// the tracker stood.
fn dead_slugs(root: &Path, live: &BTreeSet<&str>) -> BTreeSet<String> {
    let gone = git(root, &["log", "--diff-filter=D", "--format=", "--name-only"]);
    let head = git(root, &["ls-tree", "-r", "--name-only", "HEAD", "--", "issues/"]);
    let mut out = BTreeSet::new();
    for path in gone.lines().filter(|p| tracker_shaped(p)).chain(head.lines()) {
        let Some(stem) = path.rsplit('/').next().and_then(|f| f.strip_suffix(".md")) else {
            continue;
        };
        if stem.contains('-') && !live.contains(stem) {
            out.insert(stem.to_string());
        }
    }
    out
}

/// Whether a path's last three segments are `issues/<area>/<file>.md`.
fn tracker_shaped(path: &str) -> bool {
    let mut tail = path.rsplit('/');
    let (Some(file), Some(area), Some(dir)) = (tail.next(), tail.next(), tail.next()) else {
        return false;
    };
    dir == "issues"
        && !area.is_empty()
        && area.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        && file.ends_with(".md")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The gate over the tracker as it stands.
    ///
    /// The count is asserted for the reason every scan here asserts one: a walk
    /// that quietly found nothing would wave the whole directory through and
    /// read as green.
    #[test]
    fn every_issue_file_says_what_it_is() {
        let files = tracker(&repo_root());
        assert!(files.len() > 100, "only {} issue file(s) found", files.len());
        let bad = refusals(&files);
        assert!(
            bad.is_empty(),
            "`issues/README.md` closes `status` and `kind`, and a value outside either is \
             invisible to every query over this directory:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The gate over every tracked file's citations.
    ///
    /// Every count below is guarded because a scan that quietly saw nothing
    /// would wave the whole tree through and read as green.
    #[test]
    fn every_citation_resolves() {
        let root = repo_root();
        let shallow = git(&root, &["rev-parse", "--is-shallow-repository"]);
        assert_eq!(
            shallow.trim(),
            "false",
            "deleted slugs come from history, and a shallow clone would wave every bare-name \
             citation through — check out with fetch-depth: 0 the way host-tests.yml does"
        );
        let scanned = tracked_files(&root);
        assert!(scanned.len() > 500, "only {} tracked text file(s) read", scanned.len());
        let issue_files: BTreeSet<String> = tracker(&root).into_iter().map(|(p, _)| p).collect();
        assert!(issue_files.len() > 100, "only {} issue file(s) found", issue_files.len());
        let areas: BTreeSet<String> = issue_files
            .iter()
            .filter_map(|p| Some(p.split('/').nth(1)?.to_string()))
            .collect();
        assert!(areas.len() >= 8, "only {} area directorie(s) found", areas.len());
        let live: BTreeSet<&str> = issue_files
            .iter()
            .filter_map(|p| p.rsplit('/').next()?.strip_suffix(".md"))
            .collect();
        let dead = dead_slugs(&root, &live);
        assert!(!dead.is_empty(), "no deleted issue slug found — the history read is broken");
        let bad = citation_refusals(&scanned, &areas, &issue_files, &dead);
        assert!(
            bad.is_empty(),
            "a citation of a deleted issue reads as checked and teaches nothing:\n  {}",
            bad.join("\n  ")
        );
    }

    /// A tracker path built rather than typed: a literal one is a citation this
    /// gate reads out of its own source and refuses.
    fn at(area: &str, slug: &str) -> String {
        format!("issues/{area}/{slug}.md")
    }

    /// The citation gate's teeth: a dangling path, an unknown area and a dead
    /// bare name red, and the shapes beside them stay green for the stated
    /// reasons.
    #[test]
    fn the_citation_gate_refuses_a_dangling_claim_and_a_dead_name() {
        let areas: BTreeSet<String> = ["area".to_string()].into();
        let issue_files: BTreeSet<String> = [at("area", "staged")].into();
        let dead: BTreeSet<String> = ["a-name-the-tracker-dropped".to_string()].into();
        let judge = |text: &str| {
            citation_refusals(
                &[("doc.md".to_string(), text.to_string())],
                &areas,
                &issue_files,
                &dead,
            )
        };

        assert!(judge(&format!("see {}", at("area", "staged"))).is_empty());
        assert!(
            judge(&format!("see {}", at("area", "bare")))[0].contains("no such file exists")
        );
        // An area the tracker does not hold is itself a dangling claim.
        assert!(judge(&format!("see {}", at("other", "bare")))[0]
            .contains("no directory under `issues/` is named"));
        assert!(judge("the fix `a-name-the-tracker-dropped` took")[0].contains("deleted issue"));
        assert!(judge("`a-name-the-tracker-kept`").is_empty());
        // A dead slug inside a longer token is that token, not a mention.
        assert!(judge("pre-a-name-the-tracker-dropped-post").is_empty());
        // Both shapes on one line are two findings, at the same line number.
        let both = judge(&format!("{} was a-name-the-tracker-dropped", at("area", "bare")));
        assert_eq!(both.len(), 2, "{both:?}");
        assert!(both.iter().all(|b| b.contains(":1:")), "{both:?}");

        // A claim an editor wrapped is still one claim, in either shape, with
        // or without a comment leader on the continuation.
        let wrapped = judge("see issues/area/\nbare.md and then");
        assert_eq!(wrapped.len(), 1, "{wrapped:?}");
        assert!(judge("// see (`issues/area/\n// bare.md`) here")[0].contains("bare.md"));
        assert!(judge("the a-name-the-\ntracker-dropped fix")[0].contains("deleted issue"));
        // An unwrapped finding beside a seam is reported once, by the line scan.
        assert_eq!(judge("a-name-the-tracker-dropped\nprose").len(), 1);
        // A fence is recorded output, exempt to its closing line.
        let fenced = format!("```\n{}\na-name-the-tracker-dropped\n```", at("area", "bare"));
        assert!(judge(&fenced).is_empty());
        assert_eq!(judge(&format!("```\nfenced\n```\n{}", at("area", "bare"))).len(), 1);
        // An archive names deleted files as history; the gate does not read it.
        let archive = citation_refusals(
            &[(at("build", "defect-events"), "a-name-the-tracker-dropped".into())],
            &areas,
            &issue_files,
            &dead,
        );
        assert!(archive.is_empty(), "{archive:?}");
    }

    /// The dead-slug set reads the whole deletion log: the tracker has moved,
    /// and a set scoped to its current path missed everything closed before.
    #[test]
    fn a_tracker_file_is_recognised_wherever_the_tracker_stood() {
        assert!(tracker_shaped(&at("area", "some-old-entry")));
        assert!(tracker_shaped(&format!("specs/{}", at("area", "some-old-entry"))));
        assert!(!tracker_shaped("specs/some-plan.md"));
        assert!(!tracker_shaped("issues/README.md"));
        assert!(!tracker_shaped("docs/issues.md"));
    }

    /// The teeth, and the first case is the one this was written for: an area
    /// directory's name typed into the `kind` field, which is how
    /// `kind: design-debt` lived in the tracker unseen.
    #[test]
    fn the_gate_refuses_what_the_readme_does_not_define() {
        let staged =
            |front: &str| vec![(at("area", "staged"), format!("---\n{front}\n---\n\n# a heading\n"))];
        let says = |front: &str, needle: &str| {
            let bad = refusals(&staged(front));
            assert!(
                bad.iter().any(|b| b.contains(needle)),
                "expected a refusal naming {needle:?}, got {bad:?}"
            );
        };

        says("status: open\nkind: design-debt\nopened: 2026-08-15", "is not one of");
        says("status: pending\nkind: defect\nopened: 2026-08-15", "is not one of");
        says("status: open\nopened: 2026-08-15", "no `kind:` field");
        says("kind: defect\nopened: 2026-08-15", "no `status:` field");
        says("status: open\nkind: question\nopened: 2026-08-15", "already says what is owed");
        says("status: open\nkind: rejected\nopened: 2026-08-15", "already says what is owed");

        assert!(
            refusals(&[(at("area", "bare"), "# no frontmatter\n".to_string())])
                .iter()
                .any(|b| b.contains("no `---` frontmatter block"))
        );

        // The positive control: the shapes above are refused for their field
        // and not for being staged.
        assert!(refusals(&staged("status: open\nkind: defect\nopened: 2026-08-15")).is_empty());
        assert!(refusals(&staged("status: none\nkind: rejected\nopened: 2026-08-15")).is_empty());
        assert!(refusals(&staged("status: owner\nkind: question\nopened: 2026-08-15")).is_empty());
    }
}
