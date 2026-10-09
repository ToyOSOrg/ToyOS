//! The package repository's publisher: `packages.toml` in, a signed
//! repository out, in the format `toyos_update::repo` reads and nothing else
//! writes.
//!
//! **Which repository is decided by which key, as for an image**
//! (`src/signing.rs`): this checkout's throwaway key publishes into
//! [`THROWAWAY_DIR`] under its `target/`, the owner's into [`OWNER_DIR`] under
//! `$HOME`, beside his key and outside every checkout. Stage one fills every
//! role with that one key at threshold one; the root names every role and its
//! threshold, so separating the keys is a new root and not a new client.
//!
//! **A publish only moves forward.** Root `N` stays until it is within
//! [`RENEW`] of its expiry, and then root `N+1` carries the same keys; the
//! targets and the timestamp each take their next version; an item's sequence
//! never falls, and an equal one names the same archive; an archive under
//! `archives/` is never rewritten. Every file lands by rename, synced with its
//! directory, the timestamp last, so a copy of the directory taken mid-publish
//! or after a power loss names only what it holds. **What is written has first
//! been read back through the client** ([`repo::refresh`]), by a machine
//! pinning root 1 and holding the directory as it was, every archive streamed
//! through [`repo::Archive`]: the client's floors are the publisher's, and a
//! repository this writes is one such a machine accepts.
//!
//! ```toml
//! [[package]]
//! name = "gbae"
//! target = "x86_64-unknown-toyos"
//! sequence = 3
//! version = "0.2.0"
//! archive = "gbae-v0.2.0-toyos-x86_64.tar.gz"   # beside this file
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toyos_update::repo::{self, render, Grant, Held, Item, Mirror, Role, Root, Snapshot, Targets, Timestamp};

use crate::signing::{Key, Whose};

const DAY: u64 = 86_400;
/// How long a root is good for.
pub const ROOT_LIFE: u64 = 365 * DAY;
/// How long a targets is good for: the owner signs one at least this often.
pub const TARGETS_LIFE: u64 = 90 * DAY;
/// How long a timestamp is good for: a freeze lasts no longer.
pub const TIMESTAMP_LIFE: u64 = 7 * DAY;
/// How close to its expiry a root is renewed.
pub const RENEW: u64 = 30 * DAY;

/// The throwaway key's repository, under the checkout.
pub const THROWAWAY_DIR: &str = "target/pkg-repository";
/// The owner's repository, under `$HOME`.
pub const OWNER_DIR: &str = ".config/toyos/pkg-repository";
/// Where archives sit in a repository.
const ARCHIVES: &str = "archives";

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

/// The versions one publish left the repository at.
#[derive(Debug, PartialEq, Eq)]
pub struct Published {
    pub root: u64,
    pub targets: u64,
    pub timestamp: u64,
}

/// The repository `key` publishes into.
pub fn repository(checkout: &Path, key: &Key) -> Result<PathBuf, String> {
    match key.whose() {
        Whose::Throwaway => Ok(checkout.join(THROWAWAY_DIR)),
        Whose::Owner(_) => {
            let home = std::env::var_os("HOME").ok_or("no $HOME, which the owner's repository is under")?;
            Ok(PathBuf::from(home).join(OWNER_DIR))
        }
    }
}

