//! Content-addressed sysroots: one per source identity, made by whichever
//! worktree first needs it, and shared by every worktree whose sources match.
//!
//! **A sysroot is a function of its key.** The key ([`key`]) is the identity
//! (`src/identity.rs`, so a comment is no change) of everything a sysroot is
//! built from: the trees std and `libtoyos_c.a` compile
//! ([`SYSROOT_SOURCES`]), the std fork's `library/` and `src/bootstrap/` in the
//! checkout that builds it, and the compiler that builds it. `rust/build/
//! sysroots/<key>/` is a whole toolchain — the compiler's files cloned from its
//! `stage2`, the guest targets' libraries built from this key's sources. A build
//! compiles against the directory its own key names, so two worktrees with
//! different ABIs or different compilers never refuse or wait for each other,
//! and main and every branch matching it share one copy.
//!
//! **The kernel's and the loader's libraries compile none of those trees**, so
//! they are a key of their own ([`freestanding_key`]). They are built once per that key
//! into `rust/build/freestanding/<key>/` and cloned into every sysroot naming
//! it, and a crate built against a sysroot learns which targets' libraries
//! moved ([`Identity`]). Each build refuses dep-info that says otherwise.
//!
//! **Each worktree builds std in its own fork checkout.** The primary builds in its `rust/`;
//! a linked worktree in its own `rust/`, made on first need as a git worktree of
//! the primary's fork repository at the commit this tree pins ([`fork_checkout`]).
//! `library/std` names `toyos-abi` and `toyos` as `../../../`, so each
//! checkout's std compiles against its own worktree's ABI with nothing
//! rewritten. The build is bootstrap's stage-0 local rebuild: the compiler the
//! checkout names (`src/compiler.rs` — the primary's `stage2`, or one of the
//! worktree's own where its `compiler/` differs) compiles the checkout's
//! `library/` for the guest targets into `<checkout>/build/toyos-std/`.
//!
//! Locks, in the one order every acquirer takes them: the compiler key's, if the
//! compiler is a worktree's own; the sysroot key's (`buildlock::keyed_*`), with
//! this worktree's build lock put down; then the freestanding key's, while its
//! libraries are cloned in; then, to build, this worktree's exclusively (its
//! fork build directory is written); then, if the compiler is the primary's, the
//! global one shared, because it is read; then the key of the compiler's LLVM,
//! held in use while the C++ runtime is built from its sources.
//!
//! A sysroot or freestanding libraries no worktree names any more are removed
//! by `keystore::sweep`, which every placement runs: each build records the
//! keys it used in its worktree's `target/`, and a key no registered worktree
//! records, that nobody is making or using, goes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::arch::Arch;
use crate::buildlock::{self, Guard, Held, Keyed};
use crate::compiler::{self, Compiler};
use crate::identity;
use crate::keystore::{self, Key};
use crate::toolchain::{self, host_triple, GuestTarget, Owner, Role, GUEST_TARGETS};
use whole_toolchain::{whole, Whole};

/// The per-worktree sources that end up inside a sysroot: std links `toyos-abi`
/// and `toyos`, and `libtoyos_c.a` is `userland/libc` with `toyos-elf`.
pub const SYSROOT_SOURCES: [&str; 5] =
    ["toyos-abi/src", "toyos/src", "toyos-elf/src", "userland/libc/src", "userland/libc/include"];

/// Their manifests, whose features and versions decide the same build.
pub(crate) const SYSROOT_MANIFESTS: [&str; 4] =
    ["toyos-abi/Cargo.toml", "toyos/Cargo.toml", "toyos-elf/Cargo.toml", "userland/libc/Cargo.toml"];

/// Of [`SYSROOT_MANIFESTS`], the ones std's lockfile resolves with the fork's
/// own: what of a worktree can move a freestanding target's dependency versions.
const STD_MANIFESTS: [&str; 2] = ["toyos-abi/Cargo.toml", "toyos/Cargo.toml"];

/// The file a directory [`publish`] made carries last, naming what it was
/// built from. A directory without it is a build that did not finish.
const SOURCES: &str = "SOURCES";

/// What changes how a key's sources become its libraries and its sysroot and
/// is none of them, nor std's configuration ([`std_config`]), which the
/// freestanding key reads whole. Moving it moves every key.
const RECIPE: &str = "bootstrap stage-0 local rebuild, libraries from the stamp, libtoyos_c merged, \
                      a C sysroot of libc's staticlib, the empty libraries beside it, and headers \
                      per target, refused unless a C program naming each library links against \
                      it, and its C++ runtime built under n2 from the runtimes' sources of the \
                      compiler's LLVM, the freestanding libraries cloned from their key's; 12";

/// Every sysroot on this host.
pub fn sysroots_dir(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/sysroots")
}

/// Every freestanding target's libraries on this host, one directory per key.
pub fn freestanding_dir(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/freestanding")
}

/// Whose sources a guest target's libraries compile.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Libraries {
    /// The fork's and [`SYSROOT_SOURCES`]: ToyOS userland's std, which names
    /// `toyos-abi` and `toyos`.
    Worktree,
    /// The fork's alone: the kernel's `core` and `alloc` and the loader's std,
    /// which is what lets them be a key of their own.
    Freestanding,
}

impl Libraries {
    fn of(target: GuestTarget) -> Self {
        match target.role {
            Role::Userland => Self::Worktree,
            Role::Kernel | Role::Loader => Self::Freestanding,
        }
    }

    /// The triples of the [`GUEST_TARGETS`] whose libraries these are.
    fn targets(self) -> Vec<&'static str> {
        GUEST_TARGETS.into_iter().filter(|t| Self::of(*t) == self).map(GuestTarget::triple).collect()
    }
}

/// A sysroot a build compiles against, held in use for as long as this lives.
pub struct Sysroot {
    dir: PathBuf,
    /// Whether its compiler is the primary's, which the ToyOS-hosted rustc is
    /// built from.
    pub primary_compiler: bool,
    pub identity: Identity,
    _using: Option<Guard>,
}

impl Sysroot {
    /// A checkout whose toolchain arrived as an artifact has one sysroot, the
    /// artifact's, which `toolchain::check_installed_toolchain` has matched to
    /// these sources; `release`, the `TOOLCHAIN` it was published with, is the
    /// identity of all of it.
    pub(crate) fn installed(stage2: PathBuf, release: &str) -> Self {
        let id = Key::of(release.as_bytes());
        Self { dir: stage2, primary_compiler: true, identity: Identity::new(id.clone(), &id, &id), _using: None }
    }

    /// Its toolchain directory, which `RUSTUP_TOOLCHAIN` names: lent, never
    /// handed out, because a sweep may remove it once this is dropped.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// What a crate compiled against a sysroot compiles against beside its own
/// sources, as its target directory's `.deps-stamp` records it: the compiler,
/// which everything a crate's build makes reads, its host half too; and per
/// guest target the key of its libraries, which only what is made for that
/// target reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    compiler: Key,
    libraries: BTreeMap<&'static str, Key>,
}

/// What of a crate's target directory a sysroot's [`Identity`] leaves stale.
/// Cargo keys nothing it reuses on the sysroot — every ToyOS compiler prints
/// one `rustc -vV` — so this is where a moved compiler or library reaches a
/// crate.
#[derive(Debug, PartialEq)]
pub enum Stale {
    /// The compiler moved, or nothing records which one built it: all the guest
    /// build wrote, its host half too.
    All,
    /// Only these guest targets' libraries moved: what was made for them,
    /// `target/<target>/`, and nothing the compiler made for the host.
    Targets(Vec<&'static str>),
}

impl Identity {
    /// The identity of a sysroot whose compiler's key is `compiler`, whose
    /// freestanding targets' libraries are `freestanding`'s, and whose others
    /// are `key`'s.
    fn new(compiler: Key, freestanding: &Key, key: &Key) -> Self {
        let libraries = GUEST_TARGETS.map(|target| match Libraries::of(target) {
            Libraries::Worktree => (target.triple(), key.clone()),
            Libraries::Freestanding => (target.triple(), freestanding.clone()),
        });
        Self { compiler, libraries: libraries.into() }
    }

