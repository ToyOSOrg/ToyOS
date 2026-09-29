//! Sysroots: a product of the store (`src/store.rs`), one per key, made by
//! whichever checkout first needs it and shared by every checkout whose
//! sources match.
//!
//! **A sysroot is a function of its key** ([`key`]): [`RECIPE`], the key of the
//! compiler that builds it, the std fork's `library/` and `src/bootstrap/`, and
//! the three trees std and `libtoyos_c.a` compile, `toyos-abi`, `toyos` and
//! `userland/libc`. `sysroots/<key>/` is a whole toolchain — the compiler's
//! files cloned from its `stage2`, the guest targets' libraries built from this
//! key's sources — and a build names it as `RUSTUP_TOOLCHAIN`.
//!
//! **The fork a checkout's toolchain is built from is a [`Fork`]**: the
//! primary's `rust/`, or a linked worktree's own `rust/` where it made one to
//! edit the fork, keyed as it stands and built where it is; for every other
//! linked worktree, the commit its tree pins, keyed from the primary's objects
//! and built in the host's one shared checkout, [`SHARED`], which such builds
//! hold one at a time. Bootstrap's stage-0 local rebuild compiles a checkout's
//! `library/` for the guest targets into its `build/toyos-std/`; `library/std`
//! names `toyos-abi` and `toyos` as `../../../`, so the checkout sits beside the
//! building worktree's trees, or beside links to them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::compiler::{self, Compiler};
use crate::dirlock::Lock;
use crate::store::{self, Kind, Relocked, Sources, ABI_TREES};
use crate::toolchain::{self, host_triple, Owner, GUEST_TARGETS};

/// What changes how a key's sources become a sysroot and is none of them: the
/// std build's recipe below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage-0 local rebuild, profile compiler, no LLVM, \
                      libtoyos_c merged, libraries from the stamp, linked by rust-lld, \
                      a C sysroot of libc's staticlib and headers per target, \
                      run by the compiler's own cargo; 7";

/// A sysroot a build compiles against, held in use for as long as this lives.
pub struct Sysroot {
    /// A toolchain directory: `RUSTUP_TOOLCHAIN` names it.
    pub dir: PathBuf,
    _held: Option<store::Held>,
}

impl Sysroot {
    /// A checkout whose toolchain arrived as an artifact has one sysroot, the
    /// artifact's, which `toolchain::check_installed_toolchain` has matched to
    /// these sources.
    pub(crate) fn installed(stage2: PathBuf) -> Self {
        Self { dir: stage2, _held: None }
    }
}

/// What a published toolchain records of the trees its std and libc compiled,
/// so an installed one is matched to a checkout by the same hashes the store
/// keys on (`src/release.rs`).
pub fn witness(root: &Path) -> String {
    let hashes = store::trees(root, &ABI_TREES, Relocked::No);
    ABI_TREES.iter().zip(hashes).map(|(tree, hash)| format!("{tree}:{hash}\n")).collect()
}

