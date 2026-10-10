//! Content-addressed sysroots: one per source identity, made by whichever
//! worktree first needs it, and shared by every worktree whose sources match.
//!
//! **A sysroot is a function of its key.** The key ([`key`]) is the identity
//! (`src/identity.rs`, so a comment is no change) of everything a sysroot is
//! built from: the trees std and `libtoyos_c.a` compile
//! ([`SYSROOT_SOURCES`]) and how libc is built ([`SYSROOT_MANIFESTS`],
//! `libc::BUILD`), the std fork's `library/` and `src/bootstrap/` in the
//! checkout that builds it, and the compiler's key. `sysroots/<key>/` in the
//! store (`src/keystore.rs`) is a whole toolchain — the compiler's files cloned from its
//! `stage2`, the guest targets' libraries built from this key's sources. A build
//! compiles against the directory its own key names, so two worktrees with
//! different ABIs or different compilers never refuse or wait for each other,
//! and main and every branch matching it share one copy.
//!
//! **The kernel's and the loader's libraries compile none of those trees**, so
//! they are a key of their own ([`freestanding_key`]). They are built once per that key
//! into the store's `freestanding/<key>/` and cloned into every sysroot naming
//! it, and a crate built against a sysroot learns which targets' libraries
//! moved ([`Identity`]). Each build refuses dep-info that says otherwise.
//!
//! **Each worktree builds std in its own fork checkout.** The primary builds in its `rust/`,
//! moved to the commit its tree pins;
//! a linked worktree in its own `rust/`, made on first need as a git worktree of
//! the primary's fork repository at the commit this tree pins ([`fork_checkout`]).
//! `library/std` names `toyos-abi` and `toyos` as `../../../` and each file of
//! its ToyOS backend, `sdk/std`, by a `#[path]` as far up, so each checkout's
//! std compiles against its own worktree's ABI and backend with nothing
//! rewritten. The build is bootstrap's stage-0 local rebuild: the compiler the
//! checkout names (`src/compiler.rs`) compiles the checkout's `library/` for
//! the guest targets into `<checkout>/build/toyos-std/`.
//!
//! Locks, in the one order every acquirer takes them: the compiler key's; the
//! sysroot key's (`buildlock::keyed_*`), with this worktree's build lock put
//! down; then the freestanding key's, while its libraries are cloned in; then,
//! to build, this worktree's exclusively (its fork build directory is
//! written); then the key of the compiler's LLVM, held in use while the C++
//! runtime is built from its sources.
//!
//! A sysroot or freestanding libraries nothing has used for the store's keep
//! time are removed by `keystore::sweep`, which every placement runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use toyos_sha2::Sha256;

use crate::arch::Arch;
use crate::buildlock::{self, Guard, Held, Keyed};
use crate::compiler::{self, Compiler};
use crate::identity;
use crate::keystore::{self, Key};
use crate::toolchain::{self, host_triple, GuestTarget, Owner, Role, GUEST_TARGETS};
use whole_toolchain::{whole, Whole};

/// The per-worktree sources that end up inside a sysroot: std links `toyos-abi`
/// and `toyos` and compiles its ToyOS backend from `sdk/std`, and
/// `libtoyos_c.a` is `userland/libc` with `toyos-elf` and `toyos-osrelease`.
pub const SYSROOT_SOURCES: [&str; 7] = [
    "toyos-abi/src",
    "toyos/src",
    "sdk/std",
    "toyos-elf/src",
    "toyos-osrelease/src",
    "userland/libc/src",
    "userland/libc/include",
];

/// Their manifests, and the lockfile and cargo configuration libc is built
/// under: the features, versions and flags of the same build.
pub(crate) const SYSROOT_MANIFESTS: [&str; 7] = [
    "toyos-abi/Cargo.toml",
    "toyos/Cargo.toml",
    "toyos-elf/Cargo.toml",
    "toyos-osrelease/Cargo.toml",
    "userland/libc/Cargo.toml",
    "userland/libc/Cargo.lock",
    ".cargo/config.toml",
];

/// Of [`SYSROOT_MANIFESTS`], the ones std's lockfile resolves with the fork's
/// own: what of a worktree can move a freestanding target's dependency versions.
const STD_MANIFESTS: [&str; 2] = ["toyos-abi/Cargo.toml", "toyos/Cargo.toml"];

/// The file a directory [`publish`] made carries last, naming what it was
/// built from. A directory without it is a build that did not finish.
const SOURCES: &str = "SOURCES";

/// What changes how a key's sources become its libraries and its sysroot and
/// is none of them, nor std's configuration ([`std_config`]), which the
/// freestanding key reads whole. Moving it moves every key.
const RECIPE: &str = "bootstrap stage-0 local rebuild, libraries from the stamp, refused where its \
                      compiler miscompiles what `src/miscompile.rs` holds, libtoyos_c merged, \
                      a C sysroot of libc's staticlib, the empty libraries beside it, headers and \
                      CMake's description of ToyOS per target, refused unless a C program naming \
                      each library links against it, and its C++ runtime built under n2 from the \
                      runtimes' sources of the compiler's LLVM, the freestanding libraries cloned \
                      from their key's; 15";

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
    pub identity: Identity,
    _using: Option<Guard>,
}

