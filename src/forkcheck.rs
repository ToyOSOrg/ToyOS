//! `cargo run -- --check-forks` — which lockfile pins have fallen behind the
//! fork branch they name.
//!
//! # The fork estate
//!
//! Every forked crate is a `toyos` branch in its own repository, based on a
//! pinned upstream commit and consumed through a `[patch.crates-io]` git
//! dependency. `forks.toml` is the inventory: it records each fork's upstream,
//! base commit, delta size, tier and PR status, and it has to stay accurate —
//! nothing else in the tree knows what a fork is for.
//!
//! - A fresh `git clone` plus `cargo run` needs no setup; cargo fetches the
//!   forks.
//! - To *edit* a fork, clone it beside the monorepo and list it in
//!   `.cargo/config.toml` (gitignored; `.cargo/config.toml.example` is the
//!   shape). Commit and push in the fork repository — the monorepo only pins
//!   the branch. **Fork clones are shared by every worktree**: use explicit
//!   paths, never `stash`, never switch branches in one.
//! - `git log <base>..toyos` in a fork is the ToyOS delta, and it is the
//!   content of a future upstream PR.
//! - Forks depend on ToyOS crates by version, never by path: a path outside the
//!   fork's own repository cannot resolve once cargo checks the fork out alone.
//!   Local builds resolve those versions through `[patch]`; an upstream PR
//!   cannot, so `toyos-abi`, `toyos` and `toyos-window` have to be published
//!   to crates.io before one can be opened.
//! - Every change must be upstream-mergeable. ToyOS enters as a *new platform*
//!   under `#[cfg(target_os = "toyos")]` beside the existing ones, cross-platform
//!   code is not modified, comments follow upstream's idiom and density, and the
//!   ToyOS rationale goes in the commit message rather than into the diff.
//!
//! The `rust/` submodule is the same estate under stricter rules, because its
//! delta is the largest: ToyOS is a new platform alongside unix/windows/wasi and
//! never a repurposed cfg gate; prefer ToyOS-specific files (`sys/pal/toyos/`,
//! `os/toyos/`, anything with `toyos` in the path); a cross-platform file is
//! touched only to add a target arm at an existing platform-dispatch site, never
//! to change cross-platform semantics or API shape; `library/alloc` and
//! `library/core` have **zero** delta. Cherry-picking an already-merged upstream
//! commit is allowed. Copying an unmerged PR is not — the delta must stay
//! exactly the content of a future upstream PR.
//!
//! **Coverage, and this is the one that bites:** the fork sources live *outside*
//! this repository, so a repository-wide search or gate does not reach them. An
//! enumeration of call sites must also cover `~/.cargo/git/checkouts/` or the
//! local fork clones, or it is an enumeration of part of the tree.
//!
//! # Why the drift check exists
//!
//! Every lockfile pins one commit of a fork branch. Branches move; lockfiles do
//! not follow, and no build notices: the pinned commit still exists, so cargo is
//! content to build last month's code forever. On 2026-08-08 six pins across
//! five lockfiles had drifted. Two of them mattered — `raw-window-handle` was
//! pinned at the commit *before* the one its branch was moved to so the tree
//! would match the head of open upstream PR #223, so the code we built was not
//! the code we had asked upstream to merge; and `target-lexicon`'s unpinned
//! commit was the one adding `OperatingSystem::Toyos` to the SysV arm, so
//! `default_calling_convention()` answered `Err(())` in `toyos-cc` and `SystemV`
//! in cranelift.
//!
//! **On demand only, and it must stay that way.** It asks every fork remote for
//! a branch head, so it needs the network. Wiring it into `cargo test` or into
//! the landing gate would put GitHub's availability on the path of every run.
//! Its own banner says so, because that is the line the next person adding a
//! check reads.
//!
//! It reports and never re-pins. Naming the `cargo update` that would fix a
//! drift keeps a dependency change a reviewed act rather than something a
//! helper did on the way past.
//!
//! **The consumed branch comes from the manifests**, never from `forks.toml`:
//! that file records a `pr_branch` for the forks with an open PR and it is
//! deliberately not the branch `[patch]` consumes. `forks.toml` supplies the
//! inventory, so an entry nothing in the tree pins is reported rather than
//! silently absent — and an entry whose shape this check does not recognise is
//! a line in that report, not a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::toolchain;

