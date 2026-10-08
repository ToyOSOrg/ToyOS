//! What every content-addressed product of the host ([`Keyed`]) shares: the
//! store it lives in ([`host`]), its [`Key`], and the sweep that removes a
//! product nobody has used for [`KEPT`] and nobody is making or using.
//!
//! **One store per host, outside every checkout** ([`host`]). Whichever
//! checkout or clone first needs a product makes it, and every other finds it:
//! nothing in a store names the checkout that made it as its owner. A runner's
//! store is in its checkout (`src/release.rs`).
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
use std::time::Duration;

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

/// How long a product nobody uses stays in a store.
const KEPT: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// This host's store: `$TOYOS_STORE`, else `$XDG_CACHE_HOME/toyos`, else
/// `~/.cache/toyos`.
pub fn host() -> PathBuf {
    host_of(|name| std::env::var_os(name).map(PathBuf::from))
}

/// [`host`], with the environment it reads. A variable that is empty is unset,
/// and a store that is no absolute path is refused: it would be another
/// directory for every checkout that asks.
fn host_of(var: impl Fn(&str) -> Option<PathBuf>) -> PathBuf {
    let var = |name: &str| var(name).filter(|value| !value.as_os_str().is_empty());
    let store = var("TOYOS_STORE")
        .or_else(|| var("XDG_CACHE_HOME").map(|cache| cache.join("toyos")))
        .or_else(|| var("HOME").map(|home| home.join(".cache/toyos")))
        .unwrap_or_else(|| panic!("none of TOYOS_STORE, XDG_CACHE_HOME and HOME is set, so nothing says where this host keeps its toolchains"));
    assert!(store.is_absolute(), "the host's store is {}, which is no absolute path", store.display());
    store
}

/// `kind`'s `key` in `store`, held in use: made by `make` first when `defect`
/// says it is not whole (`buildlock::keyed_made`), and then the store swept of
/// that kind, so what nobody has used for [`KEPT`] goes when something is
/// placed beside it.
pub fn made(store: &Path, kind: Keyed, key: &Key, defect: impl Fn() -> Option<String>, mut make: impl FnMut()) -> Guard {
    let mut placed = false;
    let using = buildlock::keyed_made(store, kind, key, defect, || {
        make();
        placed = true;
    });
    if placed {
        for gone in sweep(store, kind) {
            eprintln!(
                "Removed {} {}: nothing used it for {} days, or it was never whole",
                kind.name(),
                gone.display(),
                KEPT.as_secs() / 86_400
            );
        }
    }
    using
}

