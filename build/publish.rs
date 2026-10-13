//! The package repository's publisher: `packages.toml` in, a signed
//! repository out, in the format `toyos_update::repo` reads and nothing else
//! writes.
//!
//! **It signs with the repository's signing key and nothing else, and mints no
//! root.** Every root is the ceremony's (`build/root`): the publisher receives
//! them as `roots/root.<N>.txt` beside its manifest, from root 1 with no gap
//! and nothing else there, copies each the repository lacks, and never
//! rewrites one it holds. Root 1 is held to [`repo::self_signed`], since a
//! machine trusts the root it pins unchecked; a root that does not grant the
//! key is refused by the read-back.
//!
//! **Every file sits at the repository's top level**, under a name of `a-z 0-9
//! . _ -`, which a GitHub release's flat asset namespace keeps as it is: an
//! archive is `<name>-<sequence>-<target>.tar.gz`, so its name changes whenever
//! its bytes may.
//!
//! **A publish only moves forward.** The targets and the timestamp each take
//! their next version; an item's sequence never falls, and an equal one names
//! the same archive; an archive is never rewritten. Every file lands by
//! rename, synced with its directory, the timestamp last, so a copy of the
//! directory taken mid-publish or after a power loss names only what it holds.
//! **What is written has first been read back through the client**
//! ([`repo::refresh`]), by a machine pinning root 1 and holding the directory
//! as it was, every archive streamed through [`repo::Archive`]: the client's
//! floors are the publisher's, and a repository this writes is one such a
//! machine accepts.
//!
//! **Then it prunes** ([`pruned`]): with targets M published, every targets
//! below M−1 and every archive neither M nor M−1 names goes, and never a root,
//! so a machine mid-install that holds M−1 still finds what it names.
//!
//! ```toml
//! [[package]]
//! name = "gbae"
//! target = "x86_64-unknown-toyos"
//! sequence = 3
//! version = "0.2.0"
//! archive = "gbae-v0.2.0-toyos-x86_64.tar.gz"   # beside this file
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toyos_update::repo::{self, render, Held, Item, Mirror, Role, Root, Snapshot, Targets, Timestamp};

use crate::signing::RepoKey;

const DAY: u64 = 86_400;
/// How long a targets is good for: the owner signs one at least this often.
pub const TARGETS_LIFE: u64 = 90 * DAY;
/// How long a timestamp is good for: a freeze lasts no longer.
pub const TIMESTAMP_LIFE: u64 = 7 * DAY;

/// The repository `--publish` writes, under `$HOME`.
pub const REPOSITORY_DIR: &str = ".config/toyos/pkg-repository";
/// Where the roots a publish receives are, beside its manifest.
pub const ROOTS: &str = "roots";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    package: Vec<Row>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    name: String,
    target: String,
    sequence: u64,
    version: String,
    /// Relative to the manifest's directory.
    archive: PathBuf,
}

/// The versions one publish left the repository at, and the files it pruned.
#[derive(Debug, PartialEq, Eq)]
pub struct Published {
    pub root: u64,
    pub targets: u64,
    pub timestamp: u64,
    pub pruned: Vec<String>,
}