const TAG: &str = "[check-forks]";

/// A branch of a remote: the thing a head is asked for and the thing a pin is
/// compared against.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Source {
    url: String,
    branch: String,
}

/// The repository's own name, which is what `forks.toml` records in `upstream`
/// and what a report should say. It is not the crate name: `libloading` lives
/// in `nagisa/rust_libloading`, `memmap2` in `RazrFalcon/memmap2-rs`.
fn repo_name(url: &str) -> &str {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or(url).trim_end_matches(".git")
}

/// One `[[package]]` in one lockfile that resolves to a git source.
struct Pin {
    lockfile: String,
    package: String,
    version: String,
    url: String,
    /// `None` when the source pins a `rev` or a `tag`: there is no branch that
    /// could have moved under it, so it is reported rather than compared.
    branch: Option<String>,
    rev: String,
}

/// One `forks.toml` entry, as much of it as this check reads: the repository
/// its `upstream` names, and whether `tier = "source"` says the tree fetches
/// and compiles it rather than patching it — the one shape no manifest
/// consumes and none is expected to.
struct Fork {
    repo: Option<String>,
    fetched: bool,
}

/// Everything read off the disk, before anything is asked of the network.
struct Estate {
    /// `forks.toml`'s entries by table name.
    forks: BTreeMap<String, Fork>,
    /// Every branch a manifest asks for, and the manifests asking for it.
    consumed: BTreeMap<Source, BTreeSet<String>>,
    /// Every git-sourced package in every lockfile.
    pins: Vec<Pin>,
    /// How many lockfiles hold at least one of them.
    lockfiles: usize,
    /// How many manifests the `rust/` walk found. **Zero means the toolchain
    /// fork is not checked out here** — every linked worktree and every clone
    /// without `--recursive` — and its forks are then unjudgeable.
    rust_manifests: usize,
}

pub fn dispatch(root: &Path) {
    // `rust/` is walked separately because in a linked worktree it is an empty
    // stub and its lockfiles live in the primary checkout. Two of them pin
    // forks, and a check that reads only this tree would not see either.
    let rust = toolchain::rust_dir(root);
    let estate = collect(root, Some(&rust));
    let heads = heads(root, &estate);
    let (report, wrong) = render(&estate, &heads);
    print!("{report}");
    if wrong > 0 {
        std::process::exit(1);
    }
}

const CALLERS_TAG: &str = "[abi-callers]";