impl Sysroot {
    /// A checkout whose toolchain arrived as an artifact has one sysroot, the
    /// artifact's, which `toolchain::check_installed_toolchain` has matched to
    /// these sources; the sysroot key named by `release`, the `TOOLCHAIN` it
    /// was published with, is the identity of all of it: the key a build of
    /// these sources computes where it builds its own.
    pub(crate) fn installed(stage2: PathBuf, release: &str) -> Self {
        let id = crate::release::named_key(release)
            .unwrap_or_else(|| panic!("the installed TOOLCHAIN names no sysroot key:\n{release}"));
        Self { dir: stage2, identity: Identity::new(id.clone(), &id, &id), _using: None }
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

    /// The key of `triple`'s libraries, which reads the compiler's key and the
    /// freestanding libraries' too: everything an image of that target's
    /// programs was compiled with.
    pub fn of_target(&self, triple: &str) -> &Key {
        self.libraries.get(triple).unwrap_or_else(|| panic!("{triple} is no guest target of this sysroot"))
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
/// covers, and each submodule as the commit its gitlink records ([`gitlink`]),
/// checked out or not — never what a build or the desktop leaves beside them
/// (bootstrap's `__pycache__`, Finder's `.DS_Store`), which would make a key
/// that moves while it is being built.
pub(crate) fn tree_identity(base: &Path, paths: &[&str], links: Links) -> String {
    let mut sources = Vec::new();
    source_files(base, paths, links, &mut sources);
    sources.sort();
    let mut hasher = Sha256::new();
    for (path, commit) in sources {
        hasher.update(path.strip_prefix(base).unwrap_or(&path).to_string_lossy().as_bytes());
        hasher.update([0]);
        match commit {
            Some(commit) => hasher.update(commit.as_bytes()),
            None => {
                let data = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                hasher.update(&*identity::of(&path, &data));
            }
        }
        hasher.update([0]);
    }
    hex(&hasher.finalize())[..16].to_string()
}

/// The commit `checkout`'s `HEAD` records for its submodule at `path`, which is
/// the one a build checks out there; refused when the submodule's checkout
/// holds what no commit does, or the index stages another commit, because a
/// key names it by that commit.
pub(crate) fn gitlink(checkout: &Path, path: &str) -> String {
    let submodule = checkout.join(path);
    // The untracked cache spares each call a walk of a whole tree.
    let status = ["-c", "core.untrackedCache=true", "status", "--porcelain", "--untracked-files=normal"];
    let edited = submodule.join(".git").exists() && !git_bytes(&submodule, &status).is_empty();
    assert!(
        !edited,
        "{} holds changes no commit does, and a key names it by the commit its gitlink records: \
         commit them there and record that commit in {}",
        submodule.display(),
        checkout.display(),
    );
    let recorded = git_out(checkout, &["ls-tree", "HEAD", path]);
    let committed = match recorded.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["160000", "commit", sha, _] => sha.to_string(),
        _ => panic!("{} records no {path} gitlink: `git ls-tree HEAD {path}` said {recorded:?}", checkout.display()),
    };
    let indexed = git_out(checkout, &["ls-files", "--stage", path]);
    let staged = match indexed.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["160000", sha, "0", _] => sha.to_string(),
        _ => panic!("{} indexes no {path} gitlink: `git ls-files --stage {path}` said {indexed:?}", checkout.display()),
    };
    assert!(
        staged == committed,
        "{} stages {path} at {staged}, and its HEAD records {committed}: a build checks out the one \
         staged, and nothing is keyed on what no commit holds; commit the gitlink, or unstage it",
        checkout.display(),
    );
    committed
}

/// What [`tree_identity`] makes of a symbolic link, which git keeps as the path
/// it names and a build reads through.
#[derive(Clone, Copy)]
pub(crate) enum Links {
    /// Refused by name.
    Refused,
    /// Left out of the identity (`issues/a-compiler-key-reads-no-symbolic-link.md`).
    Skipped,
}

/// Each source under `paths` of `checkout`, with the commit of each that is a
/// submodule ([`gitlink`]).
fn source_files(checkout: &Path, paths: &[&str], links: Links, out: &mut Vec<(PathBuf, Option<String>)>) {
    let listed = |how: &[&str]| git_bytes(checkout, &[&["ls-files", "-z"][..], how, &["--"][..], paths].concat());
    let cached = listed(&["--stage"]);
    let others = listed(&["--others", "--exclude-standard"]);
    // `<mode> <object> <stage>\t<path>`, and a gitlink's mode is 160000.
    let cached = cached.split(|b| *b == 0).filter(|e| !e.is_empty()).map(|entry| {
        let at = entry.iter().position(|b| *b == b'\t').unwrap_or_else(|| panic!("git ls-files --stage said {entry:?}"));
        (&entry[at + 1..], entry.starts_with(b"160000 "))
    });
    let others = others.split(|b| *b == 0).filter(|e| !e.is_empty()).map(|entry| (entry, false));
    let mut seen = BTreeSet::new();
    for (entry, submodule) in cached.chain(others) {
        let name = String::from_utf8_lossy(entry);
        let path = checkout.join(name.as_ref());
        if !seen.insert(path.clone()) {
            continue;
        }
        if submodule {
            out.push((path, Some(gitlink(checkout, &name))));
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
            out.push((path, None));
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
        let freestanding = freestanding_key(root, compiler.key(), fork);
        let sysroot = key(root, &freestanding);
        let identity = Identity::new(Key::of(compiler.identity().as_bytes()), &freestanding, &sysroot);
        Self { freestanding, sysroot, identity }
    }
}

/// The key of the freestanding targets' libraries `root` builds against with
/// its std fork at `fork`, compiled by the compiler whose key is `compiler`
/// (`Compiler::key`): none of [`SYSROOT_SOURCES`], and of `root` only
/// [`STD_MANIFESTS`].
pub(crate) fn freestanding_key(root: &Path, compiler: &Key, fork: &Path) -> Key {
    freestanding_key_of(root, compiler, fork, RECIPE, &keyed_std_config())
}

/// [`std_config`] with no path of this host in it, as a key reads it.
fn keyed_std_config() -> String {
    let placeholder = Path::new("<placeholder>");
    std_config(placeholder, placeholder, placeholder, "<host>")
}

/// [`freestanding_key`], with the recipe and std's configuration it reads.
fn freestanding_key_of(root: &Path, compiler: &Key, fork: &Path, recipe: &str, config: &str) -> Key {
    let parts = [
        format!("{recipe}; cargo {STAGE0_CARGO}; targets {}", Libraries::Freestanding.targets().join(" ")),
        config.to_string(),
        STD_MANIFESTS.map(|manifest| manifest_line(root, manifest)).join("\n"),
        tree_identity(fork, &["library", "src/bootstrap"], Links::Refused),
        compiler.to_string(),
    ];
    Key::of(parts.join("\n\0\n").as_bytes())
}

/// The key of the sysroot `root` builds against, whose freestanding libraries
/// are `freestanding`'s ([`freestanding_key`], which names the recipe, std's
/// configuration, the fork and the compiler the rest is built with too).
pub(crate) fn key(root: &Path, freestanding: &Key) -> Key {
    key_of(root, freestanding, &build_text())
}

/// What a sysroot's build is beyond its sources and its freestanding libraries:
/// its targets, the C++ runtime's options, CMake's description of ToyOS and
/// libc's cargo invocations.
fn build_text() -> String {
    let targets = Libraries::Worktree.targets().join(" ");
    let (libc, staticlib) = (crate::libc::BUILD, crate::libc::BUILD_C);
    format!(
        "targets {targets}; C++ runtime {:?}; CMake {:?}; libc {libc:?} {staticlib:?}",
        crate::libcxx::OPTIONS,
        crate::clang::CMAKE,
    )
}

/// [`key`], with the build it reads.
fn key_of(root: &Path, freestanding: &Key, build: &str) -> Key {
    let parts = [build.to_string(), witness(root), freestanding.to_string()];
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

/// The fork checkout `root`'s std is built in, at the commit `root`'s tree pins.
///
/// The primary's is its own `rust/`, used where it is not initialised yet, which
/// whoever initialises it does at the pin. It is no workspace, so one whose
/// `HEAD` is not the pin is moved there, refused by name if it has local
/// changes; a commit missing from it or from a nested submodule is fetched as
/// rustc's bootstrap fetches one ([`submodule_update`]).
///
/// A linked worktree's `rust/` starts as the empty stub `git worktree add`
/// leaves; it is made here, the first time it is needed, as a git worktree of
/// the primary's fork repository at the pin, sharing its objects — and
/// `library/backtrace` the same way from the primary's, or by git's own clone
/// where the primary does not hold that commit. It is where an agent edits the
/// fork, so one ahead of the pin is used as it stands; one neither at the pin
/// nor ahead of it is moved there, fetching the commit from the primary's
/// repository first if it does not hold it, and refused by name if it has local
/// changes rather than moved out from under whoever made them.
///
/// Every nested submodule checked out in it moves with it to the commit the pin
/// records there, refused by name where it is at a commit that neither the
/// checkout's `HEAD` nor the pin records there, which only its own `HEAD` may
/// record — and in a linked worktree where it does not hold that commit, which
/// [`submodule_update`] may not fetch there.
///
/// The checkout moves before its nested submodules, because git's update reads
/// their URLs and commits from the moved checkout's `.gitmodules` and index; a
/// move killed between the two is one [`moving`] says is in flight, which the
/// next finishes.
///
/// Every build in a worktree asks this at once, so the making and the move are
/// each decided and done under the worktree's lock held exclusively
/// ([`Held::act_if`]), which every build that writes or compiles the checkout
/// holds too: one build does it, and none reads a checkout another is still
/// writing.
pub fn fork_checkout(root: &Path, lock: &mut Held) -> PathBuf {
    let fork = root.join("rust");
    let primary = match toolchain::owner(root) {
        Owner::Us if !fork.join(".git").exists() => return fork,
        Owner::Us => None,
        Owner::Installed => panic!("an installed toolchain has no fork checkout to build std in"),
        Owner::Elsewhere(primary) => Some(primary),
    };
    let pinned = pinned_fork(root);
    if let Some(primary) = &primary {
        lock.act_if(
            "make the fork checkout",
            || (!fork.join(".git").exists()).then_some(()),
            |()| {
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
                let at = fork.join("library/backtrace");
                if holds(&theirs, commit) {
                    let _ = fs::remove_dir(&at);
                    git_run(&theirs, &["worktree", "add", "--detach", path_str(&at), commit]);
                } else {
                    git_run(&fork, &["submodule", "update", "--init", "library/backtrace"]);
                }
            },
        );
    }
    lock.act_if(
        "move the fork checkout to its pin",
        || {
            let head = git_out(&fork, &["rev-parse", "HEAD"]).trim().to_string();
            let ahead = primary.is_some()
                && Command::new("git")
                    .args(["merge-base", "--is-ancestor", &pinned, &head])
                    .current_dir(&fork)
                    .status()
                    .is_ok_and(|s| s.success());
            ((head != pinned || moving(&fork).exists()) && !ahead).then_some(head)
        },
        |head| {
            if let Some(primary) = &primary {
                if !holds(&fork, &pinned) {
                    git_run(&fork, &["fetch", path_str(&primary.join("rust")), &pinned]);
                }
            }
            // The primary's gets a pin it lacks with its move, so nothing it records is known yet.
            let recorded = if holds(&fork, &pinned) { gitlinks(&fork, &pinned) } else { Vec::new() };
            if primary.is_some() {
                for (path, commit) in &recorded {
                    let at = fork.join(path);
                    assert!(
                        !at.join(".git").exists() || holds(&at, commit),
                        "{} does not hold {commit}, which the fork's {pinned} records there, and a linked worktree's \
                         fork checkout gets a commit only from the primary's",
                        at.display(),
                    );
                }
            }
            // A nested submodule checked out at the commit the pin records there, and nothing more, is no work, nor is
            // one at any commit while a move is in flight. One at any other commit may hold a commit nothing else records.
            let in_flight = moving(&fork).exists();
            let status = git_out(&fork, &["status", "--porcelain=v2", "--ignore-submodules=none"]);
            let work: Vec<String> = status
                .lines()
                .filter_map(|entry| {
                    let path = entry.strip_prefix("1 .M SC.. ").and_then(|rest| rest.splitn(6, ' ').nth(5));
                    if path.is_some() && in_flight {
                        return None;
                    }
                    let Some((path, commit)) = path.and_then(|path| recorded.iter().find(|(p, _)| p == path)) else {
                        return Some(entry.to_string());
                    };
                    let at = fork.join(path);
                    let at_head = git_out(&at, &["rev-parse", "HEAD"]).trim().to_string();
                    (at_head != *commit).then(|| {
                        format!("{} is at {at_head}, not at {commit}, what {pinned} records there", at.display())
                    })
                })
                .collect();
            assert!(
                work.is_empty(),
                "{} is at {head} with uncommitted work, and this tree pins the fork at {pinned}: a build \
                 here would compile a std this tree does not name, and moving the checkout would lose \
                 that work.\n{}",
                fork.display(),
                work.join("\n"),
            );
            let moving = moving(&fork);
            fs::write(&moving, "").unwrap_or_else(|e| panic!("write {}: {e}", moving.display()));
            match (&primary, holds(&fork, &pinned)) {
                (_, true) => git_run(&fork, &["checkout", "--detach", "-q", &pinned]),
                (None, false) => submodule_update(root, "rust"),
                (Some(_), false) => unreachable!("a linked worktree's fork checkout fetched {pinned} above"),
            }
            for (path, commit) in gitlinks(&fork, "HEAD") {
                let at = fork.join(&path);
                if !at.join(".git").exists() || git_out(&at, &["rev-parse", "HEAD"]).trim() == commit {
                    continue;
                }
                match (&primary, holds(&at, &commit)) {
                    (_, true) => git_run(&at, &["checkout", "--detach", "-q", &commit]),
                    (None, false) => submodule_update(&fork, &path),
                    (Some(_), false) => unreachable!("{} lacks {commit}, which was refused above", at.display()),
                }
            }
            fs::remove_file(&moving).unwrap_or_else(|e| panic!("remove {}: {e}", moving.display()));
            eprintln!("{} was at {head}, and this tree pins {pinned}: checked it out", fork.display());
        },
    );
    fork
}

/// The file in `fork`'s own git directory whose presence says a move of it to
/// its pin is in flight: the checkout may be at the pin while a nested
/// submodule is not yet.
fn moving(fork: &Path) -> PathBuf {
    fork.join(git_out(fork, &["rev-parse", "--git-path", "toyos-fork-move"]).trim())
}

/// The submodules `tree` in `repo` records, by path, each with its commit.
fn gitlinks(repo: &Path, tree: &str) -> Vec<(String, String)> {
    git_out(repo, &["ls-tree", "-r", tree])
        .lines()
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            match meta.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["160000", "commit", commit] => Some((path.to_string(), commit.to_string())),
                _ => None,
            }
        })
        .collect()
}

