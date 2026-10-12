//! What every content-addressed product of the host ([`Keyed`]) shares: the
//! store it lives in ([`host`]), its [`Key`], and the sweep that keeps, of each
//! kind, what is in use and the [`kept`] last used, and removes every other.
//!
//! **One store per host, outside every checkout** ([`host`]). Whichever
//! checkout or clone first needs a product makes it, and every other finds it:
//! nothing in a store names the checkout that made it, in a record or as the
//! target of a link. A runner's store is in its checkout (`build/release.rs`).
//! Beside the keyed kinds, `metal/` holds each metal bench's record
//! (`toyos-metal`'s `timings`), which no sweep walks: it is no product and has no key.
//!
//! **A key hashes what its product's build reads**: its sources, the
//! configuration its build is given, the tools that run that build and the keys
//! of the products it reads; and it is known before the product is. So a
//! product found under its key, made here or restored by a CI runner from
//! another run's cache, is the one this tree's build would make. An input a
//! build reads and its key does not is a defect of the key.
//!
//! A product lives at `<store>/<kind>/<key>/`, whole once its maker renamed it
//! there; any other name beginning `<key>.` is one half-made or half-removed. A
//! whole one is renamed out of the way before anything in it is removed
//! ([`retire`]), so a sweep that is stopped leaves nothing at `<key>/` but what
//! was made. A product may be read-only, directories and all: removing one gives
//! its directories back their write permission first ([`remove`]). A hidden name
//! in a store is a desktop's (Finder writes `.DS_Store` into any directory it
//! shows) and the sweep leaves it; any other name no key owns refuses the sweep.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Guard, Keyed};

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

/// How many of `kind`'s products a sweep keeps by date, the last used, in use
/// or not.
const fn kept(kind: Keyed) -> usize {
    match kind {
        Keyed::Sysroot => 24,
        Keyed::Freestanding => 4,
        Keyed::Compiler | Keyed::Llvm | Keyed::HostedClang => 2,
    }
}

/// This host's store: `~/.cache/toyos`. Nothing else in the environment names
/// another, so no build makes a second toolchain beside the first; a home that
/// is no absolute path is refused, since its store would be another directory
/// for every checkout that asks.
pub fn host() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("HOME is unset, so nothing says where this host keeps its toolchains"));
    assert!(home.is_absolute(), "HOME is {}, which is no absolute path, and the host's store is under it", home.display());
    home.join(".cache/toyos")
}

/// `kind`'s `key` in `store`, held in use: made by `make` first when `defect`
/// says it is not whole (`buildlock::keyed_made`), and then the store swept of
/// that kind. Only a placement adds to a kind, so once one is placed its kind
/// holds no more than [`kept`] products but those in use or used while it was
/// swept.
pub fn made(store: &Path, kind: Keyed, key: &Key, defect: impl Fn() -> Option<String>, mut make: impl FnMut()) -> Guard {
    let mut placed = false;
    let using = buildlock::keyed_made(store, kind, key, defect, || {
        make();
        placed = true;
    });
    if placed {
        for gone in sweep(store, kind) {
            eprintln!(
                "Removed {} {}: it is not among the {} last used, or it was never whole",
                kind.name(),
                gone.display(),
                kept(kind)
            );
        }
    }
    using
}

/// Remove from `store` every `kind` that is not among the [`kept`] last used
/// and that nobody is making or using, and every half-made or half-removed one
/// nobody is making, the least lately used first. Returns what went. A `store`
/// holding a name that is neither a key's nor hidden is refused before anything
/// goes.
pub fn sweep(store: &Path, kind: Keyed) -> Vec<PathBuf> {
    sweep_by(store, kind, remove)
}