/// `cargo run -- --abi-callers <name>`: every use of `<name>` as a whole
/// identifier across the pinned fork estate, read from the cargo checkouts
/// the lockfiles resolve to. Offline, unlike the head comparison above. The
/// sweep reads `.rs` files only, and its count is of lines — first match per
/// line.
///
/// A "zero callers" claim about an ABI item is worth exactly the trees it
/// searched, and a monorepo grep does not search the estate — `stack_info`
/// is the recorded case: caller-less in the tree, called by the stacker fork
/// at its pinned revision. Exit 0 is the claim "every pinned source swept,
/// nothing found"; a hit or a pin with no checkout on disk both refuse it,
/// and a name `toyos-abi/` itself never spells is refused as a typo before
/// the sweep can prove it caller-less.
pub fn dispatch_callers(root: &Path, args: &[String]) {
    let at = args.iter().position(|a| a == "--abi-callers").expect("dispatched on this flag");
    let Some(name) = args.get(at + 1).filter(|n| !n.starts_with('-')) else {
        eprintln!("{CALLERS_TAG} --abi-callers takes the identifier to sweep for");
        std::process::exit(2);
    };
    let mut abi = Vec::new();
    rs_files(&root.join("toyos-abi"), &mut abi);
    let known = abi.iter().any(|f| {
        fs::read_to_string(f).is_ok_and(|text| !ident_lines(&text, name).is_empty())
    });
    if !known {
        eprintln!(
            "{CALLERS_TAG} nothing under toyos-abi/ spells `{name}`, so this sweep would \
             prove a typo caller-less — name the ABI item as its declaration does"
        );
        std::process::exit(2);
    }

    let rust = toolchain::rust_dir(root);
    let estate = collect(root, Some(&rust));
    let home = cargo_home();

    let mut revs: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for pin in &estate.pins {
        revs.entry((pin.url.clone(), pin.rev.clone())).or_default().insert(pin.lockfile.clone());
    }

    let mut hits = 0usize;
    let mut swept = 0usize;
    let mut unswept = Vec::new();
    for ((url, rev), lockfiles) in &revs {
        let pinned_by = lockfiles.iter().cloned().collect::<Vec<_>>().join(", ");
        let Some(dir) = checkout(&home, url, rev) else {
            unswept.push(format!(
                "{} at {} (pinned by {pinned_by}): no checkout under {}",
                repo_name(url),
                short(rev),
                home.join("git/checkouts").display(),
            ));
            continue;
        };
        swept += 1;
        let mut files = Vec::new();
        rs_files(&dir, &mut files);
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else { continue };
            for line in ident_lines(&text, name) {
                println!(
                    "{CALLERS_TAG} {}:{line}: {}",
                    file.display(),
                    text.lines().nth(line - 1).unwrap_or("").trim(),
                );
                hits += 1;
            }
        }
    }

    println!(
        "{CALLERS_TAG} {hits} use(s) of `{name}` across {swept} of {} pinned source(s)",
        revs.len(),
    );
    for gap in &unswept {
        eprintln!("{CALLERS_TAG} UNSWEPT: {gap}");
    }
    if hits > 0 || !unswept.is_empty() {
        std::process::exit(1);
    }
}

/// Where cargo keeps its git checkouts on this machine.
fn cargo_home() -> PathBuf {
    std::env::var_os("CARGO_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").expect("no $HOME")).join(".cargo"),
        PathBuf::from,
    )
}

/// The working copy cargo checked out for `url` at `rev`:
/// `git/checkouts/<repo>-<salt>/<seven hex digits>`. The salt is cargo's own
/// URL hash, so the repository name is matched and the salt is not.
fn checkout(cargo_home: &Path, url: &str, rev: &str) -> Option<PathBuf> {
    let repo = repo_name(url).to_lowercase();
    let short_rev = rev.get(..7)?;
    let entries = fs::read_dir(cargo_home.join("git/checkouts")).ok()?;
    for entry in entries.flatten() {
        let dir = entry.file_name().to_string_lossy().to_lowercase();
        let Some(salt) = dir.strip_prefix(&format!("{repo}-")) else { continue };
        if salt.contains('-') {
            continue;
        }
        let candidate = entry.path().join(short_rev);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Every `.rs` file under `dir`, `.git` excluded.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name != ".git" {
                rs_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    out.sort();
}

/// 1-based lines of `text` where `name` stands as a whole identifier — not as
/// a fragment of a longer one, which is what makes a short ABI name
/// sweepable at all.
fn ident_lines(text: &str, name: &str) -> Vec<usize> {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut from = 0;
        while let Some(pos) = line[from..].find(name) {
            let start = from + pos;
            let end = start + name.len();
            let free_before = start == 0 || !is_ident(bytes[start - 1]);
            let free_after = end >= bytes.len() || !is_ident(bytes[end]);
            if free_before && free_after {
                out.push(i + 1);
                break;
            }
            from = end.max(start + 1);
        }
    }
    out
}

fn collect(root: &Path, rust: Option<&Path>) -> Estate {
    let mut manifests = Vec::new();
    let mut locks = Vec::new();
    find(root, "", "Cargo.toml", &["target", "rust"], &mut manifests);
    find(root, "", "Cargo.lock", &["target", "rust"], &mut locks);
    let mut rust_manifests = 0;
    if let Some(rust) = rust {
        let before = manifests.len();
        find(rust, "rust/", "Cargo.toml", &["target", "build"], &mut manifests);
        find(rust, "rust/", "Cargo.lock", &["target", "build"], &mut locks);
        rust_manifests = manifests.len() - before;
    }

    let mut consumed: BTreeMap<Source, BTreeSet<String>> = BTreeMap::new();
    for (path, display) in &manifests {
        let Some(value) = parsed(path) else { continue };
        for (url, branch) in git_deps(&value) {
            consumed.entry(Source { url, branch }).or_default().insert(display.clone());
        }
    }

    let mut pins = Vec::new();
    let mut lockfiles = 0;
    for (path, display) in &locks {
        let Some(value) = parsed(path) else { continue };
        let before = pins.len();
        for package in value.get("package").and_then(toml::Value::as_array).into_iter().flatten() {
            let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
                continue;
            };
            let Some((url, branch, rev)) = git_source(source) else { continue };
            pins.push(Pin {
                lockfile: display.clone(),
                package: string(package, "name"),
                version: string(package, "version"),
                url,
                branch,
                rev,
            });
        }
        if pins.len() > before {
            lockfiles += 1;
        }
    }

    Estate { forks: forks_toml(root), consumed, pins, lockfiles, rust_manifests }
}

