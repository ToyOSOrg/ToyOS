//! What every content-addressed product of the host ([`Keyed`]) shares: the
//! record each worktree keeps of the key it uses, and the sweep that removes a
//! key no registered worktree records and nobody is making or using.
//!
//! A product lives at `<store>/<key>/`, whole once its maker renamed it there;
//! any other name beginning `<key>.` is one half-made or half-removed. A whole
//! one is renamed out of the way before anything in it is removed ([`retire`]),
//! so a sweep that is stopped leaves nothing at `<key>/` but what was made. A
//! product may be read-only, directories and all: removing one gives its
//! directories back their write permission first ([`remove`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Guard, Keyed};
use crate::sysroot::git_out;

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
pub fn record(root: &Path, kind: Keyed, key: &str) {
    record_by(root, kind, key, |path, key| fs::write(path, key).unwrap_or_else(|e| panic!("write {}: {e}", path.display())));
}

/// [`record`], writing with `write`, so a test can stop it.
fn record_by(root: &Path, kind: Keyed, key: &str, write: impl FnOnce(&Path, &str)) {
    let path = record_path(root, kind);
    let dir = path.parent().expect("a file under target/");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let written = path.with_extension("new");
    write(&written, key);
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
    key: &str,
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
pub fn recorded(root: &Path, kind: Keyed) -> Option<String> {
    let path = record_path(root, kind);
    match fs::read_to_string(&path) {
        Ok(key) => Some(key.trim().to_string()),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

/// Remove from `store` every `kind` no registered worktree of `root` records
/// and nobody is making or using, and every half-made or half-removed one
/// nobody is making. Returns what went.
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
    let named: BTreeSet<String> = git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter_map(|w| recorded(Path::new(w), kind))
        .collect();
    let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", store.display()));
        let name = entry
            .file_name()
            .into_string()
            .unwrap_or_else(|n| panic!("{} holds {n:?}, which names no key", store.display()));
        let key = name.split_once('.').map_or(name.as_str(), |(key, _)| key).to_string();
        by_key.entry(key).or_default().push(name);
    }
    let mut removed = Vec::new();
    for (key, mut names) in by_key {
        names.retain(|name| *name != key || !named.contains(&key));
        if names.is_empty() {
            continue;
        }
        let Some(_idle) = buildlock::keyed_idle(root, kind, &key) else { continue };
        // The whole one last: its `retire` goes where a stopped one's remains were.
        names.sort_by_key(|name| *name == key);
        for name in names {
            let path = store.join(&name);
            if name == key {
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
        for name in ["named", "linked-named", "in-use", "orphan", "named.partial", "gone.swept", "orphan.swept"] {
            fs::create_dir_all(dir.join(name).join("sub")).unwrap();
        }
        for read_only in ["orphan/sub", "orphan", "named.partial"] {
            fs::set_permissions(dir.join(read_only), fs::Permissions::from_mode(0o555)).unwrap();
        }
        record(&root, Keyed::Sysroot, "named");
        record(&linked, Keyed::Sysroot, "linked-named");
        let user = buildlock::tests::sysroot_used_elsewhere(&root, "in-use");

        let mut removed = sweep(&root, Keyed::Sysroot, &dir);
        removed.sort();
        let want = ["gone.swept", "named.partial", "orphan", "orphan.swept"].map(|n| dir.join(n));
        assert_eq!(removed, want);
        for stays in ["named", "linked-named", "in-use"] {
            assert!(dir.join(stays).is_dir(), "{stays} was swept");
        }
        for gone in want {
            assert!(!gone.exists(), "{} was reported and kept", gone.display());
        }
        user.release();
        assert_eq!(sweep(&root, Keyed::Sysroot, &dir), [dir.join("in-use")]);

        fs::remove_file(record_path(&linked, Keyed::Sysroot)).unwrap();
        fs::create_dir(record_path(&linked, Keyed::Sysroot)).unwrap();
        let unreadable = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(&root, Keyed::Sysroot, &dir)));
        assert!(unreadable.is_err(), "an unreadable record was read as naming nothing");
        assert!(dir.join("linked-named").is_dir(), "an unreadable record's key was swept");
    }

    /// **A record stopped mid-write leaves the one before it readable**: the
    /// new key is written beside it and renamed over it whole.
    #[test]
    fn a_stopped_record_leaves_the_one_before_it() {
        let root = TempDir::new("record");
        record(&root, Keyed::Llvm, "0123456789abcdef");
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            record_by(&root, Keyed::Llvm, "fedcba9876543210", |path, key| {
                fs::write(path, &key[..8]).unwrap();
                panic!("stopped");
            })
        }));
        assert!(stopped.is_err(), "the stand-in write was never asked");
        assert_eq!(recorded(&root, Keyed::Llvm).as_deref(), Some("0123456789abcdef"), "a record stopped mid-write was read");
        record(&root, Keyed::Llvm, "fedcba9876543210");
        assert_eq!(recorded(&root, Keyed::Llvm).as_deref(), Some("fedcba9876543210"));
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
