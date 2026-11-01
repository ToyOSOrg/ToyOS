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
//! **Only the writer seals**: the job that saves the entry restores nothing,
//! starts [`cold`] and [`seal`]s its tree after the last step. A reader's run
//! is never saved and never sealed. [`read`] deletes the manifest it reads,
//! since a warm tree holds units none of its steps rebuilt, and targets
//! restored without one are refused.
//!
//! **A job carries the cache when its workflow builds the driver in
//! [`DRIVER`]** ([`carried`]): cargo builds the driver before this runs, so its
//! path crates are compiled again every time, and in the steps' target that
//! would make every crate depending on them stale. A job that builds it
//! anywhere else runs as a developer's tree does.
//!
//! **The entry is held to what GitHub stores of it, after the save**
//! ([`stored`]): twice its stored bytes, since the entry before it stays until
//! evicted, and main's largest entry of each toolchain layer fit in
//! [`REPOSITORY_LIMIT`]. An entry past it is still saved and read; the run is
//! red.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST: &str = "target/ci-sources";
pub const DRIVER: &str = "target/ci-driver";

/// What every step that names the host cache archives: the cache's version is
/// computed from the list, so a restore whose list differs finds nothing.
const PATHS: [&str; 4] = [
    "~/.cargo/registry/index",
    "~/.cargo/registry/cache",
    "~/.cargo/git/db",
    "target",
];

/// GitHub's default limit on a repository's caches, in the bytes its cache list
/// says each entry stores, read as decimal gigabytes: past it, entries are
/// evicted in order of last access (GitHub Docs, "Dependency caching
/// reference", "Usage limits and eviction policy").
const REPOSITORY_LIMIT: u64 = 10_000_000_000;

/// The scope of main's entries, which every ref restores from.
const MAIN: &str = "refs/heads/main";

/// 2001-09-09T01:46:40Z: older than any build, so a file dated so is never
/// newer than one.
fn built() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

/// Every file git tracks, with the SHA-256 of its bytes on disk.
type Sources = BTreeMap<String, String>;

/// What [`seal`] holds the writer's tree to at the end.
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

/// Before the first step of a job that reads the cache: the entry restored
/// here, read by content.
pub fn read(root: &Path) -> Result<String, String> {
    open(root, &runner(|name| std::env::var(name).ok())?, SystemTime::now())
}

/// Before the first step of the writer's run: a tree that holds nothing but
/// its checkout and the driver, with every source dated [`built`], so that the
/// seal sees a step that writes one.
pub fn cold(root: &Path) -> Result<(Cold, String), String> {
    if !carried(root, &std::env::current_exe().map_err(|e| format!("the driver's own path: {e}"))?) {
        return Err(format!("only a job whose driver is built in {DRIVER} seals its tree"));
    }
    start(root, &runner(|name| std::env::var(name).ok())?)
}

/// The runner, as the variables a hosted runner sets name it: its OS, its
/// architecture and its image.
fn runner(var: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let values = ["RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion"]
        .iter()
        .map(|name| var(name).ok_or_else(|| format!("{name} is unset: a hosted runner sets it")));
    Ok(values.collect::<Result<Vec<_>, _>>()?.join(" "))
}

fn open(root: &Path, runner: &str, now: SystemTime) -> Result<String, String> {
    let manifest = root.join(MANIFEST);
    let text = match fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let restored = restored(root)?;
            if !restored.is_empty() {
                return Err(format!(
                    "{} restored without {MANIFEST}, so nothing says what they were built from",
                    listed(&restored)
                ));
            }
            return Ok("none restored: the run is cold".into());
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
        return Ok(format!("{commit}'s entry, built on {built_on}, deleted: the run is cold"));
    }
    if now <= built() {
        return Err(format!("this runner's clock reads {now:?}, which no change dated now is newer than"));
    }
    let current = sources(root)?;
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
    Ok(format!(
        "built from {commit}: {same} of {} sources dated as built; {changed} changed, {added} added, \
         {removed} removed, in {packages} packages; {build_time} packages run code at build time",
        current.len(),
    ))
}

fn start(root: &Path, runner: &str) -> Result<(Cold, String), String> {
    let restored = restored(root)?;
    if !restored.is_empty() {
        return Err(format!("{}: an entry is one cold build, and its writer restores nothing", listed(&restored)));
    }
    let sources = sources(root)?;
    for path in sources.keys() {
        date(&root.join(path), built())?;
    }
    let said = format!("{} sources dated as built, and the tree is sealed last", sources.len());
    Ok((Cold { runner: runner.to_string(), sources }, said))
}