    /// The identity of these parts' keys, for its readers' tests.
    #[cfg(test)]
    pub(crate) fn of_parts(compiler: &str, freestanding: &str, key: &str) -> Self {
        let of = |part: &str| Key::of(part.as_bytes());
        Self::new(of(compiler), &of(freestanding), &of(key))
    }

    /// What this leaves stale of a target directory whose `.deps-stamp` says
    /// `stamp`, if it has one: nothing, if it is this.
    pub fn stale(&self, stamp: Option<&str>) -> Option<Stale> {
        let Some(was) = stamp.and_then(Self::parse) else { return Some(Stale::All) };
        if was.compiler != self.compiler {
            return Some(Stale::All);
        }
        let moved: Vec<&'static str> = self
            .libraries
            .iter()
            .filter(|(target, key)| was.libraries.get(*target) != Some(key))
            .map(|(target, _)| *target)
            .collect();
        (!moved.is_empty()).then_some(Stale::Targets(moved))
    }

    /// The identity `text` spells, if it names a key for each of the
    /// [`GUEST_TARGETS`] and for nothing else.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let compiler = Key::parse(lines.next()?.strip_prefix("compiler ")?)?;
        let mut libraries = BTreeMap::new();
        for line in lines {
            let (triple, key) = line.split_once(' ')?;
            let target = GUEST_TARGETS.into_iter().find(|t| t.triple() == triple)?;
            libraries.insert(target.triple(), Key::parse(key)?);
        }
        (libraries.len() == GUEST_TARGETS.len()).then_some(Self { compiler, libraries })
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "compiler {}", self.compiler)?;
        self.libraries.iter().try_for_each(|(triple, key)| write!(f, "\n{triple} {key}"))
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first 16 hex digits of the SHA-256 of `data`.
pub(crate) fn short(data: &[u8]) -> String {
    hex(&Sha256::digest(data))[..16].to_string()
}

/// Every file under `dir` a build reads, sorted: no `target/` and no dotted
/// directory, which is where a checkout keeps what it did not write.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            if !name.starts_with('.') && name != "target" {
                files_under(&path, out);
            }
        } else if meta.is_file() && name != ".git" {
            out.push(path);
        }
    }
}

/// One line per `.rs`, `.toml` and `.h` file of [`SYSROOT_SOURCES`] under
/// `root`, and per [`SYSROOT_MANIFESTS`] entry: its repository-relative path and
/// the hash of its identity.
///
/// Also what a published toolchain records, so an installed one is matched to a
/// checkout by the same function (`src/release.rs`).
pub fn witness(root: &Path) -> String {
    let mut lines = Vec::new();
    for tree in SYSROOT_SOURCES {
        let mut files = Vec::new();
        files_under(&root.join(tree), &mut files);
        files.retain(|p| p.extension().is_some_and(|e| e == "rs" || e == "toml" || e == "h"));
        files.sort();
        for path in files {
            let data = fs::read(&path).unwrap_or_else(|e| panic!("witness {}: {e}", path.display()));
            let rel = path.strip_prefix(root).unwrap_or(&path);
            lines.push(format!("{}:{}", rel.display(), short(&identity::of(&path, &data))));
        }
    }
    lines.extend(SYSROOT_MANIFESTS.map(|manifest| manifest_line(root, manifest)));
    lines.join("\n")
}

/// `manifest`'s witness line: its path and the hash of its bytes.
fn manifest_line(root: &Path, manifest: &str) -> String {
    let path = root.join(manifest);
    let data = fs::read(&path).unwrap_or_else(|e| panic!("witness {}: {e}", path.display()));
    format!("{manifest}:{}", short(&data))
}

/// The identity of the source files under `paths` of the git checkout `base`,
/// as one hash.
///
/// **Source as git sees it**: tracked files and untracked ones no ignore rule
/// covers, into every submodule checked out there — never what a build or the
/// desktop leaves beside them (bootstrap's `__pycache__`, Finder's
/// `.DS_Store`), which would make a key that moves while it is being built.
pub(crate) fn tree_identity(base: &Path, paths: &[&str], links: Links) -> String {
    let mut files = Vec::new();
    source_files(base, paths, links, &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let data = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        hasher.update(path.strip_prefix(base).unwrap_or(&path).to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(&*identity::of(&path, &data));
        hasher.update([0]);
    }
    hex(&hasher.finalize())[..16].to_string()
}

/// What [`tree_identity`] makes of a symbolic link, which git keeps as the path
/// it names and a build reads through.
#[derive(Clone, Copy)]
pub(crate) enum Links {
    /// Refused by name.
    Refused,
    /// Left out of the identity (`issues/build/a-compiler-key-reads-no-symbolic-link.md`).
    Skipped,
}

fn source_files(checkout: &Path, paths: &[&str], links: Links, out: &mut Vec<PathBuf>) {
    let mut args = vec!["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--"];
    args.extend(paths);
    let listed = git_bytes(checkout, &args);
    let mut seen = BTreeSet::new();
    for entry in listed.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let path = checkout.join(String::from_utf8_lossy(entry).as_ref());
        if !seen.insert(path.clone()) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_symlink() {
            match links {
                Links::Refused => panic!(
                    "{} is a symbolic link, and a key of {} reads none: git keeps the path it names, \
                     and a build reads what is there",
                    path.display(),
                    checkout.display()
                ),
                Links::Skipped => {}
            }
        } else if path.join(".git").exists() {
            source_files(&path, &["."], links, out);
        } else if meta.is_file() {
            out.push(path);
        }
    }
}

/// What a build of a worktree compiles against, by key.
#[derive(Debug, PartialEq)]
struct Keys {
    /// Its freestanding targets' libraries' ([`freestanding_key`]).
    freestanding: Key,
    /// Its sysroot's ([`key`]).
    sysroot: Key,
    /// What a crate compiled against that sysroot records.
    identity: Identity,
}

impl Keys {
    /// The keys of what `root` builds against with its std fork at `fork`,
    /// compiled by `compiler`.
    fn of(root: &Path, compiler: &Compiler, fork: &Path) -> Self {
        let freestanding = freestanding_key(root, compiler, fork);
        let sysroot = key(root, &freestanding);
        let identity = Identity::new(Key::of(compiler.identity().as_bytes()), &freestanding, &sysroot);
        Self { freestanding, sysroot, identity }
    }
}

/// The key of the freestanding targets' libraries `root` builds against with
/// its std fork at `fork`, compiled by `compiler`: none of
/// [`SYSROOT_SOURCES`], and of `root` only [`STD_MANIFESTS`].
fn freestanding_key(root: &Path, compiler: &Compiler, fork: &Path) -> Key {
    freestanding_key_of(root, compiler, fork, RECIPE, &keyed_std_config())
}

/// [`std_config`] with no path of this host in it, as a key reads it.
fn keyed_std_config() -> String {
    let placeholder = Path::new("<placeholder>");
    std_config(placeholder, placeholder, placeholder, "<host>")
}

/// [`freestanding_key`], with the recipe and std's configuration it reads.
fn freestanding_key_of(root: &Path, compiler: &Compiler, fork: &Path, recipe: &str, config: &str) -> Key {
    let parts = [
        format!("{recipe}; cargo {STAGE0_CARGO}; targets {}", Libraries::Freestanding.targets().join(" ")),
        config.to_string(),
        STD_MANIFESTS.map(|manifest| manifest_line(root, manifest)).join("\n"),
        tree_identity(fork, &["library", "src/bootstrap"], Links::Refused),
        compiler.identity(),
    ];
    Key::of(parts.join("\n\0\n").as_bytes())
}

