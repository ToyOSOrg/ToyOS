//! Content-addressed sysroots: one per source identity, made by whichever
//! worktree first needs it, and shared by every worktree whose sources match.
//!
//! **A sysroot is a function of its key.** The key ([`key`]) is the identity
//! (`src/identity.rs`, so a comment is no change) of everything a sysroot is
//! built from: the three trees std and `libtoyos_c.a` compile
//! ([`SYSROOT_SOURCES`]), the std fork's `library/` and `src/bootstrap/` in the
//! checkout that builds it, and the compiler that builds it. `rust/build/
//! sysroots/<key>/` is a whole toolchain — the compiler's files cloned from the
//! primary's `stage2`, the guest targets' libraries built from this key's
//! sources — and nothing writes it after its [`SOURCES`] file exists. A build
//! compiles against the directory its own key names, so two worktrees with
//! different ABIs never refuse or wait for each other, and main and every branch
//! matching it share one copy.
//!
//! **Each worktree builds std in its own fork checkout, and nothing but the
//! primary's own sync moves the primary's.** The primary builds in its `rust/`;
//! a linked worktree in its own `rust/`, made on first need as a git worktree of
//! the primary's fork repository at the commit this tree pins ([`fork_checkout`]).
//! `library/std` names `toyos-abi` and `toyos` as `../../../`, so each
//! checkout's std compiles against its own worktree's ABI with nothing
//! rewritten. The build is bootstrap's stage-0 local rebuild: the primary's
//! `stage2` compiler compiles the checkout's `library/` for the guest targets
//! into `<checkout>/build/toyos-std/`. The compiler is built once, by the
//! primary, and a fork commit whose `compiler/` is not the one it was built from
//! is refused by name ([`check_compiler`]).
//!
//! Locks, in the one order every acquirer takes them: the key's
//! (`buildlock::sysroot_*`), with this worktree's build lock put down; then, to
//! build, this worktree's exclusively (its fork build directory is written);
//! then the global one shared, because the primary's compiler is read.
//!
//! A sysroot no worktree names any more is removed by [`sweep`], which
//! `--worktree remove` runs: each build records the key it used in its
//! worktree's `target/`, and a key no registered worktree records, that nobody
//! is making or using, goes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::buildlock::{self, Guard, Held};
use crate::identity;
use crate::toolchain::{self, host_triple, Owner, GUEST_TARGETS};

/// The per-worktree sources that end up inside a sysroot: std links `toyos-abi`
/// and `toyos`, and `libtoyos_c.a` is `userland/libc`.
pub const SYSROOT_SOURCES: [&str; 3] = ["toyos-abi/src", "toyos/src", "userland/libc/src"];

/// Their manifests, whose features and versions decide the same build.
const SYSROOT_MANIFESTS: [&str; 3] =
    ["toyos-abi/Cargo.toml", "toyos/Cargo.toml", "userland/libc/Cargo.toml"];

/// The file a finished sysroot carries last, naming what it was built from.
/// A directory without it is a build that did not finish.
const SOURCES: &str = "SOURCES";

/// What changes how a key's sources become a sysroot and is none of them: the
/// std build's recipe below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage-0 local rebuild, profile compiler, \
                      libtoyos_c merged, libraries from the stamp; 2";

/// Where each build records the key it compiled against, for [`sweep`].
const RECORD: &str = "target/toyos-sysroot-key";

/// The compiler `stage2` was built from, written by the primary: see
/// [`record_compiler`].
fn compiler_record(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-compiler")
}

/// Every sysroot on this host.
pub fn sysroots_dir(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/sysroots")
}

/// A sysroot a build compiles against, held in use for as long as this lives.
pub struct Sysroot {
    /// A toolchain directory: `RUSTUP_TOOLCHAIN` names it.
    pub dir: PathBuf,
    _using: Option<Guard>,
}

