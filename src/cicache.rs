//! The host job's cache entry is trusted by content, never by mtime.
//!
//! Cargo calls a path crate fresh when none of its sources is newer than the
//! build that read it, and a checkout dates every source at the checkout: a
//! restored target is stale in every path crate, and a date made up for a
//! source from anything but its bytes could as well call a changed one fresh.
//! An entry instead carries [`MANIFEST`], the git blob id of every source its
//! build read, and every file under its targets is dated [`built`], before any
//! real time. [`read`] dates each source whose blob matches the same, and every
//! other one now: cargo's comparison is strict, so a matching source is no
//! newer than the build, and a changed or new one is newer than everything in
//! the entry, whatever any runner's clock says.
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
//!
//! A package that lost a file keeps its `Cargo.toml` dated now: cargo's package
//! fingerprint, which decides a build script that names no input, is the
//! newest of the files that remain.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MANIFEST: &str = "target/ci-sources";
pub const DRIVER: &str = "target/ci-driver";
/// Every host entry's key starts with this; [`prune`] lists by it.
const HOST: &str = "host-";
/// A sealed entry's key is this, `${{ runner.os }}-${{ github.run_id }}`.
pub const SEALED: &str = "host-sealed-";

/// 2001-09-09T01:46:40Z: older than any build, so a file dated so is never
/// newer than one.
fn built() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

/// Every source as git tracks it, with the blob id of its bytes on disk.
type Sources = BTreeMap<String, String>;

/// What a job found before its first step.
pub enum Start {
    /// An entry, read and its manifest consumed.
    Warm,
    /// No entry: every source is dated [`built`], and these are what [`seal`]
    /// holds the tree to at the end.
    Cold(Sources),
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
    open(root)
}

fn open(root: &Path) -> Result<(Start, String), String> {
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
            for path in current.keys() {
                date(&root.join(path), built())?;
            }
            let said = format!(
                "none restored; {} sources dated as built, and the tree is sealed last",
                current.len()
            );
            return Ok((Start::Cold(current), said));
        }
        Err(e) => return Err(format!("read {MANIFEST}: {e}")),
    };
    fs::remove_file(&manifest).map_err(|e| format!("remove {MANIFEST}: {e}"))?;
    let (commit, entry) = parse(&text)?;
    let lost = lost(&entry, &current);
    let now = SystemTime::now();
    let mut same = 0;
    for (path, blob) in &current {
        let fresh = entry.get(path) == Some(blob) && !lost.contains(path);
        same += usize::from(fresh);
        date(&root.join(path), if fresh { built() } else { now })?;
    }
    let changed = current.iter().filter(|(p, b)| entry.get(*p).is_some_and(|e| e != *b)).count();
    let added = current.keys().filter(|p| !entry.contains_key(*p)).count();
    let removed = entry.keys().filter(|p| !current.contains_key(*p)).count();
    Ok((
        Start::Warm,
        format!(
            "built from {commit}: {same} of {} sources unchanged, {changed} changed, {added} added, \
             {removed} removed",
            current.len()
        ),
    ))
}