/// The submodule at `path` in `repo` checked out at the commit `repo`'s index
/// records there, fetched as rustc's bootstrap fetches its LLVM: `git submodule
/// sync`, so its `origin` is the URL `repo`'s `.gitmodules` records, then `git
/// submodule update --init`, one commit deep where `.gitmodules` declares it
/// `shallow`. Its own fetch recurses into no submodule of its own, whose
/// `origin` is synced only once it has moved.
///
/// `repo` must be the primary's superproject or its own fork checkout, whose
/// submodules' git directories are their own: run in a linked worktree's fork
/// checkout, whose nested one is a git worktree of the primary's, `update`
/// writes that checkout's path as `core.worktree` into the config the
/// primary's shares, and git in the primary's fails.
fn submodule_update(repo: &Path, path: &str) {
    let shallow = format!("submodule.{path}.shallow");
    let shallow = git_out(repo, &["config", "--file", ".gitmodules", "--type", "bool", "--default", "false", "--get", &shallow]);
    git_run(repo, &["submodule", "sync", "--", path]);
    let depth: &[&str] = if shallow.trim() == "true" { &["--depth", "1"] } else { &[] };
    let update = ["-c", "fetch.recurseSubmodules=false", "submodule", "update", "--init", "--checkout"];
    git_run(repo, &[&update[..], depth, &["--", path]].concat());
}