/// [`sweep`], removing with `remove`, so a test can stop it.
pub(crate) fn sweep_by(store: &Path, kind: Keyed, remove: impl Fn(&Path) + Copy) -> Vec<PathBuf> {
    let dir = kind.store(store);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read {}: {e}", dir.display()),
    };
    let mut by_key: BTreeMap<Key, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
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
                dir.display(),
                kind.name(),
            );
        };
        by_key.entry(key).or_default().push(name);
    }
    let mut by_use: Vec<_> = by_key.into_iter().map(|(key, names)| (buildlock::keyed_used(store, kind, &key), key, names)).collect();
    by_use.sort();
    let whole: Vec<_> = by_use.iter().filter(|(_, key, names)| names.iter().any(|name| name == key.as_str())).collect();
    // The least lately used of the whole products kept by date: any older one goes.
    let cut = whole.len().checked_sub(kept(kind)).map(|older| (whole[older].0, whole[older].1.clone()));
    let mut removed = Vec::new();
    for (_, key, mut names) in by_use {
        let Some(_idle) = buildlock::keyed_idle(store, kind, &key) else { continue };
        // Read again under the key's lock: a use since the ranking has dated the key past the cut.
        let used = buildlock::keyed_used(store, kind, &key);
        if cut.as_ref().is_none_or(|(at, last)| (used, &key) >= (*at, last)) {
            names.retain(|name| name != key.as_str());
        }
        // The whole one last: its `retire` goes where a stopped one's remains were.
        names.sort_by_key(|name| name == key.as_str());
        for name in names {
            let path = dir.join(&name);
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

/// Remove `path`, a file or a directory and all it holds, read-only or not;
/// that nothing is there is not an error.
///
/// A writer on this host — the leftovers are `.DS_Store` files — can put a file
/// into a directory while it is being emptied, so a plain recursive delete finds a directory it has just emptied not empty and
/// stops halfway. The removal runs again over what is left, at most [`PASSES`]
/// times; a tree still refusing after that has a writer this cannot outrun, and
/// the panic says so.
pub fn remove(path: &Path) {
    for pass in 1..=PASSES {
        let removed = match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => {
                writable(path);
                fs::remove_dir_all(path)
            }
            Ok(_) => fs::remove_file(path),
            Err(e) => Err(e),
        };
        match removed {
            Ok(()) => return,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) if e.kind() == ErrorKind::DirectoryNotEmpty && pass < PASSES => {
                eprintln!("{} gained files while it was removed ({e}); removing again", path.display());
            }
            Err(e) => panic!("remove {}: {e}, after {pass} pass(es)", path.display()),
        }
    }
}

/// How many times [`remove`] runs over a tree that keeps refusing.
const PASSES: usize = 10;

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
pub(crate) mod tests {
    use std::cell::Cell;
    use std::time::{Duration, SystemTime};

    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::compiler::tests::write;

    /// Date the last use of `kind`'s `key` in `store` at `ago` before now.
    pub(crate) fn last_used(store: &Path, kind: Keyed, key: &Key, ago: Duration) {
        let lock = buildlock::keyed_lock_path(store, kind, key);
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let lock = fs::File::options().create(true).truncate(false).write(true).open(lock).unwrap();
        lock.set_modified(SystemTime::now() - ago).unwrap();
    }

    /// Longer ago than any use a test makes.
    pub(crate) const LONG_AGO: Duration = Duration::from_secs(365 * 24 * 60 * 60);

    const HOUR: Duration = Duration::from_secs(60 * 60);

    /// Place in `store` the [`kept`] products of `kind` a sweep keeps by date,
    /// each used now, so that one of `kind` last used before them is not.
    pub(crate) fn used_lately(store: &Path, kind: Keyed) {
        for n in 0..kept(kind) {
            let key = Key::of(format!("used lately, {n}").as_bytes());
            fs::create_dir_all(kind.store(store).join(&key)).unwrap();
            last_used(store, kind, &key, Duration::ZERO);
        }
    }

    /// `n` products of `kind` in `store`, each used an hour before the one
    /// before it, the first an hour ago.
    fn used_hourly(store: &Path, kind: Keyed, n: usize) -> Vec<Key> {
        let keys: Vec<Key> = (0..n).map(|n| Key::of(format!("used {n} hours ago").as_bytes())).collect();
        for (hours, key) in (1..).zip(&keys) {
            fs::create_dir_all(kind.store(store).join(key)).unwrap();
            last_used(store, kind, key, HOUR * hours);
        }
        keys
    }