/// Publish what `manifest` lists into `dir`, signed by `key`, at `now`.
pub fn publish(manifest: &Path, dir: &Path, key: &Key, now: u64) -> Result<Published, String> {
    let text = fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let rows: Manifest = toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let beside = manifest.parent().unwrap_or(Path::new("."));

    let mut new: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let current = current_root(dir)?;
    let root = match &current {
        Some((r, _)) if !one_key(r, key) => {
            return Err(format!(
                "{}'s root {} is not {}'s alone, and a publish rotates no key",
                dir.display(),
                r.version,
                key.fingerprint()
            ));
        }
        Some((r, _)) if r.expires >= now + RENEW => r.version,
        _ => {
            let version = current.as_ref().map_or(1, |(r, _)| r.version + 1);
            let bytes = signed(render::root(&minted(version, now + ROOT_LIFE, key)), Role::Root, key);
            new.insert(repo::root_file(version), bytes);
            version
        }
    };

    let mut items = Vec::new();
    for row in &rows.package {
        let path = beside.join(&row.archive);
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = row.archive.file_name().and_then(|f| f.to_str()).ok_or_else(|| format!("{} names no file", path.display()))?;
        let url = format!("{ARCHIVES}/{file}");
        // An archive already published, or listed by an earlier row, is never
        // rewritten: the read-back's stream refuses one with other bytes.
        let there = dir.join(&url).try_exists().map_err(|e| format!("{}: {e}", dir.join(&url).display()))?;
        if !there && !new.contains_key(&url) {
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
    let first = match new.get(&repo::root_file(1)) {
        Some(bytes) => bytes.clone(),
        None => fs::read(dir.join(repo::root_file(1))).map_err(|e| format!("{}: {e}", dir.display()))?,
    };
    let machine = current.as_ref().map(|(_, root)| Held {
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

    fs::create_dir_all(dir.join(ARCHIVES)).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (mut last, mut rest): (Vec<_>, Vec<_>) = new.iter().partition(|(name, _)| *name == repo::TIMESTAMP_FILE);
    rest.append(&mut last);
    for (name, bytes) in rest {
        land(&dir.join(name), bytes)?;
    }
    Ok(Published { root, targets: targets.version, timestamp: timestamp.version })
}

/// Whether `root` gives every role to `key` alone, at threshold one.
fn one_key(root: &Root, key: &Key) -> bool {
    root.keys == [key.public()] && root.grants.iter().all(|g| g.threshold == 1)
}

fn minted(version: u64, expires: u64, key: &Key) -> Root {
    let grant = Grant { threshold: 1, keys: vec![repo::key_id(&key.public())] };
    Root { version, expires, keys: vec![key.public()], grants: [grant.clone(), grant.clone(), grant] }
}

fn signed(body: String, role: Role, key: &Key) -> Vec<u8> {
    let line = key.sign_document(role, body.as_bytes());
    (body + &line).into_bytes()
}

/// The newest root `dir` holds, walked from root 1 as the client walks.
fn current_root(dir: &Path) -> Result<Option<(Root, Vec<u8>)>, String> {
    let mut found = None;
    for version in 1.. {
        let path = dir.join(repo::root_file(version));
        match fs::read(&path) {
            Ok(bytes) => {
                let root = Root::parse(&bytes).map_err(|why| format!("{}: {why}", path.display()))?;
                found = Some((root, bytes));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
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

    fn key() -> Key {
        Key::throwaway_from([3; 32])
    }

    /// A manifest listing `rows` of `(name, sequence, archive file, bytes)`,
    /// written beside its archives.
    fn manifest(dir: &Path, rows: &[(&str, u64, &str, &[u8])]) -> PathBuf {
        let mut text = String::new();
        for (name, sequence, file, bytes) in rows {
            fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            fs::write(dir.join(file), bytes).unwrap();
            text += &format!(
                "[[package]]\nname = {name:?}\ntarget = \"x86_64-unknown-toyos\"\nsequence = {sequence}\nversion = \"1.0\"\narchive = {file:?}\n"
            );
        }
        let path = dir.join("packages.toml");
        fs::write(&path, text).unwrap();
        path
    }

    fn read(dir: &Path, name: &str) -> Vec<u8> {
        fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
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
    fn a_first_publish_mints_root_one_and_a_machine_accepts_it() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let m = manifest(src.path(), &[("snake", 1, "snake-v1.tar.gz", b"snake bytes"), ("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]);
        assert_eq!(publish(&m, out.path(), &key(), NOW), Ok(Published { root: 1, targets: 1, timestamp: 1 }));
        let fresh = client(out.path(), NOW, None).expect("what was published");
        let names: Vec<&str> = fresh.targets.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["gbae", "snake"]);
        assert_eq!(read(out.path(), &gbae(&fresh.targets).url), b"gbae bytes");
        let root = Root::parse(&fresh.root).unwrap();
        assert_eq!((root.keys.as_slice(), root.expires), ([key().public()].as_slice(), NOW + ROOT_LIFE));
        assert!(!out.path().read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("staged")));
    }

    /// The second publish moves every version forward, keeps the root, and a
    /// machine holding the first accepts it; one inside the root's last
    /// [`RENEW`] adds root 2, which the machine walks to.
    #[test]
    fn a_publish_moves_forward_and_renews_a_root_near_its_expiry() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let m = manifest(src.path(), &[("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]);
        publish(&m, out.path(), &key(), NOW).unwrap();
        let first = client(out.path(), NOW, None).unwrap();
        let m = manifest(src.path(), &[("gbae", 4, "gbae-v2.tar.gz", b"gbae two")]);
        assert_eq!(publish(&m, out.path(), &key(), NOW + DAY), Ok(Published { root: 1, targets: 2, timestamp: 2 }));
        let held = Held { root: &first.root, timestamp: Some(&first.timestamp), targets: Some(&first.targets_bytes) };
        let second = client(out.path(), NOW + DAY, Some(held)).expect("the next publish, past the first's floors");
        assert_eq!(gbae(&second.targets).sequence, 4);

        let late = NOW + ROOT_LIFE - RENEW + 1;
        assert_eq!(publish(&m, out.path(), &key(), late), Ok(Published { root: 2, targets: 3, timestamp: 3 }));
        let renewed = client(out.path(), late, None).expect("root 2, walked from root 1");
        assert_eq!(Root::parse(&renewed.root).unwrap().version, 2);
    }

    #[test]
    fn a_publish_that_would_move_anything_back_is_refused_by_name() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        publish(&manifest(src.path(), &[("gbae", 3, "gbae-v1.tar.gz", b"gbae bytes")]), out.path(), &key(), NOW).unwrap();
        let before = read(out.path(), "timestamp.txt");

        // The client's floors, held as the directory holds them.
        let (name, target, holder) = (String::from("gbae"), String::from("x86_64-unknown-toyos"), repo::Holder::Machine);
        let lower = publish(&manifest(src.path(), &[("gbae", 2, "gbae-v0.tar.gz", b"old")]), out.path(), &key(), NOW);
        let why = repo::Refused::Sequence { name: name.clone(), target: target.clone(), sequence: 2, floor: 3, holder };
        assert!(lower.as_ref().unwrap_err().ends_with(&why.to_string()), "{lower:?}");
        let same = publish(&manifest(src.path(), &[("gbae", 3, "gbae-v1b.tar.gz", b"other")]), out.path(), &key(), NOW);
        let why = repo::Refused::Reissued { name, target, sequence: 3, holder };
        assert!(same.as_ref().unwrap_err().ends_with(&why.to_string()), "{same:?}");
        let rewritten = publish(&manifest(src.path(), &[("gbae", 4, "gbae-v1.tar.gz", b"rewritten")]), out.path(), &key(), NOW);
        assert!(rewritten.as_ref().unwrap_err().contains("refuses archives/gbae-v1.tar.gz: "), "{rewritten:?}");
        let other = publish(&manifest(src.path(), &[("gbae", 4, "gbae-v4.tar.gz", b"new")]), out.path(), &Key::throwaway_from([4; 32]), NOW);
        assert!(other.as_ref().unwrap_err().contains("a publish rotates no key"), "{other:?}");
        let unknown = src.path().join("unknown.toml");
        fs::write(&unknown, "[[package]]\nname = \"gbae\"\nowner = \"me\"\n").unwrap();
        assert!(publish(&unknown, out.path(), &key(), NOW).unwrap_err().contains("owner"));
        let bad_name = publish(&manifest(src.path(), &[("Gbae", 1, "g.tar.gz", b"g")]), out.path(), &key(), NOW);
        assert!(bad_name.as_ref().unwrap_err().contains("refuses what this publish would leave"), "{bad_name:?}");

        assert_eq!(read(out.path(), "timestamp.txt"), before, "a refused publish wrote nothing");
        assert!(!out.path().join("archives/gbae-v0.tar.gz").exists(), "a refused publish wrote no archive");
    }

    /// Two rows whose archives share a file name share its url: one archive,
    /// or a refusal, never the second row's bytes under the first's SHA-256.
    #[test]
    fn two_rows_naming_one_archive_with_other_bytes_are_refused() {
        let src = TempDir::new("publish-src");
        let out = TempDir::new("publish-repo");
        let rows = [("gbae", 1, "a/x.tar.gz", &b"gbae bytes"[..]), ("snake", 1, "b/x.tar.gz", b"snake bytes")];
        let clash = publish(&manifest(src.path(), &rows), out.path(), &key(), NOW);
        let why = repo::Refused::ArchiveShort { length: 11, got: 10 };
        assert!(clash.as_ref().unwrap_err().ends_with(&format!("refuses archives/x.tar.gz: {why}")), "{clash:?}");
        assert!(!out.path().join("timestamp.txt").exists(), "a refused publish wrote nothing");

        let rows = [("gbae", 1, "a/x.tar.gz", &b"one archive"[..]), ("snake", 1, "b/x.tar.gz", b"one archive")];
        publish(&manifest(src.path(), &rows), out.path(), &key(), NOW).expect("one archive, named twice");
        assert_eq!(read(out.path(), "archives/x.tar.gz"), b"one archive");
    }
}
