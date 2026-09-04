//! The five crates ToyOS publishes, and the rule that keeps their versions
//! moving.
//!
//! The winit, softbuffer and cpal forks name `toyos-abi`, `toyos` and
//! `toyos-window` **by version**, because a path escaping a fork's own
//! repository cannot resolve once cargo checks it out alone. So those and the
//! two they pull in are published, and the ABI they carry is unstable by
//! policy — a built program that breaks, breaks.
//!
//! **That policy is what this gate is for.** None of the five is
//! compatible-by-construction, so a branch that changes a file under one of
//! them bumps its minor — `0.x.0` to `0.(x+1).0` — and every in-tree
//! dependent's `version` pin with it. A crate *changed* and not republished is
//! worse than one republished under a taken version, which crates.io refuses:
//! the fork naming the version still resolves, and silently gets the old code.
//!
//! [`PUBLISHED`]'s order is a dependency order, and it is the order
//! `.github/workflows/publish.yml` takes: a crate cannot go up before the index
//! holds every version it names.

use std::path::Path;

use crate::pr::git;

/// One published crate: where its manifest is, and which of the five it names
/// by version.
pub struct Crate {
    /// The package name, which is the crates.io name.
    pub name: &'static str,
    /// The directory the crate is in, repository-relative — also the prefix a
    /// change to the crate is recognised by.
    pub dir: &'static str,
    /// The others it depends on, so a bump here has to move their pins there.
    pub depends_on: &'static [&'static str],
}

/// The workflow that puts these on crates.io, and so the rule's precondition:
/// a branch judged against a base that does not hold this has no taken version
/// to collide with, because nothing of ours is on the registry yet.
///
/// `every_row_names_a_crate_the_tree_holds` refuses a tree that has lost it, so
/// the rule cannot be turned off by deleting the publisher.
const PUBLISHER: &str = ".github/workflows/publish.yml";

/// The five, in the order a publisher must take them.
pub const PUBLISHED: &[Crate] = &[
    Crate { name: "toyos-abi", dir: "toyos-abi", depends_on: &[] },
    Crate { name: "toyos-keymap", dir: "toyos-keymap", depends_on: &[] },
    Crate { name: "toyos-font", dir: "userland/toyos-font", depends_on: &[] },
    Crate { name: "toyos", dir: "toyos", depends_on: &["toyos-abi"] },
    Crate {
        name: "toyos-window",
        dir: "userland/toyos-window",
        depends_on: &["toyos-abi", "toyos-keymap", "toyos-font", "toyos"],
    },
];

/// `<name> <version> <manifest path>` for each of [`PUBLISHED`], in order —
/// what `cargo run -- --sdk-versions` prints and the publish workflow reads.
///
/// The workflow asks this rather than parsing a manifest in shell, so the set
/// of published crates and their order live here and nowhere else.
pub fn dispatch_versions(root: &Path) {
    for krate in PUBLISHED {
        let manifest = format!("{}/Cargo.toml", krate.dir);
        let text = std::fs::read_to_string(root.join(&manifest))
            .unwrap_or_else(|e| panic!("read {manifest}: {e}"));
        let version = package_version(&text)
            .unwrap_or_else(|| panic!("{manifest} declares no [package] version"));
        println!("{} {version} {manifest}", krate.name);
    }
}

/// `cargo run -- --sdk-version-check [--base <ref>]`.
pub fn dispatch_check(root: &Path, args: &[String]) {
    let base = args
        .iter()
        .position(|a| a == "--base")
        .map_or("origin/main", |pos| {
            args.get(pos + 1).map_or("origin/main", String::as_str)
        });
    match judge(root, base) {
        Ok(line) => println!("[sdk] {line}"),
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(1);
        }
    }
}

/// The `version = "…"` of the `[package]` table. A hand walk and not a parse:
/// one key of one table, in five manifests this repository writes.
fn package_version(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = line.strip_prefix("version") {
            let value = value.trim_start().strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// Every `<dep> = { … version = "…" … }` line in `text` naming one of the five,
/// as `(dependency, version)`. One line per dependency is the only spelling
/// this reaches, which is why [`judge`] states the pin it read.
fn version_pins(text: &str) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((name, rest)) = line.split_once('=') else { continue };
        let name = name.trim();
        if !PUBLISHED.iter().any(|k| k.name == name) {
            continue;
        }
        let Some(at) = rest.find("version") else { continue };
        let Some(value) = rest[at..].split_once('=') else { continue };
        let version: String =
            value.1.trim_start().trim_start_matches('"').chars().take_while(|c| *c != '"').collect();
        if !version.is_empty() {
            pins.push((name.to_string(), version));
        }
    }
    pins
}

