//! The licence gate: everything that ships as part of ToyOS is under a licence
//! [`ALLOWED`] names, or is an [`EXCEPTIONS`] row that says why it is not.
//!
//! **What ships is read out of the build, never listed.** The crates are every
//! directory [`crate::build::shipped`] says an image of the three modes is
//! built from — the kernel, the bootloader, `init` and every `[programs]` row
//! — plus std from the fork checkout this tree pins and [`crate::libc::CRATE`],
//! which every sysroot links into std. From each, `cargo metadata` gives the
//! resolved graph, and the gate walks its *normal* edges: a build-dependency
//! runs on the host and a dev-dependency builds a test, and neither is linked
//! into the image. Every edge is walked whatever its target `cfg`, and every
//! feature is on, so the set judged is a superset of what one image links: a
//! crate only another platform pulls in can red here, and none can pass unread.
//! Git and path packages are judged exactly as registry ones.
//!
//! **Committed third-party files are `NOTICE`'s rows**: every section of it
//! carries one `SPDX-License-Identifier:` line, or is in [`PROSE`] with the
//! reason it names no files. A row ships when a file it names is under a
//! shipped config's asset directory or a shipped package's directory, and only
//! a shipped row is held to the allowlist; the rest are printed as not shipped.
//! A row that names no tracked file is refused, so a stale row cannot stand.
//!
//! **An exception is named, reasoned, and matched exactly**: by package name or
//! row path *and* by the licence text it was written against, so a licence that
//! changes reds again. An exception nothing matches is refused as stale. One
//! that is [`Standing::OnlyUnder`] a `cfg` rests on its author's reading that
//! no guest target satisfies that `cfg`, which this gate cannot evaluate; what
//! it checks is that every edge into the package is under exactly that `cfg`,
//! so a second way in reds.
//!
//! The expression grammar is SPDX's (`OR`, `AND`, `WITH`, parentheses) plus
//! cargo's legacy `/` for `OR`. An identifier outside the allowlist is refused
//! whether SPDX knows it or not, so no licence list is needed to refuse the
//! unrecognised.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// What a shipped crate or file may be under. `OR` passes if any branch does,
/// `AND` only if every part does.
pub const ALLOWED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "BSL-1.0",
    // Owner ruling: MPL's obligations are per file, and the file keeps them.
    "MPL-2.0",
];

/// `WITH` pairs that pass. An exception only ever adds permissions, but a pair
/// passes only when it is named here.
const ALLOWED_WITH: &[(&str, &str)] = &[("Apache-2.0", "LLVM-exception")];

/// Allowed for a `NOTICE` row whose every file is a font, and nowhere else.
const FONTS_ONLY: &[&str] = &["OFL-1.1"];
const FONT_EXTENSIONS: &[&str] = &["ttf", "otf"];

/// Surfaced by name in every verdict, not refused: the owner wants to know
/// which they are.
const NAMED: &[&str] = &["MPL-2.0"];

/// What an exception is matched against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// A package, by name.
    Crate(&'static str),
    /// A `NOTICE` row, by the path its heading starts with.
    Notice(&'static str),
}

/// Why an exception stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// It ships, and whether it may is the owner's to rule.
    PendingOwner,
    /// Every edge into it is taken under this `cfg`, exactly as cargo prints
    /// it, and no guest target satisfies that `cfg`. The gate walks every edge
    /// whatever its `cfg`, so this is what a package it cannot link says; it
    /// matches only a finding whose every incoming edge is under this `cfg`.
    OnlyUnder(&'static str),
}

/// A shipped thing whose licence is not allowed, left in by name.
pub struct Exception {
    pub subject: Subject,
    /// The licence text exactly as the package or row declares it; empty for
    /// one that declares none.
    pub licence: &'static str,
    pub standing: Standing,
    pub reason: &'static str,
}

const fn pending(subject: Subject, licence: &'static str, reason: &'static str) -> Exception {
    Exception {
        subject,
        licence,
        standing: Standing::PendingOwner,
        reason,
    }
}

/// Every current exception.
pub const EXCEPTIONS: &[Exception] = &[
    pending(
        Subject::Crate("doom"),
        "GPL-2.0-only",
        "/system/bin/doom links doomgeneric, id's Doom source by way of Chocolate Doom (NOTICE). \
         The owner's ruling exempts what the package manager installs, and doom is in the image; \
         NOTICE and forks.toml name doom as a package as the end state",
    ),
    pending(
        Subject::Notice("userland/doom"),
        "GPL-2.0-only",
        "the C half of the doom crate excepted above, fetched at build time and compiled into it",
    ),
    pending(
        Subject::Notice("assets/DOOM1.WAD"),
        "LicenseRef-id-Software-DOOM1-Shareware",
        "id's shareware terms: redistributable unmodified and not for consideration, so an image \
         carrying it may not be sold (NOTICE). NOTICE's exit is the package manager fetching it",
    ),
    pending(
        Subject::Notice("assets/soundfont.sf2"),
        "LicenseRef-GeneralUser-GS-2.0",
        "GeneralUser GS's own permissive licence, no standard one, with its author's caveat on \
         where the samples came from (NOTICE). NOTICE's exit is an OPL3 synthesiser driven by \
         DOOM1.WAD's GENMIDI lump",
    ),
    pending(
        Subject::Crate("bcachefs"),
        "",
        "ours, and its manifest declares no licence. It implements the on-disk format of upstream \
         bcachefs, which is GPL-2.0, from upstream's definitions by name; whether it is ours to \
         license as the rest of the tree is, is the owner's to rule",
    ),
    Exception {
        subject: Subject::Crate("windows-sys"),
        licence: "",
        standing: Standing::OnlyUnder("cfg(target_os = \"windows\")"),
        reason: "std's empty in-tree stand-in for windows-sys, which declares no licence",
    },
    Exception {
        subject: Subject::Crate("windows-link"),
        licence: "",
        standing: Standing::OnlyUnder("cfg(any(windows, target_os = \"cygwin\"))"),
        reason: "std's in-tree stand-in for windows-link, which declares no licence",
    },
];

/// `NOTICE` sections that name no files, and why.
const PROSE: &[(&str, &str)] = &[(
    "Rust crates and the forks",
    "every crate, fork or not, is judged by the crate half of this gate",
)];

// --- Expressions -------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Expr {
    Id(String),
    With(String, String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, '(' | ')' | '/') {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                out.push(if c == '/' {
                    "OR".to_string()
                } else {
                    c.to_string()
                });
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn parse(text: &str) -> Result<Expr, String> {
    let tokens = tokens(text);
    let mut at = 0;
    let expr = parse_or(&tokens, &mut at)?;
    if at != tokens.len() {
        return Err(format!("{text:?}: unexpected {:?}", tokens[at]));
    }
    Ok(expr)
}

fn parse_or(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut parts = vec![parse_and(tokens, at)?];
    while tokens.get(*at).is_some_and(|t| t == "OR") {
        *at += 1;
        parts.push(parse_and(tokens, at)?);
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::Or(parts)
    })
}

fn parse_and(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut parts = vec![parse_with(tokens, at)?];
    while tokens.get(*at).is_some_and(|t| t == "AND") {
        *at += 1;
        parts.push(parse_with(tokens, at)?);
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::And(parts)
    })
}