/// The key of the sysroot `root` builds against, whose freestanding libraries
/// are `freestanding`'s ([`freestanding_key`], which names the recipe, std's
/// configuration, the fork and the compiler the rest is built with too).
fn key(root: &Path, freestanding: &Key) -> Key {
    let parts = [
        format!("targets {}; C++ runtime {:?}", Libraries::Worktree.targets().join(" "), crate::libcxx::OPTIONS),
        witness(root),
        freestanding.to_string(),
    ];
    Key::of(parts.join("\n\0\n").as_bytes())
}

/// The commit this checkout's tree pins the std fork at: the index's, so a
/// staged gitlink counts as the tree's.
fn pinned_fork(root: &Path) -> String {
    let entry = git_out(root, &["ls-files", "-s", "--", "rust"]);
    let mut words = entry.split_whitespace();
    match (words.next(), words.next()) {
        (Some("160000"), Some(commit)) => commit.to_string(),
        _ => panic!("{} pins no `rust` gitlink: `git ls-files -s rust` said {entry:?}", root.display()),
    }
}

/// The fork checkout `root`'s std is built in.
///
/// The primary's is its own `rust/`. A linked worktree's `rust/` starts as the
/// empty stub `git worktree add` leaves; it is made here, the first time it is
/// needed, as a git worktree of the primary's fork repository at the commit
/// this tree pins, sharing its objects — and `library/backtrace` the same way
/// from the primary's, or by git's own clone where the primary does not hold
/// that commit.
///
/// A checkout that exists is used as it stands, which is where an agent edits
/// the fork; one whose `HEAD` is neither the pinned commit nor ahead of it is
/// moved there itself, fetching the commit from the primary's repository first
/// if the checkout does not already hold it, unless the checkout has local
/// changes, in which case it is refused by name rather than moved out from
/// under whoever made them.
pub fn fork_checkout(root: &Path) -> PathBuf {
    let fork = root.join("rust");
    let primary = match toolchain::owner(root) {
        Owner::Us => return fork,
        Owner::Installed => panic!("an installed toolchain has no fork checkout to build std in"),
        Owner::Elsewhere(primary) => primary,
    };
    let pinned = pinned_fork(root);
    if !fork.join(".git").exists() {
        let stub = fs::read_dir(&fork).map_or(0, |d| d.count());
        assert!(
            stub == 0,
            "{} is neither a fork checkout nor the empty stub a worktree starts with",
            fork.display()
        );
        let _ = fs::remove_dir(&fork);
        eprintln!("Making {} a fork checkout at {pinned} (a git worktree of the primary's)", fork.display());
        git_run(&primary.join("rust"), &["worktree", "add", "--detach", path_str(&fork), &pinned]);
        let backtrace = git_out(&fork, &["ls-tree", "HEAD", "library/backtrace"]);
        let commit = backtrace.split_whitespace().nth(2).unwrap_or_else(|| {
            panic!("{} pins no library/backtrace: {backtrace:?}", fork.display())
        });
        let theirs = primary.join("rust/library/backtrace");
        let held = Command::new("git")
            .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
            .current_dir(&theirs)
            .status()
            .is_ok_and(|s| s.success());
        let at = fork.join("library/backtrace");
        if held {
            let _ = fs::remove_dir(&at);
            git_run(&theirs, &["worktree", "add", "--detach", path_str(&at), commit]);
        } else {
            git_run(&fork, &["submodule", "update", "--init", "library/backtrace"]);
        }
        return fork;
    }
    let head = git_out(&fork, &["rev-parse", "HEAD"]);
    let head = head.trim();
    let ahead = Command::new("git")
        .args(["merge-base", "--is-ancestor", &pinned, head])
        .current_dir(&fork)
        .status()
        .is_ok_and(|s| s.success());
    if head == pinned || ahead {
        return fork;
    }
    let dirty = git_out(&fork, &["status", "--porcelain", "--ignore-submodules=none"]);
    assert!(
        dirty.is_empty(),
        "{} is at {head} with uncommitted work, and this tree pins the fork at {pinned}, which \
         that is not ahead of: a build here would compile a std this tree does not name, and \
         moving the checkout would lose that work.\n{dirty}",
        fork.display(),
    );
    let held = Command::new("git")
        .args(["cat-file", "-e", &format!("{pinned}^{{commit}}")])
        .current_dir(&fork)
        .status()
        .is_ok_and(|s| s.success());
    if !held {
        git_run(&fork, &["fetch", path_str(&primary.join("rust")), &pinned]);
    }
    git_run(&fork, &["checkout", "--detach", "-q", &pinned]);
    eprintln!("{} was at {head}, behind this tree's pin {pinned}: checked it out", fork.display());
    fork
}

/// Why `dir` is not a directory [`publish`] finished, if it is not: it carries
/// no [`SOURCES`].
fn unpublished(dir: &Path) -> Option<String> {
    (!dir.join(SOURCES).is_file()).then(|| format!("{} carries no {SOURCES}", dir.display()))
}

/// Why `dir` is not a finished sysroot, if it is not: [`unpublished`], or not a
/// whole toolchain (`toolchain::toolchain_defect`). One found with the first and
/// not the second is made again rather than trusted — all of it even when only
/// its `bin/cargo` link dangles, because that is rare and a sysroot has no
/// repair path.
fn unfinished(dir: &Path) -> Option<String> {
    unpublished(dir).or_else(|| toolchain::toolchain_defect(dir))
}

/// The sysroot `key` names at `dir`, recorded as `root`'s, made by `make` if
/// nobody has made it, and held in use for as long as the returned guard lives.
fn held(root: &Path, key: &Key, dir: &Path, make: impl FnMut()) -> Guard {
    let store = dir.parent().expect("a sysroot is a directory of its store");
    keystore::made(root, Keyed::Sysroot, store, key, || unfinished(dir), make)
}

/// The sysroot this worktree's sources name, made if nobody has made it, and
/// held in use for as long as the returned value lives.
pub fn ensure(root: &Path, rust_dir: &Path, lock: &mut Held) -> Sysroot {
    let fork = fork_checkout(root);
    let compiler = compiler::resolve(root, rust_dir, &fork, lock);
    let keys = Keys::of(root, &compiler, &fork);
    let dir = sysroots_dir(rust_dir).join(&keys.sysroot);
    keystore::record(root, Keyed::Freestanding, &keys.freestanding);

    let using =
        lock.without_shared(|| held(root, &keys.sysroot, &dir, || build(root, rust_dir, &compiler, &fork, &keys, &dir)));
    Sysroot { dir, primary_compiler: compiler.primary, identity: keys.identity, _using: Some(using) }
}

/// Only [`whole`] makes a [`Whole`].
mod whole_toolchain {
    use std::path::Path;

    use crate::compiler::Compiler;
    use crate::toolchain;

    /// A compiler [`whole`] found to be a whole toolchain: what a sysroot is
    /// made from.
    pub(super) struct Whole<'a>(&'a Compiler);

    impl Whole<'_> {
        pub(super) fn stage2(&self) -> &Path {
            &self.0.stage2
        }
    }

    /// `compiler`, refused unless it is a whole toolchain: no sysroot is made
    /// from one that is not, and no std is built for one.
    pub(super) fn whole(compiler: &Compiler) -> Whole<'_> {
        if let Some(defect) = toolchain::toolchain_defect(&compiler.stage2) {
            let fix = if compiler.primary {
                "\nA bootstrap in the primary checkout was stopped before it finished: \
                 `cargo run -- --build-only` there completes it."
            } else {
                ""
            };
            panic!("no sysroot is made from {}, and no std was built for one: {defect}{fix}", compiler.stage2.display());
        }
        Whole(compiler)
    }
}

