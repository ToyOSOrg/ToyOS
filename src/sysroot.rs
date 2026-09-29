//! Sysroots: a product of the store (`src/store.rs`), one per key, made by
//! whichever checkout first needs it and shared by every checkout whose
//! sources match.
//!
//! **A sysroot is a function of its key** ([`key`]): [`RECIPE`], the key of the
//! compiler that builds it, the std fork's `library/` and `src/bootstrap/`, and
//! the three trees std and `libtoyos_c.a` compile, `toyos-abi`, `toyos` and
//! `userland/libc`. `sysroots/<key>/` is a whole toolchain — the compiler's
//! files cloned from its `stage2`, the guest targets' libraries built from this
//! key's sources — and a build names it as `RUSTUP_TOOLCHAIN`, so two
//! checkouts with different ABIs never refuse or wait for each other.
//!
//! **Each checkout builds std in a fork checkout of its own** ([`fork_checkout`]):
//! bootstrap's stage-0 local rebuild, the compiler compiling that checkout's
//! `library/` for the guest targets into its `build/toyos-std/`. `library/std`
//! names `toyos-abi` and `toyos` as `../../../`, so the checkout sits beside
//! this worktree's own.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::compiler::{self, Compiler};
use crate::dirlock::Lock;
use crate::store::{self, Kind, Sources, ABI_TREES};
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
    let hashes = store::trees(root, &ABI_TREES);
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

/// Where a linked worktree's std is built when its `rust/` is the empty stub
/// `git worktree add` leaves: under its ignored `target/`, so `git worktree
/// remove` takes it with the worktree.
const BUILT_FORK: &str = "target/fork";