/// Remove from `store` every `kind` nothing has used for [`KEPT`] and nobody is
/// making or using, and every half-made or half-removed one nobody is making.
/// Returns what went. A `store` holding a name that is neither a key's nor
/// hidden is refused before anything goes.
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
    let mut removed = Vec::new();
    for (key, mut names) in by_key {
        let Some(idle) = buildlock::keyed_idle(store, kind, &key) else { continue };
        // Read under the key's lock: a use that came before it has dated the key.
        if idle.used_within(KEPT) {
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
    use std::time::SystemTime;

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

    /// Longer ago than a store keeps what nobody uses.
    pub(crate) const LONG_AGO: Duration = Duration::from_secs(KEPT.as_secs() + 3600);

    /// **A product goes once nothing has used it for [`KEPT`] and nobody is
    /// using it**, read-only or not, and so does a half-made or half-removed
    /// one however lately its key was used; one used since stays, one somebody
    /// is using stays however long ago that use began, and one no lock ever
    /// dated is dated by the sweep that finds it.
    #[test]
    fn a_sweep_removes_what_nobody_used_for_the_keep_time_and_nobody_uses() {
        let store = TempDir::new("sweep");
        let dir = Keyed::Sysroot.store(&store);
        let [used, in_use, undated, unused, gone] =
            ["used", "in-use", "undated", "unused", "gone"].map(|name| Key::of(name.as_bytes()));
        let at = |key: &Key, rest: &str| dir.join(format!("{key}{rest}"));
        for (key, rest) in [(&used, ""), (&in_use, ""), (&undated, ""), (&unused, ""), (&used, ".partial"), (&gone, ".swept"), (&unused, ".swept")] {
            fs::create_dir_all(at(key, rest).join("sub")).unwrap();
        }
        for read_only in [at(&unused, "/sub"), at(&unused, ""), at(&used, ".partial")] {
            fs::set_permissions(read_only, fs::Permissions::from_mode(0o555)).unwrap();
        }
        last_used(&store, Keyed::Sysroot, &used, KEPT - Duration::from_secs(3600));
        last_used(&store, Keyed::Sysroot, &unused, LONG_AGO);
        let user = buildlock::tests::sysroot_used_elsewhere(&store, &in_use);
        last_used(&store, Keyed::Sysroot, &in_use, LONG_AGO);

        let mut removed = sweep(&store, Keyed::Sysroot);
        removed.sort();
        let mut want = [at(&gone, ".swept"), at(&used, ".partial"), at(&unused, ""), at(&unused, ".swept")];
        want.sort();
        assert_eq!(removed, want);
        for stays in [&used, &in_use, &undated] {
            assert!(dir.join(stays).is_dir(), "{stays} was swept");
        }
        for gone in want {
            assert!(!gone.exists(), "{} was reported and kept", gone.display());
        }
        user.release();
        assert_eq!(sweep(&store, Keyed::Sysroot), [dir.join(&in_use)]);

        last_used(&store, Keyed::Sysroot, &undated, LONG_AGO);
        assert_eq!(sweep(&store, Keyed::Sysroot), [dir.join(&undated)], "the sweep that first saw a product did not date it");
    }

    /// **A use dates its key**: one last used longer ago than [`KEPT`] and
    /// then used again stays, and a sweep of another kind takes none of this
    /// one.
    #[test]
    fn a_product_used_again_is_kept() {
        let store = TempDir::new("sweep-used");
        let key = Key::of(b"a product");
        let dir = Keyed::Sysroot.store(&store).join(&key);
        fs::create_dir_all(&dir).unwrap();
        last_used(&store, Keyed::Sysroot, &key, LONG_AGO);
        assert_eq!(sweep(&store, Keyed::Compiler), Vec::<PathBuf>::new());
        // The use is another process's: this one never holds the key's lock.
        buildlock::tests::sysroot_used_elsewhere(&store, &key).release();
        assert_eq!(sweep(&store, Keyed::Sysroot), Vec::<PathBuf>::new(), "a product was swept after a use");
        last_used(&store, Keyed::Sysroot, &key, LONG_AGO);
        assert_eq!(sweep(&store, Keyed::Sysroot), [dir]);
    }

    /// **The store is the one the environment names first**: `TOYOS_STORE`,
    /// then the user's cache directory, then the one under `HOME`; an empty
    /// variable names nothing, and a relative store, or none, is refused.
    #[test]
    fn the_host_s_store_is_the_first_the_environment_names() {
        let env = |set: &[(&str, &str)]| {
            let set: Vec<(String, PathBuf)> = set.iter().map(|(name, value)| (name.to_string(), PathBuf::from(value))).collect();
            move |name: &str| set.iter().find(|(set, _)| set == name).map(|(_, value)| value.clone())
        };
        let all = [("TOYOS_STORE", "/s"), ("XDG_CACHE_HOME", "/x"), ("HOME", "/h")];
        assert_eq!(host_of(env(&all)), Path::new("/s"));
        assert_eq!(host_of(env(&all[1..])), Path::new("/x/toyos"));
        assert_eq!(host_of(env(&all[2..])), Path::new("/h/.cache/toyos"));
        assert_eq!(host_of(env(&[("TOYOS_STORE", ""), ("XDG_CACHE_HOME", ""), ("HOME", "/h")])), Path::new("/h/.cache/toyos"));
        for refused in [&[("TOYOS_STORE", "store"), ("HOME", "/h")][..], &[]] {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host_of(env(refused))));
            assert!(refused.is_err(), "a store no absolute path names was taken");
        }
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