/// Whether the repository at `dir` holds `commit`.
fn holds(dir: &Path, commit: &str) -> bool {
    git_try(dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// What a sysroot's [`SOURCES`] says: its key, and the witness of the sources
/// it was built from.
fn sources_text(key: &Key, witness: &str) -> String {
    format!("{key}\n{witness}\n")
}

/// The witness the sysroot at `dir` records it was built from ([`sources_text`]).
pub(crate) fn recorded_witness(dir: &Path) -> Result<String, String> {
    let path = dir.join(SOURCES);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    match text.split_once('\n') {
        Some((key, witness)) if Key::parse(key).is_some() && !witness.is_empty() => Ok(witness.trim_end_matches('\n').to_string()),
        _ => Err(format!("{} records no witness: {text:?}", path.display())),
    }
}

/// Why `dir` is not a directory [`publish`] finished, if it is not: it carries
/// no [`SOURCES`].
pub(crate) fn unpublished(dir: &Path) -> Option<String> {
    (!dir.join(SOURCES).is_file()).then(|| format!("{} carries no {SOURCES}", dir.display()))
}

/// Why `dir` is not a finished sysroot, if it is not: [`unpublished`], or not a
/// whole toolchain (`toolchain::toolchain_defect`). One found with the first and
/// not the second is made again rather than trusted — all of it even when only
/// its `bin/cargo` link dangles, because that is rare and a sysroot has no
/// repair path.
pub(crate) fn unfinished(dir: &Path) -> Option<String> {
    unpublished(dir).or_else(|| toolchain::toolchain_defect(dir))
}

/// The sysroot `key` names at `dir` in `store`, made by `make` if nobody has
/// made it, and held in use for as long as the returned guard lives.
fn held(store: &Path, key: &Key, dir: &Path, make: impl FnMut()) -> Guard {
    keystore::made(store, Keyed::Sysroot, key, || unfinished(dir), make)
}

/// The sysroot this worktree's sources name, in `store`: made if nobody on the
/// host has made it, and held in use for as long as the returned value lives.
pub fn ensure(root: &Path, store: &Path, lock: &mut Held) -> Sysroot {
    let fork = fork_checkout(root, lock);
    let compiler = compiler::resolve(root, store, &fork, lock);
    let keys = Keys::of(root, &compiler, &fork);
    let dir = Keyed::Sysroot.store(store).join(&keys.sysroot);

    let using =
        lock.without_shared(|| held(store, &keys.sysroot, &dir, || build(root, store, &compiler, &fork, &keys, &dir)));
    Sysroot { dir, identity: keys.identity, _using: Some(using) }
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
            panic!("no sysroot is made from {}, and no std was built for one: {defect}", compiler.stage2.display());
        }
        Whole(compiler)
    }
}

/// Make the sysroot `keys` names at `dir`, from `root`'s sources and the std
/// fork at `fork`, with `compiler`, and the freestanding libraries `keys`
/// names. The caller holds the sysroot key's lock.
fn build(root: &Path, store: &Path, compiler: &Compiler, fork: &Path, keys: &Keys, dir: &Path) {
    let made_from = whole(compiler);
    let freestanding = Keyed::Freestanding.store(store).join(&keys.freestanding);
    let _freestanding = keystore::made(
        store,
        Keyed::Freestanding,
        &keys.freestanding,
        || unpublished(&freestanding),
        || build_freestanding(root, compiler, fork, &keys.freestanding, &freestanding),
    );
    let what = format!("building sysroot {}", keys.sysroot);
    let _worktree = buildlock::worktree_exclusive(root, &what);
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
                    clone_tree(&freestanding.join(triple), &lib);
                }
            }
        }
        let miscompiles = dir.with_extension("miscompile");
        crate::miscompile::refuse(partial, &miscompiles);
        keystore::remove(&miscompiles);
        let libc_target = dir.with_extension("libc-target");
        for arch in Arch::ALL {
            crate::libc::build(root, partial, &libc_target, arch);
            crate::libc::build_c(root, partial, &libc_target, arch);
        }
        let _ = fs::remove_dir_all(&libc_target);
        let llvm = crate::llvm::resolve(root, store, fork);
        let ninja = crate::n2::ninja(root);
        for arch in Arch::ALL {
            let scratch = dir.with_extension(format!("libcxx-{}", arch.name()));
            let c = crate::clang::CSysroot::of(partial, arch);
            crate::libcxx::build(&c, &llvm.dir.join("src"), &ninja, &scratch);
        }

        // The sources the key named are the ones built, or this is not that key's.
        let (key, again) = (&keys.sysroot, Keys::of(root, compiler, fork).sysroot);
        assert!(
            again == *key,
            "the sources moved while sysroot {key} was being built (they are now {again}); \
             nothing was kept, and the next build makes the one they name"
        );
        sources_text(key, &witness(root))
    });
}

