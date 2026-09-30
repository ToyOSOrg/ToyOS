//! The host's toolchain store: every LLVM, compiler and sysroot a build on this
//! host has made, at `<primary>/rust/build/<kind>/<key>/`, one directory per
//! key, read-only and never written once it is there.
//!
//! **A key is [`key`] of a recipe and the git hashes of what the product is
//! built from, and nothing else.** Those hashes are [`Sources`]: the four trees
//! a toolchain is made of — the rust fork, `toyos-abi`, `toyos` and
//! `userland/libc` — as git hashes them where they stand, edits included.
//!
//! **A product is there whole or not at all, and the store holds nothing else.**
//! One maker at a time holds the claim `<kind>.making/<key>` (`src/dirlock.rs`),
//! fills it and renames it into the store as `<key>`; a build that finds it held
//! waits for its maker instead of making the key again, and one whose maker is
//! dead takes it away and makes the key afresh. A rename onto a key already
//! placed fails, and the loser removes its copy. A product in use is held
//! shared, and holds no link that leaves it.
//!
//! **A key stays while a registered worktree records it, [`CURRENT`] names it,
//! or somebody holds it; everything else goes** — every other key, and whatever
//! a dead maker or a stopped collection left. [`collect`] runs after every
//! placement, and decides under the kind's store held exclusively.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use toyos_tmpdir::TempDir;

use crate::dirlock::Lock;
use crate::sysroot::git;

/// A product the store holds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kind {
    Llvm,
    Compiler,
    Sysroot,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Llvm, Kind::Compiler, Kind::Sysroot];

    fn name(self) -> &'static str {
        match self {
            Kind::Llvm => "LLVM",
            Kind::Compiler => "compiler",
            Kind::Sysroot => "sysroot",
        }
    }

    /// Where this kind's keys are, in the primary's `rust/`.
    pub fn dir(self, rust_dir: &Path) -> PathBuf {
        rust_dir.join("build").join(match self {
            Kind::Llvm => "llvm",
            Kind::Compiler => "compilers",
            Kind::Sysroot => "sysroots",
        })
    }

    /// Where a worktree records the key of this kind its build uses.
    fn record(self, root: &Path) -> PathBuf {
        root.join(match self {
            Kind::Llvm => "target/toyos-llvm-key",
            Kind::Compiler => "target/toyos-compiler-key",
            Kind::Sysroot => "target/toyos-sysroot-key",
        })
    }
}

/// The one stable path the rustup `toyos` toolchain names: a link, in the
/// primary's `rust/build/`, to the sysroot of the primary's last build.
pub fn current(rust_dir: &Path) -> PathBuf {
    rust_dir.join(CURRENT)
}

/// [`current`], relative to the primary's `rust/`.
pub const CURRENT: &str = "build/toyos";