/// Publish what `manifest` lists into `dir`, signed by `key`, at `now`, with
/// the roots under [`ROOTS`] beside `manifest`.
pub fn publish(manifest: &Path, dir: &Path, key: &RepoKey, now: u64) -> Result<Published, String> {
    let text = fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let rows: Manifest = toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let beside = manifest.parent().unwrap_or(Path::new("."));

    let mut new: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let received = beside.join(ROOTS);
    for (version, bytes) in roots(&received)? {
        let name = repo::root_file(version);
        match read_if_there(&dir.join(&name))? {
            Some(held) if held != bytes => {
                return Err(format!("{} holds another {name} than {}, and a root is never rewritten", dir.display(), received.display()));
            }
            Some(_) => {}
            None => {
                new.insert(name, bytes);
            }
        }
    }
    let first = match new.get(&repo::root_file(1)) {
        Some(bytes) => bytes.clone(),
        None => read_if_there(&dir.join(repo::root_file(1)))?
            .ok_or_else(|| format!("{} holds no root.1.txt, which the ceremony signs", received.display()))?,
    };
    repo::self_signed(&first).map_err(|why| format!("root 1 is not self-signed: {why}"))?;
    let (newest, _) = newest_root(&mut Overlay { dir, new: &new })?.expect("root 1, read above");

    let mut items = Vec::new();
    for row in &rows.package {
        let path = beside.join(&row.archive);
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let url = format!("{}-{}-{}.tar.gz", row.name, row.sequence, row.target);
        // An archive already published is never rewritten: the read-back's
        // stream refuses one with other bytes.
        if read_if_there(&dir.join(&url))?.is_none() {
            new.insert(url.clone(), bytes.clone());
        }
        items.push(Item {
            name: row.name.clone(),
            target: row.target.clone(),
            sequence: row.sequence,
            version: row.version.clone(),
            url,
            length: bytes.len() as u64,
            sha256: toyos_update::sha256(&bytes),
        });
    }
    items.sort_by(|a, b| (&a.name, &a.target).cmp(&(&b.name, &b.target)));

    let held = previous(dir)?;
    let (timestamp_version, targets_version) =
        held.as_ref().map_or((1, 1), |p| (p.timestamp_version + 1, p.targets_version + 1));
    let targets = Targets { version: targets_version, expires: now + TARGETS_LIFE, items };
    let targets_bytes = signed(render::targets(&targets), Role::Targets, key);
    let timestamp = Timestamp {
        version: timestamp_version,
        expires: now + TIMESTAMP_LIFE,
        targets: Snapshot {
            version: targets.version,
            length: targets_bytes.len() as u64,
            sha256: toyos_update::sha256(&targets_bytes),
        },
    };
    new.insert(repo::targets_file(targets.version), targets_bytes);
    new.insert(repo::TIMESTAMP_FILE.into(), signed(render::timestamp(&timestamp), Role::Timestamp, key));

    // Read back before anything is written, as a machine pinning root 1 and
    // holding what the directory holds now would read it, every archive
    // streamed through the client's check.
    let none = BTreeMap::new();
    let held_root = newest_root(&mut Overlay { dir, new: &none })?;
    let machine = held_root.as_ref().map(|(_, root)| Held {
        root,
        timestamp: held.as_ref().map(|p| p.timestamp.as_slice()),
        targets: held.as_ref().map(|p| p.targets.as_slice()),
    });
    let mut overlay = Overlay { dir, new: &new };
    let refused = |what: &str, why: repo::Refused| format!("a machine holding what {} holds now refuses {what}: {why}", dir.display());
    let fresh = repo::refresh(&mut overlay, Held { root: &first, timestamp: None, targets: None }, machine, now)
        .map_err(|why| refused("what this publish would leave", why))?;
    for item in &fresh.targets.items {
        overlay.stream(item)?.map_err(|why| refused(&item.url, why))?;
    }
    assert_eq!(fresh.targets, targets, "the client reads back the targets this rendered");

    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (mut last, mut rest): (Vec<_>, Vec<_>) = new.iter().partition(|(name, _)| *name == repo::TIMESTAMP_FILE);
    rest.append(&mut last);
    for (name, bytes) in rest {
        land(&dir.join(name), bytes)?;
    }

    let before = held.map(|p| Targets::parse(&p.targets)).transpose().map_err(|why| format!("{}: {why}", dir.display()))?;
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        files.push(entry.map_err(|e| format!("{}: {e}", dir.display()))?.file_name().to_string_lossy().into_owned());
    }
    let gone = pruned(&files, &targets, before.as_ref());
    for name in &gone {
        fs::remove_file(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))?;
    }
    fs::File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(Published { root: newest.version, targets: targets.version, timestamp: timestamp.version, pruned: gone })
}