/// Make the freestanding targets' libraries `key` names at `store`, from the std
/// fork at `fork` with `compiler`. The caller holds the key's lock.
fn build_freestanding(root: &Path, compiler: &Compiler, fork: &Path, key: &Key, store: &Path) {
    let what = format!("building the freestanding libraries {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
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
        let again = freestanding_key(root, compiler.key(), fork);
        assert!(
            again == *key,
            "the sources moved while the freestanding libraries {key} were being built (they are \
             now {again}); nothing was kept, and the next build makes the ones they name"
        );
        format!("{key}\n")
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
    keystore::retire(dir);
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

/// Empty bootstrap's build directory `build_dir` of all but what bootstrap
/// downloaded unless `identity` is what its `compiled-by` records as having
/// built the rest, then record `identity` there: for a std build the compiler
/// ([`Compiler::identity`]), for a compiler build its build and its LLVM
/// (`compiler::place`).
///
/// Cargo keys what it reuses on `rustc -vV`, which every ToyOS compiler prints
/// alike, so another compiler's rlibs stay fresh and the next crate that does
/// recompile is refused against them (`E0463 can't find crate`). The removal
/// comes before the record, so an interrupted switch removes again.
pub(crate) fn forget_another_compiler(build_dir: &Path, host: &str, identity: &str) {
    forget_another_compiler_by(build_dir, host, identity, keystore::remove);
}

/// [`forget_another_compiler`], removing with `remove`, so a test can stop it.
fn forget_another_compiler_by(build_dir: &Path, host: &str, identity: &str, remove: impl Fn(&Path)) {
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
                remove(&entry.path());
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
/// so these libraries are built with the options `stage2`'s own were, save
/// the profile's debug assertions, which the libraries are built without, as
/// upstream's distributed ones are.
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
debug-assertions-std = false
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
    use crate::compiler::tests::compiler as placed_compiler;
    use crate::keystore::tests::{last_used, LONG_AGO};
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

    /// A worktree's trees, a store and a fork checkout, laid out the way the
    /// key reads them.
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
        (root, base.join("store"), fork)
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
        let (root, store, fork) = keyed(&base);
        let one = placed_compiler(&store, "tree-1");
        let k = || Keys::of(&root, &one, &fork);
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

        for tree in ["toyos/src", "sdk/std", "userland/libc/src"] {
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

        for read in ["userland/libc/Cargo.toml", "userland/libc/Cargo.lock", ".cargo/config.toml"] {
            write(&root.join(read), "[package]\nversion = \"0.2.0\"\n");
            sysroot_only(&format!("{read}, which std's lockfile does not resolve,"));
            write(&root.join(read), "[package]\nversion = \"0.1.0\"\n");
            same(&format!("{read} as it was"));
        }
        assert_eq!(key_of(&root, &was.freestanding, &build_text()), was.sysroot);
        for flag in [crate::libc::BUILD[1], crate::libc::BUILD_C[1]] {
            assert!(build_text().contains(flag), "the sysroot key reads none of libc's {flag}: {}", build_text());
        }
        let options = format!("{:?}", crate::libcxx::OPTIONS);
        assert!(build_text().contains(&options), "the sysroot key reads no C++ runtime option: {}", build_text());
        assert_ne!(key_of(&root, &was.freestanding, &build_text().replace("--release", "--profile=dev")), was.sysroot,
                   "libc's cargo invocation kept the sysroot");

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

        let compiler = one.key();
        let config = keyed_std_config();
        assert_eq!(freestanding_key_of(&root, compiler, &fork, RECIPE, &config), was.freestanding);
        assert_ne!(freestanding_key_of(&root, compiler, &fork, RECIPE, ""), was.freestanding,
                   "the freestanding key reads no std configuration");
        for (what, moved) in [
            ("the recipe", freestanding_key_of(&root, compiler, &fork, "another recipe", &config)),
            ("std's configuration", freestanding_key_of(&root, compiler, &fork, RECIPE, &format!("{config}\n[rust]\n"))),
        ] {
            assert_ne!(moved, was.freestanding, "{what} kept the old freestanding libraries");
        }

        let now = Keys::of(&root, &placed_compiler(&store, "tree-2"), &fork);
        assert!(now.sysroot != was.sysroot && now.freestanding != was.freestanding, "another compiler kept a key: {now:?}");
        assert_eq!(now.identity.stale(Some(&stamp)), Some(Stale::All), "another compiler kept a crate's host half");
    }

    /// **A submodule is the commit its gitlink records, checked out or not**:
    /// a fork whose `library/backtrace` is not checked out yet, as a runner's
    /// is when it keys the stores its build then makes, keys its freestanding
    /// libraries as it does once the build has checked it out. Another commit
    /// moves the key; an edit there, or a gitlink staged and not committed, is
    /// refused.
    #[test]
    fn a_submodule_is_the_commit_its_gitlink_records_checked_out_or_not() {
        let base = TempDir::new("key-submodule");
        let (root, store, _) = keyed(&base);
        let one = placed_compiler(&store, "tree-1");
        let backtrace = base.join("backtrace-src");
        write(&backtrace.join("src/lib.rs"), "pub fn trace() {}\n");
        git(&backtrace, &["init", "-q"]);
        git(&backtrace, &["add", "-A"]);
        git(&backtrace, &["commit", "-qm", "backtrace"]);
        let fork = base.join("fork-src");
        write(&fork.join("library/std/src/lib.rs"), "pub fn exit() {}\n");
        write(&fork.join("src/bootstrap/src/lib.rs"), "fn main() {}\n");
        git(&fork, &["init", "-q"]);
        git(&fork, &["submodule", "add", "-q", backtrace.to_str().unwrap(), "library/backtrace"]);
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "the fork"]);
        let clone = base.join("clone");
        git(&base, &["clone", "-q", fork.to_str().unwrap(), clone.to_str().unwrap()]);

        let k = || freestanding_key(&root, one.key(), &clone);
        assert!(fs::read_dir(clone.join("library/backtrace")).unwrap().next().is_none(), "the clone checked its submodule out");
        let unchecked = k();
        git(&clone, &["submodule", "update", "-q", "--init", "library/backtrace"]);
        assert_eq!(k(), unchecked, "checking the submodule out moved the key");

        write(&clone.join("library/backtrace/src/lib.rs"), "pub fn trace() { loop {} }\n");
        let said = refusal(|| drop(k()));
        assert!(said.contains("library/backtrace holds changes no commit does"), "{said}");
        git(&clone.join("library/backtrace"), &["commit", "-qam", "another backtrace"]);
        git(&clone, &["add", "library/backtrace"]);
        let said = refusal(|| drop(k()));
        assert!(said.contains("stages library/backtrace"), "{said}");
        git(&clone, &["commit", "-qm", "another backtrace"]);
        assert_ne!(k(), unchecked, "another backtrace commit kept the key");
    }

    /// **A sysroot's recorded witness is the one its build wrote**, read back
    /// whole; a `SOURCES` naming a key alone, as the freestanding libraries'
    /// does, records none.
    #[test]
    fn a_sysroot_records_the_witness_it_was_built_from() {
        let dir = TempDir::new("recorded-witness");
        let witness = "toyos-abi/src/lib.rs:0011223344556677\ntoyos/Cargo.toml:8899aabbccddeeff";
        fs::write(dir.join(SOURCES), sources_text(&Key::of(b"a sysroot"), witness)).unwrap();
        assert_eq!(recorded_witness(&dir), Ok(witness.to_string()));
        fs::write(dir.join(SOURCES), "0123456789abcdef\n").unwrap();
        assert!(recorded_witness(&dir).is_err(), "a SOURCES naming a key alone recorded a witness");
    }

    /// **A crate's compiler is the one that built it, made again or not**: a
    /// compiler swept and made again under its key leaves another driver, which
    /// leaves all of a crate stale, and the same driver none of it.
    #[test]
    fn a_compiler_made_again_under_its_key_leaves_all_of_a_crate_stale() {
        let base = TempDir::new("identity-compiler");
        let (root, store, fork) = keyed(&base);
        let one = placed_compiler(&store, "tree-1");
        let identity = || Keys::of(&root, &one, &fork).identity;
        let stamp = identity().to_string();
        assert_eq!(identity().stale(Some(&stamp)), None, "a crate was stale against the compiler that built it");
        write(&one.stage2.join("lib/librustc_driver-1.dylib"), "a driver, made again");
        assert_eq!(identity().stale(Some(&stamp)), Some(Stale::All), "a crate kept what the compiler made before it was made again");
    }

    /// **What one compiler compiled in a std build directory is never another's**:
    /// all but the downloads goes when the compiler changes, or when nothing
    /// says which compiler made it, and stays while it does not; the downloads
    /// stay either way.
    #[test]
    fn another_compiler_s_std_build_goes_and_the_same_one_s_stays() {
        let base = TempDir::new("compiled-by");
        let (_root, store, _fork) = keyed(&base);
        let (one, other) = (placed_compiler(&store, "tree-1").identity(), placed_compiler(&store, "tree-2").identity());
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

        lay();
        forget_another_compiler(&build, "host", &one);
        for file in &compiled {
            assert!(!file.exists(), "{} was kept, and no record names a compiler for it", file.display());
        }
        assert!(downloaded.iter().all(|f| f.is_file()), "a download went");

        lay();
        forget_another_compiler(&build, "host", &one);
        assert!(compiled.iter().all(|f| f.is_file()), "the same compiler's build went");

        forget_another_compiler(&build, "host", &other);
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
        let (_root, store, _fork) = keyed(&base);
        let build = base.join("toyos-std");
        let host = host_triple();
        let identity = placed_compiler(&store, "tree-1").identity();
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
        let base = TempDir::new("compiled-by-stuck");
        let (_root, store, _fork) = keyed(&base);
        let build = base.join("toyos-std");
        let (one, other) = (placed_compiler(&store, "tree-1").identity(), placed_compiler(&store, "tree-2").identity());
        fs::create_dir_all(&build).unwrap();
        forget_another_compiler(&build, "host", &one);
        let deps = build.join("bootstrap/debug/deps");
        write(&deps.join("libserde-1.rlib"), "built");

        // A removal that fails as `keystore::remove` does, whoever runs it.
        let stuck = std::panic::catch_unwind(|| {
            forget_another_compiler_by(&build, "host", &other, |path| panic!("remove {}: refused", path.display()))
        });
        let refusal = stuck.expect_err("a build that could not be removed was taken for removed");
        let refusal = refusal.downcast_ref::<String>().expect("a formatted panic");
        assert_eq!(*refusal, format!("remove {}: refused", build.join("bootstrap").display()));
        assert_ne!(fs::read_to_string(build.join("compiled-by")).unwrap(), other,
                   "the new compiler was recorded over a build it did not remove");

        forget_another_compiler(&build, "host", &other);
        assert!(!build.join("bootstrap").exists(), "the next call kept the build the stuck one could not remove");
        assert_eq!(fs::read_to_string(build.join("compiled-by")).unwrap(), other);
    }

    /// A primary with the fork as its `rust` submodule at `C1`, the fork's `C2`
    /// one library change and one `library/backtrace` commit later, and a linked
    /// worktree whose tree pins `C2`.
    fn two_pins(base: &Path) -> (PathBuf, PathBuf, String, String) {
        let bt = base.join("backtrace-src");
        fs::create_dir_all(&bt).unwrap();
        git(&bt, &["init", "-q"]);
        write(&bt.join("lib.rs"), "pub fn trace() {}\n");
        git(&bt, &["add", "-A"]);
        git(&bt, &["commit", "-qm", "backtrace"]);
        let b1 = git(&bt, &["rev-parse", "HEAD"]);
        write(&bt.join("lib.rs"), "pub fn trace_more() {}\n");
        git(&bt, &["commit", "-qam", "backtrace, later"]);

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn a() {}\n");
        write(&fork.join("compiler/lib.rs"), "\n");
        write(&fork.join("x.py"), "\n");
        git(&fork, &["submodule", "add", "-q", bt.to_str().unwrap(), "library/backtrace"]);
        git(&fork.join("library/backtrace"), &["checkout", "-q", &b1]);
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "C1"]);
        let c1 = git(&fork, &["rev-parse", "HEAD"]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn b() {}\n");
        git(&fork.join("library/backtrace"), &["checkout", "-q", "-"]);
        git(&fork, &["commit", "-qam", "C2"]);
        let c2 = git(&fork, &["rev-parse", "HEAD"]);

        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        write(&primary.join("toyos-abi/src/lib.rs"), "pub struct A;\n");
        write(&primary.join(".gitignore"), ".build-locks/\n");
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
        let mut lock = buildlock::shared(&linked, "a build");

        let fork = fork_checkout(&linked, &mut lock);

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
        let c3 = git(&fork, &["rev-parse", "HEAD"]);
        assert_eq!(fork_checkout(&linked, &mut lock), fork);
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c3, "a checkout ahead of its pin was moved");
        // Even where a move of it was killed before the agent committed.
        fs::write(moving(&fork), "").unwrap();
        assert_eq!(fork_checkout(&linked, &mut lock), fork);
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c3, "a checkout ahead of its pin was moved to finish a move");
        fs::remove_file(moving(&fork)).unwrap();

        // A clean checkout behind what the tree pins is moved to the pin itself.
        git(&fork, &["checkout", "-q", &c1]);
        assert_eq!(fork_checkout(&linked, &mut lock), fork);
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c2, "a checkout behind its pin was not moved to it");
        let behind = git(&fork, &["rev-parse", &format!("{c1}:library/backtrace")]);
        git(&fork.join("library/backtrace"), &["checkout", "-q", "--detach", &behind]);
        git(&fork, &["checkout", "-q", &c1]);
        assert_eq!(fork_checkout(&linked, &mut lock), fork);
        assert_eq!(
            git(&fork.join("library/backtrace"), &["rev-parse", "HEAD"]),
            git(&fork, &["rev-parse", "HEAD:library/backtrace"]),
            "a nested submodule was left behind its pin"
        );
        assert_eq!(git(&primary.join("rust"), &["status", "--porcelain"]), before, "the primary's nested submodule moved");

        // One with local changes is never moved out from under whoever made them.
        git(&fork, &["checkout", "-q", &c1]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn uncommitted() {}\n");
        let message = refusal(|| drop(fork_checkout(&linked, &mut lock)));
        assert!(message.contains(&c1) && message.contains(&c2), "{message}");
        git(&fork, &["checkout", "-q", "--", "."]);

        // Nor one whose nested submodule holds a commit of the agent's that no gitlink records yet.
        let nested = fork.join("library/backtrace");
        git(&nested, &["checkout", "-q", "--detach", &behind]);
        write(&nested.join("lib.rs"), "pub fn trace_mine() {}\n");
        git(&nested, &["commit", "-qam", "the agent's own, not yet recorded"]);
        let mine = git(&nested, &["rev-parse", "HEAD"]);
        let recorded = git(&fork, &["rev-parse", &format!("{c2}:library/backtrace")]);
        let message = refusal(|| drop(fork_checkout(&linked, &mut lock)));
        assert!(message.contains(&mine) && message.contains(&recorded), "{message}");
        assert_eq!(git(&nested, &["rev-parse", "HEAD"]), mine, "a nested commit nothing records was moved off");
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c1, "a checkout over a nested commit nothing records was moved");

        // Nor when that commit sits on top of what the pin records there.
        git(&nested, &["checkout", "-q", "--detach", &recorded]);
        write(&nested.join("lib.rs"), "pub fn trace_mine_later() {}\n");
        git(&nested, &["commit", "-qam", "the agent's own, on the pin's record"]);
        let mine = git(&nested, &["rev-parse", "HEAD"]);
        let message = refusal(|| drop(fork_checkout(&linked, &mut lock)));
        assert!(message.contains(&mine) && message.contains(&recorded), "{message}");
        assert_eq!(git(&nested, &["rev-parse", "HEAD"]), mine, "a nested commit on the pin's record was moved off");
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c1, "a checkout over a nested commit on the pin's record was moved");
    }

    /// **Twelve builds starting at once in a worktree make its fork checkout
    /// once, and move one behind its pin once**: each holds the worktree's lock
    /// as a process of its own does, and each returns the checkout whole and at
    /// the pin.
    #[test]
    fn twelve_builds_at_once_make_and_move_one_fork_checkout() {
        let base = TempDir::new("fork-builds");
        let (_primary, linked, c1, c2) = two_pins(&base);
        let fork = linked.join("rust");
        let twelve_build = || {
            let start = std::sync::Barrier::new(12);
            std::thread::scope(|builds| {
                for _ in 0..12 {
                    builds.spawn(|| {
                        let mut lock = buildlock::shared(&linked, "a build");
                        start.wait();
                        assert_eq!(fork_checkout(&linked, &mut lock), fork);
                        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c2);
                        assert!(fork.join("library/backtrace/lib.rs").is_file(), "a build read a checkout still being made");
                    });
                }
            });
        };
        twelve_build();
        git(&fork, &["checkout", "-q", &c1]);
        twelve_build();
    }

    /// **A linked worktree's fork checkout runs no `git submodule`**: one whose
    /// pin records a nested commit no local repository holds, which only the
    /// URL the pin's tree records holds, is refused by name and not moved, and
    /// the primary's fork configuration is not written.
    #[test]
    fn a_linked_worktree_lacking_a_nested_commit_is_refused_and_the_primary_s_is_not_written() {
        let base = TempDir::new("fork-linked-nested");
        let (primary, linked, _c1, c2) = two_pins(&base);
        let mut lock = buildlock::shared(&linked, "a build");
        let fork = fork_checkout(&linked, &mut lock);
        let theirs = primary.join("rust");
        let configs = || {
            let modules = primary.join(".git/modules/rust");
            [modules.join("config"), modules.join("modules/library/backtrace/config")].map(|c| fs::read(c).unwrap())
        };
        // What git asks of a submodule's local path, and of no `https` URL.
        git(&theirs.join("library/backtrace"), &["config", "protocol.file.allow", "always"]);
        let before = configs();

        let bt = base.join("backtrace-moved");
        git(&base, &["clone", "-q", path_str(&base.join("backtrace-src")), path_str(&bt)]);
        write(&bt.join("lib.rs"), "pub fn trace_most() {}\n");
        git(&bt, &["commit", "-qam", "backtrace, moved"]);
        let b3 = git(&bt, &["rev-parse", "HEAD"]);
        let upstream = base.join("fork-src");
        git(&upstream, &["config", "--file", ".gitmodules", "submodule.library/backtrace.url", path_str(&bt)]);
        git(&upstream, &["update-index", "--cacheinfo", &format!("160000,{b3},library/backtrace")]);
        git(&upstream, &["add", ".gitmodules"]);
        git(&upstream, &["commit", "-qm", "C5"]);
        let c5 = git(&upstream, &["rev-parse", "HEAD"]);
        git(&theirs, &["-c", "fetch.recurseSubmodules=false", "fetch", "-q", path_str(&upstream), &c5]);
        git(&linked, &["update-index", "--cacheinfo", &format!("160000,{c5},rust")]);
        git(&linked, &["commit", "-qm", "pins C5"]);

        let said = refusal(|| drop(fork_checkout(&linked, &mut lock)));

        assert_eq!(configs(), before, "the primary's fork configuration was written");
        git(&theirs, &["status", "--porcelain", "--ignore-submodules=none"]);
        assert!(said.contains(&b3) && said.contains("linked worktree"), "{said}");
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), c2, "a checkout was moved over a nested commit it lacks");
    }

    /// Where a linked worktree's `rust/` is a fork checkout that is not whole,
    /// [`crate::ensure_shallow_fork`] refuses, and git in the primary's fork
    /// still runs.
    #[test]
    fn ensure_shallow_fork_initialises_no_submodule_in_a_linked_worktree() {
        let base = TempDir::new("fork-shallow");
        let (primary, linked, c1, c2) = two_pins(&base);
        let fork = linked.join("rust");
        fs::remove_dir(&fork).unwrap();
        git(&primary.join("rust"), &["worktree", "add", "-q", "--detach", "--no-checkout", path_str(&fork), &c2]);

        let refused = crate::ensure_shallow_fork(&linked);

        assert_eq!(git(&primary.join("rust"), &["rev-parse", "HEAD"]), c1);
        assert!(refused.as_ref().is_err_and(|why| why.contains("linked worktree")), "{refused:?}");
    }

    /// **The primary's fork checkout is the commit its tree pins**: one behind
    /// the pin, as a `git pull` leaves it, and one ahead of it are moved there,
    /// each nested submodule checked out in it with it; one whose nested
    /// gitlink alone is moved is no work and moves too; one with local changes
    /// is refused by name and not moved; a commit it or a nested submodule does
    /// not hold is fetched from the URL its pin's tree records, whatever its
    /// `origin` names; a move killed half-way is finished by the next; one not
    /// initialised yet is left to whoever initialises it, and git is run in no
    /// other repository.
    #[test]
    fn the_primary_s_fork_checkout_is_moved_to_its_pin() {
        let base = TempDir::new("fork-primary");
        let (primary, _linked, c1, c2) = two_pins(&base);
        let fork = primary.join("rust");
        let nested = fork.join("library/backtrace");
        let pin = |commit: &str| {
            git(&primary, &["update-index", "--cacheinfo", &format!("160000,{commit},rust")]);
            git(&primary, &["commit", "-qm", "a pin"]);
        };
        let head = || git(&fork, &["rev-parse", "HEAD"]);
        let recorded = |commit: &str| git(&fork, &["rev-parse", &format!("{commit}:library/backtrace")]);
        let nested_head = || git(&nested, &["rev-parse", "HEAD"]);
        let mut lock = buildlock::shared(&primary, "a build");

        pin(&c2);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!(head(), c2, "a checkout behind its pin was not moved to it");
        assert_eq!(nested_head(), recorded(&c2), "a nested submodule was left behind its pin");

        pin(&c1);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!(head(), c1, "a checkout ahead of its pin was kept");
        assert_eq!(nested_head(), recorded(&c1), "a nested submodule was left ahead of its pin");

        git(&nested, &["checkout", "-q", "--detach", &recorded(&c2)]);
        pin(&c2);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!((head(), nested_head()), (c2.clone(), recorded(&c2)), "a moved nested gitlink was read as work");

        pin(&c1);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        pin(&c2);
        write(&fork.join("library/std/src/lib.rs"), "pub fn uncommitted() {}\n");
        let said = refusal(|| drop(fork_checkout(&primary, &mut lock)));
        assert!(said.contains(&format!("is at {c1} with uncommitted work")) && said.contains(&c2), "{said}");
        assert_eq!(head(), c1, "a checkout with local changes was moved");
        git(&fork, &["checkout", "-q", "--", "."]);

        // The owner's case: each checkout's `origin` names a repository that holds none of the commits to come, as one
        // made before `.gitmodules` named the fork's own does, and only the URL the pin's tree records holds them.
        let stale = base.join("stale");
        git(&base, &["init", "-q", "--bare", path_str(&stale)]);
        git(&nested, &["push", "-q", path_str(&stale), "HEAD:refs/heads/stale-only"]);
        for at in [&fork, &nested] {
            git(at, &["remote", "set-url", "origin", path_str(&stale)]);
            // What git asks of a submodule's local path, and of no `https` URL.
            git(at, &["config", "protocol.file.allow", "always"]);
        }
        let origin = |at: &Path| git(at, &["remote", "get-url", "origin"]);
        let worktree_of = |at: &Path| git(at, &["config", "core.worktree"]);
        let worktrees = (worktree_of(&fork), worktree_of(&nested));

        let moved = base.join("fork-moved");
        git(&base, &["clone", "-q", path_str(&base.join("fork-src")), path_str(&moved)]);
        write(&moved.join("library/std/src/lib.rs"), "pub fn c() {}\n");
        git(&moved, &["commit", "-qam", "C3"]);
        let c3 = git(&moved, &["rev-parse", "HEAD"]);
        git(&primary, &["config", "--file", ".gitmodules", "submodule.rust.url", path_str(&moved)]);
        git(&primary, &["add", ".gitmodules"]);
        pin(&c3);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!(head(), c3, "a pin only the URL its tree records holds was not fetched");
        assert_eq!(origin(&fork), path_str(&moved), "the fork checkout's origin is not the URL its pin's tree records");

        let bt = base.join("backtrace-moved");
        git(&base, &["clone", "-q", path_str(&base.join("backtrace-src")), path_str(&bt)]);
        write(&bt.join("lib.rs"), "pub fn trace_most() {}\n");
        git(&bt, &["commit", "-qam", "backtrace, moved"]);
        let b3 = git(&bt, &["rev-parse", "HEAD"]);
        git(&moved, &["config", "--file", ".gitmodules", "submodule.library/backtrace.url", path_str(&bt)]);
        git(&moved, &["config", "--file", ".gitmodules", "submodule.library/backtrace.shallow", "true"]);
        git(&moved, &["update-index", "--cacheinfo", &format!("160000,{b3},library/backtrace")]);
        git(&moved, &["add", ".gitmodules"]);
        git(&moved, &["commit", "-qm", "C4"]);
        let c4 = git(&moved, &["rev-parse", "HEAD"]);
        pin(&c4);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!((head(), nested_head()), (c4, b3), "a nested pin only its pin's URL holds was not fetched");
        assert_eq!(origin(&nested), path_str(&bt), "the nested checkout's origin is not the URL its pin's tree records");
        assert_eq!(git(&nested, &["rev-parse", "--is-shallow-repository"]), "true", "a shallow submodule was fetched whole");
        assert_eq!((worktree_of(&fork), worktree_of(&nested)), worktrees, "a git directory was given another worktree");
        assert!(
            git_try(&nested, &["rev-parse", "--verify", "-q", "refs/remotes/origin/stale-only"]).is_err(),
            "the fork's fetch recursed into a nested submodule, from the origin it had before its move"
        );

        // A move killed after the checkout moved and before its nested submodule did is finished by the next.
        let b2 = recorded(&c3);
        fs::write(moving(&fork), "").unwrap();
        git(&fork, &["checkout", "-q", "--detach", &c3]);
        pin(&c3);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!((head(), nested_head()), (c3, b2), "a move killed half-way was left half-made");
        assert!(!moving(&fork).exists(), "a finished move is still in flight");

        fs::remove_dir_all(&fork).unwrap();
        fs::create_dir(&fork).unwrap();
        let superproject = git(&primary, &["rev-parse", "HEAD"]);
        assert_eq!(fork_checkout(&primary, &mut lock), fork);
        assert_eq!(git(&primary, &["rev-parse", "HEAD"]), superproject, "git ran in the superproject");
    }

    /// A compiler in `store`: `rustc` and `rust-lld`, and the C toolchain
    /// `src/clang.rs` provisions beside them if `clang`; no cargo.
    fn compiler_without_cargo(store: &Path, clang: bool) -> Compiler {
        let compiler = placed_compiler(store, "a compiler");
        write(&compiler.stage2.join("bin/rustc"), "rustc");
        let lld = toolchain::rust_lld(&compiler.stage2);
        write(&lld, "lld");
        if clang {
            for tool in ["clang", "llvm-ar", "ld.lld", "rust-objcopy"] {
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

    /// **A sysroot is whole, or it is made again**: a compiler without cargo is
    /// refused by name and nothing is published, and a sysroot found with its
    /// `SOURCES` and without its cargo is rebuilt rather than trusted, once.
    /// Placing one removes a sysroot nothing has used for the store's keep
    /// time.
    #[test]
    fn a_sysroot_is_whole_or_it_is_made_again() {
        let base = TempDir::new("whole");
        let sysroots = Keyed::Sysroot.store(&base);
        let compiler = compiler_without_cargo(&base, true);
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
        let fresh = sysroots.join(&key);
        let said = refusal(|| drop(held(&base, &key, &fresh, || make(&fresh, 1))));
        assert!(said.contains("is missing cargo"), "{said}");
        assert!(!fresh.exists() && !fresh.with_extension("partial").exists(), "a sysroot was published from a stage2 without cargo");
        assert_eq!(made.get(), 1);

        let key = Key::of(b"found");
        let dir = sysroots.join(&key);
        clone_tree(&compiler.stage2, &dir);
        write(&dir.join(SOURCES), "found\n");
        toolchain::provision_toolchain_cargo(&compiler.stage2);
        let unused = Key::of(b"unused for the keep time");
        let orphan = sysroots.join(&unused);
        fs::create_dir_all(&orphan).unwrap();
        last_used(&base, Keyed::Sysroot, &unused, LONG_AGO);
        let using = held(&base, &key, &dir, || make(&dir, 2));
        assert_eq!(made.get(), 2, "a sysroot without its cargo was trusted because it has SOURCES");
        assert!(!orphan.exists(), "placing a sysroot left one nothing had used for the keep time");
        assert_eq!(toolchain::toolchain_defect(&dir), None);
        assert!(dir.join("lib/rustlib/x86_64-unknown-toyos/lib/libstd.rlib").is_file());
        drop(using);
        drop(held(&base, &key, &dir, || make(&dir, 2)));
        assert_eq!(made.get(), 2, "a whole sysroot was made again");
    }

    /// **A sysroot that cannot be made whole is refused after one make, never
    /// made again**: a compiler without clang is refused before any std is
    /// built for it, with nothing published, and a make that leaves its sysroot
    /// not whole is refused by what it lacks.
    #[test]
    fn a_sysroot_that_cannot_be_made_whole_is_made_once_and_refused() {
        let base = TempDir::new("no-clang");
        let sysroots = Keyed::Sysroot.store(&base);
        let compiler = compiler_without_cargo(&base, false);
        toolchain::provision_toolchain_cargo(&compiler.stage2);
        let made = std::cell::Cell::new(0);
        let once = || {
            made.set(made.get() + 1);
            assert_eq!(made.get(), 1, "a sysroot that was not whole was made again");
        };

        let key = Key::of(b"cloned");
        let dir = sysroots.join(&key);
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
        assert!(said.contains(&compiler.stage2.display().to_string()), "{said}");
        assert!(!filled.get(), "a std was built for a sysroot of a compiler without clang");
        assert!(!dir.exists() && !dir.with_extension("partial").exists(), "a sysroot was published from a stage2 without clang");
        assert_eq!(made.get(), 1);

        made.set(0);
        let key = Key::of(b"made");
        let dir = sysroots.join(&key);
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