fn is_operand(token: &str) -> bool {
    !matches!(token, "OR" | "AND" | "WITH" | "(" | ")")
}

fn parse_with(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let first = match tokens.get(*at).map(String::as_str) {
        Some("(") => {
            *at += 1;
            let inner = parse_or(tokens, at)?;
            if tokens.get(*at).map(String::as_str) != Some(")") {
                return Err("an unclosed `(`".to_string());
            }
            *at += 1;
            return Ok(inner);
        }
        Some(id) if is_operand(id) => id.to_string(),
        Some(other) => return Err(format!("{other:?} where a licence belongs")),
        None => return Err("an expression that ends where a licence belongs".to_string()),
    };
    *at += 1;
    if tokens.get(*at).is_some_and(|t| t == "WITH") {
        *at += 1;
        match tokens.get(*at) {
            Some(exception) if is_operand(exception) => {
                *at += 1;
                return Ok(Expr::With(first, exception.clone()));
            }
            _ => return Err(format!("`{first} WITH` names no exception")),
        }
    }
    Ok(Expr::Id(first))
}

/// The leaves that keep `expr` from passing; empty when it passes.
fn refused(expr: &Expr, font: bool) -> Vec<String> {
    match expr {
        Expr::Id(id) => {
            let ok = ALLOWED.contains(&id.as_str()) || (font && FONTS_ONLY.contains(&id.as_str()));
            if ok {
                vec![]
            } else {
                vec![id.clone()]
            }
        }
        Expr::With(id, exception) => {
            if ALLOWED_WITH.contains(&(id.as_str(), exception.as_str())) {
                vec![]
            } else {
                vec![format!("{id} WITH {exception}")]
            }
        }
        Expr::And(parts) => parts.iter().flat_map(|p| refused(p, font)).collect(),
        Expr::Or(parts) => {
            let each: Vec<Vec<String>> = parts.iter().map(|p| refused(p, font)).collect();
            if each.iter().any(Vec::is_empty) {
                vec![]
            } else {
                each.concat()
            }
        }
    }
}

/// Whether `expr` names one of [`NAMED`] on a branch that passes.
fn names(expr: &Expr) -> bool {
    match expr {
        Expr::Id(id) => NAMED.contains(&id.as_str()),
        Expr::With(..) => false,
        Expr::And(parts) => parts.iter().any(names),
        Expr::Or(parts) => parts
            .iter()
            .filter(|p| refused(p, false).is_empty())
            .all(names),
    }
}

/// Why `licence` does not pass, or `None` when it does.
fn judge_licence(licence: &str, font: bool) -> Option<String> {
    match parse(licence) {
        Err(why) => Some(format!("not an SPDX expression: {why}")),
        Ok(expr) => {
            let leaves = refused(&expr, font);
            (!leaves.is_empty()).then(|| format!("not allowed: {}", leaves.join(", ")))
        }
    }
}

// --- Findings ----------------------------------------------------------------