/// The fork checkout `root`'s std is built in.
///
/// The primary's is its own `rust/`. A linked worktree whose `rust/` is a
/// checkout — a git worktree of the primary's fork repository, which is where
/// the fork is edited — builds there, as it stands, provided it is at or ahead
/// of the commit this tree pins. Any other linked worktree builds in
/// [`BUILT_FORK`]`/rust`: a detached git worktree of the primary's fork
/// repository, sharing its objects, held at the pinned commit, with
/// `toyos-abi` and `toyos` beside it as links to this worktree's.
pub fn fork_checkout(root: &Path) -> PathBuf {
    let primary = match toolchain::owner(root) {
        Owner::Us => return root.join("rust"),
        Owner::Installed => panic!("an installed toolchain has no fork checkout to build std in"),
        Owner::Elsewhere(primary) => primary,
    };
    let pinned = pinned_fork(root);
    let edited = root.join("rust");
    if edited.join(".git").exists() {
        let head = git_out(&edited, &["rev-parse", "HEAD"]);
        let at_or_ahead = Command::new("git")
            .args(["merge-base", "--is-ancestor", &pinned, head.trim()])
            .current_dir(&edited)
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            at_or_ahead,
            "{} is at {}, and this tree pins the fork at {pinned}, which that is not at or ahead of: a \
             build here would compile a std this tree does not name. Move it there: `git -C {} \
             checkout --detach {pinned}`",
            edited.display(),
            head.trim(),
            edited.display(),
        );
        return edited;
    }
    let base = root.join(BUILT_FORK);
    let fork = base.join("rust");
    if !fork.join(".git").exists() {
        eprintln!("Making {} a fork checkout at {pinned} (a git worktree of the primary's)", fork.display());
        let theirs = primary.join("rust");
        git_run(&theirs, &["worktree", "prune"]);
        fs::create_dir_all(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        git_run(&theirs, &["worktree", "add", "--detach", path_str(&fork), &pinned]);
        for tree in ["toyos-abi", "toyos"] {
            std::os::unix::fs::symlink(Path::new("../..").join(tree), base.join(tree))
                .unwrap_or_else(|e| panic!("link {}: {e}", base.join(tree).display()));
        }
        let backtrace = git_out(&fork, &["ls-tree", "HEAD", "library/backtrace"]);
        let commit = backtrace.split_whitespace().nth(2).unwrap_or_else(|| {
            panic!("{} pins no library/backtrace: {backtrace:?}", fork.display())
        });
        let theirs = theirs.join("library/backtrace");
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
    if head.trim() != pinned {
        let dirty = git_out(&fork, &["status", "--porcelain"]);
        assert!(dirty.is_empty(), "{} is the build's own fork checkout and holds edits; the fork is edited in {}:\n{dirty}", fork.display(), edited.display());
        git_run(&fork, &["checkout", "--detach", "-q", &pinned]);
    }
    fork
}

/// The sysroot this worktree's sources name, made if nobody has made it, and
/// held in use for as long as the returned value lives.
pub fn ensure(root: &Path, rust_dir: &Path) -> Sysroot {
    let fork = fork_checkout(root);
    let sources = Sources::of(root, &fork);
    let compiler = compiler::resolve(root, rust_dir, &fork, &sources);
    let key = key(&compiler.key, &sources);
    let held = store::get(root, rust_dir, Kind::Sysroot, &key, |partial| {
        assemble(&compiler.stage2, partial, |partial| build(root, &compiler, &fork, partial));
        let again = self::key(&compiler.key, &Sources::of(root, &fork));
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
    {
        // One std build at a time in a checkout: each empties the build directory
        // the one before it built in.
        let _std = Lock::exclusive(fork, &format!("a std build in {}", fork.display()));
        let built = build_std(root, compiler, fork);
        for target in GUEST_TARGETS {
            place_std(&stamp(&built, target), &partial.join("lib/rustlib").join(target).join("lib"));
        }
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

    /// **A worktree whose `rust/` is the stub builds std in a fork checkout of
    /// its own under `target/`**, at the commit its tree pins, beside links to
    /// its own ABI trees; the primary's fork is not touched; a moved pin moves
    /// the checkout; and plain `git worktree remove` takes it all, because
    /// nothing is nested where git looks.
    #[test]
    fn a_stub_worktree_builds_std_in_a_checkout_git_worktree_remove_takes() {
        let e = estate("fork-checkout");
        let linked = e.same.parent().unwrap().join("stub");
        git(&e.primary, &["worktree", "add", "-q", "-b", "stub", linked.to_str().unwrap()]);
        let pinned = git(&e.primary, &["rev-parse", "HEAD:rust"]);
        let before = git(&e.rust_dir, &["rev-parse", "HEAD"]);

        let fork = fork_checkout(&linked);
        assert_eq!(fork, linked.join(BUILT_FORK).join("rust"));
        assert_eq!(git(&fork, &["rev-parse", "HEAD"]), pinned);
        assert_eq!(fs::canonicalize(fork.join("../../../toyos-abi")).unwrap(), fs::canonicalize(linked.join("toyos-abi")).unwrap());
        assert_eq!(git(&e.rust_dir, &["rev-parse", "HEAD"]), before, "the primary's fork moved");
        assert_eq!(git(&linked, &["status", "--porcelain"]), "", "the worktree is not clean");

        let other = git(&e.a.join("rust"), &["rev-parse", "HEAD"]);
        git(&linked, &["update-index", "--cacheinfo", &format!("160000,{other},rust")]);
        assert_eq!(git(&fork_checkout(&linked), &["rev-parse", "HEAD"]), other, "a moved pin kept the old checkout");

        git(&linked, &["update-index", "--cacheinfo", &format!("160000,{pinned},rust")]);
        git(&e.primary, &["worktree", "remove", linked.to_str().unwrap()]);
        assert!(!linked.exists(), "git worktree remove left {}", linked.display());
    }

    /// **A worktree whose `rust/` is a checkout builds there, as it stands**,
    /// and one behind the commit its tree pins is refused by name.
    #[test]
    fn a_worktree_s_own_fork_checkout_is_built_as_it_stands_or_refused() {
        let e = estate("fork-own");
        let fork = e.a.join("rust");
        write(&fork.join("library/std/src/lib.rs"), "pub fn uncommitted() {}\n");
        assert_eq!(fork_checkout(&e.a), fork);
        git(&e.a, &["add", "rust"]);
        git(&e.a, &["commit", "-qm", "pins a"]);
        git(&fork, &["checkout", "-q", "HEAD~1"]);
        let said = refusal("a fork checkout behind its pin was built", || {
            fork_checkout(&e.a);
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