/// Make the sysroot `keys` names at `dir`, from `root`'s sources and the std
/// fork at `fork`, with `compiler`, and the freestanding libraries `keys`
/// names. The caller holds the sysroot key's lock.
fn build(root: &Path, rust_dir: &Path, compiler: &Compiler, fork: &Path, keys: &Keys, dir: &Path) {
    let made_from = whole(compiler);
    let store = freestanding_dir(rust_dir).join(&keys.freestanding);
    let _freestanding = keystore::made(
        root,
        Keyed::Freestanding,
        &freestanding_dir(rust_dir),
        &keys.freestanding,
        || unpublished(&store),
        || build_freestanding(root, compiler, fork, &keys.freestanding, &store),
    );
    let what = format!("building sysroot {}", keys.sysroot);
    let _worktree = buildlock::worktree_exclusive(root, &what);
    // Only the primary's compiler is rebuilt in place; one of a worktree's own
    // is written once and held in use by `compiler`.
    let _compiler = compiler.primary.then(|| buildlock::compiler_shared(root, &what));
    eprintln!(
        "Building sysroot {}: ToyOS's std from {}, the compiler {}, the freestanding libraries {}",
        keys.sysroot,
        fork.display(),
        compiler.stage2.display(),
        keys.freestanding
    );

    publish_toolchain(made_from, dir, |partial| {
        let built = build_std(root, compiler, fork, Libraries::Worktree);
        for target in GUEST_TARGETS {
            let triple = target.triple();
            let lib = partial.join("lib/rustlib").join(triple).join("lib");
            match Libraries::of(target) {
                Libraries::Worktree => place_std(&stamp(&built, triple), &lib),
                Libraries::Freestanding => {
                    keystore::remove(&lib);
                    clone_tree(&store.join(triple), &lib);
                }
            }
        }
        let libc_target = dir.with_extension("libc-target");
        for arch in Arch::ALL {
            crate::libc::build(root, partial, &libc_target, arch);
            crate::libc::build_c(root, partial, &libc_target, arch);
        }
        let _ = fs::remove_dir_all(&libc_target);
        let llvm = crate::llvm::held(root, rust_dir, fork);
        let ninja = crate::n2::ninja(root);
        for arch in Arch::ALL {
            let scratch = dir.with_extension(format!("libcxx-{}", arch.name()));
            let c = crate::clang::CSysroot::of(partial, arch);
            crate::libcxx::build(&c, arch, &llvm.dir.join("src"), &ninja, &scratch);
        }

        // The sources the key named are the ones built, or this is not that key's.
        let (key, again) = (&keys.sysroot, Keys::of(root, compiler, fork).sysroot);
        assert!(
            again == *key,
            "the sources moved while sysroot {key} was being built (they are now {again}); \
             nothing was kept, and the next build makes the one they name"
        );
        format!("{key}\nfork {}\n{}\n", fork.display(), witness(root))
    });
}

/// Make the freestanding targets' libraries `key` names at `store`, from the std
/// fork at `fork` with `compiler`. The caller holds the key's lock.
fn build_freestanding(root: &Path, compiler: &Compiler, fork: &Path, key: &Key, store: &Path) {
    let what = format!("building the freestanding libraries {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
    let _compiler = compiler.primary.then(|| buildlock::compiler_shared(root, &what));
    eprintln!(
        "Building the freestanding libraries {key}: {} from {}, the compiler {}",
        Libraries::Freestanding.targets().join(", "),
        fork.display(),
        compiler.stage2.display()
    );
    publish(store, |partial| {
        let built = build_std(root, compiler, fork, Libraries::Freestanding);
        for target in Libraries::Freestanding.targets() {
            place_std(&stamp(&built, target), &partial.join(target));
        }
        let again = freestanding_key(root, compiler, fork);
        assert!(
            again == *key,
            "the sources moved while the freestanding libraries {key} were being built (they are \
             now {again}); nothing was kept, and the next build makes the ones they name"
        );
        format!("{key}\nfork {}\n", fork.display())
    });
}

/// [`publish`] at `dir` a toolchain: `compiler`'s files, then what `fill`
/// adds to them.
fn publish_toolchain(compiler: Whole, dir: &Path, fill: impl FnOnce(&Path) -> String) {
    publish(dir, |partial| {
        clone_tree(compiler.stage2(), partial);
        fill(partial)
    });
}

/// Put at `dir` what `fill` makes at the path it is handed, then the
/// [`SOURCES`] it returns, last. A `dir` already there is one [`unpublished`]
/// or worse refused, and it is replaced.
fn publish(dir: &Path, fill: impl FnOnce(&Path) -> String) {
    let partial = dir.with_extension("partial");
    keystore::remove(&partial);
    let sources = fill(&partial);
    fs::write(partial.join(SOURCES), sources)
        .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCES).display()));
    keystore::remove(dir);
    fs::rename(&partial, dir)
        .unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
}

/// Compile the libraries of `libraries`' targets from `fork`'s `library/` with
/// `compiler`, and return the directory each target's is under, once each
/// target's dep-info shows it read of `root` what `libraries` says.
fn build_std(root: &Path, compiler: &Compiler, fork: &Path, libraries: Libraries) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let host = host_triple();
    let build_dir = fork.join("build/toyos-std");
    let targets = libraries.targets();
    prepare_std_build(&build_dir, &host, &compiler.identity(), &targets);
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, std_config(&compiler.stage2, &bootstrap_cargo(), &build_dir, &host))
        .unwrap_or_else(|e| panic!("write {}: {e}", config.display()));

    let listed = targets.join(",");
    let args = ["build", "library", "--stage", "0", "--config", path_str(&config), "--warnings", "warn",
                "--target", &listed];
    let (ok, log) = toolchain::x_build(fork, &args, "std");
    toolchain::refuse_on_compile_error(&log, "std");
    assert!(ok, "the std build failed, and nothing in its output was a compile error");
    let built = build_dir.join(&host).join("stage0-std");
    for target in targets {
        match libraries {
            Libraries::Worktree => toolchain::assert_std_built_from(root, &built.join(target)),
            Libraries::Freestanding => toolchain::assert_std_reads_no_worktree(root, fork, &built.join(target)),
        }
    }
    built
}