fn listed(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// After the last step of the writer's run, which [`cold`] began: every file
/// under every target dated [`built`], then the manifest.
pub fn seal(root: &Path, cold: &Cold) -> Result<String, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unset, and the cache's paths name it as `~`")?;
    seal_at(root, Path::new(&home), cold)
}

fn seal_at(root: &Path, home: &Path, cold: &Cold) -> Result<String, String> {
    let (files, bytes) = held(root, home)?;
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
        each_under(&target, &mut |path, _| date(path, built()))?;
        date(&target, built())?;
    }
    let head = crate::sysroot::git_out(root, &["rev-parse", "HEAD"]);
    let mut text = format!("{}\n{}\n", head.trim(), cold.runner);
    for (path, hash) in &cold.sources {
        text.push_str(&format!("{hash} {path}\n"));
    }
    fs::write(root.join(MANIFEST), text).map_err(|e| format!("write {MANIFEST}: {e}"))?;
    Ok(format!(
        "{files} files, {bytes} B, under the cache's paths; sealed: {} sources, built on {}, every target \
         dated as built",
        cold.sources.len(),
        cold.runner
    ))
}

/// The files under [`PATHS`], `~` being `home`, and their bytes, as the save
/// archives them: a file of several names once.
fn held(root: &Path, home: &Path) -> Result<(u64, u64), String> {
    let (mut files, mut bytes, mut linked) = (0, 0, BTreeSet::new());
    for path in PATHS {
        let dir = match path.strip_prefix("~/") {
            Some(under) => home.join(under),
            None => root.join(path),
        };
        // A path the save finds nothing at stores nothing.
        if !dir.try_exists().map_err(|e| format!("{}: {e}", dir.display()))? {
            continue;
        }
        each_under(&dir, &mut |_, meta| {
            if meta.is_file() && (meta.nlink() == 1 || linked.insert((meta.dev(), meta.ino()))) {
                files += 1;
                bytes += meta.len();
            }
            Ok(())
        })?;
    }
    Ok((files, bytes))
}

/// After the writer's save: the entry `ENTRY` names, held to [`budget`] as
/// the repository's cache list, read with `GH_TOKEN`, says main's caches store.
pub fn stored() -> Result<String, String> {
    let var = |name: &str| std::env::var(name).map_err(|_| format!("{name} is unset: nightly's `host` sets it"));
    let entry = var("ENTRY")?;
    let github = crate::release::Github::new(&var("GITHUB_REPOSITORY")?)?;
    let mut main = Vec::new();
    for page in 1.. {
        let url = github.api(&format!("actions/caches?ref={MAIN}&per_page=100&page={page}"));
        let answer = github.call("GET", &url, None)?.ok_or_else(|| format!("GET {url} found nothing"))?;
        let (total, entries) = cache_page(&answer)?;
        let last = entries.is_empty();
        main.extend(entries);
        if last || main.len() as u64 >= total {
            break;
        }
    }
    budget(&entry, &main)
}

/// One page of the cache list GitHub answers for [`MAIN`]: the entries it
/// counts in all, and each of these by its key and stored bytes.
fn cache_page(answer: &serde_json::Value) -> Result<(u64, Vec<(String, u64)>), String> {
    let total = answer["total_count"].as_u64().ok_or_else(|| format!("a cache list without a total: {answer}"))?;
    let entries = answer["actions_caches"].as_array().ok_or_else(|| format!("a cache list without entries: {answer}"))?;
    let entries = entries.iter().map(|entry| {
        match (entry["key"].as_str(), entry["ref"].as_str(), entry["size_in_bytes"].as_u64()) {
            (Some(key), Some(MAIN), Some(bytes)) => Ok((key.to_string(), bytes)),
            _ => Err(format!("the cache list of {MAIN} holds {entry}")),
        }
    });
    Ok((total, entries.collect::<Result<_, _>>()?))
}

