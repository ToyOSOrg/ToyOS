//! The host job's cache entry is trusted by content, never by mtime.
//!
//! Cargo calls a path crate fresh when none of its sources is newer than the
//! build that read it, and a checkout dates every source at the checkout: a
//! restored target is stale in every path crate, and a date made up for a
//! source from anything but its bytes could as well call a changed one fresh.
//! An entry instead carries [`MANIFEST`], the runner it was built on and the
//! SHA-256 of every tracked file, and every file under its targets is dated
//! [`built`], before any real time. [`read`] dates each tracked file whose hash
//! matches the same, and every other one now: cargo calls a source stale only
//! when it is strictly newer than the build, so a match is fresh, and a changed
//! or new source is newer than everything in the entry.
//!
//! **A package with a file changed, added or removed has every file dated
//! now.** Cargo is told only what a build read; a file rustc probed for and did
//! not find (`src/x/mod.rs` beside `src/x.rs`) is still its package's. **So
//! has every package with a build script or a proc macro, on every read**:
//! what code run at build time reads, cargo knows only if that code says so.
//!
//! **An entry built on another runner image is deleted, and the run is cold**:
//! the image's linker and C compiler made its units, and cargo's fingerprint
//! names neither. The cache key cannot carry the image, because no workflow
//! expression sees `ImageOS` or `ImageVersion`.
//!
//! **Only a run that restored nothing seals an entry** ([`Start::Cold`],
//! [`seal`]): a warm run's targets hold units none of its steps rebuilt,
//! compiled from sources no manifest it could write describes. [`read`] deletes
//! the manifest it reads, so a warm tree carries none and is never saved, and
//! targets restored without one are refused.
//!
//! **A job carries the cache when its workflow builds the driver in
//! [`DRIVER`]** ([`carried`]): cargo builds the driver before this runs, so its
//! path crates are compiled again every time, and in the steps' target that
//! would make every crate depending on them stale. A job that builds it
//! anywhere else runs as a developer's tree does.
//!
//! **Every run that carries the cache refuses, after its last step, a tree
//! whose [`PATHS`] hold more than [`LIMIT`]**, `~` being `HOME` ([`close`]): a
//! cold one before it seals, so the save, which follows only a green run,
//! never stores it, and a warm one so that a pull request whose tree holds
//! more reds on its own run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const MANIFEST: &str = "target/ci-sources";
pub const DRIVER: &str = "target/ci-driver";
pub const SEALED: &str = "host-sealed-";

/// What every step that names the host cache archives: the cache's version is
/// computed from the list, so a restore whose list differs finds nothing.
pub const PATHS: [&str; 8] = [
    "~/.cargo/registry/index",
    "~/.cargo/registry/cache",
    "~/.cargo/git/db",
    "target",
    "userland/target",
    "toyos/target",
    "kernel/target",
    "bootloader/target",
];

/// The most the files under [`PATHS`] may hold, uncompressed. The repository's
/// caches are evicted by last access past 10 GB, and a night whose guest jobs
/// restore after this entry is saved holds two host entries beside a guest
/// one, then one beside two guest ones.
const LIMIT: u64 = 8_000_000_000;

/// 2001-09-09T01:46:40Z: older than any build, so a file dated so is never
/// newer than one.
fn built() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

/// Every file git tracks, with the SHA-256 of its bytes on disk.
type Sources = BTreeMap<String, String>;

/// What a job found before its first step.
pub enum Start {
    /// An entry, read and its manifest consumed.
    Warm,
    /// No entry this runner can use: every source is dated [`built`].
    Cold(Cold),
}

/// What [`seal`] holds a cold run's tree to at the end.
pub struct Cold {
    runner: String,
    sources: Sources,
}