/// What a publish that left targets `newest` deletes of `files`, given the
/// `previous` targets it replaced: every targets below `newest`'s previous
/// version, and every archive neither names. Never a root.
pub fn pruned(files: &[String], newest: &Targets, previous: Option<&Targets>) -> Vec<String> {
    let named: BTreeSet<&str> = newest.items.iter().chain(previous.iter().flat_map(|t| &t.items)).map(|i| i.url.as_str()).collect();
    let keep_from = newest.version - 1;
    let mut gone: Vec<String> = files
        .iter()
        .filter(|name| match name.strip_prefix("targets.").and_then(|v| v.strip_suffix(".txt")) {
            Some(version) => version.parse::<u64>().is_ok_and(|v| v < keep_from),
            None => name.ends_with(".tar.gz") && !named.contains(name.as_str()),
        })
        .cloned()
        .collect();
    gone.sort();
    gone
}

fn signed(body: String, role: Role, key: &RepoKey) -> Vec<u8> {
    let line = key.sign_document(role, body.as_bytes());
    (body + &line).into_bytes()
}

/// `path`'s bytes, or `None` where there is no such file.
fn read_if_there(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Every root under `dir`, from root 1 while the next is there; any other
/// file there, a root past a gap among them, is refused by name.
fn roots(dir: &Path) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let mut out = Vec::new();
    while let Some(bytes) = read_if_there(&dir.join(repo::root_file(out.len() as u64 + 1)))? {
        out.push((out.len() as u64 + 1, bytes));
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let name = entry.map_err(|e| format!("{}: {e}", dir.display()))?.file_name();
        if !(1..=out.len() as u64).any(|version| name == *repo::root_file(version)) {
            return Err(format!(
                "{} holds {}, which is not a root the walk from root 1 reaches: it stops before {}",
                dir.display(),
                name.to_string_lossy(),
                repo::root_file(out.len() as u64 + 1)
            ));
        }
    }
    Ok(out)
}

/// The newest root `overlay` holds, walked from root 1 as the client walks,
/// and its bytes.
fn newest_root(overlay: &mut Overlay<'_>) -> Result<Option<(Root, Vec<u8>)>, String> {
    let mut found = None;
    for version in 1.. {
        let name = repo::root_file(version);
        let Some(bytes) = overlay.fetch(&name, repo::ROOT_CAP)? else { break };
        let root = Root::parse(&bytes).map_err(|why| format!("{name}: {why}"))?;
        found = Some((root, bytes));
    }
    Ok(found)
}

/// What `dir` holds past its roots, where it holds a timestamp.
struct Previous {
    timestamp: Vec<u8>,
    timestamp_version: u64,
    /// The targets the timestamp names.
    targets: Vec<u8>,
    targets_version: u64,
}

fn previous(dir: &Path) -> Result<Option<Previous>, String> {
    let path = dir.join(repo::TIMESTAMP_FILE);
    let timestamp = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let parsed = Timestamp::parse(&timestamp).map_err(|why| format!("{}: {why}", path.display()))?;
    let path = dir.join(repo::targets_file(parsed.targets.version));
    let targets = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(Previous { timestamp, timestamp_version: parsed.version, targets, targets_version: parsed.targets.version }))
}