/// Whether main's caches, each as its key and stored bytes, hold `entry` twice
/// beside the largest of each toolchain layer within [`REPOSITORY_LIMIT`]. The
/// largest stands for the layer main's runs restore; the rest of main's
/// entries, and every other scope's, are read by nothing, so evicted before
/// what every run reads.
fn budget(entry: &str, main: &[(String, u64)]) -> Result<String, String> {
    let (_, host) = main
        .iter()
        .find(|(key, _)| key == entry)
        .ok_or_else(|| format!("main's caches hold no {entry}: its save stored nothing"))?;
    let mut layers = 0u64;
    for prefix in crate::release::entry_prefixes() {
        let largest = main.iter().filter(|(key, _)| key.starts_with(&prefix)).map(|(_, bytes)| *bytes).max();
        let largest = largest.ok_or_else(|| format!("main's caches hold no {prefix}* entry, which every toolchain job restores"))?;
        layers = layers.saturating_add(largest);
    }
    let needed = host.saturating_mul(2).saturating_add(layers);
    let said = format!("{entry} stores {host} B; twice that and main's toolchain layers, {layers} B, need {needed} B");
    if needed > REPOSITORY_LIMIT {
        Err(format!("{said}, past the repository's {REPOSITORY_LIMIT} B: the next entry or a layer may be evicted"))
    } else {
        Ok(format!("{said} of the repository's {REPOSITORY_LIMIT} B"))
    }
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
        sources.insert(path, crate::sha256_hex(&bytes));
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

/// Hands `visit` every file and directory under `dir`, with its metadata. A
/// symbolic link is neither followed nor visited: dating one dates whatever it
/// names.
fn each_under(dir: &Path, visit: &mut impl FnMut(&Path, &fs::Metadata) -> Result<(), String>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let meta = entry.metadata().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if meta.is_dir() {
            each_under(&entry.path(), visit)?;
        }
        if meta.is_dir() || meta.is_file() {
            visit(&entry.path(), &meta)?;
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
        let (cold, _) = start(writer, RUNNER).unwrap();
        build(writer);
        seal_at(writer, &writer.join("home"), &cold).unwrap();
    }

    /// The entry at `writer`, restored under a fresh checkout of `origin`.
    fn restore(origin: &Path, writer: &Path, reader: &Path) {
        checkout(origin, reader);
        fs::rename(writer.join("target"), reader.join("target")).unwrap();
    }

    /// The entry restored at `reader`, read: a test of what a read serves holds
    /// nothing if the read went cold.
    fn read_warm(reader: &Path) {
        let said = open(reader, RUNNER, SystemTime::now()).unwrap();
        assert!(said.starts_with("built from"), "{said}");
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

        read_warm(&reader);
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
        read_warm(&reader);
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
        read_warm(&reader);
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

    /// What the save archives is the files under [`PATHS`], `~` being the home,
    /// a file of two names once, and nothing in a target no save stores.
    #[test]
    fn the_tree_counts_a_file_once_however_many_names_it_has() {
        let tmp = TempDir::new("cicache-held");
        let home = tmp.join("home");
        write(&home, ".cargo/registry/cache/one", "1");
        write(&tmp, "tests/target/unsaved", "1");
        write(&tmp, "target/debug/deps/app-0123", "0123456789");
        fs::hard_link(tmp.join("target/debug/deps/app-0123"), tmp.join("target/debug/app")).unwrap();
        assert_eq!(held(&tmp, &home), Ok((2, 11)));
    }

    /// Main's caches as the budget reads them: `entry` storing `host` bytes,
    /// the one before it, and each toolchain layer at `layer` bytes beside an
    /// older and smaller one, which no run restores.
    fn main_caches(entry: &str, host: u64, layer: u64) -> Vec<(String, u64)> {
        let mut main = vec![(entry.to_string(), host), ("host-sealed-Linux-X64-1".to_string(), host)];
        for prefix in crate::release::entry_prefixes() {
            main.push((format!("{prefix}1111111111111111"), layer - 1));
            main.push((format!("{prefix}2222222222222222"), layer));
        }
        main
    }

    /// Twice the entry and the largest of each layer may reach the limit and
    /// not pass it, whichever of them grows; an entry the cache list does not
    /// hold, and a layer main holds none of, are refused.
    #[test]
    fn the_saved_entry_is_held_to_the_repositorys_limit() {
        const ENTRY: &str = "host-sealed-Linux-X64-2";
        let layers = crate::release::entry_prefixes().count() as u64;
        let layer = 1_000_000_000;
        let host = (REPOSITORY_LIMIT - layers * layer) / 2;
        assert_eq!(2 * host + layers * layer, REPOSITORY_LIMIT, "the bound is reached exactly");
        let said = budget(ENTRY, &main_caches(ENTRY, host, layer)).expect("at the limit");
        assert!(said.contains(&format!("{ENTRY} stores {host} B")), "{said}");
        for (host, layer) in [(host + 1, layer), (host, layer + 1)] {
            let refusal = budget(ENTRY, &main_caches(ENTRY, host, layer)).expect_err("past the limit");
            assert!(refusal.contains("past the repository's"), "{refusal}");
        }
        let refusal = budget("host-sealed-Linux-X64-3", &main_caches(ENTRY, 1, 1)).expect_err("an unsaved entry");
        assert!(refusal.contains("stored nothing"), "{refusal}");
        for prefix in crate::release::entry_prefixes() {
            let mut main = main_caches(ENTRY, 1, 1);
            main.retain(|(key, _)| !key.starts_with(&prefix));
            let refusal = budget(ENTRY, &main).expect_err("a layer main holds none of");
            assert!(refusal.contains(&prefix), "{refusal}");
        }
    }

    /// A page of the cache list in the shape GitHub's REST API documents, and
    /// an entry of another scope, or without its size, refused.
    #[test]
    fn a_page_of_the_cache_list_holds_only_mains_entries() {
        let entry = |scope: &str| {
            serde_json::json!({
                "id": 7, "ref": scope, "key": "toolchain-llvm-cc80e6c5b01b2cf1", "version": "0",
                "last_accessed_at": "2026-10-20T22:32:58Z", "created_at": "2026-10-10T23:12:33Z",
                "size_in_bytes": 231_108_403u64
            })
        };
        let page = |entries: Vec<serde_json::Value>| serde_json::json!({ "total_count": 9, "actions_caches": entries });
        let read = cache_page(&page(vec![entry(MAIN)]));
        assert_eq!(read, Ok((9, vec![("toolchain-llvm-cc80e6c5b01b2cf1".to_string(), 231_108_403)])));
        let mut sizeless = entry(MAIN);
        sizeless.as_object_mut().unwrap().remove("size_in_bytes");
        for wrong in [entry("refs/heads/gh-readonly-queue/main/pr-1-0"), sizeless] {
            assert!(cache_page(&page(vec![wrong.clone()])).is_err(), "{wrong}");
        }
    }

    /// One commit of `a.rs`, begun cold on `runner`.
    fn cold_repo(tmp: &Path, runner: &str) -> Cold {
        sh(tmp, &["init", "-q"]);
        configure(tmp);
        write(tmp, "a.rs", "a\n");
        sh(tmp, &["add", "-A"]);
        sh(tmp, &["commit", "-qm", "a"]);
        start(tmp, runner).unwrap().0
    }

    /// Targets with no manifest beside them were built from nothing anyone can
    /// name, so a reader refuses them; the writer, which builds from its
    /// checkout alone, refuses them and an entry. The driver's own target is
    /// this job's.
    #[test]
    fn targets_restored_without_a_manifest_are_refused() {
        let tmp = TempDir::new("cicache-foreign");
        cold_repo(&tmp, RUNNER);
        fs::create_dir_all(tmp.join(DRIVER)).unwrap();
        open(&tmp, RUNNER, SystemTime::now()).expect("the driver's own target");
        start(&tmp, RUNNER).expect("the driver's own target");
        for target in ["tests/guest/rust-tests/target", "target/debug"] {
            fs::create_dir_all(tmp.join(target)).unwrap();
            let refusals = [open(&tmp, RUNNER, SystemTime::now()).err(), start(&tmp, RUNNER).err()];
            assert!(refusals.iter().all(|r| r.as_ref().is_some_and(|r| r.contains(target))), "{refusals:?}");
            fs::remove_dir(tmp.join(target)).unwrap();
        }
        write(&tmp, MANIFEST, "");
        let refusal = start(&tmp, RUNNER).err().expect("an entry, for the writer");
        assert!(refusal.contains(MANIFEST), "{refusal}");
    }

    /// A sealed tree is dated as built throughout, and a step that wrote a
    /// tracked source is refused, even one that wrote its bytes back: the
    /// manifest would name bytes no build read.
    #[test]
    fn a_seal_dates_every_target_and_refuses_a_written_source() {
        let tmp = TempDir::new("cicache-seal");
        let cold = cold_repo(&tmp, RUNNER);
        write(&tmp, "target/debug/deps/x", "x");
        write(&tmp, "tests/guest/rust-tests/target/y", "y");
        seal_at(&tmp, &tmp.join("home"), &cold).unwrap();
        for file in ["target/debug/deps/x", "target/debug", "tests/guest/rust-tests/target/y"] {
            assert_eq!(modified(&tmp.join(file)).unwrap(), built(), "{file}");
        }
        fs::remove_file(tmp.join(MANIFEST)).unwrap();
        for bytes in ["b\n", "a\n"] {
            write(&tmp, "a.rs", bytes);
            let refusal = seal_at(&tmp, &tmp.join("home"), &cold).unwrap_err();
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
            write(&tmp, "tests/guest/rust-tests/target/y", "y");
            seal_at(&tmp, &tmp.join("home"), &cold).unwrap();
            let mut other = image.clone();
            other.insert(name, "another");
            let said = open(&tmp, &runner_in(&other).unwrap(), SystemTime::now()).unwrap();
            assert!(!tmp.join("target/debug").exists() && !tmp.join("tests/guest/rust-tests/target").exists(), "{name}: {said}");
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
        seal_at(&tmp, &tmp.join("home"), &cold).unwrap();
        let refusal = open(&tmp, RUNNER, built()).expect_err("a clock at the entry's date");
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