/// One shipped thing that does not pass.
#[derive(Debug, PartialEq)]
struct Finding {
    subject: String,
    /// What exceptions match on: the package name or the row path.
    key: String,
    notice: bool,
    /// The licence text, empty when none is declared.
    licence: String,
    why: String,
    /// Who pulls it in, root first: one chain per workspace that does.
    via: Vec<String>,
    /// The `cfg` of every edge into it from a shipped package, `None` for an
    /// unconditional one and for a root.
    into: BTreeSet<Option<String>>,
}

/// What one run found: refusals, and the lines a green verdict still prints.
#[derive(Default)]
struct Report {
    findings: Vec<Finding>,
    named: BTreeSet<String>,
    notes: Vec<String>,
    /// Every package judged, as `name version source`, and every row.
    judged: BTreeSet<String>,
}

impl Report {
    /// Record `f`, folded into an earlier finding on the same thing: the
    /// kernel and the bootloader resolve one path crate twice.
    fn find(&mut self, f: Finding) {
        match self
            .findings
            .iter_mut()
            .find(|e| (e.notice, &e.key, &e.licence) == (f.notice, &f.key, &f.licence))
        {
            Some(earlier) => {
                earlier.into.extend(f.into);
                earlier.via.extend(f.via);
            }
            None => self.findings.push(f),
        }
    }
}

// --- Crates ------------------------------------------------------------------

/// One normal edge: the dependency, and the `cfg` it is taken under when every
/// declaration of it is target-conditional.
type Edge<'a> = (&'a str, Option<String>);

/// The package graph `cargo metadata` resolved.
struct Graph<'a> {
    packages: BTreeMap<&'a str, &'a Value>,
    normal: BTreeMap<&'a str, Vec<Edge<'a>>>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

impl<'a> Graph<'a> {
    fn new(metadata: &'a Value) -> Result<Self, String> {
        let packages = metadata["packages"]
            .as_array()
            .ok_or("cargo metadata printed no packages")?
            .iter()
            .map(|p| Ok((str_of(p, "id").ok_or("a package with no id")?, p)))
            .collect::<Result<_, String>>()?;
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("cargo metadata printed no resolve graph")?;
        let mut normal = BTreeMap::new();
        for node in nodes {
            let id = str_of(node, "id").ok_or("a node with no id")?;
            let deps = node["deps"].as_array().ok_or("a node with no deps")?;
            let mut out = Vec::new();
            for dep in deps {
                let kinds = dep["dep_kinds"]
                    .as_array()
                    .ok_or("a dep with no dep_kinds")?;
                let normal: Vec<&Value> = kinds.iter().filter(|k| k["kind"].is_null()).collect();
                if normal.is_empty() {
                    continue;
                }
                let cfg = normal
                    .iter()
                    .map(|k| str_of(k, "target"))
                    .collect::<Option<Vec<_>>>()
                    .map(|targets| targets.join(" | "));
                out.push((str_of(dep, "pkg").ok_or("a dep with no pkg")?, cfg));
            }
            normal.insert(id, out);
        }
        Ok(Self { packages, normal })
    }