/// Ready the std build directory `build_dir` for a build of `targets` by the
/// compiler `identity` names: nothing another compiler built, no LLVM, and no
/// std of those targets.
fn prepare_std_build(build_dir: &Path, host: &str, identity: &str, targets: &[&str]) {
    fs::create_dir_all(build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    forget_another_compiler(build_dir, host, identity);
    crate::llvm::retire_in_tree(build_dir);
    // Bootstrap reuses what it built before and does not see a path dependency
    // outside the fork move, so each target's std starts from nothing.
    for target in targets {
        keystore::remove(&build_dir.join(host).join("stage0-std").join(target));
    }
}

/// Empty the std build directory `build_dir` of all but what bootstrap
/// downloaded unless `identity` ([`Compiler::identity`]) is the compiler its
/// `compiled-by` records as having compiled the rest, then record `identity`
/// there.
///
/// Cargo keys what it reuses on `rustc -vV`, which every ToyOS compiler prints
/// alike, so another compiler's rlibs stay fresh and the next crate that does
/// recompile is refused against them (`E0463 can't find crate`). The removal
/// comes before the record, so an interrupted switch removes again.
fn forget_another_compiler(build_dir: &Path, host: &str, identity: &str) {
    let record = build_dir.join("compiled-by");
    if fs::read_to_string(&record).is_ok_and(|by| by == identity) {
        return;
    }
    let kept = [(build_dir.to_path_buf(), &["cache", host, "compiled-by"][..]),
                (build_dir.join(host), &["rustfmt"][..])];
    for (dir, kept) in kept {
        let entries = match fs::read_dir(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            entries => entries.unwrap_or_else(|e| panic!("read {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
            if !kept.iter().any(|name| entry.file_name() == *name) {
                keystore::remove(&entry.path());
            }
        }
    }
    fs::write(&record, identity).unwrap_or_else(|e| panic!("write {}: {e}", record.display()));
}

/// The stamp bootstrap wrote naming every library it built for `target` under
/// `built`: the one list of them. **Not `stage0-sysroot`**, which a stage-0
/// build fills with the stage-0 compiler's own libraries and never with what
/// it built.
fn stamp(built: &Path, target: &str) -> PathBuf {
    let dir = built.join(target);
    let stamps: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path().join(".libstd-stamp"))
        .filter(|p| p.is_file())
        .collect();
    match stamps.as_slice() {
        [one] => one.clone(),
        other => panic!("{} holds {} std stamps, not one: {other:?}", dir.display(), other.len()),
    }
}

/// Put the libraries `stamp` names in `lib`, which they replace, as
/// bootstrap's own `add_to_sysroot` does: `t` in `lib`, `s` in its
/// `self-contained`. A host (`h`) library is not something a std build for a
/// guest target makes, and is refused rather than placed.
fn place_std(stamp: &Path, lib: &Path) {
    if lib.exists() {
        fs::remove_dir_all(lib).unwrap_or_else(|e| panic!("remove {}: {e}", lib.display()));
    }
    let contained = lib.join("self-contained");
    fs::create_dir_all(&contained).unwrap_or_else(|e| panic!("create {}: {e}", contained.display()));
    let listed = fs::read(stamp).unwrap_or_else(|e| panic!("read {}: {e}", stamp.display()));
    let mut placed = 0;
    for part in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(String::from_utf8_lossy(&part[1..]).as_ref());
        let into = match part[0] {
            b't' => lib,
            b's' => &contained,
            kind => panic!("{} names {} as {:?}, a kind a guest std does not make", stamp.display(), path.display(), kind as char),
        };
        let name = path.file_name().unwrap_or_else(|| panic!("{} names no file", path.display()));
        fs::copy(&path, into.join(name))
            .unwrap_or_else(|e| panic!("copy {} into {}: {e}", path.display(), into.display()));
        placed += 1;
    }
    assert!(placed > 0, "{} names no library", stamp.display());
}

/// Bootstrap's configuration for a std built by an existing compiler:
/// `local-rebuild` is what lets stage 0 compile the library for a target the
/// stage-0 compiler has none for, and `profile = "compiler"` is the primary's,
/// so these libraries are built with the options `stage2`'s own were.
/// The linker is the compiler's own `rust-lld`, named by path so that which sysroot
/// a stage-0 build searches for tools decides nothing; and no rpath, which bootstrap
/// spells as a C driver's `-Wl,` arguments that a linker run directly refuses.
/// No LLVM: std builds none, and the profile's `download-ci-llvm` fetches one;
/// and no Ninja, which bootstrap otherwise demands on `PATH` for the LLVM it does
/// not build.
fn std_config(compiler: &Path, cargo: &Path, build_dir: &Path, host: &str) -> String {
    let targets = GUEST_TARGETS.map(|t| format!("\"{}\"", t.triple())).join(", ");
    let linker = toolchain::rust_lld(compiler);
    let userland: String = Arch::ALL
        .iter()
        .map(|arch| format!("\n[target.{}]\nlinker = \"{}\"\nrpath = false\n", arch.userland(), linker.display()))
        .collect();
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
rustc = "{rustc}"
cargo = "{cargo}"
local-rebuild = true
build-dir = "{build_dir}"
host = ["{host}"]
target = [{targets}]

[llvm]
download-ci-llvm = false
ninja = false

[rust]
lld = false
{userland}"#,
        rustc = compiler.join("bin/rustc").display(),
        cargo = cargo.display(),
        build_dir = build_dir.display(),
    )
}

/// The rustup toolchain whose cargo runs bootstrap's stage-0 std build.
///
/// A local rebuild passes cargo the flags of the fork's own version — the
/// fork's bootstrap spells `-Zembed-metadata=no` for it, which the fork's
/// stage-0 beta cargo refuses — so this is a nightly of the fork's version, and
/// it moves when an upstream merge moves that version: bootstrap refuses any
/// other by name (`Unexpected cargo version`).
const STAGE0_CARGO: &str = "nightly-2026-07-22";

/// [`STAGE0_CARGO`]'s cargo, installed through rustup the first time a sysroot
/// is built without it.
fn bootstrap_cargo() -> PathBuf {
    let name = format!("{STAGE0_CARGO}-{}", host_triple());
    let cargo = toolchain::rustup_home().expect("a rustup home").join("toolchains").join(&name).join("bin/cargo");
    if !cargo.exists() {
        eprintln!("Installing {STAGE0_CARGO}, whose cargo builds std for a sysroot...");
        let ok = Command::new("rustup")
            .args(["toolchain", "install", STAGE0_CARGO, "--profile", "minimal"])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok && cargo.exists(), "rustup could not install {STAGE0_CARGO}, so {} is missing", cargo.display());
    }
    cargo
}

/// Copy `from` to `to`, a symbolic link as a link: `stage2`'s own point at
/// things that outlive it. `fs::copy` clones on APFS and reflinks where Linux
/// can, so a sysroot costs the bytes its own libraries differ by.
pub(crate) fn clone_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap_or_else(|e| panic!("create {}: {e}", to.display()));
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display())).flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let meta = fs::symlink_metadata(&src).unwrap_or_else(|e| panic!("stat {}: {e}", src.display()));
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&src).unwrap_or_else(|e| panic!("readlink {}: {e}", src.display()));
            std::os::unix::fs::symlink(&target, &dst)
                .unwrap_or_else(|e| panic!("symlink {}: {e}", dst.display()));
        } else if src.is_dir() {
            clone_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));
        }
    }
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
}

/// `Err` names the command, the directory and what git said.
fn git_try(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("run git in {}: {e}", dir.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {args:?} in {}: {}", dir.display(), stderr.trim()));
    }
    Ok(out.stdout)
}

pub(crate) fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    git_try(dir, args).unwrap_or_else(|e| panic!("{e}"))
}

pub(crate) fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_bytes(dir, args)).into_owned()
}

/// The files `git` tracks under `dir` that `pathspecs` name, every one when
/// there are none, relative to `dir`. A name that is not UTF-8 is refused by
/// name rather than rewritten into one git does not track.
pub(crate) fn tracked_files(dir: &Path, pathspecs: &[&str]) -> Result<Vec<String>, String> {
    let args = [&["ls-files", "-z", "--"][..], pathspecs].concat();
    let listing = git_try(dir, &args)?;
    let names = listing.split(|b| *b == 0).filter(|f| !f.is_empty());
    names
        .map(|f| {
            String::from_utf8(f.to_vec()).map_err(|_| {
                format!("git tracks {:?} in {}, a name that is not UTF-8", String::from_utf8_lossy(f), dir.display())
            })
        })
        .collect()
}