/// Whether the driver at `exe` was built in [`DRIVER`], which is how a
/// workflow says its job carries the cache.
pub fn carried(root: &Path, exe: &Path) -> bool {
    let exe = fs::canonicalize(exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    match fs::canonicalize(root.join(DRIVER)) {
        Ok(driver) => exe.starts_with(driver),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => panic!("{DRIVER}: {e}"),
    }
}

/// Before the first step of a job that carries the cache: the entry restored
/// here, read by content.
pub fn read(root: &Path) -> Result<(Start, String), String> {
    open(root, &runner(|name| std::env::var(name).ok())?, SystemTime::now())
}

/// The runner, as the variables a hosted runner sets name it: its OS, its
/// architecture and its image.
fn runner(var: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let values = ["RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion"]
        .iter()
        .map(|name| var(name).ok_or_else(|| format!("{name} is unset: a hosted runner sets it")));
    Ok(values.collect::<Result<Vec<_>, _>>()?.join(" "))
}

fn open(root: &Path, runner: &str, now: SystemTime) -> Result<(Start, String), String> {
    let current = sources(root)?;
    let manifest = root.join(MANIFEST);
    let text = match fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let restored = restored(root)?;
            if !restored.is_empty() {
                return Err(format!(
                    "{} restored without {MANIFEST}, so nothing says what they were built from",
                    restored.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                ));
            }
            return cold(root, runner, current, "none restored".into());
        }
        Err(e) => return Err(format!("read {MANIFEST}: {e}")),
    };
    fs::remove_file(&manifest).map_err(|e| format!("remove {MANIFEST}: {e}"))?;
    let (commit, built_on, entry) = parse(&text)?;
    if built_on != runner {
        for path in restored(root)? {
            let gone = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            gone.map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        return cold(root, runner, current, format!("{commit}'s entry, built on {built_on}, deleted"));
    }
    if now <= built() {
        return Err(format!("this runner's clock reads {now:?}, which no change dated now is newer than"));
    }
    let mut dirty: BTreeSet<Option<String>> = current
        .iter()
        .filter(|(path, hash)| entry.get(*path) != Some(*hash))
        .map(|(path, _)| path)
        .chain(entry.keys().filter(|path| !current.contains_key(*path)))
        .map(|path| package(path, &current))
        .collect();
    let packages = dirty.len();
    let mut build_time = 0;
    for manifest in current.keys().filter(|path| Path::new(path).ends_with("Cargo.toml")) {
        if runs_at_build(root, manifest, &current)? {
            build_time += 1;
            dirty.insert(Some(manifest.clone()));
        }
    }
    let mut same = 0;
    for (path, hash) in &current {
        let fresh = entry.get(path) == Some(hash) && !dirty.contains(&package(path, &current));
        same += usize::from(fresh);
        date(&root.join(path), if fresh { built() } else { now })?;
    }
    let changed = current.iter().filter(|(p, h)| entry.get(*p).is_some_and(|e| e != *h)).count();
    let added = current.keys().filter(|p| !entry.contains_key(*p)).count();
    let removed = entry.keys().filter(|p| !current.contains_key(*p)).count();
    Ok((
        Start::Warm,
        format!(
            "built from {commit}: {same} of {} sources dated as built; {changed} changed, {added} \
             added, {removed} removed, in {packages} packages; {build_time} packages run code at \
             build time",
            current.len(),
        ),
    ))
}

fn cold(root: &Path, runner: &str, sources: Sources, why: String) -> Result<(Start, String), String> {
    for path in sources.keys() {
        date(&root.join(path), built())?;
    }
    let said = format!("{why}; {} sources dated as built, and the tree is sealed last", sources.len());
    Ok((Start::Cold(Cold { runner: runner.to_string(), sources }), said))
}

/// After the last step of a job that carries the cache: what its save would
/// store, refused above [`LIMIT`], and a tree that started cold sealed.
pub fn close(root: &Path, start: &Start) -> Result<String, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unset, and the cache's paths name it as `~`")?;
    close_at(root, Path::new(&home), start)
}