/// A manifest or lockfile that will not read or will not parse is skipped
/// rather than fatal: `rust/`'s tree carries fixtures that are deliberately
/// malformed, and none of them pins a fork.
fn parsed(path: &Path) -> Option<toml::Value> {
    fs::read_to_string(path).ok()?.parse().ok()
}

fn string(value: &toml::Value, key: &str) -> String {
    value.get(key).and_then(toml::Value::as_str).unwrap_or("?").to_string()
}

/// `forks.toml`'s fork entries, and the repository each one names.
///
/// Read for its inventory only. An entry this cannot read — one with no
/// `upstream`, or a shape added after this was written — carries no repository,
/// and the report refuses it: a declaration nothing can compare declares
/// nothing.
fn forks_toml(root: &Path) -> BTreeMap<String, Fork> {
    let path = root.join("forks.toml");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{TAG} read {}: {e} — it is the fork manifest", path.display()));
    let value: toml::Value =
        text.parse().unwrap_or_else(|e| panic!("{TAG} parse {}: {e}", path.display()));
    let mut forks = BTreeMap::new();
    for (name, entry) in value.as_table().into_iter().flatten() {
        if name == "meta" || !entry.is_table() {
            continue;
        }
        let repo = entry
            .get("upstream")
            .and_then(toml::Value::as_str)
            .and_then(|u| u.rsplit('/').next())
            .map(str::to_string);
        let fetched = entry.get("tier").and_then(toml::Value::as_str) == Some("source");
        forks.insert(name.clone(), Fork { repo, fetched });
    }
    forks
}

/// Every `{ git = …, branch = … }` anywhere in a manifest.
///
/// Walked rather than enumerated by section: a git dependency is legal in
/// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, any
/// `[patch.*]` and any `[target.*.dependencies]`, and a list of those five goes
/// stale the first time cargo grows a sixth.
fn git_deps(value: &toml::Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        let Some(table) = node.as_table() else { continue };
        if let (Some(git), Some(branch)) = (
            table.get("git").and_then(toml::Value::as_str),
            table.get("branch").and_then(toml::Value::as_str),
        ) {
            found.push((git.to_string(), branch.to_string()));
        }
        stack.extend(table.values());
    }
    found
}

/// `git+<url>[?branch=<b>]#<rev>`, as cargo writes it into a lockfile.
fn git_source(source: &str) -> Option<(String, Option<String>, String)> {
    let spec = source.strip_prefix("git+")?;
    let (locator, rev) = spec.split_once('#')?;
    let (url, query) = locator.split_once('?').unwrap_or((locator, ""));
    let branch = query.split('&').find_map(|p| p.strip_prefix("branch=")).map(str::to_string);
    Some((url.to_string(), branch, rev.to_string()))
}