/// A minor bump and nothing else: `0.x.0` becomes `0.(x+1).0`. Every change may
/// break by policy, so there is no patch level to move and no judgement about
/// which kind of change this was; anything but `0.x.0` is refused, not guessed.
fn next_minor(version: &str) -> Option<String> {
    let mut parts = version.split('.');
    let major = parts.next()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    if parts.next().is_some() || patch != "0" {
        return None;
    }
    Some(format!("{major}.{}.0", minor + 1))
}

/// The rule over the branch at `root` against `base`: the one-line verdict, or
/// the refusal.
pub fn judge(root: &Path, base: &str) -> Result<String, String> {
    let merge_base = git(root, &["merge-base", base, "HEAD"])?;
    if file_at(root, &merge_base, PUBLISHER)?.is_empty() {
        return Ok(format!("{base} does not publish yet, so no version here is taken."));
    }
    let changed = git(root, &["diff", "--name-only", "--no-renames", "-z", &merge_base, "HEAD"])?;
    let changed: Vec<&str> = changed.split('\0').filter(|p| !p.is_empty()).collect();

    let mut refusals = Vec::new();
    let mut bumped = Vec::new();
    for krate in PUBLISHED {
        let manifest = format!("{}/Cargo.toml", krate.dir);
        let prefix = format!("{}/", krate.dir);
        if !changed.iter().any(|p| p.starts_with(&prefix) && !p.ends_with("Cargo.lock")) {
            continue;
        }
        let at_head = version_at(root, "HEAD", &manifest)?;
        let at_base = version_at(root, &merge_base, &manifest)?;
        if at_head == at_base {
            let want = next_minor(&at_base).unwrap_or_else(|| {
                format!("a minor bump of {at_base}, which is not the 0.x.0 this rule knows")
            });
            refusals.push(format!(
                "[sdk] {} changed and its version did not: {manifest} still says {at_base}, and \
                 the next one is {want}.",
                krate.name
            ));
            continue;
        }
        bumped.push((krate, at_base, at_head));
    }

    // A bump is only half of it: every in-tree dependent resolves the crate by
    // the `version` beside its `path`, and a pin left behind names a version
    // the registry will not have.
    for (krate, _, at_head) in &bumped {
        for dependent in PUBLISHED.iter().filter(|d| d.depends_on.contains(&krate.name)) {
            let manifest = format!("{}/Cargo.toml", dependent.dir);
            let text = file_at(root, "HEAD", &manifest)?;
            // A commit that does not hold the dependent at all has no pin to be
            // stale; `every_row_names_a_crate_the_tree_holds` is what refuses a
            // row whose crate has left the tree.
            if text.is_empty() {
                continue;
            }
            match version_pins(&text).into_iter().find(|(name, _)| name == krate.name) {
                Some((_, pinned)) if &pinned == at_head => {}
                Some((_, pinned)) => refusals.push(format!(
                    "[sdk] {manifest} pins {} at {pinned}, which this branch moved to {at_head}.",
                    krate.name
                )),
                None => refusals.push(format!(
                    "[sdk] {manifest} depends on {} with no `version` beside its `path`, so the \
                     published crate names no version at all.",
                    krate.name
                )),
            }
        }
    }

    // `cargo publish` re-locks the package's own lockfile and refuses a dirty tree.
    for lockfile in tracked_lockfiles(root)? {
        let text = file_at(root, "HEAD", &lockfile)?;
        for (name, locked, has_source) in lock_packages(&text) {
            if has_source {
                continue;
            }
            let Some(krate) = PUBLISHED.iter().find(|k| k.name == name) else { continue };
            let manifest = format!("{}/Cargo.toml", krate.dir);
            let wants = version_at(root, "HEAD", &manifest)?;
            if locked != wants {
                refusals.push(format!(
                    "[sdk] {lockfile} locks {name} at {locked} with no `source` (a path \
                     dependency), and {manifest} now declares {wants}: `cargo update -p {name} \
                     --manifest-path {manifest}` (or the workspace lockfile's equivalent).",
                ));
            }
        }
    }

    if refusals.is_empty() {
        return Ok(match bumped.len() {
            0 => "this branch changes none of the five published crates.".to_string(),
            _ => format!(
                "bumped: {}",
                bumped
                    .iter()
                    .map(|(k, from, to)| format!("{} {from} -> {to}", k.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    refusals.push(
        "[sdk] These five are on crates.io and the forks resolve them by version, so a change \
         published under the version it already had is a fork silently building the old code. \
         Every change may break by policy: the bump is the minor, and every in-tree dependent's \
         pin moves with it."
            .to_string(),
    );
    Err(refusals.join("\n"))
}

/// `path`'s `[package] version` at `commit`.
fn version_at(root: &Path, commit: &str, path: &str) -> Result<String, String> {
    let text = file_at(root, commit, path)?;
    package_version(&text).ok_or_else(|| format!("[sdk] {path} at {commit} declares no version"))
}

/// `path`'s text at `commit`: empty where that commit does not hold it.
fn file_at(root: &Path, commit: &str, path: &str) -> Result<String, String> {
    if git(root, &["ls-tree", commit, "--", path])?.is_empty() {
        return Ok(String::new());
    }
    git(root, &["show", &format!("{commit}:{path}")])
}

fn tracked_lockfiles(root: &Path) -> Result<Vec<String>, String> {
    let out = git(root, &["ls-files", "-z", "*Cargo.lock"])?;
    Ok(out.split('\0').filter(|p| !p.is_empty() && !p.starts_with("rust/")).map(String::from).collect())
}

#[derive(Default)]
struct Block {
    name: Option<String>,
    version: Option<String>,
    has_source: bool,
}

fn lock_packages(text: &str) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut block: Option<Block> = None;
    let flush = |block: &mut Option<Block>, out: &mut Vec<_>| {
        if let Some(Block { name: Some(name), version: Some(version), has_source }) = block.take() {
            out.push((name, version, has_source));
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            flush(&mut block, &mut out);
            block = Some(Block::default());
            continue;
        }
        if line.starts_with('[') {
            flush(&mut block, &mut out);
            block = None;
            continue;
        }
        let Some(current) = &mut block else { continue };
        if let Some(value) = line.strip_prefix("name").and_then(|v| v.trim_start().strip_prefix('=')) {
            current.name = Some(value.trim().trim_matches('"').to_string());
        } else if let Some(value) =
            line.strip_prefix("version").and_then(|v| v.trim_start().strip_prefix('='))
        {
            current.version = Some(value.trim().trim_matches('"').to_string());
        } else if line.starts_with("source") {
            current.has_source = true;
        }
    }
    flush(&mut block, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::tests::{commit, repo, sh};

    /// A manifest for one of the five at `version`, with `pins` written the way
    /// the tree writes them.
    fn manifest(name: &str, version: &str, pins: &[(&str, &str)]) -> String {
        let mut text = format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n\
             license = \"MIT OR Apache-2.0\"\ndescription = \"x\"\n\n[dependencies]\n"
        );
        for (dep, at) in pins {
            text.push_str(&format!("{dep} = {{ path = \"../{dep}\", version = \"{at}\" }}\n"));
        }
        text
    }

    /// Everything committed so far becomes main's; the branch starts here.
    fn main_is_here(wt: &Path) {
        sh(wt, &["branch", "-f", "main", "HEAD"]);
    }

    /// The base publishes, which is what turns the rule on.
    fn publishing(wt: &Path) {
        commit(wt, PUBLISHER, "name: publish\n", "publish these");
    }

    /// **The judge, and the partial fix it must not pass**: a bump alone, with
    /// a dependent still naming the old version, publishes a crate whose
    /// dependency the registry has not got. That half is the test below.
    #[test]
    fn a_changed_crate_that_did_not_move_its_version_is_refused_by_name() {
        let (_origin, wt) = repo("sdk-unbumped");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        let refusal = judge(&wt, "main").expect_err("a changed crate must bump its version");
        assert!(refusal.contains("toyos-abi changed and its version did not"), "{refusal}");
        assert!(refusal.contains("still says 0.1.0, and the next one is 0.2.0"), "{refusal}");
    }

    /// **The rule's precondition, which is also this branch's own exemption**:
    /// the same diff that reds above passes against a base that publishes
    /// nothing, because there is no taken version on crates.io to collide with.
    #[test]
    fn a_base_that_does_not_publish_yet_has_no_taken_version() {
        let (_origin, wt) = repo("sdk-unpublished");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        main_is_here(&wt);

        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        let verdict = judge(&wt, "main").expect("nothing is published, so nothing is taken");
        assert!(verdict.contains("does not publish yet"), "{verdict}");
    }

    /// The other half, which a bump alone passes: the dependent's pin.
    #[test]
    fn a_bump_that_leaves_a_dependents_pin_behind_is_refused_by_name() {
        let (_origin, wt) = repo("sdk-stale-pin");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        commit(
            &wt,
            "toyos/Cargo.toml",
            &manifest("toyos", "0.1.0", &[("toyos-abi", "0.1.0")]),
            "sdk manifest",
        );
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.2.0", &[]), "abi: 0.2.0");
        let refusal = judge(&wt, "main").expect_err("a stale pin must be refused");
        assert!(
            refusal.contains("toyos/Cargo.toml pins toyos-abi at 0.1.0, which this branch moved \
                              to 0.2.0"),
            "{refusal}"
        );

        // And the whole change. Moving the pin changes `toyos` too, so `toyos`
        // owes its own bump: a dependent republished under its old version
        // still names the dependency version the registry has not got.
        commit(
            &wt,
            "toyos/Cargo.toml",
            &manifest("toyos", "0.2.0", &[("toyos-abi", "0.2.0")]),
            "sdk: follow the abi",
        );
        let verdict = judge(&wt, "main").expect("bump plus pin is the whole rule");
        assert!(verdict.contains("toyos-abi 0.1.0 -> 0.2.0"), "{verdict}");
        assert!(verdict.contains("toyos 0.1.0 -> 0.2.0"), "{verdict}");
    }

    fn lockfile(name: &str, version: &str, source: bool) -> String {
        let mut text =
            format!("# generated\nversion = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n");
        if source {
            text.push_str("source = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
        }
        text
    }

    #[test]
    fn a_lock_only_change_to_a_published_crate_passes_without_a_bump() {
        let (_origin, wt) = repo("sdk-lock-only");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "toyos-abi/Cargo.lock", &lockfile("toyos-abi", "0.1.0", false), "abi: re-lock");
        let verdict = judge(&wt, "main").expect("a lock-only change owes no bump");
        assert!(verdict.contains("changes none of the five"), "{verdict}");
    }

    #[test]
    fn a_bump_with_a_stale_path_dependency_lock_entry_is_refused_by_name() {
        let (_origin, wt) = repo("sdk-stale-lock");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        commit(
            &wt,
            "toyos/Cargo.toml",
            &manifest("toyos", "0.1.0", &[("toyos-abi", "0.1.0")]),
            "sdk manifest",
        );
        commit(&wt, "toyos/Cargo.lock", &lockfile("toyos-abi", "0.1.0", false), "sdk lock");
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.2.0", &[]), "abi: 0.2.0");
        commit(
            &wt,
            "toyos/Cargo.toml",
            &manifest("toyos", "0.2.0", &[("toyos-abi", "0.2.0")]),
            "sdk: follow the abi",
        );
        let refusal = judge(&wt, "main").expect_err("a stale lock entry must be refused");
        assert!(
            refusal.contains(
                "toyos/Cargo.lock locks toyos-abi at 0.1.0 with no `source` (a path dependency), \
                 and toyos-abi/Cargo.toml now declares 0.2.0"
            ),
            "{refusal}"
        );
        assert!(
            refusal.contains("cargo update -p toyos-abi --manifest-path toyos-abi/Cargo.toml"),
            "{refusal}"
        );

        commit(&wt, "toyos/Cargo.lock", &lockfile("toyos-abi", "0.2.0", false), "sdk: re-lock");
        let verdict = judge(&wt, "main").expect("a lock that agrees passes");
        assert!(verdict.contains("toyos-abi 0.1.0 -> 0.2.0"), "{verdict}");
    }

    #[test]
    fn a_registry_entry_at_the_old_version_passes() {
        let (_origin, wt) = repo("sdk-fork-pin");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A;\n", "abi source");
        commit(&wt, "userland/snake/Cargo.lock", &lockfile("toyos-abi", "0.1.0", true), "fork lock");
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.2.0", &[]), "abi: 0.2.0");
        let verdict = judge(&wt, "main").expect("a fork's registry pin is not this rule's business");
        assert!(verdict.contains("toyos-abi 0.1.0 -> 0.2.0"), "{verdict}");
    }

    /// A branch that touches none of the five is every other branch, and the
    /// rule has nothing to say about it.
    #[test]
    fn a_branch_that_changes_none_of_the_five_passes() {
        let (_origin, wt) = repo("sdk-elsewhere");
        commit(&wt, "toyos-abi/Cargo.toml", &manifest("toyos-abi", "0.1.0", &[]), "abi manifest");
        publishing(&wt);
        main_is_here(&wt);

        commit(&wt, "kernel/src/lib.rs", "// work\n", "kernel: work");
        let verdict = judge(&wt, "main").expect("a branch outside the five must pass");
        assert!(verdict.contains("changes none of the five"), "{verdict}");
    }

    /// The arithmetic, and the versions it refuses to guess at.
    #[test]
    fn the_bump_is_the_minor_and_nothing_else() {
        assert_eq!(next_minor("0.1.0").as_deref(), Some("0.2.0"));
        assert_eq!(next_minor("0.9.0").as_deref(), Some("0.10.0"));
        assert_eq!(next_minor("1.4.0").as_deref(), Some("1.5.0"));
        assert_eq!(next_minor("0.1.1"), None);
        assert_eq!(next_minor("0.1"), None);
        assert_eq!(next_minor("0.1.0.0"), None);
    }

    /// The two hand walks, over the spellings these manifests use.
    #[test]
    fn the_manifest_walk_reads_the_package_table_and_the_pins() {
        let text = manifest("toyos-window", "0.3.0", &[("toyos", "0.2.0"), ("toyos-abi", "0.1.0")]);
        assert_eq!(package_version(&text).as_deref(), Some("0.3.0"));
        assert_eq!(
            version_pins(&text),
            [("toyos".to_string(), "0.2.0".to_string()),
             ("toyos-abi".to_string(), "0.1.0".to_string())]
        );

        // A `version` under another table is not the package's, and a
        // dependency that is not one of the five is not a pin this rule holds.
        let other = "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
                     serde = { version = \"1.0\" }\n\n[lib]\nversion = \"9.9.9\"\n";
        assert_eq!(package_version(other).as_deref(), Some("0.1.0"));
        assert!(version_pins(other).is_empty());
    }

    /// **The five are the five**, and the order is the one a publisher must
    /// take: nothing names a crate that comes after it.
    #[test]
    fn the_publish_order_is_a_dependency_order() {
        for (n, krate) in PUBLISHED.iter().enumerate() {
            for dep in krate.depends_on {
                let at = PUBLISHED.iter().position(|k| k.name == *dep);
                assert!(
                    at.is_some_and(|at| at < n),
                    "{} names {dep}, which is not published before it",
                    krate.name
                );
            }
        }
    }

    /// Each row names a directory the tree holds whose package is the row's
    /// name, so a crate renamed or moved reds here and not in a publish run.
    #[test]
    fn every_row_names_a_crate_the_tree_holds() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert!(
            root.join(PUBLISHER).is_file(),
            "{PUBLISHER} is what puts these on crates.io, and the rule reads its presence on the \
             base to know a version can be taken at all"
        );
        for krate in PUBLISHED {
            let manifest = root.join(krate.dir).join("Cargo.toml");
            let text = std::fs::read_to_string(&manifest)
                .unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
            assert!(
                text.contains(&format!("name = \"{}\"", krate.name)),
                "{} does not name the package {}",
                manifest.display(),
                krate.name
            );
            assert!(
                package_version(&text).is_some(),
                "{} declares no version",
                manifest.display()
            );
            for field in ["description", "repository", "license"] {
                assert!(
                    text.contains(&format!("{field} = ")),
                    "{} carries no {field}, which crates.io asks for",
                    manifest.display()
                );
            }
        }
    }
}
