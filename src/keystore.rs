//! What every content-addressed product of the host ([`Keyed`]) shares: its
//! [`Key`], the record each worktree keeps of the key it uses, and the sweep
//! that removes a key no registered worktree records and nobody is making or
//! using.
//!
//! A product lives at `<store>/<key>/`, whole once its maker renamed it there;
//! any other name beginning `<key>.` is one half-made or half-removed. A whole
//! one is renamed out of the way before anything in it is removed ([`retire`]),
//! so a sweep that is stopped leaves nothing at `<key>/` but what was made. A
//! product may be read-only, directories and all: removing one gives its
//! directories back their write permission first ([`remove`]). A hidden name in
//! a store is a desktop's (Finder writes `.DS_Store` into any directory it
//! shows) and the sweep leaves it; any other name no key owns refuses the sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::buildlock::{self, Guard, Keyed};
use crate::sysroot::git_out;

/// A product's key: the first 16 hex digits of the SHA-256 of what it is made
/// from, and no other string, so no name in a store that is not a key is taken
/// or locked as one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// The key of what `sources` spell out.
    pub fn of(sources: &[u8]) -> Self {
        Self(crate::sysroot::short(sources))
    }

    /// `name` as a key, if it is one.
    pub fn parse(name: &str) -> Option<Self> {
        let digits = name.len() == 16 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        digits.then(|| Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for Key {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

/// Where `root`'s builds record the key of `kind` they use.
fn record_path(root: &Path, kind: Keyed) -> PathBuf {
    root.join(match kind {
        Keyed::Sysroot => "target/toyos-sysroot-key",
        Keyed::Compiler => "target/toyos-compiler-key",
        Keyed::Llvm => "target/toyos-llvm-key",
    })
}

/// Record that `root` uses `kind`'s `key`: whole or not at all, so a sweep
/// never reads a record half-written.
pub fn record(root: &Path, kind: Keyed, key: &Key) {
    record_by(root, kind, key, |path, key| fs::write(path, key).unwrap_or_else(|e| panic!("write {}: {e}", path.display())));
}

static TEMPS: AtomicU64 = AtomicU64::new(0);

/// [`record`], writing with `write`, so a test can stop it.
fn record_by(root: &Path, kind: Keyed, key: &Key, write: impl FnOnce(&Path, &str)) {
    let path = record_path(root, kind);
    let dir = path.parent().expect("a file under target/");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    // Its own name: concurrent writers of one record never share a temp file.
    let written = path.with_extension(format!("{}.{}.new", std::process::id(), TEMPS.fetch_add(1, Ordering::Relaxed)));
    write(&written, key.as_str());
    fs::rename(&written, &path).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", written.display(), path.display()));
}

/// Record that `root` uses `kind`'s `key`, whose product is `store/<key>`, and
/// hold it in use: made by `make` first when `defect` says it is not whole
/// (`buildlock::keyed_made`), and then `store` swept, so the product one
/// replaced goes once nobody names it.
pub fn made(
    root: &Path,
    kind: Keyed,
    store: &Path,
    key: &Key,
    defect: impl Fn() -> Option<String>,
    mut make: impl FnMut(),
) -> Guard {
    record(root, kind, key);
    let mut placed = false;
    let using = buildlock::keyed_made(root, kind, key, defect, || {
        make();
        placed = true;
    });
    if placed {
        for gone in sweep(root, kind, store) {
            eprintln!("Removed {} {}: no worktree names it", kind.name(), gone.display());
        }
    }
    using
}

/// Record that `root` uses no `kind` of its own.
pub fn forget(root: &Path, kind: Keyed) {
    let path = record_path(root, kind);
    match fs::remove_file(&path) {
        Err(e) if e.kind() != ErrorKind::NotFound => panic!("remove {}: {e}", path.display()),
        _ => {}
    }
}

/// The key of `kind` `root` records, if it records one.
pub fn recorded(root: &Path, kind: Keyed) -> Option<Key> {
    let path = record_path(root, kind);
    match fs::read_to_string(&path) {
        Ok(text) => {
            Some(Key::parse(text.trim()).unwrap_or_else(|| panic!("{} records {text:?}, which is no key", path.display())))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

/// Remove from `store` every `kind` no registered worktree of `root` records
/// and nobody is making or using, and every half-made or half-removed one
/// nobody is making. Returns what went. A `store` holding a name that is
/// neither a key's nor hidden is refused before anything goes.
pub fn sweep(root: &Path, kind: Keyed, store: &Path) -> Vec<PathBuf> {
    sweep_by(root, kind, store, remove)
}

/// [`sweep`], removing with `remove`, so a test can stop it.
pub(crate) fn sweep_by(root: &Path, kind: Keyed, store: &Path, remove: impl Fn(&Path) + Copy) -> Vec<PathBuf> {
    let entries = match fs::read_dir(store) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read {}: {e}", store.display()),
    };
    let named: BTreeSet<Key> = git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter_map(|w| recorded(Path::new(w), kind))
        .collect();
    let mut by_key: BTreeMap<Key, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", store.display()));
        let name = entry.file_name();
        // No key begins with a dot.
        if name.as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let owned = name.to_str().and_then(|name| {
            let key = Key::parse(name.split_once('.').map_or(name, |(key, _)| key))?;
            Some((key, name.to_string()))
        });
        let Some((key, name)) = owned else {
            panic!(
                "{} holds {name:?}, which is no {} key's and is not hidden: something other than a \
                 build writes there, and nothing in it is swept until that is gone",
                store.display(),
                kind.name(),
            );
        };
        by_key.entry(key).or_default().push(name);
    }
    let mut removed = Vec::new();
    for (key, mut names) in by_key {
        names.retain(|name| name != key.as_str() || !named.contains(&key));
        if names.is_empty() {
            continue;
        }
        let Some(_idle) = buildlock::keyed_idle(root, kind, &key) else { continue };
        // The whole one last: its `retire` goes where a stopped one's remains were.
        names.sort_by_key(|name| name == key.as_str());
        for name in names {
            let path = store.join(&name);
            if name == key.as_str() {
                retire_by(&path, remove);
            } else {
                remove(&path);
            }
            removed.push(path);
        }
    }
    removed
}

/// Remove the directory `path` if it is there, renamed to `<path>.swept` before
/// anything in it is removed; what a stopped one left there goes first.
pub fn retire(path: &Path) {
    retire_by(path, remove);
}

/// [`retire`], removing with `remove`, so a test can stop it.
fn retire_by(path: &Path, remove: impl Fn(&Path)) {
    let away = path.with_extension("swept");
    if away.exists() {
        remove(&away);
    }
    if path.exists() {
        fs::rename(path, &away).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", path.display(), away.display()));
        remove(&away);
    }
}

/// Remove the directory `path` and all it holds, read-only or not.
pub fn remove(path: &Path) {
    writable(path);
    fs::remove_dir_all(path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
}

/// Give `dir` and every directory under it back its owner's write permission.
pub(crate) fn writable(dir: &Path) {
    let meta = fs::metadata(dir).unwrap_or_else(|e| panic!("stat {}: {e}", dir.display()));
    let mut permissions = meta.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(dir, permissions).unwrap_or_else(|e| panic!("chmod {}: {e}", dir.display()));
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        if entry.file_type().unwrap_or_else(|e| panic!("stat {}: {e}", entry.path().display())).is_dir() {
            writable(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::compiler::tests::{git, write};

    /// A key no registered worktree records goes, read-only or not, and so does
    /// a half-made or half-removed one; a key a worktree records stays, and so
    /// does one somebody is using; an unreadable record is refused, never read
    /// as "names nothing".
    #[test]
    fn a_sweep_removes_what_no_worktree_names_and_nobody_uses() {
        let root = TempDir::new("sweep");
        git(&root, &["init", "-q"]);
        write(&root.join("f"), "x\n");
        git(&root, &["add", "f"]);
        git(&root, &["commit", "-qm", "init"]);
        let linked = root.join("linked");
        git(&root, &["worktree", "add", "-q", "-b", "wt", linked.to_str().unwrap()]);

        let dir = root.join("store");
        let [named, linked_named, in_use, orphan, gone] =
            ["named", "linked-named", "in-use", "orphan", "gone"].map(|name| Key::of(name.as_bytes()));
        let at = |key: &Key, rest: &str| dir.join(format!("{key}{rest}"));
        for (key, rest) in [(&named, ""), (&linked_named, ""), (&in_use, ""), (&orphan, ""), (&named, ".partial"), (&gone, ".swept"), (&orphan, ".swept")] {
            fs::create_dir_all(at(key, rest).join("sub")).unwrap();
        }
        for read_only in [at(&orphan, "/sub"), at(&orphan, ""), at(&named, ".partial")] {
            fs::set_permissions(read_only, fs::Permissions::from_mode(0o555)).unwrap();
        }
        record(&root, Keyed::Sysroot, &named);
        record(&linked, Keyed::Sysroot, &linked_named);
        let user = buildlock::tests::sysroot_used_elsewhere(&root, &in_use);

        let mut removed = sweep(&root, Keyed::Sysroot, &dir);
        removed.sort();
        let mut want = [at(&gone, ".swept"), at(&named, ".partial"), at(&orphan, ""), at(&orphan, ".swept")];
        want.sort();
        assert_eq!(removed, want);
        for stays in [&named, &linked_named, &in_use] {
            assert!(dir.join(stays).is_dir(), "{stays} was swept");
        }
        for gone in want {
            assert!(!gone.exists(), "{} was reported and kept", gone.display());
        }
        user.release();
        assert_eq!(sweep(&root, Keyed::Sysroot, &dir), [dir.join(&in_use)]);

        fs::remove_file(record_path(&linked, Keyed::Sysroot)).unwrap();
        fs::create_dir(record_path(&linked, Keyed::Sysroot)).unwrap();
        let unreadable = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(&root, Keyed::Sysroot, &dir)));
        assert!(unreadable.is_err(), "an unreadable record was read as naming nothing");
        assert!(dir.join(&linked_named).is_dir(), "an unreadable record's key was swept");
    }

    /// **A key is the 16 lowercase hex digits [`Key::of`] gives, and no other
    /// name parses as one.**
    #[test]
    fn only_what_key_of_gives_is_a_key() {
        let key = Key::of(b"sources");
        assert_eq!(Key::parse(key.as_str()), Some(key.clone()));
        for name in ["", ".DS_Store", "0123456789abcde", "0123456789abcdef0", "0123456789ABCDEF", "0123456789abcdeg", "0123456789abcdef.partial"] {
            assert_eq!(Key::parse(name), None, "{name:?} parsed as a key");
        }
    }

    /// **A hidden name is a desktop's, and any other name no key owns refuses
    /// the sweep**: Finder writes `.DS_Store` into whatever directory it shows,
    /// a store included, and it is neither swept nor taken for a key; a name
    /// no build writes is something else writing there, and nothing is swept
    /// while it is.
    #[test]
    fn a_sweep_leaves_hidden_names_and_refuses_names_no_key_owns() {
        let root = TempDir::new("strangers");
        git(&root, &["init", "-q"]);
        let dir = root.join("store");
        let orphan = dir.join("0123456789abcdef");
        let hidden = [".DS_Store", "._0123456789abcdef"].map(|name| dir.join(name));
        for path in &hidden {
            write(path, "a desktop's");
        }
        fs::create_dir_all(orphan.join("sub")).unwrap();
        assert_eq!(sweep(&root, Keyed::Sysroot, &dir), [orphan.clone()]);
        for path in &hidden {
            assert!(path.is_file(), "{} was swept", path.display());
        }

        for stranger in ["notes", "0123456789abcde", "0123456789abcdeg", "0123456789abcdef0"] {
            fs::create_dir_all(&orphan).unwrap();
            fs::create_dir(dir.join(stranger)).unwrap();
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(&root, Keyed::Sysroot, &dir)));
            assert!(refused.is_err(), "{stranger:?} was taken for a key");
            assert!(orphan.is_dir() && dir.join(stranger).is_dir(), "a store holding {stranger:?} was swept");
            fs::remove_dir(dir.join(stranger)).unwrap();
        }
    }

    /// **A record stopped mid-write leaves the one before it readable**: the
    /// new key is written beside it and renamed over it whole. A record that
    /// holds no key is refused, never read as one.
    #[test]
    fn a_stopped_record_leaves_the_one_before_it() {
        let root = TempDir::new("record");
        let (before, after) = (Key::of(b"before"), Key::of(b"after"));
        record(&root, Keyed::Llvm, &before);
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            record_by(&root, Keyed::Llvm, &after, |path, key| {
                fs::write(path, &key[..8]).unwrap();
                panic!("stopped");
            })
        }));
        assert!(stopped.is_err(), "the stand-in write was never asked");
        assert_eq!(recorded(&root, Keyed::Llvm), Some(before), "a record stopped mid-write was read");
        record(&root, Keyed::Llvm, &after);
        assert_eq!(recorded(&root, Keyed::Llvm), Some(after.clone()));

        fs::write(record_path(&root, Keyed::Llvm), &after.as_str()[..8]).unwrap();
        let refused = std::panic::catch_unwind(|| recorded(&root, Keyed::Llvm));
        assert!(refused.is_err(), "a record holding no key was read as one");
    }

    /// Concurrent records of one kind never share a temp file, so none fails
    /// and the record holds one of the keys written whole.
    #[test]
    fn concurrent_records_of_one_kind_all_land() {
        let root = TempDir::new("race");
        let keys: Vec<Key> = (0..8u8).map(|i| Key::of(&[i])).collect();
        for _ in 0..50 {
            std::thread::scope(|s| {
                let handles: Vec<_> = keys.iter().map(|key| s.spawn(|| record(&root, Keyed::Llvm, key))).collect();
                for h in handles {
                    h.join().expect("a concurrent record panicked");
                }
            });
            let got = recorded(&root, Keyed::Llvm).unwrap();
            assert!(keys.contains(&got), "the record holds {got:?}, none of the keys written");
        }
    }

    /// **A retire stopped halfway leaves nothing at the name it removes**: what
    /// it removes is out of the way before anything in it goes, so a stopped
    /// sweep leaves nothing that passes for whole, and the next retire takes
    /// what the stopped one left.
    #[test]
    fn a_stopped_retire_leaves_nothing_at_its_name() {
        let store = TempDir::new("retire");
        let whole = store.join("key");
        write(&whole.join("SOURCE"), "key\n");
        write(&whole.join("lib/libLLVMCore.a"), "core");
        let stopped = std::panic::catch_unwind(|| retire_by(&whole, |_| panic!("stopped")));
        assert!(stopped.is_err(), "the stand-in removal was never asked");
        assert!(!whole.exists(), "a stopped retire left {}", whole.display());
        retire(&whole);
        assert!(!whole.with_extension("swept").exists(), "the next retire kept what the stopped one left");
    }
}