    /// The id of the package whose manifest is `manifest`.
    fn id_of(&self, manifest: &Path) -> Option<&'a str> {
        self.packages
            .iter()
            .find(|(_, p)| str_of(p, "manifest_path").map(Path::new) == Some(manifest))
            .map(|(id, _)| *id)
    }

    fn edges(&self, id: &str) -> &[Edge<'a>] {
        self.normal.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every package `roots` reach over normal edges, each with a chain to it:
    /// an unconditional one wherever there is one.
    fn reach(&self, roots: &[&'a str]) -> BTreeMap<&'a str, String> {
        let mut chains: BTreeMap<&str, String> = roots
            .iter()
            .map(|r| (*r, self.name(r).to_string()))
            .collect();
        for conditional in [false, true] {
            let mut queue: VecDeque<&str> = chains.keys().copied().collect();
            while let Some(id) = queue.pop_front() {
                for (dep, cfg) in self.edges(id) {
                    if chains.contains_key(dep) || (cfg.is_some() && !conditional) {
                        continue;
                    }
                    let edge = cfg.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default();
                    let chain = format!("{} → {}{edge}", chains[id], self.name(dep));
                    chains.insert(dep, chain);
                    queue.push_back(dep);
                }
            }
        }
        chains
    }

    fn name(&self, id: &str) -> &'a str {
        str_of(self.packages[id], "name").unwrap_or("?")
    }
}

/// Where a package comes from, as the report names it.
fn origin(package: &Value) -> String {
    match str_of(package, "source") {
        None => format!("path {}", str_of(package, "manifest_path").unwrap_or("?")),
        Some(s) if s.starts_with("git+") => format!("git {s}"),
        Some(s) if s.starts_with("registry+") => "crates.io".to_string(),
        Some(s) => s.to_string(),
    }
}

/// Judge every package `roots` reach in one metadata document, and return the
/// directories of the path packages among them.
fn judge_crates(
    metadata: &Value,
    roots: &[PathBuf],
    report: &mut Report,
) -> Result<BTreeSet<PathBuf>, String> {
    let graph = Graph::new(metadata)?;
    let ids = roots
        .iter()
        .map(|root| {
            let manifest = root.join("Cargo.toml");
            graph
                .id_of(&manifest)
                .ok_or_else(|| format!("cargo metadata has no package at {}", manifest.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let reached = graph.reach(&ids);
    let mut into: BTreeMap<&str, BTreeSet<Option<String>>> =
        ids.iter().map(|id| (*id, BTreeSet::from([None]))).collect();
    for from in reached.keys() {
        for (dep, cfg) in graph.edges(from) {
            into.entry(dep).or_default().insert(cfg.clone());
        }
    }
    let mut local = BTreeSet::new();
    for (dep, via) in &reached {
        let package = graph.packages[dep];
        let name = graph.name(dep);
        let version = str_of(package, "version").unwrap_or("?");
        report
            .judged
            .insert(format!("{name} {version} {}", origin(package)));
        if str_of(package, "source").is_none() {
            if let Some(dir) = str_of(package, "manifest_path").and_then(|m| Path::new(m).parent())
            {
                local.insert(dir.to_path_buf());
            }
        }
        let finding = |licence: &str, why: String| Finding {
            subject: format!("crate {name} {version} ({})", origin(package)),
            key: name.to_string(),
            notice: false,
            licence: licence.to_string(),
            why,
            via: vec![via.clone()],
            into: into[dep].clone(),
        };
        match (str_of(package, "license"), str_of(package, "license_file")) {
            (Some(licence), _) => {
                if let Some(why) = judge_licence(licence, false) {
                    report.find(finding(licence, why));
                } else if parse(licence).is_ok_and(|e| names(&e)) {
                    report
                        .named
                        .insert(format!("crate {name} {version} is {licence}, via {via}"));
                }
            }
            (None, Some(file)) => report.find(finding(
                "",
                format!("declares only a licence file, {file}, which no gate reads"),
            )),
            (None, None) => report.find(finding("", "declares no licence".to_string())),
        }
    }
    Ok(local)
}

// --- NOTICE ------------------------------------------------------------------

/// One `NOTICE` section: its heading, and the SPDX line it carries if any.
#[derive(Debug, PartialEq)]
struct Section {
    heading: String,
    spdx: Vec<String>,
}

const SPDX_TAG: &str = "SPDX-License-Identifier:";

/// The `-`-underlined sections of `text`, in order.
fn sections(text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Section> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let underline = lines
            .get(i + 1)
            .is_some_and(|u| u.len() >= 3 && u.chars().all(|c| c == '-'));
        if underline && !line.trim().is_empty() {
            out.push(Section {
                heading: line.trim().to_string(),
                spdx: Vec::new(),
            });
        } else if let (Some(section), Some(expr)) =
            (out.last_mut(), line.trim().strip_prefix(SPDX_TAG))
        {
            section.spdx.push(expr.trim().to_string());
        }
    }
    out
}

/// Whether `path` matches `pattern`, where `*` stands for any run of
/// characters short of a `/`.
fn glob(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == path,
        Some((head, tail)) => {
            let Some(rest) = path.strip_prefix(head) else {
                return false;
            };
            (0..=rest.len())
                .take_while(|&i| !rest[..i].contains('/'))
                .any(|i| rest.is_char_boundary(i) && glob(tail, &rest[i..]))
        }
    }
}

/// The tracked files a row's heading path names: one file, a directory's
/// files, or a glob's matches.
fn row_files<'a>(path: &str, tracked: &'a [String]) -> Vec<&'a str> {
    let dir = format!("{}/", path.trim_end_matches('/'));
    tracked
        .iter()
        .map(String::as_str)
        .filter(|f| glob(path, f) || f.starts_with(&dir))
        .collect()
}

/// Judge `NOTICE` against the allowlist. `shipped_dirs` are root-relative
/// directories whose files an image carries or builds from.
fn judge_notice(
    text: &str,
    tracked: &[String],
    shipped_dirs: &BTreeSet<String>,
    report: &mut Report,
) {
    fn row(key: &str, licence: &str, why: String) -> Finding {
        Finding {
            subject: format!("NOTICE row {key}"),
            key: key.to_string(),
            notice: true,
            licence: licence.to_string(),
            why,
            via: vec!["NOTICE".to_string()],
            into: BTreeSet::from([None]),
        }
    }
    fn refuse(report: &mut Report, key: &str, why: String) {
        report.find(row(key, "", why));
    }
    for section in sections(text) {
        let prose = PROSE
            .iter()
            .find(|(heading, _)| *heading == section.heading);
        let path = section
            .heading
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let expr = match (prose, section.spdx.as_slice()) {
            (Some((heading, why)), []) => {
                report
                    .notes
                    .push(format!("NOTICE section {heading:?} names no files: {why}"));
                continue;
            }
            (Some(_), _) => {
                refuse(
                    report,
                    &path,
                    "is PROSE and carries an SPDX line".to_string(),
                );
                continue;
            }
            (None, [expr]) => expr.clone(),
            (None, _) => {
                refuse(
                    report,
                    &path,
                    format!("carries {} `{SPDX_TAG}` lines, not one", section.spdx.len()),
                );
                continue;
            }
        };
        report.judged.insert(format!("NOTICE {path}"));
        let files = row_files(&path, tracked);
        if files.is_empty() {
            refuse(report, &path, "names no file git tracks".to_string());
            continue;
        }
        let under = |f: &str| shipped_dirs.iter().any(|d| f.starts_with(&format!("{d}/")));
        if !files.iter().any(|f| under(f)) {
            report
                .notes
                .push(format!("NOTICE row {path} ({expr}) is not shipped"));
            continue;
        }
        let font = files.iter().all(|f| {
            Path::new(f)
                .extension()
                .is_some_and(|e| FONT_EXTENSIONS.contains(&e.to_string_lossy().as_ref()))
        });
        match judge_licence(&expr, font) {
            Some(why) => report.find(row(&path, &expr, why)),
            None if parse(&expr).is_ok_and(|e| names(&e)) => {
                report.named.insert(format!("NOTICE row {path} is {expr}"));
            }
            None => {}
        }
    }
}

// --- The verdict -------------------------------------------------------------

/// Match `report` against `exceptions`: the refusals left, and the lines to print.
fn verdict(report: Report, exceptions: &[Exception]) -> Result<String, String> {
    let mut used = vec![false; exceptions.len()];
    let mut red = Vec::new();
    let mut excepted = Vec::new();
    for f in &report.findings {
        let matched = exceptions.iter().position(|e| {
            let key = match e.subject {
                Subject::Crate(name) => !f.notice && name == f.key,
                Subject::Notice(path) => f.notice && path == f.key,
            };
            let standing = match e.standing {
                Standing::PendingOwner => true,
                Standing::OnlyUnder(cfg) => {
                    !f.into.is_empty() && f.into.iter().all(|c| c.as_deref() == Some(cfg))
                }
            };
            key && standing && e.licence == f.licence
        });
        let line = format!(
            "{} is {:?}: {}; pulled in by {}",
            f.subject,
            f.licence,
            f.why,
            f.via.join("; ")
        );
        match matched {
            Some(i) => {
                used[i] = true;
                let standing = match exceptions[i].standing {
                    Standing::PendingOwner => "pending the owner".to_string(),
                    Standing::OnlyUnder(cfg) => format!("reached only under {cfg}"),
                };
                excepted.push(format!(
                    "{line}\n    EXCEPTED, {standing}: {}",
                    exceptions[i].reason
                ));
            }
            None => red.push(line),
        }
    }
    for (e, used) in exceptions.iter().zip(used) {
        if !used {
            red.push(format!(
                "exception {:?} ({:?}) matches nothing shipped: delete it",
                e.subject, e.licence
            ));
        }
    }
    let mut out = String::new();
    for line in report.notes.iter().chain(&report.named).chain(&excepted) {
        out.push_str(&format!("  {line}\n"));
    }
    if red.is_empty() {
        println!("{out}");
        Ok(format!(
            "{} packages and NOTICE rows judged; {} exception(s) stand, and nothing else is refused",
            report.judged.len(),
            excepted.len()
        ))
    } else {
        eprintln!("{out}");
        Err(format!("{} refusal(s):\n  {}", red.len(), red.join("\n  ")))
    }
}

// --- Reading the tree --------------------------------------------------------

fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>, String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// `cargo metadata` over the workspace holding `manifest`, every feature on.
fn metadata(
    root: &Path,
    manifest: &Path,
    extra: &[&str],
    env: &[(&str, &str)],
) -> Result<Value, String> {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "metadata",
        "--format-version",
        "1",
        "--all-features",
        "--manifest-path",
    ])
    .arg(manifest)
    .args(extra)
    .envs(env.iter().copied())
    .current_dir(root);
    let out = run(
        &mut cmd,
        &format!("cargo metadata --manifest-path {}", manifest.display()),
    )?;
    serde_json::from_slice(&out).map_err(|e| format!("cargo metadata printed no JSON: {e}"))
}

