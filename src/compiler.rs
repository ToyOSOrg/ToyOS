//! The compiler a worktree's sysroot is cloned from and compiled by,
//! content-addressed: one per key on this host, shared by every checkout whose
//! fork checkout names it.
//!
//! **Every worktree builds with the compiler its own fork checkout names, and
//! no checkout's is special.** The key ([`key`]) is the identity
//! (`src/identity.rs`) of the checkout's [`KEYED`] sources, the key of the LLVM
//! it links, which names `src/bootstrap`, and the build itself
//! ([`build_text`]). A compiler nobody on the host has built is built by
//! bootstrap in the fork checkout that first names it, under that checkout's
//! `build/toyos-compiler/`, which starts from nothing unless the build and the
//! LLVM that filled it are this one's ([`place`]), and placed at
//! `compilers/<key>/` in the store (`src/keystore.rs`). Nothing writes that
//! directory after its [`SOURCE`] file exists. A sysroot is named by its
//! directory, never by a toolchain name, so no rustup toolchain is linked.
//!
//! **LLVM is the host's, built from `src/llvm-project`** (`src/llvm.rs`), and
//! linked through its `llvm-config`. The key names that LLVM's key, so another
//! LLVM is another compiler, and an LLVM checkout or a `src/bootstrap` holding
//! what no commit does names none.
//!
//! Locks, in the one order every acquirer takes them: the key's
//! (`buildlock::keyed_*` with [`Keyed::Compiler`]), with this worktree's build
//! lock put down once the key is read under it ([`resolve`]), held shared for
//! as long as a sysroot is being made from it;
//! then, to build, this worktree's exclusively, because its fork build
//! directory is written; then the LLVM key's, held shared while it is linked.
//!
//! A compiler nothing has used for the store's keep time is removed by
//! `keystore::sweep`, which every placement runs.

use std::fs;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Guard, Held, Keyed};
use crate::keystore::{self, Key};
use crate::sysroot::{clone_tree, forget_another_compiler, tree_identity, Links};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become a compiler and is neither them nor
/// [`config_text`]: the build below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage 2 of compiler/rustc and library, profile compiler, host only, with rust-lld, host linker pinned, LLVM, clang and LLD from the host's LLVM, no LLVM tool copied, rustc without debuginfo, no link to the checkout's sources; 7";

/// What a compiler's key is the identity of, in its fork checkout, beside the
/// `src/bootstrap` its LLVM's key names: what bootstrap compiles, which is
/// `library` too, for the host's std its `stage2` carries and every guest
/// crate's build scripts and proc macros link; the workspace it compiles them
/// in, with its profiles and patches; the version it gives rustc and the
/// channel file it reads beside it; and what runs bootstrap, with the crate
/// bootstrap itself is built with.
const KEYED: [&str; 11] = [
    "compiler",
    "library",
    "src/tools",
    "src/stage0",
    "Cargo.lock",
    "Cargo.toml",
    "src/version",
    "src/ci/channel",
    "src/build_helper",
    "x",
    "x.py",
];

/// What a compiler build is beyond its sources, as every key reads it:
/// [`RECIPE`], the configuration bootstrap is given ([`config_text`]) with no
/// path of this host in it, and the tools `clang::provision` puts beside the
/// compiler.
fn build_text() -> String {
    let config = config_text(Path::new("<build-dir>"), &host_triple(), Path::new("<llvm>"));
    let provisioned: Vec<&str> = crate::clang::tools().collect();
    format!("{RECIPE}\n{config}provisioned {}", provisioned.join(" "))
}

/// The submodule a compiler is built against by commit: its LLVM, which
/// bootstrap builds from that commit, so its content is never read.
pub(crate) const LLVM: &str = "src/llvm-project";

/// Where a fork checkout builds a compiler.
const BUILD_DIR: &str = "build/toyos-compiler";

/// The file a finished compiler carries last, naming its key. A directory
/// without it is a build that did not finish.
const SOURCE: &str = "SOURCE";

/// The links bootstrap puts in a `stage2` to the checkout that built it, where
/// rustup's `rust-src` and `rustc-dev` components would be. A stored compiler
/// carries neither: the checkout goes and the compiler stays, and rustc reads
/// one only to translate a library source's path to or from its remapped form
/// (`rustc_session`'s `real_source_base_dir`), which no build here asks for.
const CHECKOUT_LINKS: [&str; 2] = ["lib/rustlib/src/rust", "lib/rustlib/rustc-src/rust"];