fn git_run(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("run git in {}: {e}", dir.display()))
        .success();
    assert!(ok, "git {args:?} in {} failed", dir.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_tmpdir::TempDir;

    fn git(dir: &Path, args: &[&str]) -> String {
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

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A worktree's trees, a fork checkout and a compiler, laid out the
    /// way the key reads them.
    fn keyed(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let root = base.join("root");
        for tree in SYSROOT_SOURCES {
            write(&root.join(tree).join("lib.rs"), "/// A.\npub struct A;\n");
        }
        for manifest in SYSROOT_MANIFESTS {
            write(&root.join(manifest), "[package]\nversion = \"0.1.0\"\n");
        }
        let fork = base.join("fork");
        write(&fork.join("library/std/src/lib.rs"), "//! std\npub fn exit() {}\n");
        write(&fork.join("src/bootstrap/src/lib.rs"), "fn main() {}\n");
        write(&fork.join(".gitignore"), "__pycache__\n.DS_Store\n");
        git(&fork, &["init", "-q"]);
        let rust_dir = base.join("rust");
        write(&rust_dir.join("build/toyos-compiler"), "tree-1");
        write(&toolchain::stage2(&rust_dir).join("lib/librustc_driver-1.dylib"), "a driver");
        (root, rust_dir, fork)
    }

    /// **Each key is the identity of what it is built from, and only that**: a
    /// comment in any tree they read moves neither; an edit to a tree only
    /// ToyOS's std or the C sysroot compiles moves the sysroot and keeps its
    /// freestanding libraries; a line of the fork's code, a manifest std's
    /// lockfile resolves, the recipe, std's configuration or another compiler
    /// moves both. A crate built against the old ones is stale in the targets
    /// whose libraries moved, and in all of it when the compiler did. A
    /// symbolic link in the fork, whose target no key would read, is refused.
    #[test]
    fn a_comment_is_the_same_sysroot_and_a_signature_is_another() {
        let base = TempDir::new("key");
        let (root, rust_dir, fork) = keyed(&base);
        let k = || Keys::of(&root, &Compiler::primary(&rust_dir), &fork);
        let was = k();
        assert_eq!((was.sysroot.as_str().len(), was.freestanding.as_str().len()), (16, 16), "{was:?}");
        let stamp = was.identity.to_string();
        let toyos = vec!["aarch64-unknown-toyos", "x86_64-unknown-toyos"];
        let mut all = GUEST_TARGETS.map(GuestTarget::triple).to_vec();
        all.sort();
        let same = |what: &str| assert_eq!(k(), was, "{what} moved a key");
        let sysroot_only = |what: &str| {
            let now = k();
            assert_ne!(now.sysroot, was.sysroot, "{what} kept the old sysroot");
            assert_eq!(now.freestanding, was.freestanding, "{what} moved the freestanding libraries");
            assert_eq!(now.identity.stale(Some(&stamp)), Some(Stale::Targets(toyos.clone())), "{what}");
        };
        let both = |what: &str| {
            let now = k();
            assert_ne!(now.sysroot, was.sysroot, "{what} kept the old sysroot");
            assert_ne!(now.freestanding, was.freestanding, "{what} kept the old freestanding libraries");
            assert_eq!(now.identity.stale(Some(&stamp)), Some(Stale::Targets(all.clone())), "{what}");
        };

        let abi = root.join("toyos-abi/src/lib.rs");
        write(&abi, "//! The crate.\n/// A, said better.\n// and a plain comment\npub struct A;\n");
        same("a comment in toyos-abi");
        write(&abi, "/// A.\npub struct A(pub u64);\n");
        sysroot_only("a signature change in toyos-abi");
        write(&abi, "/// A.\npub struct A;\n");
        same("toyos-abi as it was");

        for tree in ["toyos/src", "userland/libc/src"] {
            write(&root.join(tree).join("lib.rs"), "/// A.\npub struct A(u8);\n");
            sysroot_only(tree);
            write(&root.join(tree).join("lib.rs"), "/// A.\npub struct A;\n");
            same(tree);
        }

        let header = root.join("userland/libc/include/stdio.h");
        write(&header, "int puts(const char *);\n");
        sysroot_only("a header the C sysroot carries");
        fs::remove_file(&header).unwrap();
        same("the C sysroot's headers as they were");

        write(&root.join("userland/libc/Cargo.toml"), "[package]\nversion = \"0.2.0\"\n");
        sysroot_only("libc's manifest, which std's lockfile does not resolve,");
        write(&root.join("userland/libc/Cargo.toml"), "[package]\nversion = \"0.1.0\"\n");
        same("libc's manifest as it was");

        let std = fork.join("library/std/src/lib.rs");
        write(&std, "//! std, documented\npub fn exit() {}\n");
        same("a comment in the std fork");
        write(&std, "//! std\npub fn exit() { loop {} }\n");
        both("a change to the fork's code");
        write(&std, "//! std\npub fn exit() {}\n");
        same("the fork as it was");

        // What a build and the desktop leave in the checkout is not its source.
        write(&fork.join("src/bootstrap/__pycache__/bootstrap.cpython-313.pyc"), "bytecode");
        write(&fork.join("library/.DS_Store"), "finder");
        same("a file git ignores");
        write(&fork.join("library/std/src/new.rs"), "pub fn new() {}\n");
        both("an untracked source file");
        fs::remove_file(fork.join("library/std/src/new.rs")).unwrap();
        same("the fork without it");

        let link = fork.join("library/std/src/linked.rs");
        std::os::unix::fs::symlink("lib.rs", &link).unwrap();
        let said = refusal(|| drop(k()));
        assert!(said.starts_with(&format!("{} is a symbolic link", link.display())), "{said}");
        fs::remove_file(&link).unwrap();
        same("the fork without the link");

        for manifest in STD_MANIFESTS {
            write(&root.join(manifest), "[package]\nversion = \"0.2.0\"\n");
            both(manifest);
            write(&root.join(manifest), "[package]\nversion = \"0.1.0\"\n");
            same(manifest);
        }

        let compiler = Compiler::primary(&rust_dir);
        let config = keyed_std_config();
        assert_eq!(freestanding_key_of(&root, &compiler, &fork, RECIPE, &config), was.freestanding);
        assert_ne!(freestanding_key_of(&root, &compiler, &fork, RECIPE, ""), was.freestanding,
                   "the freestanding key reads no std configuration");
        for (what, moved) in [
            ("the recipe", freestanding_key_of(&root, &compiler, &fork, "another recipe", &config)),
            ("std's configuration", freestanding_key_of(&root, &compiler, &fork, RECIPE, &format!("{config}\n[rust]\n"))),
        ] {
            assert_ne!(moved, was.freestanding, "{what} kept the old freestanding libraries");
        }

        write(&rust_dir.join("build/toyos-compiler"), "tree-2");
        let now = k();
        assert!(now.sysroot != was.sysroot && now.freestanding != was.freestanding, "another compiler kept a key: {now:?}");
        assert_eq!(now.identity.stale(Some(&stamp)), Some(Stale::All), "another compiler kept a crate's host half");
    }

    /// **A crate's compiler is the one that built it, rebuilt in place or
    /// not**: the primary's is rebuilt where it stands, so a driver its rebuild
    /// left leaves all of a crate stale, and the same driver none of it.
    #[test]
    fn a_compiler_rebuilt_in_place_leaves_all_of_a_crate_stale() {
        let base = TempDir::new("identity-compiler");
        let (root, rust_dir, fork) = keyed(&base);
        let identity = || Keys::of(&root, &Compiler::primary(&rust_dir), &fork).identity;
        let stamp = identity().to_string();
        assert_eq!(identity().stale(Some(&stamp)), None, "a crate was stale against the compiler that built it");
        write(&toolchain::stage2(&rust_dir).join("lib/librustc_driver-1.dylib"), "a driver, rebuilt in place");
        assert_eq!(identity().stale(Some(&stamp)), Some(Stale::All), "a crate kept what the compiler made before its rebuild");
    }

    /// **What one compiler compiled in a std build directory is never another's**:
    /// all but the downloads goes when the compiler changes, or when nothing
    /// says which compiler made it, and stays while it does not; the downloads
    /// stay either way.
    #[test]
    fn another_compiler_s_std_build_goes_and_the_same_one_s_stays() {
        let base = TempDir::new("compiled-by");
        let (_root, rust_dir, _fork) = keyed(&base);
        let build = base.join("toyos-std");
        let compiled = [
            build.join("bootstrap/debug/deps/libserde-1.rlib"),
            build.join("host/stage0-std/dist/build/std/build-script-build"),
            build.join("host/a-directory-bootstrap-adds/lib.rlib"),
            build.join("tmp/cc-rs-out-dir/out.o"),
            build.join("host/a-stamp-bootstrap-writes"),
            build.join("host/ci-llvm/lib/libLLVM.dylib"),
        ];
        let downloaded = [
            build.join("cache/2026-07-13/rustc.tar.xz"),
            build.join("host/rustfmt/bin/rustfmt"),
        ];
        let lay = || {
            for file in compiled.iter().chain(&downloaded) {
                write(file, "built");
            }
        };
        let identity = || Compiler::primary(&rust_dir).identity();

        lay();
        forget_another_compiler(&build, "host", &identity());
        for file in &compiled {
            assert!(!file.exists(), "{} was kept, and no record names a compiler for it", file.display());
        }
        assert!(downloaded.iter().all(|f| f.is_file()), "a download went");

        lay();
        forget_another_compiler(&build, "host", &identity());
        assert!(compiled.iter().all(|f| f.is_file()), "the same compiler's build went");

        write(&rust_dir.join("build/toyos-compiler"), "tree-2");
        forget_another_compiler(&build, "host", &identity());
        for file in &compiled {
            assert!(!file.exists(), "{} was kept for another compiler", file.display());
        }
        assert!(downloaded.iter().all(|f| f.is_file()), "a download went");
    }

    /// **A std build directory keeps no LLVM, even under the compiler that
    /// built the rest**: bootstrap's and `download-ci-llvm`'s go with their
    /// download, and what that compiler built and the other downloads stay.
    #[test]
    fn a_std_build_under_the_same_compiler_keeps_no_llvm() {
        let base = TempDir::new("std-llvm");
        let (_root, rust_dir, _fork) = keyed(&base);
        let build = base.join("toyos-std");
        let host = host_triple();
        let identity = Compiler::primary(&rust_dir).identity();
        prepare_std_build(&build, &host, &identity, &GUEST_TARGETS.map(GuestTarget::triple));
        let llvm = [
            build.join(&host).join("ci-llvm/lib/libLLVM.dylib"),
            build.join(&host).join("llvm/bin/llvm-config"),
            build.join("cache/llvm-ad3d0bc-false/rust-dev.tar.xz"),
        ];
        let kept = [build.join("bootstrap/debug/deps/libserde-1.rlib"), build.join("cache/2026-07-13/rustc.tar.xz")];
        for file in llvm.iter().chain(&kept) {
            write(file, "built");
        }
        prepare_std_build(&build, &host, &identity, &GUEST_TARGETS.map(GuestTarget::triple));
        for file in &llvm {
            assert!(!file.exists(), "{} outlived a std build's preparation", file.display());
        }
        assert!(kept.iter().all(|f| f.is_file()), "the same compiler's build went");
    }

    /// **A std build fetches no LLVM and asks for no Ninja**: it builds none,
    /// the `compiler` profile would download one, and bootstrap would refuse it
    /// with no `ninja` on `PATH`.
    #[test]
    fn a_std_build_downloads_no_llvm_and_asks_for_no_ninja() {
        let config = std_config(Path::new("/c"), Path::new("/cargo"), Path::new("/b"), "h");
        assert!(config.contains("\n[llvm]\ndownload-ci-llvm = false\nninja = false\n"), "{config}");
    }

    /// **A switch that cannot remove the other compiler's build fails and does
    /// not record the new compiler**, so the next call removes it.
    #[test]
    fn a_switch_that_cannot_remove_records_nothing_and_the_next_one_removes() {
        use std::os::unix::fs::PermissionsExt;
        let base = TempDir::new("compiled-by-stuck");
        let (_root, rust_dir, _fork) = keyed(&base);
        let build = base.join("toyos-std");
        let identity = || Compiler::primary(&rust_dir).identity();
        fs::create_dir_all(&build).unwrap();
        forget_another_compiler(&build, "host", &identity());
        let deps = build.join("bootstrap/debug/deps");
        write(&deps.join("libserde-1.rlib"), "built");
        write(&rust_dir.join("build/toyos-compiler"), "tree-2");
        // The parent: a removal gives back the write permission of what it removes.
        let mode = |bits| fs::set_permissions(&build, fs::Permissions::from_mode(bits)).unwrap();

        mode(0o555);
        let stuck = std::panic::catch_unwind(|| forget_another_compiler(&build, "host", &identity()));
        mode(0o755);
        let refusal = stuck.expect_err("a build that could not be removed was taken for removed");
        let refusal = refusal.downcast_ref::<String>().expect("a formatted panic");
        assert!(refusal.starts_with(&format!("remove {}", build.join("bootstrap").display())), "{refusal}");
        assert_ne!(fs::read_to_string(build.join("compiled-by")).unwrap(), identity(),
                   "the new compiler was recorded over a build it did not remove");

        forget_another_compiler(&build, "host", &identity());
        assert!(!build.join("bootstrap").exists(), "the next call kept the build the stuck one could not remove");
        assert_eq!(fs::read_to_string(build.join("compiled-by")).unwrap(), identity());
    }

    /// A primary with the fork as its `rust` submodule at `C1`, the fork's `C2`
    /// one library change later, and a linked worktree whose tree pins `C2`.
    fn two_pins(base: &Path) -> (PathBuf, PathBuf, String, String) {
        let bt = base.join("backtrace-src");
        fs::create_dir_all(&bt).unwrap();
        git(&bt, &["init", "-q"]);
        write(&bt.join("lib.rs"), "pub fn trace() {}\n");
        git(&bt, &["add", "-A"]);
        git(&bt, &["commit", "-qm", "backtrace"]);

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn a() {}\n");
        write(&fork.join("compiler/lib.rs"), "\n");
        write(&fork.join("x.py"), "\n");
        git(&fork, &["submodule", "add", "-q", bt.to_str().unwrap(), "library/backtrace"]);
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "C1"]);
        let c1 = git(&fork, &["rev-parse", "HEAD"]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn b() {}\n");
        git(&fork, &["commit", "-qam", "C2"]);
        let c2 = git(&fork, &["rev-parse", "HEAD"]);

        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        write(&primary.join("toyos-abi/src/lib.rs"), "pub struct A;\n");
        git(&primary, &["submodule", "add", "-q", fork.to_str().unwrap(), "rust"]);
        git(&primary.join("rust"), &["checkout", "-q", &c1]);
        git(&primary, &["add", "-A"]);
        git(&primary, &["submodule", "update", "-q", "--init", "--recursive"]);
        git(&primary, &["commit", "-qm", "pins C1"]);

        let linked = base.join("linked");
        git(&primary, &["worktree", "add", "-q", "-b", "wt", linked.to_str().unwrap()]);
        git(&linked, &["update-index", "--cacheinfo", &format!("160000,{c2},rust")]);
        git(&linked, &["commit", "-qm", "pins C2"]);
        (primary, linked, c1, c2)
    }

    /// **A worktree pinning another fork commit gets a checkout of its own at
    /// that commit, and the primary's is not touched** — neither its `HEAD` nor
    /// a file of its tree; the worktree's own `git status` is clean, because the
    /// checkout is what its gitlink names.
    #[test]
    fn a_worktree_pinning_another_fork_commit_gets_its_own_checkout() {
        let base = TempDir::new("fork-pins");
        let (primary, linked, c1, c2) = two_pins(&base);
        let before = git(&primary.join("rust"), &["status", "--porcelain"]);

        let fork = fork_checkout(&linked);

        assert_eq!(fork, linked.join("rust"));
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c2);
        assert_eq!(fs::read_to_string(fork.join("library/std/src/lib.rs")).unwrap(), "pub fn b() {}\n");
        assert!(fork.join("library/backtrace/lib.rs").is_file(), "the nested fork submodule is missing");
        assert_eq!(git(&primary.join("rust"), &["rev-parse", "HEAD"]), c1, "the primary's fork moved");
        assert_eq!(git(&primary.join("rust"), &["status", "--porcelain"]), before);
        assert_eq!(
            fs::read_to_string(primary.join("rust/library/std/src/lib.rs")).unwrap(),
            "pub fn a() {}\n",
            "the primary's fork tree was written"
        );
        assert_eq!(git(&linked, &["status", "--porcelain"]), "", "the worktree is not clean");

        // Work on the fork in the worktree's own checkout is what it builds.
        write(&fork.join("library/std/src/lib.rs"), "pub fn c() {}\n");
        git(&fork, &["commit", "-qam", "C3, the agent's own"]);
        assert_eq!(fork_checkout(&linked), fork);

        // A clean checkout behind what the tree pins is moved to the pin itself.
        git(&fork, &["checkout", "-q", &c1]);
        assert_eq!(fork_checkout(&linked), fork);
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c2, "a checkout behind its pin was not moved to it");

        // One with local changes is never moved out from under whoever made them.
        git(&fork, &["checkout", "-q", &c1]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn uncommitted() {}\n");
        let refused = std::panic::catch_unwind(|| fork_checkout(&linked))
            .expect_err("a fork checkout with local changes was moved out from under them");
        let message = refused.downcast::<String>().expect("a formatted refusal");
        assert!(message.contains(&c1) && message.contains(&c2), "{message}");
    }

    /// The primary's compiler under `base`: `rustc` and `rust-lld`, and the C
    /// toolchain `src/clang.rs` provisions beside them if `clang`; no cargo.
    fn primary_compiler(base: &Path, clang: bool) -> Compiler {
        let compiler = Compiler::primary(&base.join("rust"));
        write(&compiler.stage2.join("bin/rustc"), "rustc");
        let lld = toolchain::rust_lld(&compiler.stage2);
        write(&lld, "lld");
        write(&lld.with_file_name("llvm-ar"), "llvm-ar");
        if clang {
            for tool in ["clang", "ld.lld"] {
                write(&lld.with_file_name(tool), tool);
            }
            write(&lld.parent().unwrap().parent().unwrap().join("lib/clang/22/include/stddef.h"), "stddef");
        }
        compiler
    }

    /// What a panic in `f` said.
    fn refusal(f: impl FnOnce()) -> String {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("nothing was refused");
        *refused.downcast::<String>().expect("a formatted refusal")
    }

    /// **A sysroot is whole, or it is made again**: a `stage2` without cargo —
    /// what bootstrap leaves until the primary completes it — is refused by
    /// name and nothing is published, and one found with its `SOURCES` and
    /// without its cargo is rebuilt rather than trusted, once. Placing one
    /// removes a sysroot no worktree names.
    #[test]
    fn a_sysroot_is_whole_or_it_is_made_again() {
        let base = TempDir::new("whole");
        git(&base, &["init", "-q"]);
        let compiler = primary_compiler(&base, true);
        let made = std::cell::Cell::new(0);
        // `most` bounds the makes so far, so a make that loops fails rather than hangs.
        let make = |dir: &Path, most: usize| {
            made.set(made.get() + 1);
            assert!(made.get() <= most, "a sysroot that was not whole was made again: make {}", made.get());
            publish_toolchain(whole(&compiler), dir, |partial| {
                write(&partial.join("lib/rustlib/x86_64-unknown-toyos/lib/libstd.rlib"), "std");
                "found\n".to_string()
            })
        };

        let key = Key::of(b"fresh");
        let fresh = sysroots_dir(&base.join("rust")).join(&key);
        let said = refusal(|| drop(held(&base, &key, &fresh, || make(&fresh, 1))));
        assert!(said.contains("is missing cargo") && said.contains("`cargo run -- --build-only`"), "{said}");
        assert!(!fresh.exists() && !fresh.with_extension("partial").exists(), "a sysroot was published from a stage2 without cargo");
        assert_eq!(made.get(), 1);

        let key = Key::of(b"found");
        let dir = sysroots_dir(&base.join("rust")).join(&key);
        clone_tree(&compiler.stage2, &dir);
        write(&dir.join(SOURCES), "found\n");
        toolchain::provision_toolchain_cargo(&compiler.stage2);
        let orphan = sysroots_dir(&base.join("rust")).join(Key::of(b"named by no worktree"));
        fs::create_dir_all(&orphan).unwrap();
        let using = held(&base, &key, &dir, || make(&dir, 2));
        assert_eq!(made.get(), 2, "a sysroot without its cargo was trusted because it has SOURCES");
        assert!(!orphan.exists(), "placing a sysroot left one no worktree names");
        assert_eq!(toolchain::toolchain_defect(&dir), None);
        assert!(dir.join("lib/rustlib/x86_64-unknown-toyos/lib/libstd.rlib").is_file());
        drop(using);
        drop(held(&base, &key, &dir, || make(&dir, 2)));
        assert_eq!(made.get(), 2, "a whole sysroot was made again");
    }

    /// **A sysroot that cannot be made whole is refused after one make, never
    /// made again**: a `stage2` without clang — what a stopped bootstrap leaves —
    /// is refused before any std is built for it, with nothing published, and a
    /// make that leaves its sysroot not whole is refused by what it lacks.
    #[test]
    fn a_sysroot_that_cannot_be_made_whole_is_made_once_and_refused() {
        let base = TempDir::new("no-clang");
        git(&base, &["init", "-q"]);
        let compiler = primary_compiler(&base, false);
        toolchain::provision_toolchain_cargo(&compiler.stage2);
        let made = std::cell::Cell::new(0);
        let once = || {
            made.set(made.get() + 1);
            assert_eq!(made.get(), 1, "a sysroot that was not whole was made again");
        };

        let key = Key::of(b"cloned");
        let dir = sysroots_dir(&base.join("rust")).join(&key);
        let filled = std::cell::Cell::new(false);
        let said = refusal(|| {
            drop(held(&base, &key, &dir, || {
                once();
                publish_toolchain(whole(&compiler), &dir, |_| {
                    filled.set(true);
                    "cloned\n".to_string()
                })
            }))
        });
        assert!(said.contains("carries no") && said.contains("/clang"), "{said}");
        assert!(said.contains(&compiler.stage2.display().to_string()) && said.contains("`cargo run -- --build-only`"), "{said}");
        assert!(!filled.get(), "a std was built for a sysroot of a compiler without clang");
        assert!(!dir.exists() && !dir.with_extension("partial").exists(), "a sysroot was published from a stage2 without clang");
        assert_eq!(made.get(), 1);

        made.set(0);
        let key = Key::of(b"made");
        let dir = sysroots_dir(&base.join("rust")).join(&key);
        let said = refusal(|| {
            drop(held(&base, &key, &dir, || {
                once();
                clone_tree(&compiler.stage2, &dir);
                write(&dir.join(SOURCES), "made\n");
            }))
        });
        assert!(said.starts_with(&format!("sysroot {key} was made, and is not whole")) && said.contains("/clang"), "{said}");
        assert_eq!(made.get(), 1);
    }

    /// **What a stage-0 std build made is what its stamp names**: its
    /// `stage0-sysroot` holds the compiler's own libraries, and taking those was
    /// a sysroot built from sources it never compiled. Each kind goes where
    /// bootstrap puts it, whatever the directory held before goes, and a host
    /// library is refused.
    #[test]
    fn the_libraries_placed_are_the_ones_the_stamp_names() {
        let base = TempDir::new("stamp");
        let built = base.join("built/x86_64-unknown-toyos/dist");
        write(&built.join("out/libstd-new.rlib"), "new std");
        write(&built.join("out/crt0.o"), "start");
        let entries = [format!("t{}", built.join("out/libstd-new.rlib").display()),
                       format!("s{}", built.join("out/crt0.o").display())];
        write(&built.join(".libstd-stamp"), &format!("{}\0", entries.join("\0")));
        let lib = base.join("sysroot/lib/rustlib/x86_64-unknown-toyos/lib");
        write(&lib.join("libstd-compilers-own.rlib"), "the stage-0 compiler's std");

        place_std(&stamp(&base.join("built"), "x86_64-unknown-toyos"), &lib);
        assert_eq!(fs::read_to_string(lib.join("libstd-new.rlib")).unwrap(), "new std");
        assert_eq!(fs::read_to_string(lib.join("self-contained/crt0.o")).unwrap(), "start");
        assert!(!lib.join("libstd-compilers-own.rlib").exists(), "a library nobody built stayed");

        write(&built.join(".libstd-stamp"), &format!("h{}\0", built.join("out/libstd-new.rlib").display()));
        let refused = std::panic::catch_unwind(|| place_std(&built.join(".libstd-stamp"), &lib));
        assert!(refused.is_err(), "a host library was placed in a guest target");
    }
}