/// The fork's `library/`, checked out at the commit this tree pins. A checkout
/// whose `rust/` was never initialised — a CI runner's — fetches that commit
/// alone.
fn std_library(root: &Path) -> Result<PathBuf, String> {
    let fork = crate::sysroot::fork_checkout(root);
    if !fork.join("library/Cargo.toml").exists() {
        run(
            Command::new("git")
                .args(["submodule", "update", "--init", "--depth", "1", "rust"])
                .current_dir(root),
            "git submodule update --init --depth 1 rust",
        )?;
    }
    Ok(fork.join("library"))
}

/// The gate `cargo run -- --ci host` runs.
pub fn judge(root: &Path) -> Result<String, String> {
    // Cargo names every manifest by its canonical path.
    let root = &std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let shipped = crate::build::shipped(root)?;
    let mut report = Report::default();
    let mut local = BTreeSet::new();

    let mut roots: Vec<PathBuf> = shipped.crates.iter().cloned().collect();
    roots.push(root.join(crate::libc::CRATE));
    let roots = roots
        .iter()
        .map(|d| std::fs::canonicalize(d).map_err(|e| format!("{}: {e}", d.display())))
        .collect::<Result<Vec<_>, _>>()?;
    let mut read: Vec<(Value, Vec<PathBuf>)> = Vec::new();
    for crate_dir in roots {
        let manifest = crate_dir.join("Cargo.toml");
        if let Some((_, owners)) = read
            .iter_mut()
            .find(|(m, _)| Graph::new(m).is_ok_and(|g| g.id_of(&manifest).is_some()))
        {
            owners.push(crate_dir);
            continue;
        }
        let doc = metadata(root, &manifest, &["--locked"], &[])?;
        read.push((doc, vec![crate_dir]));
    }

    // Bootstrap re-locks `library/Cargo.lock` to this tree's `toyos-abi` and
    // `toyos` versions on every std build and puts the fork's file back, so the
    // committed lock is stale by design: it is re-locked into a scratch copy,
    // and the fork's is never written. `RUSTC_BOOTSTRAP` because the fork's
    // manifests use cargo features a stable cargo otherwise refuses.
    let library = std_library(root)?;
    let scratch = root.join("target/licence");
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
    let lock = scratch.join("Cargo.lock");
    std::fs::copy(library.join("Cargo.lock"), &lock)
        .map_err(|e| format!("copy std's Cargo.lock: {e}"))?;
    let lockfile = format!("resolver.lockfile-path={:?}", lock.display().to_string());
    let std_doc = metadata(
        root,
        &library.join("Cargo.toml"),
        &["--config", &lockfile],
        &[("RUSTC_BOOTSTRAP", "1")],
    )?;
    read.push((std_doc, vec![library.join("std")]));

    for (doc, owners) in &read {
        local.extend(judge_crates(doc, owners, &mut report)?);
    }

    let tracked: Vec<String> = String::from_utf8_lossy(&run(
        Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root),
        "git ls-files",
    )?)
    .split('\0')
    .filter(|f| !f.is_empty())
    .map(String::from)
    .collect();
    let relative = |dir: &Path| dir.strip_prefix(root).ok().map(|d| d.display().to_string());
    let shipped_dirs: BTreeSet<String> = shipped
        .assets
        .iter()
        .chain(&local)
        .filter_map(|d| relative(d))
        .filter(|d| !d.is_empty())
        .collect();
    let notice =
        std::fs::read_to_string(root.join("NOTICE")).map_err(|e| format!("read NOTICE: {e}"))?;
    judge_notice(&notice, &tracked, &shipped_dirs, &mut report);

    verdict(report, EXCEPTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verdict_of(licence: &str) -> Option<String> {
        judge_licence(licence, false)
    }

    #[test]
    fn a_disallowed_licence_is_refused_by_name() {
        let why = verdict_of("GPL-2.0-only").expect("GPL passed");
        assert!(why.contains("GPL-2.0-only"), "{why}");
        assert!(verdict_of("LGPL-2.1-or-later").is_some());
        assert!(verdict_of("AGPL-3.0-only").is_some());
        assert!(
            verdict_of("Frobnicate-1.0").is_some(),
            "an unrecognised licence passed"
        );
        assert!(verdict_of("OFL-1.1").is_some(), "OFL passed outside a font");
        assert_eq!(judge_licence("OFL-1.1", true), None);
        assert_eq!(verdict_of("MPL-2.0"), None);
    }

    #[test]
    fn an_or_passes_when_any_branch_does() {
        assert_eq!(verdict_of("MIT OR Apache-2.0"), None);
        assert_eq!(verdict_of("GPL-2.0-only OR MIT"), None);
        assert_eq!(verdict_of("MIT/Apache-2.0"), None);
        assert_eq!(
            verdict_of("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"),
            None
        );
        let why = verdict_of("GPL-2.0-only OR LGPL-2.1-only").expect("no branch is allowed");
        assert!(
            why.contains("GPL-2.0-only") && why.contains("LGPL-2.1-only"),
            "{why}"
        );
    }

    #[test]
    fn an_and_passes_only_when_every_part_does() {
        assert_eq!(verdict_of("(MIT OR Apache-2.0) AND Unicode-DFS-2016"), None);
        assert_eq!(
            verdict_of("MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)"),
            None
        );
        let why = verdict_of("MIT AND GPL-3.0-only").expect("one part is GPL");
        assert!(
            why.contains("GPL-3.0-only") && !why.contains("MIT"),
            "{why}"
        );
        assert!(verdict_of("MIT AND (GPL-2.0-only OR LGPL-2.1-only)").is_some());
        assert!(verdict_of("GPL-2.0-only WITH Classpath-exception-2.0").is_some());
    }

    #[test]
    fn a_malformed_expression_is_refused_not_guessed() {
        for bad in [
            "",
            "MIT OR",
            "(MIT",
            "MIT AND AND ISC",
            "Apache-2.0 WITH",
            "MIT ISC",
        ] {
            let why = verdict_of(bad).unwrap_or_else(|| panic!("{bad:?} passed"));
            assert!(why.starts_with("not an SPDX expression"), "{bad:?}: {why}");
        }
    }

    #[test]
    fn mpl_passes_and_is_named() {
        assert!(names(&parse("MPL-2.0").unwrap()));
        assert!(
            !names(&parse("MPL-2.0 OR MIT").unwrap()),
            "an MIT branch needs no MPL"
        );
        assert!(names(&parse("MIT AND MPL-2.0").unwrap()));
    }

    type Package<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);
    type Dep<'a> = (&'a str, &'a str, Option<&'a str>);

    /// A metadata document of `packages`, each `(name, license, source,
    /// license_file)`, and `edges`, each `(from, to, kind)`, where a kind
    /// starting `cfg(` is a normal edge taken under that target.
    fn doc(packages: &[Package], edges: &[Dep]) -> Value {
        let packages: Vec<Value> = packages
            .iter()
            .map(|(name, license, source, file)| {
                json!({
                    "id": name, "name": name, "version": "1.0.0",
                    "license": license, "license_file": file, "source": source,
                    "manifest_path": format!("/t/{name}/Cargo.toml"),
                })
            })
            .collect();
        let nodes: Vec<Value> = packages
            .iter()
            .map(|p| {
                let id = p["id"].as_str().unwrap();
                let deps: Vec<Value> = edges
                    .iter()
                    .filter(|(from, _, _)| *from == id)
                    .map(|(_, to, kind)| {
                        let (kind, target) = match kind {
                            Some(cfg) if cfg.starts_with("cfg(") => (None, Some(cfg)),
                            other => (*other, None),
                        };
                        json!({"name": to, "pkg": to, "dep_kinds": [{"kind": kind, "target": target}]})
                    })
                    .collect();
                json!({"id": id, "deps": deps})
            })
            .collect();
        json!({"packages": packages, "resolve": {"nodes": nodes}})
    }

    fn crates(doc: &Value) -> Report {
        let mut report = Report::default();
        judge_crates(doc, &[PathBuf::from("/t/app")], &mut report).expect("judged");
        report
    }

    const GIT: Option<&str> = Some("git+https://github.com/ToyOSOrg/fork?branch=toyos#abc");
    const CRATES_IO: Option<&str> = Some("registry+https://github.com/rust-lang/crates.io-index");

    #[test]
    fn a_graph_is_walked_over_normal_edges_from_the_shipped_root() {
        let d = doc(
            &[
                ("app", Some("MIT OR Apache-2.0"), None, None),
                ("fine", Some("MIT/Apache-2.0"), CRATES_IO, None),
                ("gpl", Some("GPL-3.0-only"), CRATES_IO, None),
                ("builder", Some("GPL-3.0-only"), CRATES_IO, None),
                ("tester", Some("AGPL-3.0-only"), CRATES_IO, None),
                ("unreached", Some("GPL-3.0-only"), CRATES_IO, None),
                ("bare", None, CRATES_IO, None),
                ("filed", None, CRATES_IO, Some("LICENSE")),
            ],
            &[
                ("app", "fine", None),
                ("fine", "gpl", None),
                ("app", "builder", Some("build")),
                ("app", "tester", Some("dev")),
                ("fine", "bare", None),
                ("fine", "filed", None),
            ],
        );
        let report = crates(&d);
        let keys: Vec<&str> = report.findings.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["bare", "filed", "gpl"], "{:?}", report.findings);
        let gpl = &report.findings[2];
        assert_eq!(gpl.via, ["app → fine → gpl"]);
        assert_eq!(gpl.into, BTreeSet::from([None]));
        assert_eq!(gpl.licence, "GPL-3.0-only");
        assert_eq!(report.findings[0].why, "declares no licence");
        assert!(
            report.findings[1]
                .why
                .contains("only a licence file, LICENSE"),
            "{}",
            report.findings[1].why
        );
    }

    #[test]
    fn a_cfg_edge_is_walked_and_excepted_only_while_every_edge_in_is_under_it() {
        let packages = [
            ("app", Some("MIT"), None, None),
            ("mid", Some("MIT"), CRATES_IO, None),
            ("shim", None, None, None),
        ];
        let d = doc(
            &packages,
            &[("app", "mid", None), ("mid", "shim", Some("cfg(windows)"))],
        );
        let report = crates(&d);
        assert_eq!(report.findings.len(), 1, "a cfg edge was not walked");
        assert_eq!(report.findings[0].via, ["app → mid → shim [cfg(windows)]"]);
        assert_eq!(
            report.findings[0].into,
            BTreeSet::from([Some("cfg(windows)".to_string())])
        );

        let only = [Exception {
            subject: Subject::Crate("shim"),
            licence: "",
            standing: Standing::OnlyUnder("cfg(windows)"),
            reason: "r",
        }];
        assert!(verdict(report, &only).is_ok());

        // A second way in, unconditional or under another cfg, is not what the
        // exception was written against.
        for second in [None, Some("cfg(target_os = \"toyos\")")] {
            let d = doc(
                &packages,
                &[
                    ("app", "mid", None),
                    ("mid", "shim", Some("cfg(windows)")),
                    ("app", "shim", second),
                ],
            );
            let why = verdict(crates(&d), &only).unwrap_err();
            assert!(
                why.contains("crate shim") && why.contains("matches nothing shipped"),
                "{second:?}: {why}"
            );
        }
    }

    #[test]
    fn a_git_dependency_is_judged_like_any_other() {
        let d = doc(
            &[
                ("app", Some("MIT"), None, None),
                ("fork", Some("LGPL-2.1-only"), GIT, None),
            ],
            &[("app", "fork", None)],
        );
        let report = crates(&d);
        assert_eq!(report.findings.len(), 1);
        assert!(
            report.findings[0]
                .subject
                .contains("git git+https://github.com/ToyOSOrg/fork"),
            "{:?}",
            report.findings
        );
        let d = doc(
            &[
                ("app", Some("MIT"), None, None),
                ("fork", Some("MIT OR Apache-2.0"), GIT, None),
            ],
            &[("app", "fork", None)],
        );
        assert!(crates(&d).findings.is_empty());
    }

    #[test]
    fn a_root_the_metadata_does_not_hold_is_refused() {
        let d = doc(&[("other", Some("MIT"), None, None)], &[]);
        let mut report = Report::default();
        let why = judge_crates(&d, &[PathBuf::from("/t/app")], &mut report).unwrap_err();
        assert!(why.contains("/t/app/Cargo.toml"), "{why}");
    }

    const NOTICE: &str = "\
Title
=====