fn close_at(root: &Path, home: &Path, start: &Start) -> Result<String, String> {
    let (mut files, mut bytes) = (0u64, 0u64);
    for path in PATHS {
        let dir = match path.strip_prefix("~/") {
            Some(under) => home.join(under),
            None => root.join(path),
        };
        size(&dir, &mut files, &mut bytes)?;
    }
    if bytes > LIMIT {
        return Err(format!(
            "{bytes} B in {files} files under the cache's paths, above the {LIMIT} B an entry may \
             hold: saved, it could evict the guest entry or the next host one"
        ));
    }
    let stored = format!("{files} files, {bytes} B of the {LIMIT} B an entry may hold");
    match start {
        Start::Warm => Ok(format!("{stored}; started warm, so not sealed")),
        Start::Cold(cold) => Ok(format!("{stored}; {}", seal(root, cold)?)),
    }
}

/// After the last step of a run that started [`Start::Cold`], once [`close`]
/// has bounded its tree: every file under every target dated [`built`], then
/// the manifest.
fn seal(root: &Path, cold: &Cold) -> Result<String, String> {
    let now = sources(root)?;
    let moved: Vec<&String> = cold
        .sources
        .iter()
        .filter(|(path, hash)| {
            now.get(*path) != Some(*hash) || modified(&root.join(path)).ok() != Some(built())
        })
        .map(|(path, _)| path)
        .chain(now.keys().filter(|path| !cold.sources.contains_key(*path)))
        .collect();
    if !moved.is_empty() {
        return Err(format!("a step wrote tracked sources, which no entry can describe: {moved:?}"));
    }
    for target in targets(root)? {
        age(&target)?;
    }
    let head = crate::sync::git(root, &["rev-parse", "HEAD"])?;
    let mut text = format!("{head}\n{}\n", cold.runner);
    for (path, hash) in &cold.sources {
        text.push_str(&format!("{hash} {path}\n"));
    }
    fs::write(root.join(MANIFEST), text).map_err(|e| format!("write {MANIFEST}: {e}"))?;
    Ok(format!("sealed: {} sources, built on {}, every target dated as built", cold.sources.len(), cold.runner))
}

/// The commit an entry was built from, the runner, and its sources.
fn parse(text: &str) -> Result<(String, String, Sources), String> {
    let mut lines = text.lines();
    let (Some(commit), Some(runner)) = (lines.next(), lines.next()) else {
        return Err(format!("{MANIFEST} ends before its commit and runner"));
    };
    let entry = lines
        .map(|line| {
            line.split_once(' ')
                .filter(|(hash, _)| !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|(hash, path)| (path.to_string(), hash.to_string()))
                .ok_or_else(|| format!("{MANIFEST} holds {line:?}"))
        })
        .collect::<Result<_, _>>()?;
    Ok((commit.to_string(), runner.to_string(), entry))
}

/// Whether the package of `manifest` has a build script or is a proc macro, in
/// every spelling cargo accepts.
fn runs_at_build(root: &Path, manifest: &str, current: &Sources) -> Result<bool, String> {
    let text = fs::read_to_string(root.join(manifest)).map_err(|e| format!("read {manifest}: {e}"))?;
    let doc: toml::Value = text.parse().map_err(|e| format!("{manifest}: {e}"))?;
    let lib = |key: &str, alias: &str| doc.get("lib").and_then(|lib| lib.get(key).or_else(|| lib.get(alias)));
    let package = doc.get("package").or_else(|| doc.get("project"));
    let script = match package.and_then(|package| package.get("build")) {
        None => current.contains_key(&*Path::new(manifest).with_file_name("build.rs").to_string_lossy()),
        Some(build) => build.as_bool() != Some(false),
    };
    let proc_macro = lib("proc-macro", "proc_macro").and_then(toml::Value::as_bool) == Some(true)
        || lib("crate-type", "crate_type")
            .and_then(toml::Value::as_array)
            .is_some_and(|types| types.iter().any(|t| t.as_str() == Some("proc-macro")));
    Ok(script || proc_macro)
}