/// The first 16 hex digits of the SHA-256 of `data`.
pub(crate) fn short(data: &[u8]) -> String {
    Sha256::digest(data).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The key of a product made by `recipe` from `parts`: the one key function.
pub fn key(recipe: &str, parts: &[&str]) -> String {
    let all: Vec<&str> = std::iter::once(recipe).chain(parts.iter().copied()).collect();
    short(all.join("\n\0\n").as_bytes())
}

/// What of the rust fork a toolchain is built from: its LLVM by the commit
/// the fork names, bootstrap, the compiler and its tools, and std.
pub const FORK_TREES: [&str; 7] =
    ["src/llvm-project", "src/bootstrap", "compiler", "src/tools", "src/stage0", "Cargo.lock", "library"];

/// The three trees of this repository std and `libtoyos_c.a` compile.
pub const ABI_TREES: [&str; 3] = ["toyos-abi", "toyos", "userland/libc"];

/// The four trees a toolchain is built from, as git hashes them.
#[derive(PartialEq, Debug)]
pub struct Sources(BTreeMap<&'static str, String>);

impl Sources {
    /// `root`'s ABI trees, and the fork checkout at `fork`, as they stand.
    pub fn of(root: &Path, fork: &Path) -> Self {
        Self::with(root, FORK_TREES.into_iter().zip(trees(fork, &FORK_TREES, Relocked::Yes)))
    }

    /// `root`'s ABI trees as they stand, and the fork's as `commit` holds them
    /// in the fork repository at `rust_dir`: what a clean checkout of `commit`
    /// hashes to, asked of no checkout.
    pub fn pinned(root: &Path, rust_dir: &Path, commit: &str) -> Self {
        let spec: Vec<String> = FORK_TREES.iter().map(|tree| format!("{commit}:{tree}")).collect();
        let args: Vec<&str> = std::iter::once("rev-parse").chain(spec.iter().map(String::as_str)).collect();
        let hashes = git(rust_dir, &args, None).unwrap_or_else(|e| {
            panic!("{e}\nThe fork repository at {} holds no commit {commit}, which this tree pins: fetch it there", rust_dir.display())
        });
        let hashes: Vec<String> = String::from_utf8_lossy(&hashes).lines().map(str::to_string).collect();
        Self::with(root, FORK_TREES.into_iter().zip(hashes))
    }

    fn with(root: &Path, fork: impl Iterator<Item = (&'static str, String)>) -> Self {
        Self(fork.chain(ABI_TREES.into_iter().zip(trees(root, &ABI_TREES, Relocked::No))).collect())
    }

    /// The hash of `tree`, one of [`FORK_TREES`] or [`ABI_TREES`].
    pub fn get(&self, tree: &str) -> &str {
        self.0.get(tree).unwrap_or_else(|| panic!("{tree} is not one of the trees a toolchain is built from"))
    }
}

/// Whether a running bootstrap rewrites a checkout's `Cargo.lock`s.
#[derive(Clone, Copy, PartialEq)]
pub enum Relocked {
    /// The fork's: bootstrap rewrites them while it runs and puts them back
    /// after, and a key read meanwhile would name neither, so each is keyed as
    /// the index holds it.
    Yes,
    /// Any other checkout's: a lockfile is keyed as it stands, like every file.
    No,
}

/// The git hash of each of `paths` in the checkout `repo` as it stands:
/// committed, staged or neither, untracked files included and ignored ones not.
/// A submodule is the commit its gitlink names, and never the one its checkout
/// happens to be at; a checkout of one holding changes no commit does is
/// refused, since bootstrap builds them and the gitlink does not name them.
/// Hashed through a copy of the checkout's index, so the checkout's own is
/// never written.
pub fn trees(repo: &Path, paths: &[&str], lockfiles: Relocked) -> Vec<String> {
    let scratch = TempDir::new("store-index");
    let index = scratch.join("index");
    let run = |dir: &Path, args: &[&str], index: Option<&Path>| {
        let out = git(dir, args, index).unwrap_or_else(|e| panic!("{e}"));
        String::from_utf8(out).unwrap_or_else(|e| panic!("git {args:?} printed no UTF-8: {e}"))
    };
    let real = run(repo, &["rev-parse", "--path-format=absolute", "--git-path", "index"], None);
    fs::copy(real.trim(), &index).unwrap_or_else(|e| panic!("copy {}: {e}", real.trim()));
    let gitlinks: Vec<String> = gitlinks(repo, paths, Some(&index)).into_iter().map(|(path, _)| path).collect();
    for checkout in gitlinks.iter().map(|path| repo.join(path)).filter(|checkout| checkout.join(".git").exists()) {
        let changes = run(&checkout, &["--no-optional-locks", "status", "--porcelain"], None);
        assert!(
            changes.is_empty(),
            "{} holds changes no commit does, and a submodule is keyed on the commit its gitlink names: \
             commit them there and record that commit in {}\n{changes}",
            checkout.display(),
            repo.display(),
        );
    }
    let mut excluded: Vec<String> = gitlinks.iter().map(|path| format!(":(exclude){path}")).collect();
    if lockfiles == Relocked::Yes {
        excluded.push(":(exclude,glob)**/Cargo.lock".to_string());
    }
    let added = paths.iter().filter(|p| lockfiles == Relocked::No || !p.ends_with("Cargo.lock")).copied();
    let add: Vec<&str> = ["add", "-A", "--"].into_iter().chain(added).chain(excluded.iter().map(String::as_str)).collect();
    run(repo, &add, Some(&index));
    let tree = run(repo, &["write-tree"], Some(&index));
    let spec: Vec<String> = paths.iter().map(|p| format!("{}:{p}", tree.trim())).collect();
    let spec: Vec<&str> = std::iter::once("rev-parse").chain(spec.iter().map(String::as_str)).collect();
    run(repo, &spec, None).lines().map(str::to_string).collect()
}

/// Each submodule under `paths` in the checkout `repo`, every one when there
/// are none, with the commit its gitlink in the index names; in `index`, if
/// given, rather than the checkout's own.
pub(crate) fn gitlinks(repo: &Path, paths: &[&str], index: Option<&Path>) -> Vec<(String, String)> {
    let args: Vec<&str> = ["ls-files", "--stage", "--"].into_iter().chain(paths.iter().copied()).collect();
    let listed = git(repo, &args, index).unwrap_or_else(|e| panic!("{e}"));
    let listed = String::from_utf8(listed).unwrap_or_else(|e| panic!("git {args:?} printed no UTF-8: {e}"));
    listed
        .lines()
        .filter_map(|line| {
            let (entry, path) = line.split_once('\t')?;
            let mut words = entry.split(' ');
            (words.next() == Some("160000")).then(|| (path.to_string(), words.next().expect("a gitlink names a commit").to_string()))
        })
        .collect()
}

/// Refuse `what`, which bootstrap just built in the fork checkout `fork` from
/// `built`, the paths it compiles, unless every submodule under them is
/// checked out at the commit its gitlink in the index names. Bootstrap leaves
/// a submodule that is at `HEAD`'s gitlink where it is, under a staged one too,
/// and one it fails to move; it builds whatever is there, and from an empty one
/// nothing.
pub fn assert_built_at_gitlinks(fork: &Path, built: &[&str], what: &str) {
    for (path, gitlink) in gitlinks(fork, built, None) {
        let checkout = fork.join(path);
        if !checkout.join(".git").exists() {
            let empty = match fs::read_dir(&checkout) {
                Ok(mut entries) => entries.next().is_none(),
                Err(e) if e.kind() == ErrorKind::NotFound => true,
                Err(e) => panic!("read {}: {e}", checkout.display()),
            };
            assert!(
                empty,
                "{} holds files and is no checkout of its gitlink {gitlink}, and bootstrap built {what} from \
                 them; nothing was kept",
                checkout.display(),
            );
            continue;
        }
        let at = git(&checkout, &["rev-parse", "HEAD"], None).unwrap_or_else(|e| panic!("{e}"));
        let at = String::from_utf8_lossy(&at);
        assert!(
            at.trim() == gitlink,
            "{} is at {}, and its gitlink names {gitlink}: bootstrap built {what} from the commit checked \
             out there, which its sources do not name; nothing was kept. `git -C {} checkout --detach \
             {gitlink}` checks the gitlink's commit out",
            checkout.display(),
            at.trim(),
            checkout.display(),
        );
    }
}

/// A product in use: shared, so any number of builds use it at once and
/// [`collect`] cannot take it.
pub struct Held {
    pub dir: PathBuf,
    _lock: Lock,
}

/// The product `key` names, held in use; made by `make` first when nobody has
/// made it. `make` fills the directory it is given with the whole product or
/// panics; `root` records the key before anything is looked at.
pub fn get(root: &Path, rust_dir: &Path, kind: Kind, key: &str, mut make: impl FnMut(&Path)) -> Held {
    record(root, kind, key);
    let store = kind.dir(rust_dir);
    let dir = store.join(key);
    loop {
        if let Some(held) = in_use(&dir, kind, key) {
            return held;
        }
        match claim(&store, key, &Lock::try_exclusive) {
            Claim::Mine(making, lock) if dir.is_dir() => remove(&take_away(&lock, &making, &claims(&store), key)),
            Claim::Mine(making, lock) => {
                eprintln!("Making {} {key}", kind.name());
                make(&making);
                let placed = publish(&making, &lock, &dir, remove);
                // Its waiters take the key now, not behind the collection.
                drop(lock);
                if placed {
                    for gone in collect(root, rust_dir) {
                        eprintln!("Removed {}: nothing names it", gone.display());
                    }
                }
            }
            Claim::Theirs(making) => {
                let pid = fs::read_to_string(making.join(MAKER)).unwrap_or_default();
                drop(Lock::shared_if_there(&making, &format!("{} {key} is being made by pid {}", kind.name(), pid.trim())));
            }
        }
    }
}

/// `dir`, held in use, if it is there.
fn in_use(dir: &Path, kind: Kind, key: &str) -> Option<Held> {
    let lock = named(dir, Lock::shared_if_there(dir, &format!("{} {key} is being removed", kind.name()))?)?;
    Some(Held { dir: dir.to_path_buf(), _lock: lock })
}

/// `lock`, if it holds the directory `dir` still names: a key or a claim is
/// renamed away before it is removed, and a lock taken on what was opened
/// before that holds what is being removed.
fn named(dir: &Path, lock: Lock) -> Option<Lock> {
    let named = fs::metadata(dir).ok()?.ino();
    let held = lock.file().metadata().unwrap_or_else(|e| panic!("stat {}: {e}", dir.display())).ino();
    (named == held).then_some(lock)
}

/// `path` exclusively, taken by `take` as [`Lock::try_exclusive`] takes it, if
/// nobody holds it and `path` still names what was taken.
fn unheld(path: &Path, take: &impl Fn(&Path) -> Option<Lock>) -> Option<Lock> {
    named(path, take(path)?)
}

/// Who makes a key: this process, holding its claim, or the process that does.
enum Claim {
    Mine(PathBuf, Lock),
    Theirs(PathBuf),
}

/// The file a maker writes its pid in, so a waiter can say whom it waits for.
const MAKER: &str = ".maker";

static MADE: AtomicU64 = AtomicU64::new(0);

/// Claim the making of `key` in `store`. The claim is a directory made and held
/// under a name of its own and renamed to `<key>` among the claims, which a rename never
/// replaces once it holds anything: so exactly one process holds the name, and
/// it held it before anybody could see it. One whose holder is dead, which
/// `take` takes as [`Lock::try_exclusive`] does, is taken away, and claimed
/// afresh.
fn claim(store: &Path, key: &str, take: &impl Fn(&Path) -> Option<Lock>) -> Claim {
    let claims = claims(store);
    fs::create_dir_all(&claims).unwrap_or_else(|e| panic!("create {}: {e}", claims.display()));
    let making = claims.join(key);
    loop {
        let n = MADE.fetch_add(1, Ordering::Relaxed);
        let mine = claims.join(format!("{key}.{}-{n}.partial", std::process::id()));
        fs::create_dir(&mine).unwrap_or_else(|e| panic!("create {}: {e}", mine.display()));
        let lock = Lock::exclusive(&mine, "a probe of a claim just made");
        fs::write(mine.join(MAKER), std::process::id().to_string()).unwrap_or_else(|e| panic!("write {}: {e}", mine.display()));
        match fs::rename(&mine, &making) {
            Ok(()) => return Claim::Mine(making, lock),
            Err(e) if placed_before(&e, &making) => remove(&mine),
            Err(e) => panic!("rename {} -> {}: {e}", mine.display(), making.display()),
        }
        drop(lock);
        let Some(dead) = unheld(&making, take) else { return Claim::Theirs(making) };
        let away = take_away(&dead, &making, &claims, key);
        drop(dead);
        remove(&away);
    }
}

/// Place what was made at `made`, which `held` holds, as the key `dir`,
/// read-only; `false`, and `made` taken away and removed with `remove`, which a
/// test acts in the gap before, if another maker placed it first. One holding
/// a link that leaves it is refused: its bytes would name whoever made it.
pub(crate) fn publish(made: &Path, held: &Lock, dir: &Path, remove: impl Fn(&Path)) -> bool {
    let _ = fs::remove_file(made.join(MAKER));
    let out = links_out(made, made);
    assert!(out.is_empty(), "{} holds links that leave it, and a key is only what it names: {out:?}", made.display());
    read_only(made);
    // Renaming a directory writes its `..`, so its own mode waits for the rename.
    set_writable(made, true);
    let store = dir.parent().expect("a key is in a store");
    fs::create_dir_all(store).unwrap_or_else(|e| panic!("create {}: {e}", store.display()));
    match fs::rename(made, dir) {
        Ok(()) => {
            set_writable(dir, false);
            true
        }
        Err(e) if placed_before(&e, dir) => {
            let key = dir.file_name().and_then(|k| k.to_str()).expect("a key is a name");
            remove(&take_away(held, made, made.parent().expect("what was made is beside its store"), key));
            false
        }
        Err(e) => panic!("rename {} -> {}: {e}", made.display(), dir.display()),
    }
}

/// Whether `e`, from a rename of a directory onto `to`, says `to` was already
/// there holding something: not empty, or, when it is read-only, not writable.
fn placed_before(e: &std::io::Error, to: &Path) -> bool {
    match e.kind() {
        ErrorKind::DirectoryNotEmpty | ErrorKind::AlreadyExists => true,
        ErrorKind::PermissionDenied => to.is_dir(),
        _ => false,
    }
}

/// Every link under `dir` whose target is outside `product`.
fn links_out(product: &Path, dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).path();
        let meta = fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
        if meta.is_dir() {
            out.extend(links_out(product, &path));
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap_or_else(|e| panic!("readlink {}: {e}", path.display()));
            let mut depth = dir.strip_prefix(product).expect("walked from the product").components().count();
            let leaves = target.components().any(|part| match part {
                Component::Normal(_) => {
                    depth += 1;
                    false
                }
                Component::CurDir => false,
                Component::ParentDir => match depth.checked_sub(1) {
                    Some(up) => {
                        depth = up;
                        false
                    }
                    None => true,
                },
                Component::RootDir | Component::Prefix(_) => true,
            });
            if leaves {
                out.push(path);
            }
        }
    }
    out
}

/// Record that `root`'s builds use `kind`'s `key`: whole or not at all, so
/// [`collect`] never reads a record half-written. A name that is no key is
/// refused, so the store never locks or removes one.
pub fn record(root: &Path, kind: Kind, key: &str) {
    assert!(is_key(key), "{key:?} is no key: a key is 16 hex digits");
    let path = kind.record(root);
    let dir = path.parent().expect("a record is under target/");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let written = path.with_extension(format!("{}-{}.new", std::process::id(), MADE.fetch_add(1, Ordering::Relaxed)));
    fs::write(&written, key).unwrap_or_else(|e| panic!("write {}: {e}", written.display()));
    fs::rename(&written, &path).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", written.display(), path.display()));
}

/// `kind`'s store, made if it is not there.
fn store(rust_dir: &Path, kind: Kind) -> PathBuf {
    let store = kind.dir(rust_dir);
    fs::create_dir_all(&store).unwrap_or_else(|e| panic!("create {}: {e}", store.display()));
    store
}

/// The key of `kind` `root` records, if it records one.
pub fn recorded(root: &Path, kind: Kind) -> Option<String> {
    let path = kind.record(root);
    match fs::read_to_string(&path) {
        Ok(key) => Some(key.trim().to_string()),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

/// Remove from the store everything no registered worktree of `root` records,
/// [`CURRENT`] does not name, and nobody holds. Returns what went.
pub fn collect(root: &Path, rust_dir: &Path) -> Vec<PathBuf> {
    collect_by(root, rust_dir, &Lock::try_exclusive, remove)
}

/// [`collect`], taking what may go with `take` and removing with `remove`, so
/// a test can stop it or act in the gap before either. It acts on the store's
/// own names alone, a key or a name among the claims ([`claimed`]), and leaves
/// every other name where it is.
fn collect_by(root: &Path, rust_dir: &Path, take: &impl Fn(&Path) -> Option<Lock>, remove: impl Fn(&Path)) -> Vec<PathBuf> {
    let listed = git(root, &["worktree", "list", "--porcelain"], None).unwrap_or_else(|e| panic!("{e}"));
    let worktrees: Vec<PathBuf> =
        String::from_utf8_lossy(&listed).lines().filter_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from).collect();
    let mut removed = Vec::new();
    for kind in Kind::ALL {
        let store = store(rust_dir, kind);
        let claims = claims(&store);
        fs::create_dir_all(&claims).unwrap_or_else(|e| panic!("create {}: {e}", claims.display()));
        let mut away = Vec::new();
        {
            let _deciding = Lock::exclusive(&store, &format!("the {} store, behind another collection", kind.name()));
            let mut kept: BTreeSet<String> = worktrees.iter().filter_map(|w| recorded(w, kind)).collect();
            if kind == Kind::Sysroot {
                if let Ok(link) = fs::read_link(current(rust_dir)) {
                    kept.extend(link.file_name().map(|k| k.to_string_lossy().into_owned()));
                }
            }
            for key in entries(&store).into_iter().filter(|name| is_key(name) && !kept.contains(name)) {
                let path = store.join(&key);
                let Some(held) = unheld(&path, take) else { continue };
                set_writable(&path, true);
                away.push((take_away(&held, &path, &claims, &key), path));
            }
        }
        for (gone, path) in away {
            remove(&gone);
            removed.push(path);
        }
        for name in entries(&claims) {
            let Some((key, maker)) = claimed(&name) else { continue };
            // A claim is held from before its name exists; a partial or a
            // removal, only once its maker holds it.
            if maker.is_some_and(alive) {
                continue;
            }
            let path = claims.join(&name);
            let Some(held) = unheld(&path, take) else { continue };
            let gone = take_away(&held, &path, &claims, key);
            drop(held);
            remove(&gone);
            removed.push(path);
        }
    }
    removed
}

/// Rename `path`, which `_held` holds exclusively, to a removal of this
/// process's among `claims`, and return that: what is removed is out of the
/// way first, so a collection that is stopped leaves nothing at its name that
/// passes for whole, and nobody takes the name back while it goes.
fn take_away(_held: &Lock, path: &Path, claims: &Path, key: &str) -> PathBuf {
    let n = MADE.fetch_add(1, Ordering::Relaxed);
    let gone = claims.join(format!("{key}.{}-{n}.gone", std::process::id()));
    fs::rename(path, &gone).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", path.display(), gone.display()));
    gone
}

/// Where `store`'s keys are claimed, made and removed: beside it, so the store
/// itself holds whole keys and nothing else.
fn claims(store: &Path) -> PathBuf {
    store.with_extension("making")
}

/// The UTF-8 names in `store`, none when there is no `store`: no other name is
/// the store's.
fn entries(store: &Path) -> Vec<String> {
    let listing = match fs::read_dir(store) {
        Ok(listing) => listing,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read {}: {e}", store.display()),
    };
    let mut names: Vec<String> = listing
        .map(|e| e.unwrap_or_else(|e| panic!("read {}: {e}", store.display())).file_name())
        .filter_map(|n| n.into_string().ok())
        .collect();
    names.sort();
    names
}

/// Whether `name` is a key: what [`key`] makes, 16 lowercase hex digits.
fn is_key(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The key a name among the claims is of, and the pid a partial's or a
/// removal's name carries: `<key>`, `<key>.<pid>-<n>.partial` or
/// `<key>.<pid>-<n>.gone`, as [`claim`] and [`take_away`] name them. `None`
/// for every other name.
fn claimed(name: &str) -> Option<(&str, Option<i32>)> {
    let mut parts = name.split('.');
    let key = parts.next().filter(|key| is_key(key))?;
    let Some(made) = parts.next() else { return Some((key, None)) };
    let (pid, n) = made.split_once('-')?;
    let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 0)?;
    n.parse::<u64>().ok()?;
    (matches!(parts.next(), Some("partial" | "gone")) && parts.next().is_none()).then_some((key, Some(pid)))
}

/// Whether the process `pid` is running: a maker between creating its partial
/// and holding it is not yet told apart from a dead one by the lock alone.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 runs the existence and permission checks and delivers nothing.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().kind() == ErrorKind::PermissionDenied }
}

/// Take write permission from every file and directory under `dir`, and from `dir`.
fn read_only(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).path();
        let meta = fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
        if meta.is_dir() {
            read_only(&path);
        } else if meta.is_file() {
            let mut permissions = meta.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions).unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
        }
    }
    let mut permissions = fs::metadata(dir).unwrap_or_else(|e| panic!("stat {}: {e}", dir.display())).permissions();
    permissions.set_readonly(true);
    fs::set_permissions(dir, permissions).unwrap_or_else(|e| panic!("chmod {}: {e}", dir.display()));
}