/// Ask each remote once for every branch of it a manifest names.
///
/// `GIT_TERMINAL_PROMPT=0` because a URL that does not resolve makes GitHub ask
/// for credentials, and a check that stops on a hidden password prompt is worse
/// than one that says out loud it could not reach the remote.
fn heads(root: &Path, estate: &Estate) -> BTreeMap<Source, Result<String, String>> {
    let mut by_remote: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for source in estate.consumed.keys() {
        by_remote.entry(&source.url).or_default().push(&source.branch);
    }
    eprintln!(
        "{TAG} asking {} remotes for {} branches...",
        by_remote.len(),
        estate.consumed.len()
    );

    let mut heads = BTreeMap::new();
    for (url, branches) in by_remote {
        let answered = match Command::new("git")
            .arg("ls-remote")
            .arg(url)
            .args(branches.iter().map(|b| format!("refs/heads/{b}")))
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(root)
            .output()
        {
            Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().replace('\n', "; ")),
            Err(e) => Err(format!("run git ls-remote: {e}")),
        };
        for branch in branches {
            let head = match &answered {
                Ok(text) => text
                    .lines()
                    .find_map(|line| {
                        let (sha, name) = line.split_once('\t')?;
                        (name.trim() == format!("refs/heads/{branch}")).then(|| sha.to_string())
                    })
                    .ok_or_else(|| format!("{url} has no branch {branch}")),
                Err(e) => Err(e.clone()),
            };
            heads.insert(Source { url: url.to_string(), branch: branch.to_string() }, head);
        }
    }
    heads
}