/// The `Cargo.toml` of the package `path` is in: the nearest one above it.
fn package(path: &str, current: &Sources) -> Option<String> {
    Path::new(path)
        .ancestors()
        .skip(1)
        .map(|dir| dir.join("Cargo.toml").to_string_lossy().into_owned())
        .find(|manifest| current.contains_key(manifest))
}

fn sources(root: &Path) -> Result<Sources, String> {
    let mut sources = Sources::new();
    for path in crate::sysroot::tracked_files(root, &[])? {
        let file = root.join(&path);
        // A gitlink names a commit, not bytes: what is under it keeps its
        // checkout's date, which no entry is newer than.
        if file.is_dir() {
            continue;
        }
        let bytes = fs::read(&file).map_err(|e| format!("read {path}: {e}"))?;
        sources.insert(path, format!("{:x}", Sha256::digest(bytes)));
    }
    Ok(sources)
}

/// Every `target` directory under `root`, none inside another.
fn targets(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let kind = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if !kind.is_dir() || entry.file_name() == ".git" {
                continue;
            }
            if entry.file_name() == "target" {
                found.push(entry.path());
            } else {
                walk(&entry.path(), found)?;
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(root, &mut found)?;
    Ok(found)
}

/// What a restore put here: every target but the root's, and in the root's
/// everything but [`DRIVER`], which this job's own `cargo run` made.
fn restored(root: &Path) -> Result<Vec<PathBuf>, String> {
    let ours = root.join("target");
    let mut found = Vec::new();
    for target in targets(root)? {
        if target != ours {
            found.push(target);
            continue;
        }
        for entry in fs::read_dir(&target).map_err(|e| format!("read {}: {e}", target.display()))? {
            let path = entry.map_err(|e| format!("read {}: {e}", target.display()))?.path();
            if path != root.join(DRIVER) {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// Date everything under `dir`, and `dir`, as [`built`]. A symbolic link is
/// left alone: dating one dates whatever it names.
fn age(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let meta = entry.metadata().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if meta.is_dir() {
            age(&entry.path())?;
        } else if meta.is_file() {
            date(&entry.path(), built())?;
        }
    }
    date(dir, built())
}

/// Count the files under `dir` and their bytes as an archive of it holds them:
/// a symbolic link is stored as a link, and a path that does not exist as
/// nothing.
fn size(dir: &Path, files: &mut u64, bytes: &mut u64) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        entries => entries.map_err(|e| format!("read {}: {e}", dir.display()))?,
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let meta = entry.metadata().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if meta.is_dir() {
            size(&entry.path(), files, bytes)?;
        } else if meta.is_file() {
            *files += 1;
            *bytes += meta.len();
        }
    }
    Ok(())
}

fn date(path: &Path, when: SystemTime) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .map_err(|e| format!("date {}: {e}", path.display()))
}

fn modified(path: &Path) -> std::io::Result<SystemTime> {
    fs::metadata(path)?.modified()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitfixture::{configure, sh};
    use std::process::{Command, Output};
    use toyos_tmpdir::TempDir;

    const RUNNER: &str = "macOS ARM64 macos15 20260928.1";

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// `files` committed to a new repository at `dir`.
    fn origin(dir: &Path, files: &[(&str, &str)]) {
        fs::create_dir_all(dir).unwrap();
        sh(dir, &["init", "-q", "-b", "main"]);
        configure(dir);
        for (path, text) in files {
            write(dir, path, text);
        }
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-qm", "files"]);
    }

    fn crate_toml(name: &str, deps: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}")
    }

    fn cargo_build(root: &Path) -> Output {
        Command::new("cargo")
            .args(["build", "--offline", "--message-format=json"])
            .current_dir(root)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .expect("run cargo")
    }

    /// `cargo build` in `root`, and each workspace crate with whether cargo
    /// called it fresh.
    fn build(root: &Path) -> BTreeMap<String, bool> {
        let out = cargo_build(root);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let mut fresh = BTreeMap::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let doc: serde_json::Value = serde_json::from_str(line).unwrap();
            if doc["reason"] == "compiler-artifact" && doc["target"]["kind"][0] != "custom-build" {
                let name = doc["target"]["name"].as_str().unwrap().to_string();
                fresh.insert(name, doc["fresh"].as_bool().unwrap());
            }
        }
        fresh
    }

    fn run(root: &Path) -> String {
        let out = Command::new(root.join("target/debug/app")).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// A checkout of `origin` at its head, as a runner makes one.
    fn checkout(origin: &Path, dir: &Path) {
        let (from, to) = (origin.to_str().unwrap(), dir.to_str().unwrap());
        sh(origin.parent().unwrap(), &["clone", "-q", from, to]);
        configure(dir);
    }

    /// An entry at `writer`: a checkout of `origin` built cold and sealed.
    fn entry(origin: &Path, writer: &Path) {
        checkout(origin, writer);
        let Start::Cold(cold) = open(writer, RUNNER, SystemTime::now()).unwrap().0 else {
            panic!("a tree with no target is cold")
        };
        build(writer);
        seal(writer, &cold).unwrap();
    }

    /// The entry at `writer`, restored under a fresh checkout of `origin`.
    fn restore(origin: &Path, writer: &Path, reader: &Path) {
        checkout(origin, reader);
        fs::rename(writer.join("target"), reader.join("target")).unwrap();
    }

    /// The oracle is cargo and the program it builds: an entry serves what
    /// a cold build of the reader's tree would, recompiling exactly what
    /// changed and what depends on it — a changed source the checkout dated
    /// older than the entry's build included, a build script whose package
    /// lost a file it never named, and a package that lost a file nothing read.
    #[test]
    fn an_entry_serves_exactly_the_sources_it_was_built_from() {
        let tmp = TempDir::new("cicache-entry");
        let script = r#"fn main() {
    let files = std::fs::read_dir("src").unwrap().count();
    println!("cargo:rustc-env=FILES={files}");
}
"#;
        let deps = "leaf = { path = \"../leaf\" }\ncount = { path = \"../count\" }\n";
        let source = tmp.join("origin");
        origin(&source, &[
            (".gitignore", "target/\n"),
            ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"leaf\", \"count\", \"idle\", \"gone\"]\nresolver = \"2\"\n"),
            ("leaf/Cargo.toml", &crate_toml("leaf", "")),
            ("leaf/src/lib.rs", "pub fn word() -> &'static str { \"one\" }\n"),
            ("count/Cargo.toml", &crate_toml("count", "")),
            ("count/build.rs", script),
            ("count/src/lib.rs", "pub const FILES: &str = env!(\"FILES\");\n"),
            ("count/src/spare.txt", "read by nothing but the count\n"),
            ("idle/Cargo.toml", &crate_toml("idle", "")),
            ("idle/src/lib.rs", "pub fn idle() {}\n"),
            ("gone/Cargo.toml", &crate_toml("gone", "")),
            ("gone/src/lib.rs", "pub fn gone() {}\n"),
            ("gone/notes.txt", "read by nothing\n"),
            ("app/Cargo.toml", &crate_toml("app", deps)),
            ("app/src/main.rs", "fn main() { print!(\"{} {}\", leaf::word(), count::FILES); }\n"),
        ]);
        let writer = tmp.join("writer");
        entry(&source, &writer);
        assert_eq!(run(&writer), "one 2");

        write(&source, "leaf/src/lib.rs", "pub fn word() -> &'static str { \"two\" }\n");
        sh(&source, &["rm", "-q", "count/src/spare.txt", "gone/notes.txt"]);
        sh(&source, &["commit", "-qam", "read"]);
        let reader = tmp.join("reader");
        restore(&source, &writer, &reader);
        // What an mtime made up from history would say of a file committed
        // before the entry was built.
        date(&reader.join("leaf/src/lib.rs"), UNIX_EPOCH + Duration::from_secs(1)).unwrap();

        let said = open(&reader, RUNNER, SystemTime::now()).unwrap();
        assert!(matches!(said.0, Start::Warm), "{}", said.1);
        assert!(!reader.join(MANIFEST).exists(), "a warm tree keeps no manifest");
        let fresh = build(&reader);
        assert_eq!(run(&reader), "two 1");
        let expected = [("app", false), ("count", false), ("gone", false), ("idle", true), ("leaf", false)];
        assert_eq!(fresh, expected.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    }

    /// A file no dep-info names still decides a build: beside `src/x.rs`, a
    /// new `src/x/mod.rs` is E0761 to a cold build, and so to a warm one.
    #[test]
    fn a_file_cargo_was_never_told_about_rebuilds_its_package() {
        let tmp = TempDir::new("cicache-probe");
        let source = tmp.join("origin");
        origin(&source, &[
            (".gitignore", "target/\n"),
            ("Cargo.toml", "[workspace]\nmembers = [\"probed\"]\nresolver = \"2\"\n"),
            ("probed/Cargo.toml", &crate_toml("probed", "")),
            ("probed/src/lib.rs", "mod x;\npub fn f() -> u8 { x::X }\n"),
            ("probed/src/x.rs", "pub const X: u8 = 1;\n"),
        ]);
        let writer = tmp.join("writer");
        entry(&source, &writer);

        write(&source, "probed/src/x/mod.rs", "pub const X: u8 = 2;\n");
        sh(&source, &["add", "-A"]);
        sh(&source, &["commit", "-qm", "probed"]);
        let reader = tmp.join("reader");
        restore(&source, &writer, &reader);
        assert!(matches!(open(&reader, RUNNER, SystemTime::now()).unwrap().0, Start::Warm));
        let out = cargo_build(&reader);
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success() && said.contains("E0761"), "{said}");
    }

    /// A build script and a proc macro that read another package's file and
    /// never say so are run again on a read, as a cold build runs them: `app`'s
    /// script reads `word.txt`, and so does `mac`, expanded in `said`.
    #[test]
    fn code_run_at_build_time_runs_again_on_every_read() {
        let tmp = TempDir::new("cicache-buildtime");
        let script = "fn main() {
    let word = std::fs::read_to_string(\"../word.txt\").unwrap();
    println!(\"cargo:rustc-env=WORD={}\", word.trim());
}
";
        let mac = r#"#[proc_macro]
pub fn word(_: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let word = std::fs::read_to_string(format!("{dir}/../word.txt")).unwrap();
    format!("{:?}", word.trim()).parse().unwrap()
}
"#;
        let source = tmp.join("origin");
        origin(&source, &[
            (".gitignore", "target/\n"),
            ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"mac\", \"said\"]\nresolver = \"2\"\n"),
            ("word.txt", "one\n"),
            ("mac/Cargo.toml", &format!("{}\n[lib]\nproc-macro = true\n", crate_toml("mac", ""))),
            ("mac/src/lib.rs", mac),
            ("said/Cargo.toml", &crate_toml("said", "mac = { path = \"../mac\" }\n")),
            ("said/src/lib.rs", "pub const WORD: &str = mac::word!();\n"),
            ("app/Cargo.toml", &crate_toml("app", "said = { path = \"../said\" }\n")),
            ("app/build.rs", script),
            ("app/src/main.rs", "fn main() { print!(\"{} {}\", env!(\"WORD\"), said::WORD); }\n"),
        ]);
        let writer = tmp.join("writer");
        entry(&source, &writer);
        assert_eq!(run(&writer), "one one");

        write(&source, "word.txt", "two\n");
        sh(&source, &["commit", "-qam", "word"]);
        let reader = tmp.join("reader");
        restore(&source, &writer, &reader);
        assert!(matches!(open(&reader, RUNNER, SystemTime::now()).unwrap().0, Start::Warm));
        build(&reader);
        assert_eq!(run(&reader), "two two");
    }

    /// Each way a manifest can declare a build script or a proc macro.
    #[test]
    fn every_spelling_of_code_run_at_build_time_is_found() {
        let tmp = TempDir::new("cicache-spellings");
        let cases = [
            ("[package]", "", false, false),
            ("[package]", "", true, true),
            ("[package]", "build = false\n", true, false),
            ("[package]", "build = \"gen.rs\"\n", false, true),
            ("[project]", "build = \"gen.rs\"\n", false, true),
            ("[project]", "build = false\n", true, false),
            ("[package]", "[lib]\nproc-macro = true\n", false, true),
            ("[package]", "[lib]\nproc_macro = true\n", false, true),
            ("[package]", "[lib]\ncrate-type = [\"proc-macro\"]\n", false, true),
            ("[package]", "[lib]\ncrate_type = [\"proc-macro\"]\n", false, true),
            ("[package]", "[lib]\ncrate-type = [\"rlib\"]\n", false, false),
        ];
        for (table, keys, script, expected) in cases {
            write(&tmp, "Cargo.toml", &format!("{table}\nname = \"p\"\n{keys}"));
            let mut current = Sources::from([("Cargo.toml".to_string(), String::new())]);
            if script {
                current.insert("build.rs".into(), String::new());
            }
            assert_eq!(runs_at_build(&tmp, "Cargo.toml", &current), Ok(expected), "{table} {keys:?}, build.rs: {script}");
        }
    }

    /// What a save would store is the files under [`PATHS`], `~` being the
    /// home: one byte above the bound, between a target and the home's
    /// registry, is refused whether the run started warm or cold, and a cold
    /// tree refused gets no manifest; at the bound both close, and a target no
    /// save stores is not counted. The bound's bytes are a hole `set_len`
    /// made, so no test writes them.
    #[test]
    fn what_a_save_would_store_is_refused_above_the_bound_warm_or_cold() {
        let tmp = TempDir::new("cicache-bound");
        let home = tmp.join("home");
        let cold = Start::Cold(cold_repo(&tmp, RUNNER));
        write(&home, ".cargo/registry/cache/one", "1");
        write(&tmp, "tests/target/unsaved", "1");
        let big = tmp.join("target/debug/big");
        fs::create_dir_all(big.parent().unwrap()).unwrap();
        fs::File::create(&big).unwrap().set_len(LIMIT).unwrap();
        for start in [&Start::Warm, &cold] {
            let refusal = close_at(&tmp, &home, start).expect_err("one byte above the bound");
            assert!(refusal.starts_with(&format!("{} B", LIMIT + 1)), "{refusal}");
        }
        assert!(!tmp.join(MANIFEST).exists(), "a refused tree carries a manifest");
        fs::remove_file(home.join(".cargo/registry/cache/one")).unwrap();
        for start in [&Start::Warm, &cold] {
            close_at(&tmp, &home, start).expect("at the bound");
        }
        assert!(tmp.join(MANIFEST).exists(), "a tree at the bound is sealed");
    }

    /// One commit of `a.rs`, read cold on `runner`.
    fn cold_repo(tmp: &Path, runner: &str) -> Cold {
        sh(tmp, &["init", "-q"]);
        configure(tmp);
        write(tmp, "a.rs", "a\n");
        sh(tmp, &["add", "-A"]);
        sh(tmp, &["commit", "-qm", "a"]);
        let Start::Cold(cold) = open(tmp, runner, SystemTime::now()).unwrap().0 else { panic!("cold") };
        cold
    }

    /// Targets with no manifest beside them were built from nothing anyone can
    /// name; the driver's own is this job's.
    #[test]
    fn targets_restored_without_a_manifest_are_refused() {
        let tmp = TempDir::new("cicache-foreign");
        cold_repo(&tmp, RUNNER);
        fs::create_dir_all(tmp.join(DRIVER)).unwrap();
        assert!(matches!(open(&tmp, RUNNER, SystemTime::now()).unwrap().0, Start::Cold(_)));
        for target in ["kernel/target", "target/debug"] {
            fs::create_dir_all(tmp.join(target)).unwrap();
            let refusal = open(&tmp, RUNNER, SystemTime::now()).err().expect("a target with no manifest");
            assert!(refusal.contains(target), "{refusal}");
            fs::remove_dir(tmp.join(target)).unwrap();
        }
    }

    /// A sealed tree is dated as built throughout, and a step that wrote a
    /// tracked source is refused, even one that wrote its bytes back: the
    /// manifest would name bytes no build read.
    #[test]
    fn a_seal_dates_every_target_and_refuses_a_written_source() {
        let tmp = TempDir::new("cicache-seal");
        let cold = cold_repo(&tmp, RUNNER);
        write(&tmp, "target/debug/deps/x", "x");
        write(&tmp, "userland/target/y", "y");
        seal(&tmp, &cold).unwrap();
        for file in ["target/debug/deps/x", "target/debug", "userland/target/y"] {
            assert_eq!(modified(&tmp.join(file)).unwrap(), built(), "{file}");
        }
        fs::remove_file(tmp.join(MANIFEST)).unwrap();
        for bytes in ["b\n", "a\n"] {
            write(&tmp, "a.rs", bytes);
            let refusal = seal(&tmp, &cold).unwrap_err();
            assert!(refusal.contains("a.rs"), "{bytes:?}: {refusal}");
        }
    }

    /// The runner [`read`] names in an environment holding `vars`.
    fn runner_in(vars: &BTreeMap<&str, &str>) -> Result<String, String> {
        runner(|name| vars.get(name).map(|value| value.to_string()))
    }

    /// Another runner's entry is no entry: its targets go and the run is cold,
    /// whichever variable that names a runner differs, and a runner without
    /// one of them names none.
    #[test]
    fn an_entry_built_on_another_runner_is_deleted() {
        let image = BTreeMap::from([
            ("RUNNER_OS", "macOS"),
            ("RUNNER_ARCH", "ARM64"),
            ("ImageOS", "macos15"),
            ("ImageVersion", "20260928.1"),
        ]);
        for name in image.keys() {
            let tmp = TempDir::new("cicache-image");
            let cold = cold_repo(&tmp, &runner_in(&image).unwrap());
            write(&tmp, "target/debug/x", "x");
            write(&tmp, "kernel/target/y", "y");
            seal(&tmp, &cold).unwrap();
            let mut other = image.clone();
            other.insert(name, "another");
            let (start, said) = open(&tmp, &runner_in(&other).unwrap(), SystemTime::now()).unwrap();
            assert!(matches!(start, Start::Cold(_)), "{name}: {said}");
            assert!(!tmp.join("target/debug").exists() && !tmp.join("kernel/target").exists(), "{name}: {said}");
            other.remove(name);
            let refusal = runner_in(&other).expect_err("a runner without a variable");
            assert!(refusal.contains(name), "{refusal}");
        }
    }

    /// A source dated now is newer than the entry only on a clock that reads
    /// after [`built`].
    #[test]
    fn a_reader_whose_clock_is_not_after_the_entry_is_refused() {
        let tmp = TempDir::new("cicache-clock");
        let cold = cold_repo(&tmp, RUNNER);
        write(&tmp, "target/debug/x", "x");
        seal(&tmp, &cold).unwrap();
        let refusal = open(&tmp, RUNNER, built()).err().expect("a clock at the entry's date");
        assert!(refusal.contains("clock"), "{refusal}");
    }

    #[test]
    fn only_a_driver_built_in_its_own_target_carries_the_cache() {
        let tmp = TempDir::new("cicache-driver");
        let (ours, other) = (format!("{DRIVER}/debug/toyos-build"), "target/debug/toyos-build");
        write(&tmp, other, "");
        assert!(!carried(&tmp, &tmp.join(other)), "a tree with no {DRIVER}");
        write(&tmp, &ours, "");
        assert!(carried(&tmp, &tmp.join(&ours)), "{ours}");
        assert!(!carried(&tmp, &tmp.join(other)), "{other}");
    }
}