impl Sysroot {
    /// A checkout whose toolchain arrived as an artifact has one sysroot, the
    /// artifact's, which `toolchain::check_installed_toolchain` has matched to
    /// these sources.
    pub(crate) fn installed(stage2: PathBuf) -> Self {
        Self { dir: stage2, _using: None }
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first 16 hex digits of the SHA-256 of `data`.
fn short(data: &[u8]) -> String {
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
    for manifest in SYSROOT_MANIFESTS {
        let path = root.join(manifest);
        let data = fs::read(&path).unwrap_or_else(|e| panic!("witness {}: {e}", path.display()));
        lines.push(format!("{manifest}:{}", short(&data)));
    }
    lines.join("\n")
}

/// The identity of the source files under `paths` of the git checkout `base`,
/// as one hash.
///
/// **Source as git sees it**: tracked files and untracked ones no ignore rule
/// covers, into every submodule checked out there — never what a build or the
/// desktop leaves beside them (bootstrap's `__pycache__`, Finder's
/// `.DS_Store`), which would make a key that moves while it is being built.
fn tree_identity(base: &Path, paths: &[&str]) -> String {
    let mut files = Vec::new();
    source_files(base, paths, &mut files);
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

fn source_files(checkout: &Path, paths: &[&str], out: &mut Vec<PathBuf>) {
    let mut args = vec!["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--"];
    args.extend(paths);
    let listed = git_bytes(checkout, &args);
    let mut seen = BTreeSet::new();
    for entry in listed.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let path = checkout.join(String::from_utf8_lossy(entry).as_ref());
        if !seen.insert(path.clone()) {
            continue;
        }
        if path.join(".git").exists() {
            source_files(&path, &["."], out);
        } else if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            out.push(path);
        }
    }
}

/// The compiler as the key sees it: the source it was built from, as
/// [`record_compiler`] wrote it, and the driver that build left, so a rebuild
/// of the same source is a new compiler too.
fn compiler_identity(rust_dir: &Path) -> String {
    let record = compiler_record(rust_dir);
    let source = fs::read_to_string(&record).unwrap_or_else(|_| {
        panic!(
            "{} is missing, so no sysroot can say which compiler it was built with.\n\
             The primary checkout writes it: run `cargo run -- --build-only` in {} once.",
            record.display(),
            rust_dir.parent().unwrap_or(rust_dir).display(),
        )
    });
    let lib = toolchain::stage2(rust_dir).join("lib");
    let driver = fs::read_dir(&lib)
        .unwrap_or_else(|e| panic!("read {}: {e}", lib.display()))
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("librustc_driver"))
        .unwrap_or_else(|| panic!("{} holds no librustc_driver", lib.display()));
    let meta = driver.metadata().unwrap_or_else(|e| panic!("stat the driver: {e}"));
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{} {} {} {mtime}", source.trim(), driver.file_name().to_string_lossy(), meta.len())
}

/// What `checkout`'s `compiler/` is: its commit's tree, and whatever the working
/// tree changes in it.
fn compiler_source(checkout: &Path) -> String {
    let tree = git_out(checkout, &["rev-parse", "HEAD:compiler"]);
    let local = git_bytes(checkout, &["diff", "HEAD", "--", "compiler"]);
    if local.is_empty() {
        tree.trim().to_string()
    } else {
        format!("{} with local changes {}", tree.trim(), short(&local))
    }
}

/// Record which compiler `stage2` is. The primary calls this after a toolchain
/// build, and when the record is missing — its compiler stamp has just said
/// `stage2` is built from what its `rust/` holds.
pub fn record_compiler(rust_dir: &Path) {
    let record = compiler_record(rust_dir);
    let want = compiler_source(rust_dir);
    if fs::read_to_string(&record).ok().as_deref() != Some(want.as_str()) {
        fs::write(&record, &want).unwrap_or_else(|e| panic!("write {}: {e}", record.display()));
    }
}

/// Refuse a fork checkout whose `compiler/` is not the one `stage2` was built
/// from: its std would be compiled by a compiler it was not written for.
fn check_compiler(rust_dir: &Path, fork: &Path) {
    let record = compiler_record(rust_dir);
    let built = fs::read_to_string(&record).unwrap_or_default();
    let here = compiler_source(fork);
    assert!(
        built.trim() == here,
        "{} holds the fork at a `compiler/` ({here}) the shared compiler was not built from \
         ({}).\nA compiler change is built once, by the primary: land it, and the primary's \
         sync and next build rebuild the compiler every worktree uses.",
        fork.display(),
        if built.is_empty() { "nothing recorded" } else { built.trim() },
    );
}