/// After the last step of a run that started [`Start::Cold`]: every file under
/// every target dated [`built`], then the manifest.
pub fn seal(root: &Path, stamped: &Sources) -> Result<String, String> {
    let now = sources(root)?;
    let moved: Vec<&String> = stamped
        .iter()
        .filter(|(path, blob)| {
            now.get(*path) != Some(*blob) || modified(&root.join(path)).ok() != Some(built())
        })
        .map(|(path, _)| path)
        .chain(now.keys().filter(|path| !stamped.contains_key(*path)))
        .collect();
    if !moved.is_empty() {
        return Err(format!("a step wrote tracked sources, which no entry can describe: {moved:?}"));
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    for target in targets(root)? {
        age(&target, &mut files, &mut bytes)?;
    }
    let head = crate::sync::git(root, &["rev-parse", "HEAD"])?;
    let mut text = format!("{head}\n");
    for (path, blob) in stamped {
        text.push_str(&format!("{blob} {path}\n"));
    }
    fs::write(root.join(MANIFEST), text).map_err(|e| format!("write {MANIFEST}: {e}"))?;
    Ok(format!("{} sources; {files} files, {} MiB, dated as built", stamped.len(), bytes >> 20))
}

/// Every host entry but the one this run saved, which must be on `main`:
/// pull requests read that one, and every other only fills the repository's
/// cache budget.
pub fn prune(root: &Path) -> Result<String, String> {
    let var =
        |name: &str| std::env::var(name).map_err(|_| format!("{name} is unset: a runner prunes"));
    let (repo, os, run) = (var("GITHUB_REPOSITORY")?, var("RUNNER_OS")?, var("GITHUB_RUN_ID")?);
    let ours = format!("{SEALED}{os}-{run}");
    let caches = format!("repos/{repo}/actions/caches");
    let listing =
        gh(root, &["api", "-X", "GET", &caches, "-f", &format!("key={HOST}"), "-F", "per_page=100"])?;
    let doc: serde_json::Value =
        serde_json::from_str(&listing).map_err(|e| format!("the cache listing is not JSON: {e}"))?;
    let doomed = doomed(&doc, &ours)?;
    for (id, _) in &doomed {
        gh(root, &["api", "-X", "DELETE", &format!("{caches}/{id}")])?;
    }
    let keys: Vec<&str> = doomed.iter().map(|(_, key)| key.as_str()).collect();
    Ok(format!("kept {ours}; deleted {}: {}", keys.len(), keys.join(", ")))
}

/// Every entry in the listing but `ours`, which must be in it on `main`.
fn doomed(doc: &serde_json::Value, ours: &str) -> Result<Vec<(u64, String)>, String> {
    let entries = doc["actions_caches"].as_array().ok_or("the listing holds no actions_caches")?;
    let total = doc["total_count"].as_u64().ok_or("the listing holds no total_count")?;
    if total != entries.len() as u64 {
        return Err(format!(
            "{total} host entries and a page of {}: pruning a page is no bound",
            entries.len()
        ));
    }
    let mut kept = false;
    let mut doomed = Vec::new();
    for entry in entries {
        let (Some(id), Some(key), Some(scope)) =
            (entry["id"].as_u64(), entry["key"].as_str(), entry["ref"].as_str())
        else {
            return Err(format!("an entry without an id, key or ref: {entry}"));
        };
        if key == ours && scope == "refs/heads/main" {
            kept = true;
        } else {
            doomed.push((id, key.to_string()));
        }
    }
    if !kept {
        return Err(format!(
            "{ours} is not among main's entries: the save before this wrote none, and pruning would \
             leave pull requests nothing to read"
        ));
    }
    Ok(doomed)
}

fn gh(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(format!("gh {} exited {}: {}", args.join(" "), out.status, said.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse(text: &str) -> Result<(String, Sources), String> {
    let mut lines = text.lines();
    let commit = lines.next().ok_or_else(|| format!("{MANIFEST} is empty"))?.to_string();
    let entry = lines
        .map(|line| {
            line.split_once(' ')
                .filter(|(blob, _)| !blob.is_empty() && blob.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|(blob, path)| (path.to_string(), blob.to_string()))
                .ok_or_else(|| format!("{MANIFEST} holds {line:?}"))
        })
        .collect::<Result<_, _>>()?;
    Ok((commit, entry))
}

/// The `Cargo.toml` of every package that lost a source since `entry`.
fn lost(entry: &Sources, current: &Sources) -> BTreeSet<String> {
    let mut lost = BTreeSet::new();
    for gone in entry.keys().filter(|p| !current.contains_key(*p)) {
        let manifest = Path::new(gone)
            .ancestors()
            .skip(1)
            .map(|dir| dir.join("Cargo.toml").to_string_lossy().into_owned())
            .find(|manifest| current.contains_key(manifest));
        lost.extend(manifest);
    }
    lost
}

fn sources(root: &Path) -> Result<Sources, String> {
    let out = Command::new("git")
        .args(["ls-files", "-s", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-files: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut paths = Vec::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| "git tracks a name that is not UTF-8")?;
        let (meta, path) =
            entry.split_once('\t').ok_or_else(|| format!("git ls-files -s printed {entry:?}"))?;
        // A gitlink names a commit, not bytes: what is under it keeps its
        // checkout's date, which no entry is newer than.
        if meta.starts_with("160000 ") {
            continue;
        }
        if path.contains('\n') {
            return Err(format!("git tracks {path:?}, which `--stdin-paths` cannot be given"));
        }
        paths.push(path.to_string());
    }
    let mut child = Command::new("git")
        .args(["hash-object", "--no-filters", "--stdin-paths"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git hash-object: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    // Written beside the read: the ids fill the pipe before the paths are in.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|e| format!("git hash-object: {e}"))?;
    writer.join().expect("the writer").map_err(|e| format!("git hash-object's input: {e}"))?;
    if !out.status.success() {
        return Err(format!("git hash-object: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let blobs: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
    if blobs.len() != paths.len() {
        return Err(format!("git hash-object gave {} ids for {} paths", blobs.len(), paths.len()));
    }
    Ok(paths.into_iter().zip(blobs).collect())
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
    use toyos_tmpdir::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// `cargo build` in `root`, and each workspace crate with whether cargo
    /// called it fresh.
    fn build(root: &Path) -> BTreeMap<String, bool> {
        let out = Command::new("cargo")
            .args(["build", "--offline", "--message-format=json"])
            .current_dir(root)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .expect("run cargo");
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

    /// The oracle is cargo and the program it builds: an entry serves what
    /// a cold build of the reader's tree would, recompiling exactly what
    /// changed and what depends on it — a changed source the checkout dated
    /// older than the entry's build included, and a build script whose package
    /// lost a file it never named.
    #[test]
    fn an_entry_serves_exactly_the_sources_it_was_built_from() {
        let tmp = TempDir::new("cicache-entry");
        let origin = tmp.join("origin");
        fs::create_dir(&origin).unwrap();
        sh(&origin, &["init", "-q", "-b", "main"]);
        configure(&origin);
        write(&origin, ".gitignore", "target/\n");
        let members = "[workspace]\nmembers = [\"app\", \"leaf\", \"count\", \"idle\"]\nresolver = \"2\"\n";
        write(&origin, "Cargo.toml", members);
        let package = |name: &str, deps: &str| {
            let head = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
            format!("{head}\n[dependencies]\n{deps}")
        };
        write(&origin, "leaf/Cargo.toml", &package("leaf", ""));
        write(&origin, "leaf/src/lib.rs", "pub fn word() -> &'static str { \"one\" }\n");
        write(&origin, "count/Cargo.toml", &package("count", ""));
        let script = r#"fn main() {
    let files = std::fs::read_dir("src").unwrap().count();
    println!("cargo:rustc-env=FILES={files}");
}
"#;
        write(&origin, "count/build.rs", script);
        write(&origin, "count/src/lib.rs", "pub const FILES: &str = env!(\"FILES\");\n");
        write(&origin, "count/src/spare.txt", "read by nothing but the count\n");
        write(&origin, "idle/Cargo.toml", &package("idle", ""));
        write(&origin, "idle/src/lib.rs", "pub fn idle() {}\n");
        let deps = "leaf = { path = \"../leaf\" }\ncount = { path = \"../count\" }\n";
        write(&origin, "app/Cargo.toml", &package("app", deps));
        let main = "fn main() { print!(\"{} {}\", leaf::word(), count::FILES); }\n";
        write(&origin, "app/src/main.rs", main);
        sh(&origin, &["add", "-A"]);
        sh(&origin, &["commit", "-qm", "built"]);

        let writer = tmp.join("writer");
        checkout(&origin, &writer);
        let Start::Cold(stamped) = open(&writer).unwrap().0 else {
            panic!("a tree with no target is cold")
        };
        build(&writer);
        assert_eq!(run(&writer), "one 2");
        seal(&writer, &stamped).unwrap();

        write(&origin, "leaf/src/lib.rs", "pub fn word() -> &'static str { \"two\" }\n");
        sh(&origin, &["rm", "-q", "count/src/spare.txt"]);
        sh(&origin, &["commit", "-qam", "read"]);
        let reader = tmp.join("reader");
        checkout(&origin, &reader);
        fs::rename(writer.join("target"), reader.join("target")).unwrap();
        // What an mtime made up from history would say of a file committed
        // before the entry was built.
        date(&reader.join("leaf/src/lib.rs"), UNIX_EPOCH + Duration::from_secs(1)).unwrap();

        let said = open(&reader).unwrap();
        assert!(matches!(said.0, Start::Warm), "{}", said.1);
        assert!(!reader.join(MANIFEST).exists(), "a warm tree keeps no manifest");
        let fresh = build(&reader);
        assert_eq!(run(&reader), "two 1");
        let expected = [("app", false), ("count", false), ("idle", true), ("leaf", false)];
        assert_eq!(fresh, expected.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    }

    /// Targets with no manifest beside them were built from nothing anyone can
    /// name; the driver's own is this job's.
    #[test]
    fn targets_restored_without_a_manifest_are_refused() {
        let tmp = TempDir::new("cicache-foreign");
        sh(&tmp, &["init", "-q"]);
        configure(&tmp);
        write(&tmp, "kernel/src/lib.rs", "");
        sh(&tmp, &["add", "-A"]);
        fs::create_dir_all(tmp.join(DRIVER)).unwrap();
        assert!(matches!(open(&tmp).unwrap().0, Start::Cold(_)));
        fs::create_dir_all(tmp.join("kernel/target/debug")).unwrap();
        let refusal = open(&tmp).err().expect("a restored target with no manifest");
        assert!(refusal.contains("kernel/target"), "{refusal}");
    }

    /// A sealed tree is dated as built throughout, and a step that wrote a
    /// tracked source is refused: the manifest would name bytes no build read.
    #[test]
    fn a_seal_dates_every_target_and_refuses_a_written_source() {
        let tmp = TempDir::new("cicache-seal");
        sh(&tmp, &["init", "-q"]);
        configure(&tmp);
        write(&tmp, "a.rs", "a\n");
        sh(&tmp, &["add", "-A"]);
        sh(&tmp, &["commit", "-qm", "a"]);
        let Start::Cold(stamped) = open(&tmp).unwrap().0 else { panic!("cold") };
        write(&tmp, "target/debug/deps/x", "x");
        write(&tmp, "userland/target/y", "y");
        seal(&tmp, &stamped).unwrap();
        for file in ["target/debug/deps/x", "target/debug", "userland/target/y"] {
            assert_eq!(modified(&tmp.join(file)).unwrap(), built(), "{file}");
        }
        fs::remove_file(tmp.join(MANIFEST)).unwrap();
        write(&tmp, "a.rs", "b\n");
        let refusal = seal(&tmp, &stamped).unwrap_err();
        assert!(refusal.contains("a.rs"), "{refusal}");
    }

    #[test]
    fn a_prune_keeps_this_runs_entry_on_main_and_nothing_else() {
        let entry =
            |id: u64, key: &str, scope: &str| serde_json::json!({"id": id, "key": key, "ref": scope});
        let ours = "host-sealed-Linux-7";
        let listing = |entries: Vec<serde_json::Value>| {
            serde_json::json!({"total_count": entries.len(), "actions_caches": entries})
        };
        let doc = listing(vec![
            entry(1, ours, "refs/heads/main"),
            entry(2, "host-sealed-Linux-6", "refs/heads/main"),
            entry(3, "host-sealed-Linux-5", "refs/pull/9/merge"),
            entry(4, "host-36696295750", "refs/heads/main"),
        ]);
        let ids: Vec<u64> = doomed(&doc, ours).unwrap().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, [2, 3, 4]);

        let unsaved = listing(vec![
            entry(1, ours, "refs/pull/9/merge"),
            entry(2, "host-sealed-Linux-6", "refs/heads/main"),
        ]);
        assert!(doomed(&unsaved, ours).unwrap_err().contains("not among main's"));
        let page = [entry(1, ours, "refs/heads/main")];
        let paged = serde_json::json!({"total_count": 101, "actions_caches": page});
        assert!(doomed(&paged, ours).is_err());
    }
}