/// A compiler, held in use for as long as this lives.
pub struct Compiler {
    /// Its toolchain directory: `bin/rustc`, `lib/`.
    pub stage2: PathBuf,
    key: Key,
    _using: Guard,
}

impl Compiler {
    /// The compiler as a sysroot's key sees it: its key, and the driver its
    /// build left, so one made again under its key is a new compiler too.
    pub fn identity(&self) -> String {
        let lib = self.stage2.join("lib");
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
        format!("{} {} {} {mtime}", self.key, driver.file_name().to_string_lossy(), meta.len())
    }

    /// What a store built with this compiler is keyed by.
    pub fn key(&self) -> &Key {
        &self.key
    }
}

/// The key of the compiler `fork`'s sources name: their content, and the build.
pub fn key(fork: &Path) -> Key {
    key_of(fork, &build_text(), &crate::llvm::key(fork))
}

/// [`key`], with the build it reads and the key of the LLVM `fork` names.
fn key_of(fork: &Path, build: &str, llvm: &Key) -> Key {
    let parts = [build, &tree_identity(fork, &KEYED, Links::Skipped), llvm.as_str()];
    Key::of(parts.join("\n\0\n").as_bytes())
}

/// Why `dir` is not a compiler a build finished, if it is not: it carries no
/// [`SOURCE`].
pub(crate) fn unplaced(dir: &Path) -> Option<String> {
    (!dir.join(SOURCE).is_file()).then(|| format!("{} carries no {SOURCE}", dir.display()))
}

/// The compiler `root`'s fork checkout at `fork` names, in `store`: built if
/// nobody on the host has built it, and held in use for as long as the
/// returned value lives.
pub fn resolve(root: &Path, store: &Path, fork: &Path, lock: &mut Held) -> Compiler {
    // Under the shared lock: a bootstrap of this worktree, which holds it
    // exclusively, rewrites the fork's lockfiles for as long as it runs.
    let key = key(fork);
    lock.without_shared(|| choose(root, store, fork, key, |fork| build_in_fork(root, store, fork)))
}

/// [`resolve`] of the compiler `key` names, with the build that makes its
/// `stage2` passed in, so a test can stand in for bootstrap: `build` compiles
/// the fork checkout it is given and returns the `stage2` it left there.
fn choose(root: &Path, store: &Path, fork: &Path, key: Key, build: impl Fn(&Path) -> PathBuf) -> Compiler {
    let dir = Keyed::Compiler.store(store).join(&key);
    let using = keystore::made(store, Keyed::Compiler, &key, || unplaced(&dir), || place(root, fork, &key, &dir, &build));
    Compiler { stage2: dir.join("stage2"), key, _using: using }
}

/// Build the compiler `key` names from `fork` and put it at `dir`. The caller
/// holds the key's lock.
fn place(root: &Path, fork: &Path, key: &Key, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
    let what = format!("building compiler {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
    eprintln!("Building compiler {key} in {}: nobody on this host has", fork.display());
    let build_dir = fork.join(BUILD_DIR);
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    // Bootstrap and cargo reuse what the directory holds whatever configuration
    // and LLVM built it; only a source that moved do they see.
    forget_another_compiler(&build_dir, &host_triple(), &format!("{}\n{}", build_text(), crate::llvm::key(fork)));
    let stage2 = build(fork);
    crate::llvm::retire_in_tree(&build_dir);
    let partial = dir.with_extension("partial");
    keystore::remove(&partial);
    clone_tree(&stage2, &partial.join("stage2"));
    for link in CHECKOUT_LINKS {
        keystore::remove(&partial.join("stage2").join(link));
    }
    // The sources the key named are the ones built, or this is not that key's.
    let again = self::key(fork);
    assert!(
        again == *key,
        "the fork's compiler sources moved while compiler {key} was being built (they are now \
         {again}); nothing was kept, and the next build makes the one they name"
    );
    fs::write(partial.join(SOURCE), format!("{key}\n"))
        .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCE).display()));
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
}

