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
//! shared.
//!
//! **A key stays while a registered worktree records it, [`CURRENT`] names it,
//! or somebody holds it; everything else goes** — every other key, and whatever
//! a dead maker or a stopped collection left. [`collect`] runs after every
//! placement.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use toyos_tmpdir::TempDir;

use crate::dirlock::Lock;

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
        let fork = FORK_TREES.into_iter().zip(trees(fork, &FORK_TREES));
        Self(fork.chain(ABI_TREES.into_iter().zip(trees(root, &ABI_TREES))).collect())
    }

    /// The hash of `tree`, one of [`FORK_TREES`] or [`ABI_TREES`].
    pub fn get(&self, tree: &str) -> &str {
        self.0.get(tree).unwrap_or_else(|| panic!("{tree} is not one of the trees a toolchain is built from"))
    }
}

/// The git hash of each of `paths` in the checkout `repo` as it stands:
/// committed, staged or neither, untracked files included and ignored ones not.
/// A submodule is the commit its gitlink names, which is what bootstrap checks
/// out, and never the one its checkout happens to be at; a `Cargo.lock` is what
/// the index holds, because bootstrap rewrites the fork's while it runs and puts
/// them back after, and a key read meanwhile would name neither. Hashed through a
/// copy of the checkout's index, so the checkout's own is never written.
pub fn trees(repo: &Path, paths: &[&str]) -> Vec<String> {
    let scratch = TempDir::new("store-index");
    let index = scratch.join("index");
    let real = git(repo, &["rev-parse", "--path-format=absolute", "--git-path", "index"], None);
    fs::copy(real.trim(), &index).unwrap_or_else(|e| panic!("copy {}: {e}", real.trim()));
    let listed: Vec<&str> = ["ls-files", "--stage", "--"].iter().chain(paths).copied().collect();
    let staged = git(repo, &listed, Some(&index));
    let gitlinks = staged.lines().filter(|l| l.starts_with("160000 ")).filter_map(|l| l.split_once('\t'));
    let mut excluded: Vec<String> = gitlinks.map(|(_, path)| format!(":(exclude){path}")).collect();
    excluded.push(":(exclude,glob)**/Cargo.lock".to_string());
    let added = paths.iter().filter(|p| !p.ends_with("Cargo.lock")).copied();
    let add: Vec<&str> = ["add", "-A", "--"].into_iter().chain(added).chain(excluded.iter().map(String::as_str)).collect();
    git(repo, &add, Some(&index));
    let tree = git(repo, &["write-tree"], Some(&index));
    let spec: Vec<String> = paths.iter().map(|p| format!("{}:{p}", tree.trim())).collect();
    let spec: Vec<&str> = std::iter::once("rev-parse").chain(spec.iter().map(String::as_str)).collect();
    git(repo, &spec, None).lines().map(str::to_string).collect()
}