/// Write `bytes` to `path` whole or not at all, and durably before the next.
fn land(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staged = path.with_extension(format!("staged.{}", std::process::id()));
    let parent = path.parent().expect("a file under the repository");
    fs::write(&staged, bytes)
        .and_then(|()| fs::File::open(&staged)?.sync_all())
        .and_then(|()| fs::rename(&staged, path))
        .and_then(|()| fs::File::open(parent)?.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The repository as it will be: this publish's files over the directory's.
struct Overlay<'a> {
    dir: &'a Path,
    new: &'a BTreeMap<String, Vec<u8>>,
}

impl Overlay<'_> {
    /// `item`'s archive, streamed through the client's check.
    fn stream(&self, item: &Item) -> Result<Result<(), repo::Refused>, String> {
        let mut archive = repo::Archive::of(item);
        if let Some(bytes) = self.new.get(&item.url) {
            return Ok(archive.take(bytes).and_then(|()| archive.finish()));
        }
        let path = self.dir.join(&item.url);
        let mut file = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut chunk = vec![0; 1 << 16];
        loop {
            match file.read(&mut chunk).map_err(|e| format!("{}: {e}", path.display()))? {
                0 => return Ok(archive.finish()),
                n => {
                    if let Err(why) = archive.take(&chunk[..n]) {
                        return Ok(Err(why));
                    }
                }
            }
        }
    }
}