    /// **A sweep keeps the [`kept`] last used and what is in use, and removes
    /// every other, the least lately used first**: of products all used within
    /// the week, those older than the newest `kept` go, read-only or not, one
    /// no lock ever dated is dated by the sweep that finds it and stays; one
    /// somebody uses stays however long ago that use began, and goes once let
    /// go; and a half-made or half-removed one goes however lately its key was
    /// used.
    #[test]
    fn a_sweep_keeps_the_last_used_and_what_is_in_use() {
        let store = TempDir::new("sweep");
        let dir = Keyed::Sysroot.store(&store);
        let at = |key: &Key, rest: &str| dir.join(format!("{key}{rest}"));
        let recent = used_hourly(&store, Keyed::Sysroot, kept(Keyed::Sysroot) - 1);
        let [undated, older, oldest, in_use, gone] =
            ["undated", "older", "oldest", "in use", "gone"].map(|name| Key::of(name.as_bytes()));
        let names = [(&undated, ""), (&older, ""), (&oldest, ""), (&in_use, ""), (&recent[0], ".partial"), (&gone, ".swept"), (&oldest, ".swept")];
        for (key, rest) in names {
            fs::create_dir_all(at(key, rest).join("sub")).unwrap();
        }
        for read_only in [at(&oldest, "/sub"), at(&oldest, ""), at(&recent[0], ".partial")] {
            fs::set_permissions(read_only, fs::Permissions::from_mode(0o555)).unwrap();
        }
        last_used(&store, Keyed::Sysroot, &older, HOUR * 48);
        last_used(&store, Keyed::Sysroot, &oldest, HOUR * 72);
        let user = buildlock::tests::sysroot_used_elsewhere(&store, &in_use);
        last_used(&store, Keyed::Sysroot, &in_use, LONG_AGO);

        let removed = sweep(&store, Keyed::Sysroot);
        let want = [at(&oldest, ".swept"), at(&oldest, ""), at(&older, ""), at(&recent[0], ".partial"), at(&gone, ".swept")];
        assert_eq!(removed, want, "the sweep kept more than {} idle products, or took them out of the order of their use", kept(Keyed::Sysroot));
        for stays in recent.iter().chain([&undated, &in_use]) {
            assert!(dir.join(stays).is_dir(), "{stays} was swept");
        }
        for gone in want {
            assert!(!gone.exists(), "{} was reported and kept", gone.display());
        }
        user.release();
        assert_eq!(sweep(&store, Keyed::Sysroot), [dir.join(&in_use)]);
    }

    /// **A use dates its key**: of one product more than a sweep keeps by
    /// date, the least lately used is used again and stays, and the next goes;
    /// a sweep of another kind takes none of them.
    #[test]
    fn a_product_used_again_is_kept() {
        let store = TempDir::new("sweep-used");
        let dir = Keyed::Sysroot.store(&store);
        let keys = used_hourly(&store, Keyed::Sysroot, kept(Keyed::Sysroot) + 1);
        let [.., next, again] = &keys[..] else { unreachable!() };
        assert_eq!(sweep(&store, Keyed::Compiler), Vec::<PathBuf>::new());
        // The use is another process's, and one sweep decides every product:
        // none here takes a key whose lock this process has held.
        buildlock::tests::sysroot_used_elsewhere(&store, again).release();
        assert_eq!(sweep(&store, Keyed::Sysroot), [dir.join(next)], "a product was swept after a use, or one more than the sweep keeps was kept");
        assert!(dir.join(again).is_dir());
    }

    /// **A use between a sweep's ranking and its key's lock keeps the key**:
    /// of two products the ranking leaves out, the second is used by another
    /// process while the first is removed, and stays.
    #[test]
    fn a_product_used_while_the_sweep_runs_is_kept() {
        let store = TempDir::new("sweep-race");
        let dir = Keyed::Sysroot.store(&store);
        let keys = used_hourly(&store, Keyed::Sysroot, kept(Keyed::Sysroot) + 2);
        let [.., next, oldest] = &keys[..] else { unreachable!() };
        let used = Cell::new(false);
        let removing = |path: &Path| {
            if !used.replace(true) {
                buildlock::tests::sysroot_used_elsewhere(&store, next).release();
            }
            remove(path);
        };
        assert_eq!(sweep_by(&store, Keyed::Sysroot, removing), [dir.join(oldest)], "a product used after the sweep ranked it was removed");
        assert!(dir.join(next).is_dir());
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
    /// the sweep.**
    #[test]
    fn a_sweep_leaves_hidden_names_and_refuses_names_no_key_owns() {
        let store = TempDir::new("strangers");
        let dir = Keyed::Sysroot.store(&store);
        let unused = Key::parse("0123456789abcdef").unwrap();
        let orphan = dir.join(&unused);
        let hidden = [".DS_Store", "._0123456789abcdef"].map(|name| dir.join(name));
        for path in &hidden {
            write(path, "a desktop's");
        }
        fs::create_dir_all(orphan.join("sub")).unwrap();
        last_used(&store, Keyed::Sysroot, &unused, LONG_AGO);
        used_lately(&store, Keyed::Sysroot);
        assert_eq!(sweep(&store, Keyed::Sysroot), std::slice::from_ref(&orphan));
        for path in &hidden {
            assert!(path.is_file(), "{} was swept", path.display());
        }

        for stranger in ["notes", "0123456789abcde", "0123456789abcdeg", "0123456789abcdef0", "0123456789abcdef copy"] {
            fs::create_dir_all(&orphan).unwrap();
            fs::create_dir(dir.join(stranger)).unwrap();
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(&store, Keyed::Sysroot)));
            assert!(refused.is_err(), "{stranger:?} was taken for a key");
            assert!(orphan.is_dir() && dir.join(stranger).is_dir(), "a store holding {stranger:?} was swept");
            fs::remove_dir(dir.join(stranger)).unwrap();
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