/// The key of the sysroot the compiler `compiler` builds from `sources`.
pub fn key(compiler: &str, sources: &Sources) -> String {
    let recipe = format!("{RECIPE}; targets {}", GUEST_TARGETS.join(" "));
    let trees = ["library", "src/bootstrap"].into_iter().chain(ABI_TREES).map(|tree| sources.get(tree));
    let parts: Vec<&str> = std::iter::once(compiler).chain(trees).collect();
    store::key(&recipe, &parts)
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

/// Where, in the primary's `rust/`, the host builds the toolchain of every
/// linked worktree whose `rust/` is the stub: one git worktree of the primary's
/// fork repository, sharing its objects, at `rust/`, and `toyos-abi` and
/// `toyos` beside it as links to the trees of the build that holds it.
pub const SHARED: &str = "build/fork";

/// The submodules a toolchain build needs, taken from the primary's own clone
/// of each where it holds the commit, so no build fetches what the host has.
const SUBMODULES: [&str; 2] = ["library/backtrace", "src/tools/cargo"];

/// The rust fork a checkout's toolchain is built from.
pub enum Fork {
    /// A fork checkout, keyed as it stands and built where it is: the
    /// primary's `rust/`, or a linked worktree's own, which is where the fork
    /// is edited.
    Checkout(PathBuf),
    /// The commit a linked worktree whose `rust/` is the stub pins, in the fork
    /// repository at `rust_dir`, the primary's: keyed from its objects and built
    /// in [`SHARED`].
    Pinned { rust_dir: PathBuf, commit: String },
}

impl Fork {
    /// The fork `root`'s toolchain is built from.
    ///
    /// A checkout is used as it stands when it is at or ahead of the commit
    /// this tree pins. The primary's that is not is refused, since nothing but
    /// its owner moves it; a linked worktree's is moved there, unless it holds
    /// uncommitted work, which is refused rather than moved out from under
    /// whoever made it.
    pub fn of(root: &Path) -> Fork {
        let pinned = pinned_fork(root);
        let own = root.join("rust");
        match toolchain::owner(root) {
            Owner::Installed => panic!("an installed toolchain has no fork to build from"),
            Owner::Us => {
                let head = git_out(&own, &["rev-parse", "HEAD"]);
                assert!(
                    at_or_ahead(&own, &pinned, head.trim()),
                    "{} is at {}, and this tree pins the fork at {pinned}, which that is not at or ahead \
                     of: a build here would make a toolchain this tree does not name. Move it there: \
                     `git -C {} checkout --detach {pinned}`",
                    own.display(),
                    head.trim(),
                    own.display(),
                );
                Fork::Checkout(own)
            }
            Owner::Elsewhere(_) if own.join(".git").exists() => {
                let behind = || {
                    let head = git_out(&own, &["rev-parse", "HEAD"]);
                    (!at_or_ahead(&own, &pinned, head.trim())).then_some(head)
                };
                if behind().is_none() {
                    return Fork::Checkout(own);
                }
                let _held = Lock::exclusive(&own, &format!("{}, behind a build in it", own.display()));
                if let Some(head) = behind() {
                    let dirty = git_out(&own, &["status", "--porcelain", "--ignore-submodules=none"]);
                    assert!(
                        dirty.is_empty(),
                        "{} is at {} with uncommitted work, and this tree pins the fork at {pinned}, which \
                         that is not at or ahead of: a build here would make a toolchain this tree does not \
                         name, and moving the checkout would lose that work.\n{dirty}",
                        own.display(),
                        head.trim(),
                    );
                    git_out(&own, &["checkout", "--detach", "-q", &pinned]);
                    eprintln!("{} was at {}, not at or ahead of this tree's pin {pinned}: checked it out", own.display(), head.trim());
                }
                Fork::Checkout(own)
            }
            Owner::Elsewhere(primary) => Fork::Pinned { rust_dir: primary.join("rust"), commit: pinned },
        }
    }

    /// `root`'s sources: its ABI trees as they stand, and this fork's trees.
    pub fn sources(&self, root: &Path) -> Sources {
        match self {
            Fork::Checkout(dir) => Sources::of(root, dir),
            Fork::Pinned { rust_dir, commit } => Sources::pinned(root, rust_dir, commit),
        }
    }

    /// A checkout to build `root`'s toolchain in, held for it alone for as
    /// long as the returned value lives: each build there empties the build
    /// directory the one before it built in.
    pub fn checkout(&self, root: &Path) -> Checkout {
        match self {
            Fork::Checkout(dir) => {
                Checkout { _held: Lock::exclusive(dir, &format!("a toolchain build in {}", dir.display())), dir: dir.clone() }
            }
            Fork::Pinned { rust_dir, commit } => shared(rust_dir, root, commit),
        }
    }
}

/// A fork checkout held by one build.
pub struct Checkout {
    pub dir: PathBuf,
    _held: Lock,
}

/// Whether `head`, in the fork checkout `dir`, is `commit` or ahead of it.
fn at_or_ahead(dir: &Path, commit: &str, head: &str) -> bool {
    git(dir, &["merge-base", "--is-ancestor", commit, head], None).is_ok()
}

/// [`SHARED`] in the fork repository at `rust_dir`, held for `root`, at
/// `commit`, beside links to `root`'s ABI trees. Nothing edits it, so whatever
/// a build killed in it left goes.
fn shared(rust_dir: &Path, root: &Path, commit: &str) -> Checkout {
    let base = rust_dir.join(SHARED);
    fs::create_dir_all(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
    let held = Lock::exclusive(&base, &format!("{}, behind another worktree's toolchain build", base.display()));
    let dir = base.join("rust");
    if !dir.join(".git").exists() {
        eprintln!("Making {} a fork checkout (a git worktree of {})", dir.display(), rust_dir.display());
        remove(&dir);
        git_out(rust_dir, &["worktree", "prune"]);
        git_out(rust_dir, &["worktree", "add", "--detach", path_str(&dir), commit]);
    }
    git_out(&dir, &["checkout", "--detach", "--force", "-q", commit]);
    git_out(&dir, &["clean", "-d", "--force", "-q"]);
    for path in SUBMODULES {
        share_submodule(rust_dir, &dir, path);
    }
    for tree in ["toyos-abi", "toyos"] {
        toolchain::swap_link(&root.join(tree), &base.join(tree));
    }
    Checkout { dir, _held: held }
}

/// Check out `fork`'s submodule `path` at the commit its gitlink names from the
/// primary's clone of it at `rust_dir`, sharing its objects, when that clone
/// holds the commit; bootstrap fetches it otherwise.
fn share_submodule(rust_dir: &Path, fork: &Path, path: &str) {
    let listed = git_out(fork, &["ls-tree", "HEAD", path]);
    let commit = listed.split_whitespace().nth(2).unwrap_or_else(|| panic!("{} pins no {path}: {listed:?}", fork.display()));
    let holds = |dir: &Path| git(dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")], None).is_ok();
    let at = fork.join(path);
    let theirs = rust_dir.join(path);
    if at.join(".git").exists() {
        if holds(&at) {
            git_out(&at, &["checkout", "--detach", "-q", commit]);
        }
    } else if theirs.join(".git").exists() && holds(&theirs) {
        remove(&at);
        git_out(&theirs, &["worktree", "add", "--detach", path_str(&at), commit]);
    }
}

/// The sysroot this worktree's sources name, made if nobody has made it, and
/// held in use for as long as the returned value lives.
pub fn ensure(root: &Path, rust_dir: &Path) -> Sysroot {
    let fork = Fork::of(root);
    let sources = fork.sources(root);
    let compiler = compiler::resolve(root, rust_dir, &fork, &sources);
    let key = key(&compiler.key, &sources);
    let held = store::get(root, rust_dir, Kind::Sysroot, &key, |partial| {
        let checkout = fork.checkout(root);
        assemble(&compiler.stage2, partial, |partial| build(root, &compiler, &checkout.dir, partial));
        let again = self::key(&compiler.key, &Sources::of(root, &checkout.dir));
        assert!(
            again == key,
            "the sources moved while sysroot {key} was being built (they are now {again}); \
             nothing was kept, and the next build makes the one they name"
        );
    });
    if let Some(defect) = toolchain::toolchain_defect(&held.dir) {
        panic!("sysroot {key} at {} is not whole: {defect}", held.dir.display());
    }
    Sysroot { dir: held.dir.clone(), _held: Some(held) }
}

/// Put in `partial` a whole toolchain: the compiler's files at `stage2` and
/// what `fill` adds to them. One that is not whole is refused.
fn assemble(stage2: &Path, partial: &Path, fill: impl FnOnce(&Path)) {
    clone_tree(stage2, partial);
    fill(partial);
    if let Some(defect) = toolchain::toolchain_defect(partial) {
        panic!("a sysroot was made from {}, and is not whole: {defect}", stage2.display());
    }
}

/// Build the guest targets' libraries from `fork`'s `library/` and `root`'s libc
/// with `compiler`, into `partial`.
fn build(root: &Path, compiler: &Compiler, fork: &Path, partial: &Path) {
    eprintln!("Building a sysroot: std from {}, the compiler {}", fork.display(), compiler.key);
    let built = build_std(root, compiler, fork);
    for target in GUEST_TARGETS {
        place_std(&stamp(&built, target), &partial.join("lib/rustlib").join(target).join("lib"));
    }
    // Inside the product being made, which nothing else writes or collects.
    let libc_target = partial.join(".libc-target");
    for arch in Arch::ALL {
        crate::libc::build(root, partial, &libc_target, arch);
        crate::libc::build_c(root, partial, &libc_target, arch);
    }
    fs::remove_dir_all(&libc_target).unwrap_or_else(|e| panic!("remove {}: {e}", libc_target.display()));
}

/// Compile the guest targets' libraries from `fork`'s `library/` with
/// `compiler`, and return the directory each target's is under.
fn build_std(root: &Path, compiler: &Compiler, fork: &Path) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let host = host_triple();
    let build_dir = fork.join("build/toyos-std");
    prepare_std_build(&build_dir, &host, &compiler.key);
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, std_config(&compiler.stage2, &compiler.stage2.join("bin/cargo"), &build_dir, &host))
        .unwrap_or_else(|e| panic!("write {}: {e}", config.display()));

    let targets = GUEST_TARGETS.join(",");
    let args = ["build", "library", "--stage", "0", "--config", path_str(&config), "--warnings", "warn",
                "--target", &targets];
    let (ok, log) = toolchain::x_build(fork, &args, "std");
    toolchain::refuse_on_compile_error(&log, "std");
    assert!(ok, "the std build failed, and nothing in its output was a compile error");
    for arch in Arch::ALL {
        toolchain::assert_std_built_from(root, &build_dir.join(&host).join("stage0-std").join(arch.userland()));
    }
    build_dir.join(&host).join("stage0-std")
}

/// Ready the std build directory `build_dir` for a build by the compiler `key`
/// names: nothing another compiler built, and no guest target's std.
fn prepare_std_build(build_dir: &Path, host: &str, key: &str) {
    fs::create_dir_all(build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    forget_another_compiler(build_dir, host, key);
    // Bootstrap reuses what it built before and does not see a path dependency
    // outside the fork move, so each target's std starts from nothing.
    for target in GUEST_TARGETS {
        remove(&build_dir.join(host).join("stage0-std").join(target));
    }
}

/// Empty the std build directory `build_dir` of all but what bootstrap
/// downloaded unless `identity`, a compiler's key, is the compiler its
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
                remove(&entry.path());
            }
        }
    }
    fs::write(&record, identity).unwrap_or_else(|e| panic!("write {}: {e}", record.display()));
}