/// Give `dir` alone its owner's write permission, or take it.
fn set_writable(dir: &Path, writable: bool) {
    let mut permissions = fs::metadata(dir).unwrap_or_else(|e| panic!("stat {}: {e}", dir.display())).permissions();
    permissions.set_mode(if writable { permissions.mode() | 0o200 } else { permissions.mode() & !0o222 });
    fs::set_permissions(dir, permissions).unwrap_or_else(|e| panic!("chmod {}: {e}", dir.display()));
}

/// Remove the directory `path` and all it holds, read-only or not.
pub(crate) fn remove(path: &Path) {
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
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};
    use std::process::Command;

    use super::*;
    use crate::dirlock::tests::{held_until_killed, Elsewhere};

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"])
            .args(crate::gitfixture::NO_AUTO_MAINTENANCE)
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    pub(crate) fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The LLVM commits the fixtures' forks record. Nothing reads their content.
    pub(crate) const LLVM_A: &str = "1111111111111111111111111111111111111111";
    pub(crate) const LLVM_B: &str = "2222222222222222222222222222222222222222";

    /// A primary checkout whose `rust` pins fork commit `C0`, and three linked
    /// worktrees whose `rust/` is a fork checkout of their own: `same` at `C0`,
    /// `a` and `b` each at a commit whose `compiler/` is its own.
    pub(crate) struct Estate {
        pub primary: PathBuf,
        pub rust_dir: PathBuf,
        pub same: PathBuf,
        pub a: PathBuf,
        pub b: PathBuf,
        scratch: TempDir,
    }

    impl Drop for Estate {
        /// The store is read-only, and the scratch goes whole.
        fn drop(&mut self) {
            writable(&self.scratch);
        }
    }

    pub(crate) fn estate(name: &str) -> Estate {
        let scratch = TempDir::new(name);
        let base = fs::canonicalize(&scratch).unwrap();

        let backtrace = base.join("backtrace-src");
        fs::create_dir_all(&backtrace).unwrap();
        git(&backtrace, &["init", "-q"]);
        write(&backtrace.join("lib.rs"), "pub fn trace() {}\n");
        git(&backtrace, &["add", "-A"]);
        git(&backtrace, &["commit", "-qm", "backtrace"]);

        let cargo = base.join("cargo-src");
        fs::create_dir_all(&cargo).unwrap();
        git(&cargo, &["init", "-q"]);
        write(&cargo.join("Cargo.toml"), "[package]\nname = \"cargo\"\n");
        git(&cargo, &["add", "-A"]);
        git(&cargo, &["commit", "-qm", "cargo"]);

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        git(&fork, &["submodule", "add", "-q", backtrace.to_str().unwrap(), "library/backtrace"]);
        git(&fork, &["submodule", "add", "-q", cargo.to_str().unwrap(), "src/tools/cargo"]);
        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() {}\n");
        write(&fork.join("src/bootstrap/src/lib.rs"), "fn main() {}\n");
        write(&fork.join("src/tools/lld-wrapper/src/main.rs"), "fn main() {}\n");
        write(&fork.join("src/stage0"), "compiler_version=beta\n");
        write(&fork.join("Cargo.lock"), "# lock\n");
        write(&fork.join("library/std/src/lib.rs"), "pub fn a() {}\n");
        write(&fork.join(".gitignore"), "/build\n");
        write(&fork.join("x.py"), "# bootstrap\n");
        git(&fork, &["add", "-A"]);
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_A},{}", crate::llvm::LLVM)]);
        // What an uninitialised submodule leaves, so `commit -a` keeps the gitlink.
        fs::create_dir_all(fork.join(crate::llvm::LLVM)).unwrap();
        git(&fork, &["commit", "-qm", "C0"]);
        let c0 = git(&fork, &["rev-parse", "HEAD"]);
        let mut pins = Vec::new();
        for spec in ["pub fn targets() { aarch64() }\n", "pub fn targets() { riscv() }\n"] {
            git(&fork, &["checkout", "-q", &c0]);
            write(&fork.join("compiler/rustc_target/src/lib.rs"), spec);
            git(&fork, &["commit", "-qam", "a target"]);
            pins.push(git(&fork, &["rev-parse", "HEAD"]));
        }
        git(&fork, &["checkout", "-q", &c0]);

        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        for tree in ABI_TREES {
            write(&primary.join(tree).join("src/lib.rs"), "pub struct A;\n");
        }
        write(&primary.join("userland/libc/Cargo.lock"), "# libc's lock\n");
        write(&primary.join(".gitignore"), "target/\n");
        git(&primary, &["submodule", "add", "-q", fork.to_str().unwrap(), "rust"]);
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "-qm", "pins C0"]);
        let rust_dir = primary.join("rust");
        git(&rust_dir, &["submodule", "update", "-q", "--init", "library/backtrace"]);

        let mut linked = Vec::new();
        for (name, pin) in [("same", c0.as_str()), ("a", pins[0].as_str()), ("b", pins[1].as_str())] {
            let wt = base.join(name);
            git(&primary, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
            let _ = fs::remove_dir(wt.join("rust"));
            git(&rust_dir, &["worktree", "add", "-q", "--detach", wt.join("rust").to_str().unwrap(), pin]);
            linked.push(wt);
        }
        let [same, a, b]: [PathBuf; 3] = linked.try_into().unwrap();
        Estate { primary, rust_dir, same, a, b, scratch }
    }

    /// Check `fork`'s `library/backtrace` out at its gitlink, stage a newer
    /// commit of it, and leave the checkout where it was: a staged bump, under
    /// which bootstrap leaves the submodule at `HEAD`'s gitlink. Returns the
    /// commit checked out and the one staged.
    pub(crate) fn backtrace_behind_a_staged_gitlink(fork: &Path) -> (String, String) {
        let backtrace = fork.join("library/backtrace");
        git(fork, &["submodule", "update", "-q", "--init", "library/backtrace"]);
        let head = git(&backtrace, &["rev-parse", "HEAD"]);
        write(&backtrace.join("lib.rs"), "pub fn trace() { newer() }\n");
        git(&backtrace, &["commit", "-qam", "a newer backtrace"]);
        let staged = git(&backtrace, &["rev-parse", "HEAD"]);
        git(fork, &["add", "library/backtrace"]);
        git(&backtrace, &["checkout", "-q", "--detach", &head]);
        (head, staged)
    }

    /// What `f` panicked with; `expect` if it returned.
    pub(crate) fn refusal(expect: &str, f: impl FnOnce()) -> String {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err(expect);
        refused.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    /// The key most tests make, use or collect.
    const K: &str = "0123456789abcdef";

    /// A key of its own for `what`.
    fn key_for(what: &str) -> String {
        key(what, &[])
    }

    /// Make a stand-in product, one file saying who made it, and publish it as
    /// `key` in `store`.
    fn placed(store: &Path, key: &str, by: &str) -> bool {
        let made = store.join(format!("{by}.made"));
        write(&made.join("made-by"), by);
        publish(&made, &Lock::exclusive(&made, "its maker"), &store.join(key), remove)
    }

    /// **Concurrent makers of one key make it once**: every other one waits for
    /// the maker and uses what it placed.
    #[test]
    fn concurrent_makers_of_one_key_make_it_once() {
        let e = estate("store-concurrent");
        let makes = std::sync::atomic::AtomicUsize::new(0);
        let dirs: Vec<PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..6)
                .map(|_| {
                    s.spawn(|| {
                        let held = get(&e.same, &e.rust_dir, Kind::Sysroot, K, |dir| {
                            makes.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            write(&dir.join("made-by"), "a maker");
                        });
                        held.dir
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(makes.load(Ordering::SeqCst), 1, "one key was made more than once");
        assert!(dirs.iter().all(|d| *d == Kind::Sysroot.dir(&e.rust_dir).join(K)));
        assert_eq!(entries(&Kind::Sysroot.dir(&e.rust_dir)), [K]);
        assert!(entries(&claims(&Kind::Sysroot.dir(&e.rust_dir))).is_empty(), "a claim outlived its placement");
    }

    /// **The loser of a race to place a key discards its copy**, and what the
    /// winner placed is what stays.
    #[test]
    fn the_loser_of_a_placement_discards_its_copy() {
        let e = estate("store-loser");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        assert!(placed(&store, K, "the first"));
        assert!(!placed(&store, K, "the second"), "a second placement of one key won");
        assert_eq!(fs::read_to_string(store.join(K).join("made-by")).unwrap(), "the first");
        assert_eq!(entries(&store), [K], "the loser's copy stayed");
        let written = fs::OpenOptions::new().write(true).open(store.join(K).join("made-by"));
        assert_eq!(written.map_err(|e| e.kind()).err(), Some(ErrorKind::PermissionDenied), "a placed product can be written");
        let added = fs::write(store.join(K).join("new"), "x");
        assert_eq!(added.map_err(|e| e.kind()).err(), Some(ErrorKind::PermissionDenied), "a placed key's own directory can be written");
    }

    /// **The loser of a placement takes its claim away before removing it**: a
    /// build that claims the key in the gap gets the name, and never a claim
    /// that is being removed under it.
    #[test]
    fn a_losing_claim_is_taken_away_before_it_is_removed() {
        let e = estate("store-lost");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        assert!(placed(&store, K, "the first"));
        let Claim::Mine(making, lock) = claim(&store, K, &Lock::try_exclusive) else { panic!("a free claim was not taken") };
        write(&making.join("made-by"), "the second");
        let taken = RefCell::new(None);
        let won = publish(&making, &lock, &store.join(K), |gone| {
            taken.replace(Some(claim(&store, K, &Lock::try_exclusive)));
            remove(gone);
        });
        assert!(!won, "a second placement of one key won");
        let taken = taken.into_inner().expect("the removal was never asked");
        assert!(matches!(taken, Claim::Mine(..)), "a build claiming the key while the loser's claim went did not get it");
    }

    /// **A product holding a link out of itself is never placed**, and one whose
    /// links stay inside it is.
    #[test]
    fn a_product_linking_out_of_itself_is_refused() {
        let e = estate("store-links");
        let store = Kind::Compiler.dir(&e.rust_dir);
        let made = store.join("inside.made");
        write(&made.join("lib/rustlib/bin/rust-lld"), "lld");
        std::os::unix::fs::symlink("rust-lld", made.join("lib/rustlib/bin/ld.lld")).unwrap();
        std::os::unix::fs::symlink("../bin", made.join("lib/rustlib/up")).unwrap();
        assert!(publish(&made, &Lock::exclusive(&made, "its maker"), &store.join("inside"), remove));
        for (name, target) in [("absolute", e.primary.join("rust")), ("escaping", PathBuf::from("../../../elsewhere"))] {
            let made = store.join(format!("{name}.made"));
            write(&made.join("lib/rustlib/x"), "x");
            std::os::unix::fs::symlink(&target, made.join("lib/rustlib/src")).unwrap();
            let said = refusal("a product linking out of itself was placed", || {
                publish(&made, &Lock::exclusive(&made, "its maker"), &store.join(name), remove);
            });
            assert!(said.contains("links that leave it"), "{said}");
            assert!(!store.join(name).exists());
        }
    }

    const ROOT: &str = "TOYOS_STORE_TEST_ROOT";
    const RUST_DIR: &str = "TOYOS_STORE_TEST_RUST_DIR";

    #[test]
    #[ignore = "the maker the test below kills; never runs on its own"]
    fn a_maker_that_never_finishes() {
        let root = PathBuf::from(std::env::var(ROOT).unwrap_or_else(|_| panic!("run without {ROOT}; it is not a test")));
        let rust_dir = PathBuf::from(std::env::var(RUST_DIR).unwrap());
        get(&root, &rust_dir, Kind::Compiler, K, |dir| {
            write(&dir.join("half"), "half of it");
            held_until_killed();
        });
    }

    /// **A maker killed halfway leaves nothing under the key**: while it runs
    /// its partial is held and nobody makes the key beside it; once it is dead
    /// the next build makes the key whole, and a collection takes the partial.
    #[test]
    fn a_killed_maker_leaves_no_half_product() {
        let e = estate("store-killed");
        let store = Kind::Compiler.dir(&e.rust_dir);
        let env = [(ROOT, e.same.as_os_str()), (RUST_DIR, e.rust_dir.as_os_str())];
        let maker = Elsewhere::hold("store::tests::a_maker_that_never_finishes", &env);
        assert!(!store.join(K).exists(), "a product was visible under its key before it was whole");
        assert!(Lock::try_exclusive(&claims(&store).join(K)).is_none(), "a key being made is not held");
        maker.kill();
        assert!(!store.join(K).exists(), "a killed maker left its half under the key");
        assert!(Lock::try_exclusive(&claims(&store).join(K)).is_some(), "a dead maker's claim is still held");

        let made = Cell::new(0);
        let held = get(&e.same, &e.rust_dir, Kind::Compiler, K, |dir| {
            made.set(made.get() + 1);
            write(&dir.join("whole"), "all of it");
        });
        assert_eq!(made.get(), 1);
        assert!(held.dir.join("whole").is_file() && !held.dir.join("half").exists());
        assert_eq!(entries(&store), [K]);
        assert!(entries(&claims(&store)).is_empty(), "a dead maker's claim outlived the key's making");
    }

    /// **A collection keeps what a registered worktree records, what `CURRENT`
    /// names and what somebody holds**, and takes every other key, read-only
    /// or not, and whatever a dead maker or a stopped collection left.
    #[test]
    fn a_collection_keeps_what_is_named_held_or_current() {
        let e = estate("store-collect");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        let [named_primary, named_linked, held, pointed, orphan] = ["named-primary", "named-linked", "held", "current", "orphan"].map(key_for);
        for key in [&named_primary, &named_linked, &held, &pointed, &orphan] {
            placed(&store, key, key);
        }
        let claims = claims(&store);
        let leftovers = [key_for("j"), format!("{K}.2000000000-0.partial"), format!("{orphan}.2000000000-1.gone")];
        for leftover in &leftovers {
            write(&claims.join(leftover).join("x"), "left");
        }
        // A maker that is running, between making its partial and holding it.
        let making = format!("{K}.{}-9.partial", std::process::id());
        write(&claims.join(&making).join("x"), "being made");
        record(&e.primary, Kind::Sysroot, &named_primary);
        record(&e.a, Kind::Sysroot, &named_linked);
        std::os::unix::fs::symlink(format!("sysroots/{pointed}"), current(&e.rust_dir)).unwrap();
        let holding = Lock::shared(&store.join(&held), "a build using it");

        let mut removed = collect(&e.primary, &e.rust_dir);
        removed.sort();
        let mut gone = vec![store.join(&orphan)];
        gone.extend(leftovers.iter().map(|n| claims.join(n)));
        gone.sort();
        assert_eq!(removed, gone);
        let mut kept = vec![named_primary, named_linked.clone(), held.clone(), pointed];
        kept.sort();
        assert_eq!(entries(&store), kept);
        assert_eq!(entries(&claims), [making], "a running maker's partial was taken");
        drop(holding);
        assert_eq!(collect(&e.primary, &e.rust_dir), [store.join(&held)], "a key nobody names or holds stayed");

        fs::remove_file(Kind::Sysroot.record(&e.a)).unwrap();
        fs::create_dir(Kind::Sysroot.record(&e.a)).unwrap();
        refusal("an unreadable record was read as naming nothing", || {
            collect(&e.primary, &e.rust_dir);
        });
        assert!(store.join(&named_linked).is_dir(), "an unreadable record's key was taken");
    }

    /// **A collection acts on the store's own names alone**: a file Finder
    /// leaves, or any name that is no key, claim, partial or removal, stays
    /// where it is, in the store or among its claims, and no collection stops
    /// at it.
    #[test]
    fn a_collection_leaves_every_name_not_the_store_s() {
        let e = estate("store-strays");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        placed(&store, K, "a key nobody names");
        let claims = claims(&store);
        let strays = [
            store.join(".DS_Store"),
            store.join("notes"),
            store.join(format!("{K}.partial")),
            claims.join(".DS_Store"),
            claims.join("k"),
            claims.join(format!("{K}.notes")),
            claims.join(format!("{K}.2000000000-0.partial.old")),
        ];
        for stray in &strays {
            write(stray, "none of the store's");
        }
        assert_eq!(collect(&e.primary, &e.rust_dir), [store.join(K)]);
        assert_eq!(collect(&e.primary, &e.rust_dir), Vec::<PathBuf>::new());
        let left: Vec<&PathBuf> = strays.iter().filter(|s| !s.is_file()).collect();
        assert!(left.is_empty(), "a collection took {left:?}");
    }

    /// **A name that is no key is refused before the store is touched**: an
    /// empty one would name the store itself.
    #[test]
    fn a_name_that_is_no_key_is_refused() {
        let e = estate("store-no-key");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        placed(&store, K, "a key");
        for name in ["", "../compilers", "0123456789ABCDEF", "0123456789abcdef0"] {
            let said = refusal("a name that is no key was used as one", || {
                get(&e.same, &e.rust_dir, Kind::Sysroot, name, |_| panic!("made {name:?}"));
            });
            assert!(said.contains("is no key"), "{said}");
        }
        assert_eq!(recorded(&e.same, Kind::Sysroot), None, "a name that is no key was recorded");
        assert!(Lock::try_exclusive(&store).is_some(), "the store is held");
    }

    /// **A lock granted on a key a collection renamed away holds nothing**: a
    /// build that opened the key before the rename and was granted its lock
    /// after it gets the key placed since, or nothing, and never what is being
    /// removed.
    #[test]
    fn a_lock_on_a_key_renamed_away_is_not_the_key() {
        let e = estate("store-renamed");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        placed(&store, K, "the first");
        let dir = store.join(K);
        let opened_before = Lock::shared(&dir, "a build using it");
        set_writable(&dir, true);
        fs::rename(&dir, store.join(format!("{K}.away"))).unwrap();
        placed(&store, K, "the second");
        assert!(named(&dir, opened_before).is_none(), "a lock on the key renamed away was taken for the key");
        named(&dir, Lock::shared(&dir, "a build using it")).expect("the key placed since");
        assert_eq!(fs::read_to_string(dir.join("made-by")).unwrap(), "the second");
    }

    /// **A collection that is stopped leaves nothing at a key's name**: what it
    /// removes is out of the way before anything in it goes.
    #[test]
    fn a_stopped_collection_leaves_nothing_at_its_name() {
        let e = estate("store-stopped");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        placed(&store, K, "a key nobody names");
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collect_by(&e.primary, &e.rust_dir, &Lock::try_exclusive, |_| panic!("stopped"))
        }));
        assert!(stopped.is_err(), "the stand-in removal was never asked");
        assert!(!store.join(K).exists(), "a stopped collection left the key it was removing at its name");
        assert!(entries(&store).is_empty());
    }

    /// **A claim taken afresh in the gap before a collection removes a dead
    /// one survives it**: what the collection removes is the dead claim, never
    /// the name a maker has taken back.
    #[test]
    fn a_claim_taken_back_before_a_removal_survives_it() {
        let e = estate("store-gap");
        let store = Kind::Compiler.dir(&e.rust_dir);
        write(&claims(&store).join(K).join(MAKER), "a maker that died");
        let taken = RefCell::new(Vec::new());
        collect_by(&e.primary, &e.rust_dir, &Lock::try_exclusive, |gone| {
            // Another build, in the gap: it takes the dead claim away and
            // claims the key afresh.
            if taken.borrow().is_empty() {
                taken.borrow_mut().push(claim(&store, K, &Lock::try_exclusive));
            }
            remove(gone);
        });
        let claim = taken.into_inner().pop().expect("the removal was never asked");
        assert!(matches!(claim, Claim::Mine(..)), "the fresh maker did not get the claim");
        assert!(claims(&store).join(K).is_dir(), "the collection removed the claim a live maker took back");
        assert!(Lock::try_exclusive(&claims(&store).join(K)).is_none(), "the claim at the key's name is not the live maker's");
    }

    /// Take `path` as [`Lock::try_exclusive`] does, and let another build claim
    /// `K` afresh between the open and the lock: it takes the dead claim
    /// `path` names away first. What that build got is left in `taken`.
    fn claimed_between(path: &Path, store: &Path, taken: &RefCell<Option<Claim>>) -> Option<Lock> {
        let opened = fs::File::open(path).unwrap();
        taken.replace(Some(claim(store, K, &Lock::try_exclusive)));
        crate::dirlock::tests::try_exclusive_opened(opened)
    }

    /// Whether the claim at `K`'s name in `store` is the one `taken` holds.
    fn still_held(store: &Path, taken: RefCell<Option<Claim>>) -> bool {
        let live = taken.into_inner().expect("the lock was never taken");
        matches!(live, Claim::Mine(..)) && claims(store).join(K).is_dir() && Lock::try_exclusive(&claims(store).join(K)).is_none()
    }

    /// **A collection takes a claim only through the name it renames**: one
    /// that opened a dead claim, and was granted its lock after another build
    /// took that claim away and claimed the key afresh, holds nothing at the
    /// name and leaves the live claim there.
    #[test]
    fn a_collection_locked_after_a_claim_was_taken_back_leaves_it() {
        let e = estate("store-open-lock");
        let store = Kind::Compiler.dir(&e.rust_dir);
        write(&claims(&store).join(K).join(MAKER), "a maker that died");
        let taken = RefCell::new(None);
        collect_by(&e.primary, &e.rust_dir, &|path: &Path| claimed_between(path, &store, &taken), remove);
        assert!(still_held(&store, taken), "the collection took the claim a live maker holds");
    }

    /// **A claim takes a dead claim only through the name it renames**, as a
    /// collection does, and waits for the live one it finds there instead.
    #[test]
    fn a_claim_locked_after_a_claim_was_taken_back_leaves_it() {
        let e = estate("store-open-claim");
        let store = Kind::Compiler.dir(&e.rust_dir);
        write(&claims(&store).join(K).join(MAKER), "a maker that died");
        let taken = RefCell::new(None);
        let mine = claim(&store, K, &|path: &Path| claimed_between(path, &store, &taken));
        assert!(matches!(mine, Claim::Theirs(_)), "a claim took the name another build holds");
        assert!(still_held(&store, taken), "a claim took the claim a live maker holds");
    }

    /// **A key recorded while a collection runs is kept by every decision made
    /// after the record**: the collection asks for the records of each kind
    /// when it decides that kind, not once before it starts.
    #[test]
    fn a_key_recorded_while_a_collection_runs_is_kept() {
        let e = estate("store-late");
        let late = key_for("late");
        placed(&Kind::Llvm.dir(&e.rust_dir), &key_for("first"), "an LLVM nobody names");
        placed(&Kind::Compiler.dir(&e.rust_dir), &late, "a compiler recorded while the collection runs");
        collect_by(&e.primary, &e.rust_dir, &Lock::try_exclusive, |gone| {
            record(&e.same, Kind::Compiler, &late);
            remove(gone);
        });
        assert!(Kind::Compiler.dir(&e.rust_dir).join(&late).is_dir(), "a key recorded before its kind was decided was taken");
    }

    /// **A key is the recipe and the trees, byte for byte, as they stand**: a
    /// comment is another key, so is an untracked file, and an ignored one or
    /// an edit put back is not; a submodule is its gitlink and not its
    /// checkout; and asking writes nothing to the checkout's index.
    #[test]
    fn a_key_is_the_recipe_and_the_trees_as_they_stand() {
        let e = estate("store-key");
        let fork = e.same.join("rust");
        let k = || key("recipe", &[Sources::of(&e.same, &fork).get("toyos-abi"), Sources::of(&e.same, &fork).get("library")]);
        let index = fs::read(e.primary.join(".git/worktrees/same/index")).unwrap();
        let base = k();
        assert_eq!(base.len(), 16);
        assert_ne!(key("another recipe", &[]), key("recipe", &[]));
        assert_ne!(key("recipe", &["ab", "c"]), key("recipe", &["a", "bc"]), "parts that differ only in where they split are one key");

        let abi = e.same.join("toyos-abi/src/lib.rs");
        write(&abi, "/// A comment.\npub struct A;\n");
        assert_ne!(k(), base, "a comment kept the key");
        write(&abi, "pub struct A;\n");
        assert_eq!(k(), base, "an edit put back is another key");
        write(&e.same.join("toyos-abi/src/new.rs"), "pub struct B;\n");
        assert_ne!(k(), base, "an untracked file kept the key");
        fs::remove_file(e.same.join("toyos-abi/src/new.rs")).unwrap();
        write(&fork.join("library/std/src/lib.rs"), "pub fn b() {}\n");
        assert_ne!(k(), base, "a fork edit kept the key");
        git(&fork, &["checkout", "-q", "--", "library"]);
        write(&fork.join("build/out"), "ignored");
        assert_eq!(k(), base, "an ignored file moved the key");
        assert_eq!(fs::read(e.primary.join(".git/worktrees/same/index")).unwrap(), index, "asking wrote the index");
        let lock = || Sources::of(&e.same, &fork).get("Cargo.lock").to_string();
        let committed = lock();
        write(&fork.join("Cargo.lock"), "# re-locked by a bootstrap that is running\n");
        assert_eq!(lock(), committed, "a lockfile a running bootstrap rewrote moved the key");
        git(&fork, &["add", "Cargo.lock"]);
        assert_ne!(lock(), committed, "a staged lockfile kept the key");
        git(&fork, &["reset", "-q", "--hard"]);

        let llvm = || Sources::of(&e.same, &fork).get(crate::llvm::LLVM).to_string();
        assert_eq!(llvm(), LLVM_A);
        let checkout = fork.join(crate::llvm::LLVM);
        git(&checkout, &["init", "-q"]);
        write(&checkout.join("f"), "x");
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-qm", "a checkout elsewhere"]);
        assert_eq!(llvm(), LLVM_A, "a submodule was keyed by its checkout, not its gitlink");
        git(&fork, &["update-index", "--cacheinfo", &format!("160000,{LLVM_B},{}", crate::llvm::LLVM)]);
        assert_eq!(llvm(), LLVM_B, "a staged gitlink is not what bootstrap checks out");
    }

    /// **A submodule checked out holding changes no commit does is refused**,
    /// an edit or an untracked file: bootstrap builds them, and the gitlink the
    /// key names does not name them.
    #[test]
    fn a_submodule_holding_uncommitted_changes_is_refused() {
        let e = estate("store-submodule-edit");
        let library = || Sources::of(&e.primary, &e.rust_dir).get("library").to_string();
        let clean = library();
        let backtrace = e.rust_dir.join("library/backtrace");
        for (file, text) in [("lib.rs", "pub fn trace() { edited() }\n"), ("new.rs", "pub fn new() {}\n")] {
            let before = fs::read_to_string(backtrace.join(file)).ok();
            write(&backtrace.join(file), text);
            let said = refusal("a change in a checked-out submodule was keyed as its gitlink", || {
                library();
            });
            let named = format!("{} holds changes no commit does", backtrace.display());
            assert!(said.contains(&named) && said.contains(file), "{said}");
            match before {
                Some(text) => write(&backtrace.join(file), &text),
                None => fs::remove_file(backtrace.join(file)).unwrap(),
            }
        }
        assert_eq!(library(), clean);
    }

    /// **A maker lets go of the key it placed before it collects**: a build
    /// waiting for that key takes it while the collection that follows is held
    /// back.
    #[test]
    fn a_placed_key_is_free_while_its_maker_collects() {
        use std::time::{Duration, Instant};
        let e = estate("store-free");
        let key = Kind::Sysroot.dir(&e.rust_dir).join(K);
        let deciding = Lock::shared(&store(&e.rust_dir, Kind::Llvm), "holding the collection back");
        let free = std::thread::scope(|s| {
            let maker = s.spawn(|| get(&e.same, &e.rust_dir, Kind::Sysroot, K, |dir| write(&dir.join("made-by"), "the maker")));
            let deadline = Instant::now() + Duration::from_secs(20);
            let free = loop {
                if Lock::try_exclusive(&key).is_some() {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            drop(deciding);
            maker.join().unwrap();
            free
        });
        assert!(free, "the key its maker placed stayed held while it collected, 20 s");
    }
}
