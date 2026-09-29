//! What every content-addressed product of the host ([`Keyed`]) shares: the
//! record each worktree keeps of the key it uses, and the sweep that removes a
//! key no registered worktree records and nobody is making or using.
//!
//! A product lives at `<store>/<key>/`, whole once its maker renamed it there;
//! any other name beginning `<key>.` is one half-made or half-removed. A whole
//! one is renamed out of the way before anything in it is removed ([`retire`]),
//! so a sweep that is stopped leaves nothing at `<key>/` but what was made.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Keyed};
use crate::sysroot::git_out;

/// Where `root`'s builds record the key of `kind` they use.
fn record_path(root: &Path, kind: Keyed) -> PathBuf {
    root.join(match kind {
        Keyed::Sysroot => "target/toyos-sysroot-key",
        Keyed::Compiler => "target/toyos-compiler-key",
        Keyed::Llvm => "target/toyos-llvm-key",
    })
}

/// Record that `root` uses `kind`'s `key`.
pub fn record(root: &Path, kind: Keyed, key: &str) {
    let path = record_path(root, kind);
    let dir = path.parent().expect("a file under target/");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    fs::write(&path, key).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
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
                retire(&path);
            } else {
                fs::remove_dir_all(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
            }
            removed.push(path);
        }
    }
    removed
}

/// Remove the directory `path` if it is there, renamed to `<path>.swept` before
/// anything in it is removed; what a stopped one left there goes first.
pub fn retire(path: &Path) {
    let away = path.with_extension("swept");
    if away.exists() {
        fs::remove_dir_all(&away).unwrap_or_else(|e| panic!("remove {}: {e}", away.display()));
    }
    if path.exists() {
        fs::rename(path, &away).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", path.display(), away.display()));
        fs::remove_dir_all(&away).unwrap_or_else(|e| panic!("remove {}: {e}", away.display()));
    }
}

#[cfg(test)]
mod tests {
    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::compiler::tests::{git, write};

    /// A key no registered worktree records goes, and so does a half-made or
    /// half-removed one; a key a worktree records stays, and so does one
    /// somebody is using; an unreadable record is refused, never read as
    /// "names nothing".
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
}
