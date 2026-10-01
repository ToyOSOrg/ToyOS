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
//! not find (`src/x/mod.rs` beside `src/x.rs`), or one a build script read
//! without naming it, is still its package's. What a build reads outside its
//! own package without telling cargo, a warm run trusts as cargo's own
//! incremental build does:
//! `issues/build/a-warm-host-run-trusts-cargo-for-what-a-build-reads-outside-its-package.md`.
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
//! **The driver is built in [`DRIVER`]**: cargo builds it before this runs, so
//! its path crates are compiled again every time, and in the steps' target that
//! would make every crate depending on them stale.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub const MANIFEST: &str = "target/ci-sources";
pub const DRIVER: &str = "target/ci-driver";
/// A sealed entry's key is this, `${{ runner.os }}-${{ runner.arch }}-${{ github.run_id }}`.
pub const SEALED: &str = "host-sealed-";

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

/// Before the first step: the entry restored here, read by content.
pub fn read(root: &Path) -> Result<(Start, String), String> {
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("the driver's own path: {e}"))?;
    let driver = fs::canonicalize(root.join(DRIVER)).map_err(|e| format!("{DRIVER}: {e}"))?;
    if !exe.starts_with(&driver) {
        return Err(format!(
            "the driver runs from {}: a workflow builds it with `cargo run --target-dir {DRIVER} \
             -- --ci <job>`",
            exe.display()
        ));
    }
    let runner = ["RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion"]
        .iter()
        .map(|name| std::env::var(name).map_err(|_| format!("{name} is unset: a hosted runner sets it")))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    open(root, &runner, SystemTime::now())
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
    let dirty: BTreeSet<Option<String>> = current
        .iter()
        .filter(|(path, hash)| entry.get(*path) != Some(*hash))
        .map(|(path, _)| path)
        .chain(entry.keys().filter(|path| !current.contains_key(*path)))
        .map(|path| package(path, &current))
        .collect();
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
             added, {removed} removed, in {} packages",
            current.len(),
            dirty.len()
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

/// After the last step of a run that started [`Start::Cold`]: every file under
/// every target dated [`built`], then the manifest.
pub fn seal(root: &Path, cold: &Cold) -> Result<String, String> {
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
    let (mut files, mut bytes) = (0u64, 0u64);
    for target in targets(root)? {
        age(&target, &mut files, &mut bytes)?;
    }
    let head = crate::sync::git(root, &["rev-parse", "HEAD"])?;
    let mut text = format!("{head}\n{}\n", cold.runner);
    for (path, hash) in &cold.sources {
        text.push_str(&format!("{hash} {path}\n"));
    }
    fs::write(root.join(MANIFEST), text).map_err(|e| format!("write {MANIFEST}: {e}"))?;
    Ok(format!("{} sources; {files} files, {} MiB, dated as built", cold.sources.len(), bytes >> 20))
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
fn age(dir: &Path, files: &mut u64, bytes: &mut u64) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let meta = entry.metadata().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if meta.is_dir() {
            age(&entry.path(), files, bytes)?;
        } else if meta.is_file() {
            date(&entry.path(), built())?;
            *files += 1;
            *bytes += meta.len();
        }
    }
    date(dir, built())
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
    /// older than the entry's build included, and a build script whose package
    /// lost a file it never named.
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
            ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"leaf\", \"count\", \"idle\"]\nresolver = \"2\"\n"),
            ("leaf/Cargo.toml", &crate_toml("leaf", "")),
            ("leaf/src/lib.rs", "pub fn word() -> &'static str { \"one\" }\n"),
            ("count/Cargo.toml", &crate_toml("count", "")),
            ("count/build.rs", script),
            ("count/src/lib.rs", "pub const FILES: &str = env!(\"FILES\");\n"),
            ("count/src/spare.txt", "read by nothing but the count\n"),
            ("idle/Cargo.toml", &crate_toml("idle", "")),
            ("idle/src/lib.rs", "pub fn idle() {}\n"),
            ("app/Cargo.toml", &crate_toml("app", deps)),
            ("app/src/main.rs", "fn main() { print!(\"{} {}\", leaf::word(), count::FILES); }\n"),
        ]);
        let writer = tmp.join("writer");
        entry(&source, &writer);
        assert_eq!(run(&writer), "one 2");

        write(&source, "leaf/src/lib.rs", "pub fn word() -> &'static str { \"two\" }\n");
        sh(&source, &["rm", "-q", "count/src/spare.txt"]);
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
        let expected = [("app", false), ("count", false), ("idle", true), ("leaf", false)];
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

    /// One commit of `a.rs`, read cold.
    fn cold_repo(tmp: &Path) -> Cold {
        sh(tmp, &["init", "-q"]);
        configure(tmp);
        write(tmp, "a.rs", "a\n");
        sh(tmp, &["add", "-A"]);
        sh(tmp, &["commit", "-qm", "a"]);
        let Start::Cold(cold) = open(tmp, RUNNER, SystemTime::now()).unwrap().0 else { panic!("cold") };
        cold
    }

    /// Targets with no manifest beside them were built from nothing anyone can
    /// name; the driver's own is this job's.
    #[test]
    fn targets_restored_without_a_manifest_are_refused() {
        let tmp = TempDir::new("cicache-foreign");
        cold_repo(&tmp);
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
        let cold = cold_repo(&tmp);
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

    /// Another image's entry is no entry: its targets go and the run is cold.
    #[test]
    fn an_entry_built_on_another_runner_is_deleted() {
        let tmp = TempDir::new("cicache-image");
        let cold = cold_repo(&tmp);
        write(&tmp, "target/debug/x", "x");
        write(&tmp, "kernel/target/y", "y");
        seal(&tmp, &cold).unwrap();
        let (start, said) = open(&tmp, "macOS ARM64 macos15 20261005.1", SystemTime::now()).unwrap();
        assert!(matches!(start, Start::Cold(_)), "{said}");
        assert!(!tmp.join("target/debug").exists() && !tmp.join("kernel/target").exists(), "{said}");
    }

    /// A source dated now is newer than the entry only on a clock that reads
    /// after [`built`].
    #[test]
    fn a_reader_whose_clock_is_not_after_the_entry_is_refused() {
        let tmp = TempDir::new("cicache-clock");
        let cold = cold_repo(&tmp);
        write(&tmp, "target/debug/x", "x");
        seal(&tmp, &cold).unwrap();
        let refusal = open(&tmp, RUNNER, built()).err().expect("a clock at the entry's date");
        assert!(refusal.contains("clock"), "{refusal}");
    }

    #[test]
    fn a_driver_built_in_any_other_target_is_refused() {
        let tmp = TempDir::new("cicache-driver");
        fs::create_dir_all(tmp.join(DRIVER)).unwrap();
        let refusal = read(&tmp).err().expect("a test binary is no driver");
        assert!(refusal.contains("the driver runs from"), "{refusal}");
    }
}