/// The report, and how many branches it found something wrong with.
fn render(estate: &Estate, heads: &BTreeMap<Source, Result<String, String>>) -> (String, usize) {
    let mut out = String::new();
    let mut say = |line: &str| {
        out.push_str(TAG);
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    };

    say("on demand only: this asks the network, so it is in neither `cargo test` nor the landing gate.");
    say(&format!(
        "forks.toml names {}; manifests name {} branches; {} lockfiles hold {} pins.",
        estate.forks.len(),
        estate.consumed.len(),
        estate.lockfiles,
        estate.pins.len()
    ));
    say("");

    let mut wrong = 0;
    let mut uncompared = Vec::new();
    for (source, manifests) in &estate.consumed {
        let repo = repo_name(&source.url);
        let head = match heads.get(source) {
            Some(Ok(head)) => head,
            Some(Err(e)) => {
                wrong += 1;
                say(&format!("UNKNOWN {repo} branch {} — {e}", source.branch));
                continue;
            }
            None => panic!("{TAG} nothing was asked of {} {}", source.url, source.branch),
        };
        let mine: Vec<&Pin> = estate
            .pins
            .iter()
            .filter(|p| p.url == source.url && p.branch.as_deref() == Some(&source.branch))
            .collect();
        if mine.is_empty() {
            uncompared.push(format!(
                "{repo} branch {} — named by {}, pinned by no lockfile",
                source.branch,
                manifests.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
            continue;
        }
        let behind: Vec<&Pin> = mine.iter().copied().filter(|p| p.rev != *head).collect();
        if behind.is_empty() {
            let holders: BTreeSet<&str> = mine.iter().map(|p| p.lockfile.as_str()).collect();
            say(&format!(
                "current {repo} branch {} at {} — {}",
                source.branch,
                short(head),
                holders.into_iter().collect::<Vec<_>>().join(", ")
            ));
            continue;
        }
        wrong += 1;
        say(&format!("BEHIND  {repo} branch {}", source.branch));
        say(&format!("        branch head {head}"));
        let mut fixes: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for pin in &behind {
            say(&format!(
                "        pinned at   {} by {} {} in {}",
                short(&pin.rev),
                pin.package,
                pin.version,
                pin.lockfile
            ));
            fixes
                .entry(&pin.lockfile)
                .or_default()
                .insert(format!("-p {}@{}", pin.package, pin.version));
        }
        for (lockfile, specs) in fixes {
            say(&format!(
                "        fix         cargo update --manifest-path {} {}",
                lockfile.replace("Cargo.lock", "Cargo.toml"),
                specs.into_iter().collect::<Vec<_>>().join(" ")
            ));
        }
        say("");
    }

    // Everything the comparison above could not reach, so a fork whose shape
    // this does not understand is a line here rather than a silence or a panic.
    let named: BTreeSet<&str> = estate.consumed.keys().map(|s| repo_name(&s.url)).collect();
    // A declaration nothing consumes is a dead declaration and not a note: this
    // manifest is worth something only as the estate's inventory. Only if the
    // walk that would have consumed it ran, though — the advice below is to
    // delete the entry.
    let judged = estate.rust_manifests > 0;
    let mut dead = Vec::new();
    for (name, fork) in &estate.forks {
        match &fork.repo {
            Some(repo) if named.contains(repo.as_str()) => {}
            Some(repo) if fork.fetched => uncompared.push(format!(
                "{name} — forks.toml names {repo} at tier `source`, fetched rather than patched, \
                 so no manifest consumes it"
            )),
            Some(repo) if !judged => uncompared.push(format!(
                "{name} — forks.toml names {repo} and nothing walked here consumes it"
            )),
            Some(repo) => dead.push(format!(
                "{name} — forks.toml names {repo}, which no manifest in this tree consumes"
            )),
            None => dead
                .push(format!("{name} — forks.toml entry carries no `upstream` this can read")),
        }
    }
    let declared: BTreeSet<&str> =
        estate.forks.values().filter_map(|f| f.repo.as_deref()).collect();
    for repo in &named {
        if !declared.contains(repo) {
            uncompared.push(format!("{repo} — consumed by a manifest and not in forks.toml"));
        }
    }
    for pin in &estate.pins {
        let repo = repo_name(&pin.url);
        match &pin.branch {
            None => uncompared.push(format!(
                "{repo} — {} pins it by rev, so no branch can have moved under it",
                pin.lockfile
            )),
            Some(branch) => {
                let source = Source { url: pin.url.clone(), branch: branch.clone() };
                if !estate.consumed.contains_key(&source) {
                    uncompared.push(format!(
                        "{repo} — {} pins branch {branch}, which no manifest names",
                        pin.lockfile
                    ));
                }
            }
        }
    }
    if !judged {
        say("not judged: rust/ is not checked out here, so a fork only the toolchain's manifests \
             consume cannot be told from a dead declaration.");
        say("");
    }
    uncompared.sort();
    uncompared.dedup();
    if !uncompared.is_empty() {
        say("not compared:");
        for line in &uncompared {
            say(&format!("  {line}"));
        }
        say("");
    }
    dead.sort();
    dead.dedup();
    if !dead.is_empty() {
        say("dead declarations — an entry nothing consumes is not an inventory:");
        for line in &dead {
            say(&format!("  DEAD  {line}"));
        }
        say("");
    }

    say(&match wrong {
        0 => format!("{} branches asked, all current.", estate.consumed.len()),
        n => format!(
            "{} branches asked, {n} not current. Nothing was changed — run the `fix` line.",
            estate.consumed.len()
        ),
    });
    if !dead.is_empty() {
        say(&format!(
            "{} forks.toml entr(ies) declare a fork this tree does not consume. Delete the \
             entry, or consume it.",
            dead.len()
        ));
    }
    (out, wrong + dead.len())
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(10)]
}

fn find(base: &Path, prefix: &str, wanted: &str, skip: &[&str], out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if name.starts_with('.') || skip.contains(&name.as_str()) {
                continue;
            }
            find(&entry.path(), &format!("{prefix}{name}/"), wanted, skip, out);
        } else if name == wanted {
            out.push((entry.path(), format!("{prefix}{wanted}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test's scratch: the fake remote goes in `widget/`, the tree that
    /// consumes it in `tree/`. The remote's directory name is the repository
    /// name every assertion below reads.
    fn case(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forkcheck-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A repository standing in for a fork remote. `git ls-remote` takes a path
    /// as readily as a URL, so the whole check runs end to end here with no
    /// network — which is what lets these tests live inside `cargo test` while
    /// the command itself may not.
    fn remote(case: &Path, branch: &str) -> (PathBuf, String) {
        let dir = case.join("widget");
        sh(case, &["init", "-q", "-b", branch, "widget"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "user.name", "t"]);
        // The host's global config signs every commit, and a test that waited
        // on gpg would be a test that hangs.
        sh(&dir, &["config", "commit.gpgsign", "false"]);
        let head = commit(&dir, "one");
        (dir, head)
    }

    fn commit(dir: &Path, text: &str) -> String {
        fs::write(dir.join("f"), text).unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-qm", text]);
        let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn sh(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A tree with one fork in `forks.toml`, one manifest consuming it, one
    /// lockfile pinning `rev`, and a `rust/` the walk can find a manifest in —
    /// without which nothing here would be judged at all.
    fn tree(case: &Path, url: &Path, branch: &str, rev: &str) -> PathBuf {
        let root = case.join("tree");
        fs::create_dir_all(root.join("rust")).unwrap();
        fs::write(root.join("rust/Cargo.toml"), "[package]\nname = \"r\"\n").unwrap();
        fs::write(
            root.join("forks.toml"),
            "[meta]\nowner = \"Japabu\"\n\n[widget]\nupstream = \"someone/widget\"\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"t\"\n\n[patch.crates-io]\n\
                 widget = {{ git = \"{}\", branch = \"{branch}\" }}\n",
                url.display()
            ),
        )
        .unwrap();
        lock(&root.join("Cargo.lock"), url, branch, rev);
        root
    }

    fn lock(path: &Path, url: &Path, branch: &str, rev: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "[[package]]\nname = \"widget\"\nversion = \"1.0.0\"\n\
                 source = \"git+{}?branch={branch}#{rev}\"\n",
                url.display()
            ),
        )
        .unwrap();
    }

    fn check(root: &Path) -> (String, usize) {
        checked(root, Some(root.join("rust")))
    }

    /// The whole run, with `rust` the toolchain checkout the walk is pointed at.
    fn checked(root: &Path, rust: Option<PathBuf>) -> (String, usize) {
        let estate = collect(root, rust.as_deref());
        let heads = heads(root, &estate);
        render(&estate, &heads)
    }

    #[test]
    fn a_pin_at_the_branch_head_is_current() {
        let case = case("current");
        let (url, rev) = remote(&case, "toyos");
        let root = tree(&case, &url, "toyos", &rev);
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 0, "{report}");
        assert!(report.contains("current widget branch toyos"), "{report}");
        assert!(report.contains("1 branches asked, all current."), "{report}");
    }

    /// The teeth: the same tree, with the branch moved one commit on.
    #[test]
    fn a_branch_that_moved_leaves_the_pin_behind_and_the_fix_is_named() {
        let case = case("moved");
        let (url, old) = remote(&case, "toyos");
        let root = tree(&case, &url, "toyos", &old);
        let new = commit(&url, "two");
        assert_ne!(old, new);
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 1, "{report}");
        assert!(report.contains("BEHIND  widget branch toyos"), "{report}");
        assert!(report.contains(&format!("branch head {new}")), "{report}");
        assert!(report.contains(short(&old)), "{report}");
        assert!(
            report.contains("cargo update --manifest-path Cargo.toml -p widget@1.0.0"),
            "{report}"
        );
    }

    /// Two lockfiles pinning one branch at two revisions — the shape the audit
    /// found for `libloading`, and the one a check reading a single lockfile
    /// cannot see. The root lockfile is the current one here, so only a check
    /// that reads the other says anything at all.
    #[test]
    fn a_second_lockfile_is_judged_on_its_own_pin() {
        let case = case("split");
        let (url, old) = remote(&case, "toyos");
        let root = tree(&case, &url, "toyos", &old);
        let new = commit(&url, "two");
        lock(&root.join("Cargo.lock"), &url, "toyos", &new);
        lock(&root.join("sub/Cargo.lock"), &url, "toyos", &old);
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 1, "{report}");
        assert!(report.contains("in sub/Cargo.lock"), "{report}");
        assert!(!report.contains("in Cargo.lock"), "{report}");
    }

    /// A `forks.toml` entry no manifest consumes is a dead declaration and the
    /// run is not clean, whether it names a repository or carries no
    /// `upstream` at all; `tier = "source"` is the one shape that stays a note.
    #[test]
    fn a_fork_no_manifest_consumes_is_a_dead_declaration() {
        let case = case("orphan");
        let (url, rev) = remote(&case, "toyos");
        let root = tree(&case, &url, "toyos", &rev);
        fs::write(
            root.join("forks.toml"),
            "[meta]\nowner = \"Japabu\"\n\n[widget]\nupstream = \"someone/widget\"\n\n\
             [doomgeneric]\nupstream = \"ozkl/doomgeneric\"\n\n[nameless]\nwhy = \"no upstream\"\n\n\
             [fetched]\nupstream = \"someone/fetched\"\ntier = \"source\"\n",
        )
        .unwrap();
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 2, "{report}");
        assert!(report.contains("DEAD  doomgeneric — forks.toml names doomgeneric"), "{report}");
        assert!(
            report.contains("DEAD  nameless — forks.toml entry carries no `upstream`"),
            "{report}"
        );
        assert!(report.contains("fetched — forks.toml names fetched at tier `source`"), "{report}");
        assert!(!report.contains("DEAD  fetched"), "{report}");
    }

    /// **A stub `rust/` accuses nothing.** The toolchain's forks are consumed
    /// by its manifests alone, so without this the run calls three live forks
    /// dead and tells the reader to delete them.
    #[test]
    fn a_stub_rust_checkout_judges_nothing_dead() {
        let case = case("stub-rust");
        let (url, rev) = remote(&case, "toyos");
        let root = tree(&case, &url, "toyos", &rev);
        fs::write(
            root.join("forks.toml"),
            "[meta]\nowner = \"Japabu\"\n\n[widget]\nupstream = \"someone/widget\"\n\n\
             [doomgeneric]\nupstream = \"ozkl/doomgeneric\"\n",
        )
        .unwrap();
        fs::remove_dir_all(root.join("rust")).unwrap();

        let (report, wrong) = checked(&root, Some(root.join("rust")));
        assert_eq!(wrong, 0, "{report}");
        assert!(report.contains("not judged: rust/ is not checked out here"), "{report}");
        assert!(!report.contains("DEAD"), "{report}");

        // And with the toolchain there, the same entry is dead.
        fs::create_dir_all(root.join("rust")).unwrap();
        fs::write(root.join("rust/Cargo.toml"), "[package]\nname = \"r\"\n").unwrap();
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 1, "{report}");
        assert!(report.contains("DEAD  doomgeneric"), "{report}");
    }

    /// A remote that cannot be reached is not a clean run.
    #[test]
    fn an_unreachable_remote_is_not_silence() {
        let case = case("gone");
        let root = tree(&case, &case.join("widget"), "toyos", &"0".repeat(40));
        let (report, wrong) = check(&root);
        assert_eq!(wrong, 1, "{report}");
        assert!(report.contains("UNKNOWN widget branch toyos"), "{report}");
    }

    #[test]
    fn an_identifier_is_found_whole_and_never_as_a_fragment() {
        let text = "let (base, _size) = toyos_abi::syscall::stack_info()?;\n\
                    fn stack_info_extended() {}\n\
                    // a stack_info mention in prose\n\
                    let restack_info = 0;\n";
        assert_eq!(ident_lines(text, "stack_info"), vec![1, 3]);
        assert_eq!(ident_lines(text, "stack_info_extended"), vec![2]);
        assert_eq!(ident_lines(text, "absent_name"), Vec::<usize>::new());
    }

    /// The checkout layout is cargo's: `<repo>-<salt>/<seven hex>`. The salt
    /// is not matched, and a repository whose name extends another's must not
    /// answer for it.
    #[test]
    fn a_checkout_is_found_by_repository_name_and_revision() {
        let dir = case("checkout");
        let rev = "c25842ac264c7121e33c5ad81f93dc7bba22cca2";
        let inner = dir.join("git/checkouts/stacker-dd045e8025e5c69e").join(&rev[..7]);
        fs::create_dir_all(&inner).unwrap();
        let decoy = dir.join("git/checkouts/stacker-rs-ffffffffffffffff").join(&rev[..7]);
        fs::create_dir_all(&decoy).unwrap();

        let found = checkout(&dir, "https://github.com/Japabu/stacker", rev).unwrap();
        assert_eq!(found, inner);
        assert!(checkout(&dir, "https://github.com/Japabu/stacker", "0000000000").is_none());
        assert_eq!(
            checkout(&dir, "https://github.com/Japabu/stacker-rs", rev).unwrap(),
            decoy,
        );
    }
}