/// Remove `path`, a directory with all it holds or anything else; that nothing
/// is there is not an error.
fn remove(path: &Path) {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match removed {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove {}: {e}", path.display()),
        _ => {}
    }
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
/// No LLVM: std builds none, and the profile's `download-ci-llvm` fetches one.
/// The cargo is the compiler's own: a local rebuild passes it the flags of the
/// fork's own version, which any other cargo may refuse.
fn std_config(compiler: &Path, cargo: &Path, build_dir: &Path, host: &str) -> String {
    let targets = GUEST_TARGETS.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
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

[rust]
lld = false
{userland}"#,
        rustc = compiler.join("bin/rustc").display(),
        cargo = cargo.display(),
        build_dir = build_dir.display(),
    )
}

/// Copy `from` to `to`, a symbolic link as a link. `fs::copy` clones on APFS
/// and reflinks where Linux can, so a sysroot costs the bytes its own libraries
/// differ by.
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

/// What `git args` printed in `dir`, with `index` as its index if given: the
/// build system's one git runner. `Err` names the command, the directory and
/// what git said.
pub(crate) fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let out = command.output().map_err(|e| format!("run git in {}: {e}", dir.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {args:?} in {}: {}", dir.display(), stderr.trim()));
    }
    Ok(out.stdout)
}