/// What `git args` printed in `dir`, with `index` as its index if given; a
/// failure is refused with what git said.
fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> String {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let out = command.output().unwrap_or_else(|e| panic!("run git in {}: {e}", dir.display()));
    assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr).trim());
    String::from_utf8(out.stdout).unwrap_or_else(|e| panic!("git {args:?} printed no UTF-8: {e}"))
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
        match claim(&store, key) {
            Claim::Mine(making, _lock) if dir.is_dir() => remove(&making),
            Claim::Mine(making, _lock) => {
                eprintln!("Making {} {key}", kind.name());
                make(&making);
                if publish(&making, &dir) {
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
    let lock = Lock::shared_if_there(dir, &format!("{} {key} is being removed", kind.name()))?;
    // Held, and still the directory the key names: `collect` renames a key
    // away before it removes it.
    let named = fs::metadata(dir).ok()?.ino();
    let held = lock.file().metadata().unwrap_or_else(|e| panic!("stat {}: {e}", dir.display())).ino();
    (named == held).then(|| Held { dir: dir.to_path_buf(), _lock: lock })
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
/// it held it before anybody could see it. One whose holder is dead is taken
/// away, and claimed afresh.
fn claim(store: &Path, key: &str) -> Claim {
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
        let Some(dead) = Lock::try_exclusive(&making) else { return Claim::Theirs(making) };
        let away = claims.join(format!("{key}.{}-{n}.gone", std::process::id()));
        match fs::rename(&making, &away) {
            Ok(()) => {
                drop(dead);
                remove(&away);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => panic!("rename {} -> {}: {e}", making.display(), away.display()),
        }
    }
}

/// Place what was made at `made` as the key `dir`, read-only; `false`, and
/// `made` removed, if another maker placed it first.
pub(crate) fn publish(made: &Path, dir: &Path) -> bool {
    let _ = fs::remove_file(made.join(MAKER));
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
            remove(made);
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

/// Record that `root`'s builds use `kind`'s `key`: whole or not at all, so
/// [`collect`] never reads a record half-written.
pub fn record(root: &Path, kind: Kind, key: &str) {
    let path = kind.record(root);
    let dir = path.parent().expect("a record is under target/");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let written = path.with_extension(format!("{}-{}.new", std::process::id(), MADE.fetch_add(1, Ordering::Relaxed)));
    fs::write(&written, key).unwrap_or_else(|e| panic!("write {}: {e}", written.display()));
    fs::rename(&written, &path).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", written.display(), path.display()));
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
    let worktrees: Vec<PathBuf> = git(root, &["worktree", "list", "--porcelain"], None)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect();
    let mut kept: BTreeSet<(Kind, String)> = BTreeSet::new();
    for kind in Kind::ALL {
        kept.extend(worktrees.iter().filter_map(|w| recorded(w, kind)).map(|key| (kind, key)));
    }
    if let Ok(link) = fs::read_link(current(rust_dir)) {
        let key = link.file_name().map(|k| k.to_string_lossy().into_owned()).unwrap_or_default();
        kept.insert((Kind::Sysroot, key));
    }
    let mut removed = Vec::new();
    for kind in Kind::ALL {
        let store = kind.dir(rust_dir);
        let claims = claims(&store);
        // A name with a dot is none of this store's: a key is hex.
        for key in entries(&store).into_iter().filter(|name| !name.contains('.')) {
            let path = store.join(&key);
            if kept.contains(&(kind, key.clone())) {
                continue;
            }
            let Some(held) = Lock::try_exclusive(&path) else { continue };
            // Renamed away before anything in it goes, so a collection that is
            // stopped leaves nothing that passes for whole.
            let n = MADE.fetch_add(1, Ordering::Relaxed);
            let away = claims.join(format!("{key}.{}-{n}.gone", std::process::id()));
            fs::create_dir_all(&claims).unwrap_or_else(|e| panic!("create {}: {e}", claims.display()));
            set_writable(&path, true);
            fs::rename(&path, &away).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", path.display(), away.display()));
            drop(held);
            remove(&away);
            removed.push(path);
        }
        for name in entries(&claims) {
            let path = claims.join(&name);
            // A claim is held from before its name exists; a partial or a
            // removal, only once its maker holds it.
            if name.contains('.') && maker_alive(&name) {
                continue;
            }
            if Lock::try_exclusive(&path).is_none() {
                continue;
            }
            remove(&path);
            removed.push(path);
        }
    }
    removed
}

/// Where `store`'s keys are claimed, made and removed: beside it, so the store
/// itself holds whole keys and nothing else.
fn claims(store: &Path) -> PathBuf {
    store.with_extension("making")
}

/// The names in `store`, none when there is no `store`.
fn entries(store: &Path) -> Vec<String> {
    let listing = match fs::read_dir(store) {
        Ok(listing) => listing,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read {}: {e}", store.display()),
    };
    let mut names: Vec<String> = listing
        .map(|e| e.unwrap_or_else(|e| panic!("read {}: {e}", store.display())).file_name())
        .map(|n| n.into_string().unwrap_or_else(|n| panic!("{} holds {n:?}, which names no key", store.display())))
        .collect();
    names.sort();
    names
}

/// Whether the process a partial's name carries is running: a maker between
/// creating its partial and holding it is not yet told apart from a dead one
/// by the lock alone. A name carrying none is a dead maker's.
fn maker_alive(name: &str) -> bool {
    let Some(pid) = name.split('.').nth(1).and_then(|p| p.split('-').next()).and_then(|p| p.parse::<i32>().ok()) else {
        return false;
    };
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
    use std::cell::Cell;

    use super::*;
    use crate::dirlock::tests::{held_until_killed, Elsewhere};

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"])
            .args(crate::pr::tests::NO_AUTO_MAINTENANCE)
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

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        git(&fork, &["submodule", "add", "-q", backtrace.to_str().unwrap(), "library/backtrace"]);
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

    /// What `f` panicked with; `expect` if it returned.
    pub(crate) fn refusal(expect: &str, f: impl FnOnce()) -> String {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err(expect);
        refused.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    /// Make a stand-in product, one file saying who made it, and publish it as
    /// `key` in `store`.
    fn placed(store: &Path, key: &str, by: &str) -> bool {
        let made = store.join(format!("{by}.made"));
        write(&made.join("made-by"), by);
        publish(&made, &store.join(key))
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
                        let held = get(&e.same, &e.rust_dir, Kind::Sysroot, "k", |dir| {
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
        assert!(dirs.iter().all(|d| *d == Kind::Sysroot.dir(&e.rust_dir).join("k")));
        assert_eq!(entries(&Kind::Sysroot.dir(&e.rust_dir)), ["k"]);
        assert!(entries(&claims(&Kind::Sysroot.dir(&e.rust_dir))).is_empty(), "a claim outlived its placement");
    }

    /// **The loser of a race to place a key discards its copy**, and what the
    /// winner placed is what stays.
    #[test]
    fn the_loser_of_a_placement_discards_its_copy() {
        let e = estate("store-loser");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        assert!(placed(&store, "k", "the first"));
        assert!(!placed(&store, "k", "the second"), "a second placement of one key won");
        assert_eq!(fs::read_to_string(store.join("k/made-by")).unwrap(), "the first");
        assert_eq!(entries(&store), ["k"], "the loser's copy stayed");
        let written = fs::OpenOptions::new().write(true).open(store.join("k/made-by"));
        assert_eq!(written.map_err(|e| e.kind()).err(), Some(ErrorKind::PermissionDenied), "a placed product can be written");
    }

    const ROOT: &str = "TOYOS_STORE_TEST_ROOT";
    const RUST_DIR: &str = "TOYOS_STORE_TEST_RUST_DIR";

    #[test]
    #[ignore = "the maker the test below kills; never runs on its own"]
    fn a_maker_that_never_finishes() {
        let root = PathBuf::from(std::env::var(ROOT).unwrap_or_else(|_| panic!("run without {ROOT}; it is not a test")));
        let rust_dir = PathBuf::from(std::env::var(RUST_DIR).unwrap());
        get(&root, &rust_dir, Kind::Compiler, "k", |dir| {
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
        assert!(!store.join("k").exists(), "a product was visible under its key before it was whole");
        assert!(Lock::try_exclusive(&claims(&store).join("k")).is_none(), "a key being made is not held");
        maker.kill();
        assert!(!store.join("k").exists(), "a killed maker left its half under the key");
        assert!(Lock::try_exclusive(&claims(&store).join("k")).is_some(), "a dead maker's claim is still held");

        let made = Cell::new(0);
        let held = get(&e.same, &e.rust_dir, Kind::Compiler, "k", |dir| {
            made.set(made.get() + 1);
            write(&dir.join("whole"), "all of it");
        });
        assert_eq!(made.get(), 1);
        assert!(held.dir.join("whole").is_file() && !held.dir.join("half").exists());
        assert_eq!(entries(&store), ["k"]);
        assert!(entries(&claims(&store)).is_empty(), "a dead maker's claim outlived the key's making");
    }

    /// **A collection keeps what a registered worktree records, what `CURRENT`
    /// names and what somebody holds**, and takes every other key, read-only
    /// or not, and whatever a dead maker or a stopped collection left.
    #[test]
    fn a_collection_keeps_what_is_named_held_or_current() {
        let e = estate("store-collect");
        let store = Kind::Sysroot.dir(&e.rust_dir);
        for key in ["named-primary", "named-linked", "held", "current", "orphan"] {
            placed(&store, key, key);
        }
        let claims = claims(&store);
        for leftover in ["j", "k.2000000000-0.partial", "orphan.2000000000-1.gone"] {
            write(&claims.join(leftover).join("x"), "left");
        }
        write(&store.join("k.partial").join("x"), "none of the store's");
        record(&e.primary, Kind::Sysroot, "named-primary");
        record(&e.a, Kind::Sysroot, "named-linked");
        std::os::unix::fs::symlink("sysroots/current", current(&e.rust_dir)).unwrap();
        let held = Lock::shared(&store.join("held"), "a build using it");

        let mut removed = collect(&e.primary, &e.rust_dir);
        removed.sort();
        let mut gone = vec![store.join("orphan")];
        gone.extend(["j", "k.2000000000-0.partial", "orphan.2000000000-1.gone"].map(|n| claims.join(n)));
        gone.sort();
        assert_eq!(removed, gone);
        assert_eq!(entries(&store), ["current", "held", "k.partial", "named-linked", "named-primary"]);
        assert!(entries(&claims).is_empty());
        drop(held);
        assert_eq!(collect(&e.primary, &e.rust_dir), [store.join("held")], "a key nobody names or holds stayed");

        fs::remove_file(Kind::Sysroot.record(&e.a)).unwrap();
        fs::create_dir(Kind::Sysroot.record(&e.a)).unwrap();
        refusal("an unreadable record was read as naming nothing", || {
            collect(&e.primary, &e.rust_dir);
        });
        assert!(store.join("named-linked").is_dir(), "an unreadable record's key was taken");
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
}