assets/font.ttf — a font
------------------------

    SPDX-License-Identifier: OFL-1.1

assets/game.wad — a game
------------------------

    SPDX-License-Identifier: LicenseRef-Shareware

tests/corpus/ — test input
--------------------------

    SPDX-License-Identifier: LGPL-2.1-or-later

assets/icons/*.svg — icons
--------------------------

    SPDX-License-Identifier: MIT

Rust crates and the forks
-------------------------

prose.
";

    fn tracked() -> Vec<String> {
        [
            "assets/font.ttf",
            "assets/game.wad",
            "assets/icons/a.svg",
            "tests/corpus/x.c",
            "src/main.rs",
        ]
        .map(String::from)
        .to_vec()
    }

    fn notice(text: &str) -> Report {
        let mut report = Report::default();
        judge_notice(
            text,
            &tracked(),
            &BTreeSet::from(["assets".to_string()]),
            &mut report,
        );
        report
    }

    #[test]
    fn a_notice_row_is_judged_when_it_ships() {
        let report = notice(NOTICE);
        let keys: Vec<(&str, &str)> = report
            .findings
            .iter()
            .map(|f| (f.key.as_str(), f.licence.as_str()))
            .collect();
        assert_eq!(
            keys,
            [("assets/game.wad", "LicenseRef-Shareware")],
            "{:?}",
            report.findings
        );
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("tests/corpus/") && n.contains("not shipped")),
            "{:?}",
            report.notes
        );
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("Rust crates and the forks")),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn a_notice_row_without_one_spdx_line_or_without_files_is_refused() {
        let missing = NOTICE.replace("    SPDX-License-Identifier: MIT\n", "");
        let report = notice(&missing);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.key == "assets/icons/*.svg" && f.why.contains("0 `SPDX")),
            "{:?}",
            report.findings
        );
        let stale = NOTICE.replace("assets/icons/*.svg — icons", "assets/gone/*.svg — icon");
        let report = notice(&stale);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.key == "assets/gone/*.svg" && f.why.contains("names no file")),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_font_licence_does_not_reach_a_file_that_is_no_font() {
        let report = notice(&NOTICE.replace("LicenseRef-Shareware", "OFL-1.1"));
        assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
        assert_eq!(report.findings[0].key, "assets/game.wad");
    }

    #[test]
    fn an_exception_matches_by_name_and_licence_and_a_stale_one_reds() {
        let finding = |licence: &str| Finding {
            subject: "crate x".into(),
            key: "x".into(),
            notice: false,
            licence: licence.into(),
            why: "not allowed".into(),
            via: vec!["app → x".into()],
            into: BTreeSet::from([None]),
        };
        let exception = [pending(Subject::Crate("x"), "GPL-2.0-only", "r")];
        let report = Report {
            findings: vec![finding("GPL-2.0-only")],
            ..Report::default()
        };
        assert!(verdict(report, &exception).is_ok());
        let report = Report {
            findings: vec![finding("GPL-3.0-only")],
            ..Report::default()
        };
        let why = verdict(report, &exception).unwrap_err();
        assert!(
            why.contains("GPL-3.0-only") && why.contains("matches nothing shipped"),
            "{why}"
        );
        let notice_row = [pending(Subject::Notice("x"), "GPL-2.0-only", "r")];
        let report = Report {
            findings: vec![finding("GPL-2.0-only")],
            ..Report::default()
        };
        assert!(
            verdict(report, &notice_row).is_err(),
            "a NOTICE exception excused a crate"
        );
    }

    #[test]
    fn glob_stops_at_a_separator() {
        assert!(glob("assets/icons/*.svg", "assets/icons/a.svg"));
        assert!(!glob("assets/icons/*.svg", "assets/icons/sub/a.svg"));
        assert!(glob(
            "tests/fixtures/gbae-v0.2.0-*",
            "tests/fixtures/gbae-v0.2.0-toyos.tar.gz"
        ));
        assert!(!glob("ovmf/*.fd", "ovmf/readme"));
    }
}