impl Mirror for Overlay<'_> {
    fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
        if let Some(bytes) = self.new.get(name) {
            return Ok(Some(bytes[..bytes.len().min(cap + 1)].to_vec()));
        }
        let mut out = Vec::new();
        match fs::File::open(self.dir.join(name)) {
            Ok(file) => file.take(cap as u64 + 1).read_to_end(&mut out).map(|_| Some(out)).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_tmpdir::TempDir;

    /// 2026-10-09T00:00:00Z.
    const NOW: u64 = 1_791_504_000;
    /// The ceremony's root key, for these tests.
    const ROOT_KEY: [u8; 32] = [9; 32];
    const TRIPLE: &str = "x86_64-unknown-toyos";
    const ROWS: &[(&str, u64, &str, &[u8])] = &[("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")];

    fn key() -> RepoKey {
        RepoKey::from_seed([3; 32])
    }

    /// Root 1 as the ceremony signs it, granting `signing`.
    fn first_root(signing: &RepoKey) -> Vec<u8> {
        render::first_root(&ROOT_KEY, &signing.public(), NOW + 365 * DAY)
    }

    /// A manifest listing `rows` of `(name, sequence, archive file, bytes)`,
    /// written beside its archives and `root` as its root 1.
    fn manifest(dir: &Path, root: &[u8], rows: &[(&str, u64, &str, &[u8])]) -> PathBuf {
        fs::create_dir_all(dir.join(ROOTS)).unwrap();
        fs::write(dir.join(ROOTS).join("root.1.txt"), root).unwrap();
        let mut text = String::new();
        for (name, sequence, file, bytes) in rows {
            fs::write(dir.join(file), bytes).unwrap();
            text += &format!(
                "[[package]]\nname = {name:?}\ntarget = {TRIPLE:?}\nsequence = {sequence}\nversion = \"1.0\"\narchive = {file:?}\n"
            );
        }
        let path = dir.join("packages.toml");
        fs::write(&path, text).unwrap();
        path
    }

    fn read(dir: &Path, name: &str) -> Vec<u8> {
        fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = dir.read_dir().unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    fn gbae(targets: &Targets) -> &Item {
        targets.items.iter().find(|i| i.name == "gbae").expect("gbae")
    }

    /// What a machine holding `dir`'s files accepts next.
    fn client(dir: &Path, now: u64, machine: Option<Held<'_>>) -> Result<repo::Fresh, repo::Refused> {
        let first = read(dir, "root.1.txt");
        let none = BTreeMap::new();
        repo::refresh(&mut Overlay { dir, new: &none }, Held { root: &first, timestamp: None, targets: None }, machine, now)
    }

    #[test]
    fn a_first_publish_carries_the_received_root_and_a_machine_accepts_it() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let root = first_root(&key());
        let m = manifest(src.path(), &root, &[("snake", 1, "snake-v1.tar.gz", b"snake bytes"), ("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]);
        assert_eq!(publish(&m, out.path(), &key(), NOW), Ok(Published { root: 1, targets: 1, timestamp: 1, pruned: vec![] }));
        let fresh = client(out.path(), NOW, None).expect("what was published");
        let names: Vec<&str> = fresh.targets.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["gbae", "snake"]);
        assert_eq!(read(out.path(), &gbae(&fresh.targets).url), b"gbae bytes");
        assert_eq!(fresh.root, root, "the root received, and no other");
        assert!(!listing(out.path()).iter().any(|name| name.contains("staged")));
    }

    /// The second publish moves every version forward and keeps the root,
    /// and a machine holding the first accepts it.
    #[test]
    fn a_publish_moves_forward_past_a_machines_floors() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let root = first_root(&key());
        publish(&manifest(src.path(), &root, &[("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]), out.path(), &key(), NOW).unwrap();
        let first = client(out.path(), NOW, None).unwrap();
        let m = manifest(src.path(), &root, &[("gbae", 4, "gbae-v2.tar.gz", b"gbae two")]);
        assert_eq!(publish(&m, out.path(), &key(), NOW + DAY), Ok(Published { root: 1, targets: 2, timestamp: 2, pruned: vec![] }));
        let held = Held { root: &first.root, timestamp: Some(&first.timestamp), targets: Some(&first.targets_bytes) };
        let second = client(out.path(), NOW + DAY, Some(held)).expect("the next publish, past the first's floors");
        assert_eq!(gbae(&second.targets).sequence, 4);
    }

    /// A received root 1 that grants another key the targets and timestamp
    /// roles is refused by the read-back, before anything is written.
    #[test]
    fn a_root_that_does_not_grant_the_key_is_refused_and_nothing_is_written() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let other = first_root(&RepoKey::from_seed([4; 32]));
        let refused = publish(&manifest(src.path(), &other, ROWS), out.path(), &key(), NOW);
        assert!(refused.as_ref().unwrap_err().contains("refuses what this publish would leave"), "{refused:?}");
        assert_eq!(listing(out.path()), Vec::<String>::new(), "a refused publish wrote nothing");
    }

    /// A received root 1 its own root key did not sign is refused by name,
    /// before anything is written.
    #[test]
    fn a_root_one_that_is_not_self_signed_is_refused_and_nothing_is_written() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let mut forged = render::root(&Root::parse(&first_root(&key())).unwrap());
        forged += &key().sign_document(Role::Root, forged.as_bytes());
        let refused = publish(&manifest(src.path(), forged.as_bytes(), ROWS), out.path(), &key(), NOW);
        assert!(refused.as_ref().unwrap_err().starts_with("root 1 is not self-signed: "), "{refused:?}");
        assert_eq!(listing(out.path()), Vec::<String>::new(), "a refused publish wrote nothing");
    }

    /// A received root past a gap in `roots/` is refused by name, before
    /// anything is written.
    #[test]
    fn a_root_past_a_gap_in_roots_is_refused_and_nothing_is_written() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let m = manifest(src.path(), &first_root(&key()), ROWS);
        fs::write(src.path().join(ROOTS).join("root.3.txt"), first_root(&key())).unwrap();
        let refused = publish(&m, out.path(), &key(), NOW);
        assert!(refused.as_ref().unwrap_err().ends_with("holds root.3.txt, which is not a root the walk from root 1 reaches: it stops before root.2.txt"), "{refused:?}");
        assert_eq!(listing(out.path()), Vec::<String>::new(), "a refused publish wrote nothing");
    }

    /// One publish of [`ROWS`] under root 1, and what it left.
    fn published_once() -> (TempDir, TempDir, Vec<u8>, Vec<String>) {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let root = first_root(&key());
        publish(&manifest(src.path(), &root, ROWS), out.path(), &key(), NOW).unwrap();
        let before = listing(out.path());
        (src, out, root, before)
    }

    /// A refused publish left `out` holding what it held, timestamp included.
    fn left_as_it_was(out: &Path, before: &[String], timestamp: &[u8]) {
        assert_eq!((listing(out), read(out, "timestamp.txt")), (before.to_vec(), timestamp.to_vec()), "a refused publish wrote nothing");
    }

    /// A sequence below the floor a machine holds is refused by the
    /// client's name for it, and nothing is written.
    #[test]
    fn a_lower_sequence_is_refused_by_name() {
        let (src, out, root, before) = published_once();
        let timestamp = read(out.path(), "timestamp.txt");
        let lower = publish(&manifest(src.path(), &root, &[("gbae", 2, "gbae-v0.tar.gz", b"old")]), out.path(), &key(), NOW);
        let why = repo::Refused::Sequence { name: "gbae".into(), target: TRIPLE.into(), sequence: 2, floor: 3, holder: repo::Holder::Machine };
        assert!(lower.as_ref().unwrap_err().ends_with(&why.to_string()), "{lower:?}");
        left_as_it_was(out.path(), &before, &timestamp);
    }

    /// An equal sequence naming other bytes is refused by the client's name
    /// for it, and nothing is written.
    #[test]
    fn an_equal_sequence_with_other_bytes_is_refused_by_name() {
        let (src, out, root, before) = published_once();
        let timestamp = read(out.path(), "timestamp.txt");
        let same = publish(&manifest(src.path(), &root, &[("gbae", 3, "gbae-v1b.tar.gz", b"other")]), out.path(), &key(), NOW);
        let why = repo::Refused::Reissued { name: "gbae".into(), target: TRIPLE.into(), sequence: 3, holder: repo::Holder::Machine };
        assert!(same.as_ref().unwrap_err().ends_with(&why.to_string()), "{same:?}");
        left_as_it_was(out.path(), &before, &timestamp);
    }

    /// A manifest row with a field the format does not name is refused by
    /// that field's name, and nothing is written.
    #[test]
    fn a_manifest_field_the_format_does_not_name_is_refused() {
        let (src, out, _, before) = published_once();
        let timestamp = read(out.path(), "timestamp.txt");
        let unknown = src.path().join("unknown.toml");
        fs::write(&unknown, "[[package]]\nname = \"gbae\"\nowner = \"me\"\n").unwrap();
        assert!(publish(&unknown, out.path(), &key(), NOW).unwrap_err().contains("owner"));
        left_as_it_was(out.path(), &before, &timestamp);
    }

    /// A package name the client refuses is refused by the read-back, and
    /// nothing is written.
    #[test]
    fn a_name_the_client_refuses_is_refused_by_the_read_back() {
        let (src, out, root, before) = published_once();
        let timestamp = read(out.path(), "timestamp.txt");
        let bad_name = publish(&manifest(src.path(), &root, &[("Gbae", 1, "g.tar.gz", b"g")]), out.path(), &key(), NOW);
        assert!(bad_name.as_ref().unwrap_err().contains("refuses what this publish would leave"), "{bad_name:?}");
        left_as_it_was(out.path(), &before, &timestamp);
    }

    /// A received root 1 other than the one the repository holds is refused
    /// by name, and nothing is written.
    #[test]
    fn a_received_root_that_would_rewrite_a_held_one_is_refused() {
        let (src, out, _, before) = published_once();
        let timestamp = read(out.path(), "timestamp.txt");
        let bent = manifest(src.path(), &first_root(&RepoKey::from_seed([4; 32])), &[("gbae", 4, "gbae-v4.tar.gz", b"new")]);
        let rewritten = publish(&bent, out.path(), &key(), NOW);
        assert!(rewritten.as_ref().unwrap_err().contains("holds another root.1.txt than"), "{rewritten:?}");
        left_as_it_was(out.path(), &before, &timestamp);
    }

    /// A timestamp on disk naming targets 2^64 − 1 leaves no next version:
    /// the publish refuses it by name rather than counting past it.
    #[test]
    fn a_timestamp_naming_the_last_targets_version_is_refused() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let m = manifest(src.path(), &first_root(&key()), &[("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]);
        publish(&m, out.path(), &key(), NOW).unwrap();
        let mut held = Timestamp::parse(&read(out.path(), "timestamp.txt")).unwrap();
        held.targets.version = u64::MAX;
        let last = signed(render::timestamp(&held), Role::Timestamp, &key());
        fs::write(out.path().join("timestamp.txt"), &last).unwrap();
        fs::copy(out.path().join("targets.1.txt"), out.path().join(repo::targets_file(u64::MAX))).unwrap();

        let why = repo::Refused::Malformed {
            role: Role::Timestamp,
            line: 3,
            why: "`targets` is not `<version> <length> <sha256>` within the targets cap",
        };
        assert_eq!(Timestamp::parse(&last).err(), Some(why.clone()));
        let refused = publish(&m, out.path(), &key(), NOW + DAY);
        assert!(refused.as_ref().unwrap_err().ends_with(&format!("timestamp.txt: {why}")), "{refused:?}");
        assert_eq!(read(out.path(), "timestamp.txt"), last, "a refused publish wrote nothing");
    }

    /// **Pruning keeps targets M and M−1 and what they name.** Three
    /// publishes of gbae at sequences 1, 2 and 3 leave targets 2 and 3 and
    /// sequences 2 and 3's archives, so a machine that holds targets 2 still
    /// finds its archive; targets 1 and sequence 1's archive go, and the root
    /// stays.
    #[test]
    fn pruning_keeps_targets_m_and_m_minus_one_and_what_they_name() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let root = first_root(&key());
        for sequence in 1..=2 {
            let m = manifest(src.path(), &root, &[("gbae", sequence, "gbae.tar.gz", format!("gbae {sequence}").as_bytes())]);
            publish(&m, out.path(), &key(), NOW).unwrap();
        }
        let m = manifest(src.path(), &root, &[("gbae", 3, "gbae.tar.gz", b"gbae 3")]);
        let published = publish(&m, out.path(), &key(), NOW).unwrap();
        let one = format!("gbae-1-{TRIPLE}.tar.gz");
        assert_eq!(published.pruned, [one, "targets.1.txt".into()]);
        let mut kept = vec![
            format!("gbae-2-{TRIPLE}.tar.gz"),
            format!("gbae-3-{TRIPLE}.tar.gz"),
            "root.1.txt".into(),
            "targets.2.txt".into(),
            "targets.3.txt".into(),
            "timestamp.txt".into(),
        ];
        kept.sort();
        assert_eq!(listing(out.path()), kept);
        let two = Targets::parse(&read(out.path(), "targets.2.txt")).unwrap();
        assert_eq!(read(out.path(), &gbae(&two).url), b"gbae 2");
    }

    /// **Every name is flat**: what a publish writes for both ToyOS triples
    /// sits at the repository's top level, named in `a-z 0-9 . _ -`, which a
    /// GitHub release keeps unrenamed.
    #[test]
    fn every_file_a_publish_writes_has_a_flat_name() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        fs::create_dir_all(src.path().join("aarch64")).unwrap();
        let m = manifest(src.path(), &first_root(&key()), &[("gbae", 3, "gbae.tar.gz", b"x86-64 bytes")]);
        fs::write(src.path().join("aarch64/gbae.tar.gz"), b"aarch64 bytes").unwrap();
        let mut text = fs::read_to_string(&m).unwrap();
        text += "[[package]]\nname = \"gbae\"\ntarget = \"aarch64-unknown-toyos\"\nsequence = 3\nversion = \"1.0\"\narchive = \"aarch64/gbae.tar.gz\"\n";
        fs::write(&m, text).unwrap();
        publish(&m, out.path(), &key(), NOW).unwrap();
        let names = listing(out.path());
        assert_eq!(names.len(), 5, "{names:?}");
        for name in names {
            assert!(out.path().join(&name).is_file(), "{name} is a file");
            assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)), "{name}");
        }
    }
}