/// Bootstrap's build of the compiler in `fork`, into its own build directory,
/// against the LLVM `fork` names (`src/llvm.rs`), and the `stage2` it made,
/// with the cargo and the clang every toolchain directory carries.
fn build_in_fork(root: &Path, store: &Path, fork: &Path) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let llvm = crate::llvm::resolve(root, store, fork);
    let host = host_triple();
    let build_dir = fork.join(BUILD_DIR);
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host, &llvm.dir)).unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args = ["build", "--stage", "2", "--config", config, "--warnings", "warn", "compiler/rustc", "library"];
    let (ok, log) = toolchain::x_build_compiler(fork, &args, "the compiler", &llvm.dir);
    toolchain::refuse_on_compile_error(&log, "the compiler");
    assert!(ok, "the compiler build in {} failed, and nothing in its output was a compile error", fork.display());
    let stage2 = build_dir.join(&host).join("stage2");
    assert!(stage2.join("bin/rustc").is_file(), "the compiler build left no {}", stage2.join("bin/rustc").display());
    toolchain::provision_toolchain_cargo(&stage2);
    crate::clang::provision(&stage2, &llvm.dir);
    toolchain::assert_toolchain_is_honest(&stage2);
    stage2
}

/// What the host rustc links its own binaries with, pinned off: with
/// `lld = true` bootstrap otherwise makes `rust-lld` the default linker of
/// `x86_64-unknown-linux-gnu`, and of no other host.
const HOST_LINKER_PIN: &str = "default-linker-linux-override = \"off\"";

/// The `[rust]` options of a compiler build beyond its profile's: no LLVM tool
/// copied into the compiler's sysroot, since `clang::provision` puts there the
/// ones a build runs; no debuginfo in rustc, which no build reads; and no
/// codegen test, for which bootstrap demands LLVM's `FileCheck` beside
/// `llvm-config` (`src/bootstrap/src/core/sanity.rs`).
const LEAN: &str = "llvm-tools = false\ndebuginfo-level-rustc = 0\ncodegen-tests = false";

/// Bootstrap's configuration for a compiler built in `build_dir`: for the host
/// alone, since every guest target's libraries are the sysroot's to build,
/// linking the LLVM at `llvm` (`clang::LLVM_CONFIG`, `llvm::host_lines`).
/// `lld = true` is what puts `rust-lld` in every stage's sysroot, where rustc
/// finds the linker every guest target names.
fn config_text(build_dir: &Path, host: &str, llvm: &Path) -> String {
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]

[llvm]
{llvm}

[rust]
incremental = true
lld = true
{LEAN}