/// The key of the sysroot `root` builds against with its std fork at `fork`.
pub fn key(root: &Path, rust_dir: &Path, fork: &Path) -> String {
    let parts = [
        format!("{RECIPE}; cargo {STAGE0_CARGO}; targets {}", GUEST_TARGETS.join(" ")),
        witness(root),
        tree_identity(fork, &["library", "src/bootstrap"]),
        compiler_identity(rust_dir),
    ];
    short(parts.join("\n\0\n").as_bytes())
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

/// The key `root`'s last build compiled against.
pub fn recorded_key(root: &Path) -> Option<String> {
    fs::read_to_string(root.join(RECORD)).ok().map(|k| k.trim().to_string())
}

/// Whether `dir` is a finished sysroot.
fn finished(dir: &Path) -> bool {
    dir.join(SOURCES).is_file()
}

/// The sysroot this worktree's sources name, made if nobody has made it, and
/// held in use for as long as the returned value lives.
pub fn ensure(root: &Path, rust_dir: &Path, lock: &mut Held) -> Sysroot {
    let fork = fork_checkout(root);
    if fork != rust_dir {
        check_compiler(rust_dir, &fork);
    }
    let key = key(root, rust_dir, &fork);
    let dir = sysroots_dir(rust_dir).join(&key);
    let record = root.join(RECORD);
    fs::create_dir_all(record.parent().expect("a file under target/")).ok();
    fs::write(&record, &key).unwrap_or_else(|e| panic!("write {}: {e}", record.display()));

    let using = lock.without_shared(|| loop {
        let using = buildlock::sysroot_using(root, &key);
        if finished(&dir) {
            break using;
        }
        drop(using);
        let _building = buildlock::sysroot_building(root, &key);
        if !finished(&dir) {
            build(root, rust_dir, &fork, &key, &dir);
        }
    });
    toolchain::assert_toolchain_is_honest(&dir);
    Sysroot { dir, _using: Some(using) }
}

/// Make the sysroot `key` names at `dir`, from `root`'s sources and the std fork
/// at `fork`. The caller holds the key's lock.
fn build(root: &Path, rust_dir: &Path, fork: &Path, key: &str, dir: &Path) {
    let what = format!("building sysroot {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
    let _compiler = buildlock::compiler_shared(root, &what);
    eprintln!("Building sysroot {key}: std from {}, the compiler from {}", fork.display(), rust_dir.display());

    let built = build_std(root, rust_dir, fork);
    let partial = dir.with_extension("partial");
    if partial.exists() {
        fs::remove_dir_all(&partial).unwrap_or_else(|e| panic!("remove {}: {e}", partial.display()));
    }
    clone_tree(&toolchain::stage2(rust_dir), &partial);
    for target in GUEST_TARGETS {
        place_std(&stamp(&built, target), &partial.join("lib/rustlib").join(target).join("lib"));
    }
    let libc_target = dir.with_extension("libc-target");
    for arch in toolchain::USERLAND_ARCHS {
        crate::libc::build(root, &partial, &libc_target, arch);
    }
    let _ = fs::remove_dir_all(&libc_target);

    // The sources the key named are the ones built, or this is not that key's.
    let again = self::key(root, rust_dir, fork);
    assert!(
        again == key,
        "the sources moved while sysroot {key} was being built (they are now {again}); \
         nothing was kept, and the next build makes the one they name"
    );
    fs::write(
        partial.join(SOURCES),
        format!("{key}\nfork {}\n{}\n", fork.display(), witness(root)),
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCES).display()));
    fs::rename(&partial, dir)
        .unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
}