pub(crate) fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    git(dir, args, None).unwrap_or_else(|e| panic!("{e}"))
}

pub(crate) fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_bytes(dir, args)).into_owned()
}

/// The files `git` tracks under `dir` that `pathspecs` name, every one when
/// there are none, relative to `dir`. A name that is not UTF-8 is refused by
/// name rather than rewritten into one git does not track.
pub(crate) fn tracked_files(dir: &Path, pathspecs: &[&str]) -> Result<Vec<String>, String> {
    let args = [&["ls-files", "-z", "--"][..], pathspecs].concat();
    let listing = git(dir, &args, None)?;
    let names = listing.split(|b| *b == 0).filter(|f| !f.is_empty());
    names
        .map(|f| {
            String::from_utf8(f.to_vec()).map_err(|_| {
                format!("git tracks {:?} in {}, a name that is not UTF-8", String::from_utf8_lossy(f), dir.display())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{estate, git, refusal, write};
    use toyos_tmpdir::TempDir;

    /// **The key is the compiler's and the trees std and libc are built
    /// from**: an ABI edit, a std edit and another compiler are each another
    /// sysroot, and a compiler edit alone reaches it only through the
    /// compiler's key.
    #[test]
    fn a_sysroot_key_is_its_compiler_and_its_trees() {
        let e = estate("sysroot-key");
        let fork = e.same.join("rust");
        let k = |compiler: &str| key(compiler, &Sources::of(&e.same, &fork));
        let base = k("c1");
        assert_ne!(k("c2"), base, "another compiler kept the sysroot");
        write(&e.same.join("userland/libc/src/lib.rs"), "pub struct B;\n");
        assert_ne!(k("c1"), base, "a libc edit kept the sysroot");
        git(&e.same, &["checkout", "-q", "--", "userland"]);
        write(&e.same.join("userland/libc/Cargo.lock"), "# re-locked, and neither staged nor committed\n");
        assert_ne!(k("c1"), base, "an edit to libc's own lockfile kept the sysroot");
        git(&e.same, &["checkout", "-q", "--", "userland"]);
        assert_eq!(k("c1"), base);
        write(&fork.join("library/std/src/lib.rs"), "pub fn b() {}\n");
        assert_ne!(k("c1"), base, "a std edit kept the sysroot");
        git(&fork, &["checkout", "-q", "--", "library"]);
        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn t() { x() }\n");
        assert_eq!(k("c1"), base, "a compiler edit reached the sysroot but through the compiler's key");
    }

    /// **What one compiler compiled in a std build directory is never another's**:
    /// all but the downloads goes when the compiler changes, or when nothing
    /// says which compiler made it, and stays while it does not; the downloads
    /// stay either way.
    #[test]
    fn another_compiler_s_std_build_goes_and_the_same_one_s_stays() {
        let build = TempDir::new("compiled-by");
        let compiled = [
            build.join("bootstrap/debug/deps/libserde-1.rlib"),
            build.join("host/stage0-std/dist/build/std/build-script-build"),
            build.join("host/a-directory-bootstrap-adds/lib.rlib"),
            build.join("tmp/cc-rs-out-dir/out.o"),
            build.join("host/a-stamp-bootstrap-writes"),
        ];
        let downloaded = [build.join("cache/2026-07-13/rustc.tar.xz"), build.join("host/rustfmt/bin/rustfmt")];
        let lay = || {
            for file in compiled.iter().chain(&downloaded) {
                write(file, "built");
            }
        };
        lay();
        forget_another_compiler(&build, "host", "c1");
        for file in &compiled {
            assert!(!file.exists(), "{} was kept, and no record names a compiler for it", file.display());
        }
        assert!(downloaded.iter().all(|f| f.is_file()), "a download went");
        lay();
        forget_another_compiler(&build, "host", "c1");
        assert!(compiled.iter().all(|f| f.is_file()), "the same compiler's build went");
        forget_another_compiler(&build, "host", "c2");
        for file in &compiled {
            assert!(!file.exists(), "{} was kept for another compiler", file.display());
        }
        assert!(downloaded.iter().all(|f| f.is_file()), "a download went");
    }

    /// **A std build fetches no LLVM**: it builds none, and the `compiler`
    /// profile would download one.
    #[test]
    fn a_std_build_downloads_no_llvm() {
        let config = std_config(Path::new("/c"), Path::new("/cargo"), Path::new("/b"), "h");
        assert!(config.contains("\n[llvm]\ndownload-ci-llvm = false\n"), "{config}");
    }

    /// **A switch that cannot remove the other compiler's build fails and does
    /// not record the new compiler**, so the next call removes it.
    #[test]
    fn a_switch_that_cannot_remove_records_nothing_and_the_next_one_removes() {
        use std::os::unix::fs::PermissionsExt;
        let build = TempDir::new("compiled-by-stuck");
        forget_another_compiler(&build, "host", "c1");
        let deps = build.join("bootstrap/debug/deps");
        write(&deps.join("libserde-1.rlib"), "built");
        let mode = |bits| fs::set_permissions(&deps, fs::Permissions::from_mode(bits)).unwrap();
        mode(0o555);
        let stuck = std::panic::catch_unwind(|| forget_another_compiler(&build, "host", "c2"));
        mode(0o755);
        let refusal = stuck.expect_err("a build that could not be removed was taken for removed");
        let refusal = refusal.downcast_ref::<String>().expect("a formatted panic");
        assert!(refusal.starts_with(&format!("remove {}", build.join("bootstrap").display())), "{refusal}");
        assert_eq!(fs::read_to_string(build.join("compiled-by")).unwrap(), "c1", "the new compiler was recorded over a build it did not remove");
        forget_another_compiler(&build, "host", "c2");
        assert!(!build.join("bootstrap").exists(), "the next call kept the build the stuck one could not remove");
        assert_eq!(fs::read_to_string(build.join("compiled-by")).unwrap(), "c2");
    }

    /// **A worktree whose `rust/` is the stub holds no fork checkout of its
    /// own**: its toolchain is keyed from the primary's objects at the commit its
    /// tree pins, which is what a clean checkout of that commit hashes to, and
    /// built in the host's one shared checkout — held by one build at a time,
    /// moved to the pin of the build holding it with whatever a killed build left
    /// gone, beside links to that build's ABI trees. The primary's fork is not
    /// touched, and plain `git worktree remove` takes the worktree whole.
    #[test]
    fn a_stub_worktree_builds_in_the_host_s_shared_checkout() {
        let e = estate("fork-shared");
        let stub = |name: &str| {
            let worktree = e.same.parent().unwrap().join(name);
            git(&e.primary, &["worktree", "add", "-q", "-b", name, worktree.to_str().unwrap()]);
            worktree
        };
        let beside = |dir: &Path, tree: &str| fs::canonicalize(dir.join("library/std/../../..").join(tree)).unwrap();
        let linked = stub("stub");
        let pinned = git(&e.primary, &["rev-parse", "HEAD:rust"]);
        let before = git(&e.rust_dir, &["rev-parse", "HEAD"]);

        let fork = Fork::of(&linked);
        assert!(matches!(&fork, Fork::Pinned { commit, .. } if *commit == pinned));
        assert_eq!(fork.sources(&linked), Sources::of(&linked, &e.same.join("rust")), "the pin's trees are not a clean checkout's");
        assert!(!linked.join("rust/.git").exists() && !linked.join("target").exists(), "a stub worktree holds fork state");

        let checkout = fork.checkout(&linked);
        assert_eq!(checkout.dir, e.rust_dir.join(SHARED).join("rust"));
        assert_eq!(git(&checkout.dir, &["rev-parse", "HEAD"]), pinned);
        assert_eq!(beside(&checkout.dir, "toyos-abi"), fs::canonicalize(linked.join("toyos-abi")).unwrap());
        assert!(checkout.dir.join("library/backtrace/lib.rs").is_file(), "backtrace was not taken from the primary's clone");
        assert!(Lock::try_exclusive(&e.rust_dir.join(SHARED)).is_none(), "the shared checkout is not held");
        assert_eq!(git(&e.rust_dir, &["rev-parse", "HEAD"]), before, "the primary's fork moved");
        write(&checkout.dir.join("library/std/src/lib.rs"), "left by a killed build");
        write(&checkout.dir.join("left.rs"), "left by a killed build");
        drop(checkout);

        let other = stub("other");
        let moved = git(&e.a.join("rust"), &["rev-parse", "HEAD"]);
        git(&other, &["update-index", "--cacheinfo", &format!("160000,{moved},rust")]);
        let checkout = Fork::of(&other).checkout(&other);
        assert_eq!(git(&checkout.dir, &["rev-parse", "HEAD"]), moved, "a moved pin kept the old checkout");
        assert_eq!(git(&checkout.dir, &["status", "--porcelain"]), "", "what a killed build left stayed");
        assert_eq!(beside(&checkout.dir, "toyos"), fs::canonicalize(other.join("toyos")).unwrap(), "the links name the last build's trees");
        drop(checkout);

        git(&e.primary, &["worktree", "remove", linked.to_str().unwrap()]);
        assert!(!linked.exists(), "git worktree remove left {}", linked.display());
    }

    /// **A fork checkout is built as it stands when it is at or ahead of its
    /// pin**; a linked worktree's behind it is moved there when clean and
    /// refused, its work named, when not; and the primary's behind it is
    /// refused, since nothing but its owner moves it.
    #[test]
    fn a_fork_checkout_behind_its_pin_is_moved_or_refused() {
        let e = estate("fork-own");
        let fork = e.a.join("rust");
        let ahead = git(&fork, &["rev-parse", "HEAD"]);
        write(&fork.join("library/std/src/lib.rs"), "pub fn uncommitted() {}\n");
        assert!(matches!(Fork::of(&e.a), Fork::Checkout(dir) if dir == fork));
        git(&e.a, &["add", "rust"]);
        git(&e.a, &["commit", "-qm", "pins a"]);
        git(&fork, &["checkout", "-q", "HEAD~1"]);
        let said = refusal("a fork checkout behind its pin was moved over uncommitted work", || {
            Fork::of(&e.a);
        });
        assert!(said.contains("uncommitted work") && said.contains("library/std/src/lib.rs"), "{said}");
        git(&fork, &["checkout", "-q", "--", "library"]);
        assert!(matches!(Fork::of(&e.a), Fork::Checkout(dir) if dir == fork));
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), ahead, "a clean checkout behind its pin was not moved to it");

        git(&e.primary, &["update-index", "--cacheinfo", &format!("160000,{ahead},rust")]);
        let said = refusal("the primary's fork behind its pin was built", || {
            Fork::of(&e.primary);
        });
        assert!(said.contains("is not at or ahead of"), "{said}");
    }

    /// **A sysroot that is not whole is refused**: a compiler without clang
    /// makes one without it.
    #[test]
    fn a_sysroot_that_is_not_whole_is_refused() {
        let base = TempDir::new("sysroot-whole");
        let stage2 = base.join("stage2");
        let lld = toolchain::rust_lld(&stage2);
        write(&stage2.join("bin/rustc"), "rustc");
        write(&lld, "lld");
        write(&lld.with_file_name("llvm-ar"), "llvm-ar");
        write(&stage2.join("bin/cargo"), "cargo");
        let said = refusal("a sysroot without clang was taken for whole", || {
            assemble(&stage2, &base.join("partial"), |partial| write(&partial.join("lib/rustlib/x/lib/libstd.rlib"), "std"));
        });
        assert!(said.contains("is not whole") && said.contains("clang"), "{said}");
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