[target.{host}]
{HOST_LINKER_PIN}
{external}
"#,
        build_dir = build_dir.display(),
        llvm = crate::clang::LLVM_CONFIG,
        external = crate::llvm::host_lines(llvm),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use toyos_tmpdir::TempDir;
    use std::process::Command;

    use super::*;
    use crate::keystore::tests::{last_used, LONG_AGO};

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

    /// The URL the fixtures' forks name for their LLVM.
    pub(crate) const LLVM_URL: &str = "https://llvm.invalid/llvm-project.git";

    /// A primary whose `rust` pins fork commit `C0`, a store nothing is in
    /// yet, and three linked worktrees: `same` pins `C0`, `a` and `b` each pin
    /// a commit whose `compiler/` is its own.
    pub(crate) fn estate(scratch: &Path) -> (PathBuf, PathBuf, [PathBuf; 3]) {
        let base = fs::canonicalize(scratch).unwrap();

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() {}\n");
        write(&fork.join("src/bootstrap/src/lib.rs"), "fn main() {}\n");
        write(&fork.join("src/stage0"), "compiler_version=beta\n");
        write(&fork.join("Cargo.lock"), "# lock\n");
        write(&fork.join("x.py"), "\n");
        write(&fork.join("library/std/src/lib.rs"), "pub fn a() {}\n");
        write(&fork.join(".gitignore"), "/build\n");
        write(&fork.join(".gitmodules"), &format!("[submodule \"{LLVM}\"]\n\tpath = {LLVM}\n\turl = {LLVM_URL}\n"));
        git(&fork, &["add", "-A"]);
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_A},{LLVM}")]);
        // What an uninitialised submodule leaves, so `commit -a` keeps the gitlink.
        fs::create_dir_all(fork.join(LLVM)).unwrap();
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
        write(&primary.join("README"), "x\n");
        git(&primary, &["submodule", "add", "-q", fork.to_str().unwrap(), "rust"]);
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "-qm", "pins C0"]);
        let rust_dir = primary.join("rust");

        let mut linked = Vec::new();
        for (name, pin) in [("same", c0.as_str()), ("a", pins[0].as_str()), ("b", pins[1].as_str())] {
            let wt = base.join(name);
            git(&primary, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
            let _ = fs::remove_dir(wt.join("rust"));
            git(&rust_dir, &["worktree", "add", "-q", "--detach", wt.join("rust").to_str().unwrap(), pin]);
            linked.push(wt);
        }
        (primary, base.join("store"), linked.try_into().unwrap())
    }

    /// A compiler placed in `store` under the key of `name`: a driver, and no
    /// more.
    pub(crate) fn compiler(store: &Path, name: &str) -> Compiler {
        let key = Key::of(name.as_bytes());
        let dir = Keyed::Compiler.store(store).join(&key);
        let using = keystore::made(store, Keyed::Compiler, &key, || unplaced(&dir), || {
            write(&dir.join("stage2/lib/librustc_driver-1.dylib"), "a driver");
            write(&dir.join(SOURCE), &format!("{key}\n"));
        });
        Compiler { stage2: dir.join("stage2"), key, _using: using }
    }

    /// **One compiler per key, whichever checkout names it.** The primary and a
    /// worktree naming one `compiler/` share one compiler, built once and found
    /// again; worktrees naming others each get their own, side by side; an
    /// uncommitted file in `compiler/` is a new compiler and committing it is
    /// not another; and a sweep takes a compiler only once nothing has used it
    /// for the store's keep time and nobody uses it.
    #[test]
    fn one_compiler_per_key_whichever_checkout_names_it() {
        let scratch = TempDir::new("compiler");
        let (primary, store, [same, a, b]) = estate(&scratch);
        let compilers = Keyed::Compiler.store(&store);
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            fake_build(fork)
        };

        let mine = chosen(&same, &store, &same.join("rust"), fake);
        let primarys = chosen(&primary, &store, &primary.join("rust"), fake);
        assert_eq!((primarys.stage2.clone(), builds.get()), (mine.stage2.clone(), 1), "one compiler/ named two compilers");
        for link in CHECKOUT_LINKS {
            assert!(same.join("rust").join(BUILD_DIR).join("stage2").join(link).is_dir(), "the stand-in build made no {link}");
            assert!(fs::symlink_metadata(mine.stage2.join(link)).is_err(), "a stored compiler links the checkout that built it at {link}");
        }

        let ca = chosen(&a, &store, &a.join("rust"), fake);
        let cb = chosen(&b, &store, &b.join("rust"), fake);
        assert_eq!(builds.get(), 3);
        assert_ne!(ca.stage2, cb.stage2, "two compilers were given one directory");
        assert!(ca.stage2.starts_with(&compilers) && cb.stage2.starts_with(&compilers));
        assert!(fs::read_to_string(ca.stage2.join("bin/rustc")).unwrap().contains("aarch64"));
        assert!(fs::read_to_string(cb.stage2.join("bin/rustc")).unwrap().contains("riscv"));
        assert_ne!(ca.identity(), cb.identity());
        assert_ne!(ca.identity(), mine.identity());

        // Found again, not rebuilt; and still both there.
        let again = chosen(&a, &store, &a.join("rust"), fake);
        assert_eq!((again.stage2, builds.get()), (ca.stage2.clone(), 3));
        assert!(ca.stage2.join("bin/rustc").is_file() && cb.stage2.join("bin/rustc").is_file());

        // An uncommitted file in `compiler/` is a new compiler too, and
        // committing it is not another one.
        let pinned = git(&a.join("rust"), &["rev-parse", "HEAD"]);
        write(&a.join("rust/compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        let ca2 = chosen(&a, &store, &a.join("rust"), fake);
        assert_ne!(ca2.stage2, ca.stage2, "an untracked target spec kept the old compiler");
        git(&a.join("rust"), &["add", "-A"]);
        git(&a.join("rust"), &["commit", "-qm", "the target, committed"]);
        let committed = chosen(&a, &store, &a.join("rust"), fake);
        assert_eq!((committed.stage2, builds.get()), (ca2.stage2.clone(), 4), "a commit rebuilt the compiler");
        git(&a.join("rust"), &["checkout", "-q", &pinned]);

        // A sweep takes the compiler nothing has used for the keep time, and
        // only that one — and not while it is still in use.
        let spec = a.join("rust/compiler/rustc_target/src/orphan.rs");
        write(&spec, "pub fn o() {}\n");
        let unused = key(&a.join("rust"));
        let orphan = compilers.join(&unused);
        let user = chosen_elsewhere(&a, &store);
        fs::remove_file(&spec).unwrap();
        last_used(&store, Keyed::Compiler, &unused, LONG_AGO);
        let sweep = || keystore::sweep(&store, Keyed::Compiler);
        assert_eq!(sweep(), Vec::<PathBuf>::new(), "the sweep took a compiler still in use");
        assert!(orphan.is_dir());
        user.release();
        assert_eq!(sweep(), [orphan], "the sweep took a compiler in use, or left one nothing had used for the keep time");
        assert!(ca.stage2.is_dir() && ca2.stage2.is_dir() && mine.stage2.is_dir());

        // A placement sweeps too: the compiler an edit replaces goes once
        // nothing has used it for the keep time.
        let spec = a.join("rust/compiler/rustc_target/src/another.rs");
        write(&spec, "pub fn u() {}\n");
        let replaced = key(&a.join("rust"));
        chosen_elsewhere(&a, &store).release();
        last_used(&store, Keyed::Compiler, &replaced, LONG_AGO);
        write(&spec, "pub fn v() {}\n");
        let ca3 = chosen(&a, &store, &a.join("rust"), fake);
        assert!(!compilers.join(&replaced).exists(), "placing a compiler left one nothing had used for the keep time");
        assert!(ca3.stage2.is_dir() && cb.stage2.is_dir());
    }

    /// **A fork build directory whose compiler links the host's LLVM keeps
    /// none of its own**: placing the compiler removes it, from a directory
    /// this build and this LLVM filled too.
    #[test]
    fn a_compiler_s_build_directory_keeps_no_llvm_of_its_own() {
        let scratch = TempDir::new("compiler-in-tree-llvm");
        let (_primary, store, [_same, a, _b]) = estate(&scratch);
        let fork = a.join("rust");
        drop(chosen(&a, &store, &fork, fake_build));
        let own = fork.join(BUILD_DIR).join(host_triple()).join("llvm");
        write(&own.join("bin/llvm-config"), "the build directory's own");
        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() { edited() }\n");
        let seen = Cell::new(false);
        let fake = |fork: &Path| {
            seen.set(own.is_dir());
            fake_build(fork)
        };
        drop(chosen(&a, &store, &fork, fake));
        assert!(seen.get(), "the LLVM went before the build, with the directory: this test shows nothing");
        assert!(!own.exists(), "a compiler built against the store left the LLVM its build directory built");
    }

    /// **A build directory is built in again only by the build and the LLVM
    /// that filled it**: a source edit keeps what the last build left there,
    /// and a compiler linking another LLVM starts from nothing.
    #[test]
    fn a_build_directory_another_llvm_filled_starts_from_nothing() {
        let scratch = TempDir::new("compiler-build-dir");
        let (_primary, store, [_same, a, _b]) = estate(&scratch);
        let fork = a.join("rust");
        let left = fork.join(BUILD_DIR).join(host_triple()).join("stage1-rustc/left-by-the-last-build");
        let found = Cell::new(false);
        let fake = |fork: &Path| {
            found.set(left.is_file());
            write(&left, "what bootstrap would reuse");
            fake_build(fork)
        };
        drop(chosen(&a, &store, &fork, fake));

        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() { edited() }\n");
        drop(chosen(&a, &store, &fork, fake));
        assert!(found.get(), "a source edit emptied the build directory, so no compiler build is incremental");

        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        git(&fork, &["commit", "-qm", "another LLVM"]);
        drop(chosen(&a, &store, &fork, fake));
        assert!(!found.get(), "a compiler linking another LLVM was built over what the last one left");
    }

    const WORKTREE: &str = "TOYOS_COMPILER_TEST_WORKTREE";
    const STORE: &str = "TOYOS_COMPILER_TEST_STORE";

    /// The competing process for the test above: the compiler the worktree in
    /// [`WORKTREE`] names, chosen and held in use until released.
    #[test]
    #[ignore = "the competing process for the test above; never runs on its own"]
    fn child_role() {
        let worktree = std::env::var(WORKTREE).unwrap_or_else(|_| panic!("child_role ran without {WORKTREE}; it is not a test"));
        let worktree = PathBuf::from(worktree);
        let store = PathBuf::from(std::env::var(STORE).unwrap());
        let _chosen = chosen(&worktree, &store, &worktree.join("rust"), fake_build);
        buildlock::tests::hold_until_released();
    }

    /// The compiler `worktree` names, made if nobody has and held in use by a
    /// process of its own.
    fn chosen_elsewhere(worktree: &Path, store: &Path) -> buildlock::tests::Elsewhere {
        let env = [(WORKTREE, worktree.as_os_str()), (STORE, store.as_os_str())];
        buildlock::tests::Elsewhere::hold("compiler::tests::child_role", &env)
    }

    /// [`choose`] of the compiler `fork` names.
    fn chosen(root: &Path, store: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Compiler {
        choose(root, store, fork, key(fork), build)
    }

    /// Bootstrap's stand-in: a `stage2` that says which target spec it knows.
    fn fake_build(fork: &Path) -> PathBuf {
        let stage2 = fork.join("build/toyos-compiler/stage2");
        let spec = fs::read_to_string(fork.join("compiler/rustc_target/src/lib.rs")).unwrap();
        write(&stage2.join("bin/rustc"), &format!("a rustc knowing {spec}"));
        write(&stage2.join("lib/librustc_driver-1.dylib"), &spec);
        for link in CHECKOUT_LINKS {
            let link = stage2.join(link);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(fork, &link).unwrap();
        }
        stage2
    }

    /// **Only a compiler built from what its key names is stored**: one whose
    /// sources moved while it was being built is refused, and nothing is at
    /// the key the build began under.
    #[test]
    fn a_compiler_whose_sources_moved_while_it_was_built_is_not_stored() {
        let scratch = TempDir::new("compiler-moved");
        let (_primary, store, [_same, a, _b]) = estate(&scratch);
        let fork = a.join("rust");
        let named = key(&fork);
        let moving = |fork: &Path| {
            let stage2 = fake_build(fork);
            write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() { moved() }\n");
            git(fork, &["commit", "-qam", "moved while built"]);
            stage2
        };
        let said = refusal("a compiler whose sources moved while it was built was stored", || {
            chosen(&a, &store, &fork, moving);
        });
        assert!(said.contains("moved while compiler"), "{said}");
        let stored: Vec<_> = fs::read_dir(Keyed::Compiler.store(&store)).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(stored, [std::ffi::OsString::from(format!("{named}.partial"))], "a compiler its key does not name was kept");
    }

    /// `fork`'s LLVM checked out at a commit of its own.
    fn llvm_checkout(fork: &Path) -> PathBuf {
        let llvm = fork.join(LLVM);
        git(&llvm, &["init", "-q"]);
        write(&llvm.join("llvm/lib/IR/Core.cpp"), "int core;\n");
        git(&llvm, &["add", "-A"]);
        git(&llvm, &["commit", "-qm", "LLVM"]);
        llvm
    }

    /// What `f` panicked with; `expect` if it returned.
    fn refusal(expect: &str, f: impl FnOnce()) -> String {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err(expect);
        refused.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    /// An LLVM checkout holding what no commit does, an edit or a file git does
    /// not track, names no compiler.
    #[test]
    fn an_uncommitted_llvm_edit_is_refused() {
        let scratch = TempDir::new("compiler-llvm-edit");
        let (_primary, store, [same, _, _]) = estate(&scratch);
        let fork = same.join("rust");
        let llvm = llvm_checkout(&fork);
        let committed = key(&fork);
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            fork.join("build/toyos-compiler/stage2")
        };

        write(&llvm.join("llvm/lib/IR/Core.cpp"), "int core_edited;\n");
        let said = refusal("an uncommitted LLVM edit named a compiler", || {
            chosen(&same, &store, &fork, fake);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        git(&llvm, &["commit", "-qam", "the edit"]);
        assert_eq!(key(&fork), committed, "the key read the submodule's commit rather than the gitlink");

        write(&llvm.join("llvm/lib/IR/Untracked.cpp"), "int untracked;\n");
        let said = refusal("an untracked file in LLVM named a compiler", || {
            chosen(&same, &store, &fork, fake);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        assert_eq!(builds.get(), 0, "an LLVM checkout no commit holds built a compiler");
    }

    /// Every source a compiler is built from moves its key: the library its
    /// host std is built from, the tools, the stage-0 pin, the lockfile, the
    /// workspace's manifest, the version, the channel, bootstrap's helper crate
    /// and what runs bootstrap by content, committed or not, and LLVM by the
    /// commit its gitlink records.
    #[test]
    fn every_source_of_a_compiler_moves_its_key() {
        let scratch = TempDir::new("compiler-key");
        let (_primary, _store, [same, _, _]) = estate(&scratch);
        let fork = same.join("rust");
        let sources = [
            ("library/std/src/lib.rs", "pub fn b() {}\n"),
            ("src/tools/lld-wrapper/src/main.rs", "fn main() {}\n"),
            ("src/stage0", "compiler_version=nightly\n"),
            ("Cargo.lock", "# relocked\n"),
            ("Cargo.toml", "[profile.release]\nopt-level = 3\n"),
            ("src/version", "1.0.0\n"),
            ("src/ci/channel", "beta\n"),
            ("src/build_helper/src/lib.rs", "pub fn helper() {}\n"),
            ("x", "#!/bin/sh\n"),
            ("x.py", "# edited\n"),
        ];
        for (file, text) in sources {
            let before = key(&fork);
            write(&fork.join(file), text);
            let edited = key(&fork);
            assert_ne!(edited, before, "{file} kept the key");
            git(&fork, &["add", "-A"]);
            git(&fork, &["commit", "-qm", file]);
            assert_eq!(key(&fork), edited, "committing {file} moved the key");
        }
        let before = key(&fork);
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        let said = refusal("a gitlink staged and not committed named a compiler", || {
            key(&fork);
        });
        assert!(said.contains("stages"), "{said}");
        git(&fork, &["commit", "-qm", "another LLVM"]);
        assert_ne!(key(&fork), before, "another LLVM commit did not move the key");
    }

    /// **Every compiler's key reads the whole build**: the configuration
    /// bootstrap is given and the tools put beside the compiler are in it, and
    /// another configuration is another key.
    #[test]
    fn a_compiler_is_keyed_by_its_whole_build() {
        let scratch = TempDir::new("compiler-build");
        let (_primary, _store, [same, _, _]) = estate(&scratch);
        let fork = same.join("rust");
        let llvm = crate::llvm::key(&fork);
        assert_eq!(key_of(&fork, &build_text(), &llvm), key(&fork));
        let config = config_text(Path::new("<build-dir>"), &host_triple(), Path::new("<llvm>"));
        let tools: Vec<&str> = crate::clang::tools().collect();
        for part in [config.as_str(), LEAN, HOST_LINKER_PIN, &tools.join(" ")] {
            assert!(build_text().contains(part), "a compiler's key reads no {part:?}");
        }
        let more = build_text().replace("debuginfo-level-rustc = 0", "debuginfo-level-rustc = 1");
        assert_ne!(key_of(&fork, &more, &llvm), key(&fork), "another configuration kept the key");
    }

    /// **Every compiler is built lean against the host's LLVM**: for the host
    /// alone, linking that LLVM through its `llvm-config` and taking its LLD as
    /// `rust-lld`, copying none of its tools, with rustc without debuginfo and
    /// no codegen test, and the host's own linker left as it is.
    #[test]
    fn every_compiler_is_built_lean_against_the_host_s_llvm() {
        let config = config_text(Path::new("/b"), "h", Path::new("/llvm"));
        let lean = "\n[rust]\nincremental = true\nlld = true\nllvm-tools = false\ndebuginfo-level-rustc = 0\ncodegen-tests = false\n";
        assert!(config.contains(lean) && config.contains("\ntarget = [\"h\"]\n"), "{config}");
        let host = "[target.h]\ndefault-linker-linux-override = \"off\"\nllvm-config = \"/llvm/bin/llvm-config\"\nllvm-has-rust-patches = true\n";
        assert!(config.contains(host), "{config}");
        assert!(!config.contains("\"rust-lld\""), "{config}");
    }
}