/// Compile the guest targets' libraries from `fork`'s `library/` with the
/// primary's compiler, and return the directory each target's is under.
fn build_std(root: &Path, rust_dir: &Path, fork: &Path) -> PathBuf {
    if fork == rust_dir {
        crate::ensure_submodule(fork, "library/backtrace");
    }
    let host = host_triple();
    let build_dir = fork.join("build/toyos-std");
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    // Bootstrap reuses what it built before and does not see a path dependency
    // outside the fork move, so each target's std starts from nothing.
    for target in GUEST_TARGETS {
        let _ = fs::remove_dir_all(build_dir.join(&host).join("stage0-std").join(target));
    }
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, std_config(rust_dir, &build_dir, &host, &toolchain::toyos_ld_binary(root)))
        .unwrap_or_else(|e| panic!("write {}: {e}", config.display()));

    // Bootstrap re-locks `library/Cargo.lock` to this worktree's `toyos-abi` and
    // `toyos` versions. What it writes follows from their manifests, which the
    // key already names, so the fork's own file is put back as it was: the
    // checkout stays clean, and the key stays the one it was built for.
    let _lock = Restore::holding(&fork.join("library/Cargo.lock"));
    let targets = GUEST_TARGETS.join(",");
    let args = ["build", "library", "--stage", "0", "--config", path_str(&config), "--warnings", "warn",
                "--target", &targets];
    let (ok, log) = toolchain::x_build(fork, &args, "std");
    toolchain::refuse_on_compile_error(&log, "std");
    assert!(ok, "the std build failed, and nothing in its output was a compile error");
    for arch in toolchain::USERLAND_ARCHS {
        toolchain::assert_std_built_from(root, &build_dir.join(&host).join("stage0-std").join(arch.userland()));
    }
    build_dir.join(&host).join("stage0-std")
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
fn std_config(rust_dir: &Path, build_dir: &Path, host: &str, toyos_ld: &Path) -> String {
    let targets = GUEST_TARGETS.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
    let linker = toyos_ld.display();
    let userland: String = toolchain::USERLAND_ARCHS
        .iter()
        .map(|arch| format!("\n[target.{}]\nlinker = \"{linker}\"\n", arch.userland()))
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

[rust]
lld = false
{userland}"#,
        rustc = toolchain::stage2(rust_dir).join("bin/rustc").display(),
        cargo = bootstrap_cargo().display(),
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

/// A file put back to its bytes when this drops, however the scope ends.
struct Restore {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Restore {
    fn holding(path: &Path) -> Self {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        Self { path: path.to_path_buf(), bytes }
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        fs::write(&self.path, &self.bytes)
            .unwrap_or_else(|e| panic!("restore {}: {e}", self.path.display()));
    }
}

/// Copy `from` to `to`, a symbolic link as a link: `stage2`'s own point at
/// things that outlive it. `fs::copy` clones on APFS and reflinks where Linux
/// can, so a sysroot costs the bytes its own libraries differ by.
fn clone_tree(from: &Path, to: &Path) {
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

/// Remove every sysroot no registered worktree records and nobody is making
/// or using, and every half-built one nobody is making. Returns what went.
pub fn sweep(root: &Path) -> Vec<PathBuf> {
    let rust_dir = toolchain::rust_dir(root);
    let dir = sysroots_dir(&rust_dir);
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let named: BTreeSet<String> = git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter_map(|w| fs::read_to_string(Path::new(w).join(RECORD)).ok())
        .map(|k| k.trim().to_string())
        .collect();
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (key, whole) = match name.split_once('.') {
            Some((key, _)) => (key.to_string(), false),
            None => (name.clone(), true),
        };
        if whole && named.contains(&key) {
            continue;
        }
        let Some(_idle) = buildlock::sysroot_idle(root, &key) else { continue };
        let path = entry.path();
        fs::remove_dir_all(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        removed.push(path);
    }
    removed
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
}

fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run git in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    out.stdout
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_bytes(dir, args)).into_owned()
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("toyos-sysroot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"])
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

    /// A worktree's three trees, a fork checkout and a compiler, laid out the
    /// way the key reads them.
    fn keyed(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = scratch(name);
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
        write(&compiler_record(&rust_dir), "tree-1");
        write(&toolchain::stage2(&rust_dir).join("lib/librustc_driver-1.dylib"), "a driver");
        (root, rust_dir, fork)
    }

    /// **The key is the identity, and only the identity**: a comment in any tree
    /// it reads — the ABI or the std fork — is the same sysroot, and a signature,
    /// a line of the fork's code or another compiler is another.
    #[test]
    fn a_comment_is_the_same_sysroot_and_a_signature_is_another() {
        let (root, rust_dir, fork) = keyed("key");
        let k = || key(&root, &rust_dir, &fork);
        let base = k();
        assert_eq!(base.len(), 16, "{base}");

        let abi = root.join("toyos-abi/src/lib.rs");
        write(&abi, "//! The crate.\n/// A, said better.\n// and a plain comment\npub struct A;\n");
        assert_eq!(k(), base, "a comment in toyos-abi made a new sysroot");
        write(&abi, "/// A.\npub struct A(pub u64);\n");
        assert_ne!(k(), base, "a signature change kept the old sysroot");
        write(&abi, "/// A.\npub struct A;\n");
        assert_eq!(k(), base);

        let std = fork.join("library/std/src/lib.rs");
        write(&std, "//! std, documented\npub fn exit() {}\n");
        assert_eq!(k(), base, "a comment in the std fork made a new sysroot");
        write(&std, "//! std\npub fn exit() { loop {} }\n");
        assert_ne!(k(), base, "a change to the fork's code kept the old sysroot");
        write(&std, "//! std\npub fn exit() {}\n");
        assert_eq!(k(), base);

        // What a build and the desktop leave in the checkout is not its source.
        write(&fork.join("src/bootstrap/__pycache__/bootstrap.cpython-313.pyc"), "bytecode");
        write(&fork.join("library/.DS_Store"), "finder");
        assert_eq!(k(), base, "a file git ignores moved the key");
        write(&fork.join("library/std/src/new.rs"), "pub fn new() {}\n");
        assert_ne!(k(), base, "an untracked source file was not in the key");
        fs::remove_file(fork.join("library/std/src/new.rs")).unwrap();
        assert_eq!(k(), base);

        write(&root.join("toyos-abi/Cargo.toml"), "[package]\nversion = \"0.2.0\"\n");
        assert_ne!(k(), base, "a manifest change kept the old sysroot");
        write(&root.join("toyos-abi/Cargo.toml"), "[package]\nversion = \"0.1.0\"\n");
        assert_eq!(k(), base);

        write(&compiler_record(&rust_dir), "tree-2");
        assert_ne!(k(), base, "another compiler kept the old sysroot");
    }

    /// A primary with the fork as its `rust` submodule at `C1`, the fork's `C2`
    /// one library change later, and a linked worktree whose tree pins `C2`.
    fn two_pins(name: &str) -> (PathBuf, PathBuf, String, String) {
        let base = scratch(name);
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
        let (primary, linked, c1, c2) = two_pins("fork-pins");
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

    /// A key no registered worktree records goes, and so does a half-built one;
    /// a key a worktree records stays, and so does one somebody is using.
    #[test]
    fn a_sweep_removes_what_no_worktree_names_and_nobody_uses() {
        let root = scratch("sweep");
        git(&root, &["init", "-q"]);
        write(&root.join("f"), "x\n");
        git(&root, &["add", "f"]);
        git(&root, &["commit", "-qm", "init"]);
        let linked = root.join("linked");
        git(&root, &["worktree", "add", "-q", "-b", "wt", linked.to_str().unwrap()]);

        let dir = sysroots_dir(&root.join("rust"));
        for name in ["named", "linked-named", "in-use", "orphan", "named.partial"] {
            fs::create_dir_all(dir.join(name)).unwrap();
        }
        write(&root.join(RECORD), "named");
        write(&linked.join(RECORD), "linked-named");
        let using = buildlock::sysroot_using(&root, "in-use");

        let mut removed = sweep(&root);
        removed.sort();
        assert_eq!(removed, [dir.join("named.partial"), dir.join("orphan")]);
        for stays in ["named", "linked-named", "in-use"] {
            assert!(dir.join(stays).is_dir(), "{stays} was swept");
        }
        drop(using);
        assert_eq!(sweep(&root), [dir.join("in-use")]);
    }

    /// **What a stage-0 std build made is what its stamp names**: its
    /// `stage0-sysroot` holds the compiler's own libraries, and taking those was
    /// a sysroot built from sources it never compiled. Each kind goes where
    /// bootstrap puts it, whatever the directory held before goes, and a host
    /// library is refused.
    #[test]
    fn the_libraries_placed_are_the_ones_the_stamp_names() {
        let base = scratch("stamp");
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

    /// `--worktree remove` takes the worktree's fork checkout with it — git will
    /// not remove a worktree around one — unless that checkout holds the only
    /// copy of something.
    #[test]
    fn a_removed_worktree_takes_its_fork_checkout_and_refuses_to_lose_fork_work() {
        let (primary, linked, _c1, _c2) = two_pins("fork-remove");
        let fork = fork_checkout(&linked);
        write(&fork.join("library/std/src/lib.rs"), "pub fn unsaved() {}\n");
        let refused = std::panic::catch_unwind(|| crate::worktree::remove(&primary, linked.to_str().unwrap()));
        assert!(refused.is_err(), "a fork checkout with uncommitted work was removed");
        assert!(fork.join("library/std/src/lib.rs").is_file());

        git(&fork, &["commit", "-qam", "committed, and on no ref"]);
        let refused = std::panic::catch_unwind(|| crate::worktree::remove(&primary, linked.to_str().unwrap()));
        assert!(refused.is_err(), "a fork commit no ref reaches was thrown away");

        git(&fork, &["branch", "kept"]);
        crate::worktree::remove(&primary, linked.to_str().unwrap());
        assert!(!linked.exists(), "{} is still on disk", linked.display());
        let listed = git(&primary.join("rust"), &["worktree", "list", "--porcelain"]);
        assert_eq!(listed.lines().filter(|l| l.starts_with("worktree ")).count(), 1, "{listed}");
    }
}
