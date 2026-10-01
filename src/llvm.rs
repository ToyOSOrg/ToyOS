//! The LLVM every host compiler links, with its clang and LLD, content-addressed:
//! one per key on this host, shared by every compiler build that names it.
//!
//! **An LLVM is a function of its key** ([`key`]): the `src/llvm-project` commit
//! the fork checkout's gitlink names (`compiler::llvm_commit`, which refuses a
//! checkout holding what no commit does and a gitlink staged and not committed),
//! the committed tree of its `src/bootstrap` (one holding what no commit does is
//! refused), the bootstrap configuration below, [`RECIPE`], and the tools the
//! host builds it with ([`host_tools`]). `rust/build/llvm/<key>/` in the primary
//! is bootstrap's install of that LLVM and its clang, with its LLD in `bin/`
//! beside `llvm-config` and in `src/` the runtimes' sources the C++ runtime is
//! built from (`src/libcxx.rs`) as its commit holds them, made by whichever
//! build first needs it ([`resolve`]), and stored only when it was built from
//! what the key names. Once its [`SOURCE`] file exists it is read-only, its
//! directories as well as its files.
//! Every compiler build, the primary's and a worktree's own, names it as the
//! host's `llvm-config` with `llvm-has-rust-patches`, so bootstrap builds no
//! LLVM and takes LLD from beside it as `rust-lld`; `clang::provision` copies its
//! clang. Once a build directory's compiler is built against it, the LLVM that
//! directory built itself goes ([`retire_in_tree`]).
//!
//! **Nothing of the environment it is asked from reaches it but
//! [`ENVIRONMENT`]**: the build and every tool its key asks run with the rest
//! cleared ([`clear`]), the configuration names the C and C++ compilers by path,
//! and every host library LLVM would otherwise find and link is turned off
//! ([`NO_HOST_LIBRARIES`]).
//!
//! Its lock (`buildlock::keyed_*` with [`Keyed::Llvm`]) is taken inside the
//! worktree or global lock that covers the fork build directory its maker
//! writes, `build/toyos-llvm/`, which is removed once the LLVM is placed.
//!
//! An LLVM no worktree names any more is removed by `keystore::sweep`, which
//! `--worktree remove` and every placement run: each worktree records the key of
//! the LLVM its compiler links, and a key no registered worktree records, that
//! nobody is making or using, goes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::buildlock::{Guard, Keyed};
use crate::compiler::{llvm_commit, LLVM};
use crate::keystore::{self, Key};
use crate::sysroot::{clone_tree, git_bytes, git_out};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become an LLVM and is none of the other
/// parts: the build's targets and what is kept of it. Moving it moves every key.
const RECIPE: &str = "bootstrap build of src/llvm-project/llvm and src/llvm-project/lld; the install's bin, \
                      include and lib, and lld in bin, and the runtimes' sources in src, read-only; 3";

/// What of the caller's environment the LLVM build, and every tool its key
/// asks, sees:
/// - `PATH` finds what runs the build: the Python behind `./x`, git, curl, and
///   CMake, which the key names by path and version. The C and C++ compilers
///   it finds, the configuration names by path. The build's has n2's directory
///   first ([`build_in_fork`]).
/// - `TMPDIR` is where those tools write what they discard; the sandbox a build
///   runs in may allow no other place.
///
/// Everything else is dropped on purpose, the proxy, CA-bundle and Nix
/// variables a fetch may need among them: such a host's fetch fails loudly.
const ENVIRONMENT: [&str; 2] = ["PATH", "TMPDIR"];

/// Every host library the pinned LLVM and clang look for, and link when they
/// find one, turned off by the CMake option that asks for it
/// (`llvm/CMakeLists.txt`, `llvm/cmake/config-ix.cmake`, `clang/CMakeLists.txt`):
/// what a host has installed never decides what is built. Bootstrap's LLD step
/// takes none of these: LLD looks for no library unless asked, and links what this
/// LLVM chose.
const NO_HOST_LIBRARIES: [&str; 12] = [
    "LLVM_ENABLE_ZLIB",
    "LLVM_ENABLE_ZSTD",
    "LLVM_ENABLE_LIBXML2",
    "CLANG_ENABLE_LIBXML2",
    "LLVM_ENABLE_LIBEDIT",
    "LLVM_ENABLE_LIBPFM",
    "LLVM_ENABLE_FFI",
    "LLVM_ENABLE_CURL",
    "LLVM_ENABLE_HTTPLIB",
    "LLVM_ENABLE_ICU",
    "LLVM_ENABLE_ICONV",
    "LLVM_ENABLE_Z3_SOLVER",
];

/// What of bootstrap's install an LLVM keeps: `build/` beside them is CMake's
/// tree, which nothing reads once the install is made.
const KEPT: [&str; 3] = ["bin", "include", "lib"];

/// What a compiler build and `clang::provision` read of an LLVM.
const TOOLS: [&str; 4] = ["bin/llvm-config", "bin/lld", "bin/clang", "bin/llvm-ar"];

/// The file a finished LLVM carries last, naming its key. A directory without
/// it is a build that did not finish.
const SOURCE: &str = "SOURCE";

/// The fork's bootstrap, whose `Llvm` step and `compiler` profile decide how
/// LLVM is configured.
const BOOTSTRAP: &str = "src/bootstrap";

/// The build directory the key's configuration names.
const KEYED_BUILD_DIR: &str = "<build-dir>";

/// An LLVM, held in use for as long as this lives.
pub struct Llvm {
    /// Its install: `bin/`, `include/`, `lib/`.
    pub dir: PathBuf,
    _using: Guard,
}

/// The `[target.<host>]` lines that make a `bootstrap.toml` link the LLVM at
/// `dir` and take its LLD.
pub fn host_lines(dir: &Path) -> String {
    format!("llvm-config = \"{}\"\nllvm-has-rust-patches = true", dir.join("bin/llvm-config").display())
}

/// Every LLVM on this host.
pub fn store(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/llvm")
}

/// The key of the LLVM `fork` names; refused while its `src/bootstrap` holds
/// changes no commit does.
pub fn key(fork: &Path) -> Key {
    let tools = host_tools();
    key_of(fork, RECIPE, &config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), tools), &tools.identity)
}

fn key_of(fork: &Path, recipe: &str, config: &str, tools: &str) -> Key {
    refuse_uncommitted_bootstrap(fork);
    let bootstrap = git_out(fork, &["rev-parse", &format!("HEAD:{BOOTSTRAP}")]);
    Key::of([recipe, config, &llvm_commit(fork), bootstrap.trim(), tools].join("\n\0\n").as_bytes())
}

/// Give `command` nothing of this process's environment but [`ENVIRONMENT`].
pub(crate) fn clear(command: &mut Command) {
    command.env_clear();
    for name in ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// The tools this host builds an LLVM with, as the cleared environment finds
/// them.
struct HostTools {
    cc: PathBuf,
    cxx: PathBuf,
    /// The C and C++ compilers and CMake, each by its resolved path and all its
    /// `--version` says, and on macOS the SDK path and version `xcrun` resolves.
    identity: String,
}

fn host_tools() -> &'static HostTools {
    static TOOLS: OnceLock<HostTools> = OnceLock::new();
    TOOLS.get_or_init(|| tools_with(|question| asked(Command::new("xcrun").arg(question))))
}

/// [`host_tools`], with `xcrun`'s answer to each question it is asked, so a
/// test can stand in for it.
fn tools_with(xcrun: impl Fn(&str) -> String) -> HostTools {
    let [cc, cxx, cmake] = ["cc", "c++", "cmake"].map(on_path);
    let mut identity = String::new();
    for tool in [&cc, &cxx, &cmake] {
        let mut version = Command::new(tool);
        identity += &format!("{}\n{}", tool.display(), asked(version.arg("--version")));
    }
    if host_triple().ends_with("apple-darwin") {
        for question in ["--show-sdk-path", "--show-sdk-version"] {
            identity += &xcrun(question);
        }
    }
    HostTools { cc, cxx, identity }
}

/// Everything `command` prints, run with the environment [`clear`]ed; a
/// failure is refused.
fn asked(command: &mut Command) -> String {
    clear(command);
    let out = command.output().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(out.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    format!("{}{}\n", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// `name` as the `PATH` of [`ENVIRONMENT`] finds it, resolved.
fn on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("no `{name}` on PATH, and the LLVM build runs it"));
    fs::canonicalize(&found).unwrap_or_else(|e| panic!("resolve {}: {e}", found.display()))
}

/// The LLVM `fork` names, made if nobody on this host has made it, and held in
/// use for as long as the returned value lives. `root` records its key.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Path) -> Llvm {
    choose(root, rust_dir, fork, |fork| build_in_fork(root, fork))
}

/// [`resolve`] with the build that makes an LLVM passed in, so a test can stand
/// in for bootstrap: `build` builds in the fork checkout it is given and returns
/// the build directory, holding `<host>/llvm` and `<host>/lld`.
fn choose(root: &Path, rust_dir: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Llvm {
    let key = key(fork);
    let dir = store(rust_dir).join(&key);
    let using = keystore::made(root, Keyed::Llvm, &store(rust_dir), &key, || defect(&dir), || place(fork, &key, &dir, &build));
    Llvm { dir, _using: using }
}

/// [`resolve`], recording nothing for `root`: what a sysroot build reads of the
/// LLVM its compiler links, whose record is that compiler's
/// (`compiler::choose`).
pub fn held(root: &Path, rust_dir: &Path, fork: &Path) -> Llvm {
    held_with(root, rust_dir, fork, |fork| build_in_fork(root, fork))
}

/// [`held`] with the build passed in, as [`choose`] takes it.
fn held_with(root: &Path, rust_dir: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Llvm {
    let key = key(fork);
    let dir = store(rust_dir).join(&key);
    let using = crate::buildlock::keyed_made(root, Keyed::Llvm, &key, || defect(&dir), || place(fork, &key, &dir, &build));
    Llvm { dir, _using: using }
}

/// Why `dir` is not a finished LLVM, if it is not.
fn defect(dir: &Path) -> Option<String> {
    if !dir.join(SOURCE).is_file() {
        return Some(format!("{} carries no {SOURCE}", dir.display()));
    }
    let kept = KEPT.iter().map(|k| dir.join(k)).chain(crate::libcxx::SOURCES.iter().map(|s| dir.join("src").join(s)));
    let tools = TOOLS.iter().map(|t| dir.join(t)).filter(|p| !p.is_file());
    let gone: Vec<String> = kept.filter(|p| !p.is_dir()).chain(tools).map(|p| p.display().to_string()).collect();
    (!gone.is_empty()).then(|| format!("{} carries no {}", dir.display(), gone.join(", ")))
}

/// Refuse what `fork`'s `src/bootstrap` holds that no commit does: an LLVM is
/// keyed on the tree its commit records.
fn refuse_uncommitted_bootstrap(fork: &Path) {
    let status = git_bytes(fork, &["status", "--porcelain", "--untracked-files=normal", "--", BOOTSTRAP]);
    assert!(
        status.is_empty(),
        "{} holds changes no commit does, and an LLVM is keyed on the tree its commit records: \
         commit them, and the build makes the LLVM they name\n{}",
        fork.join(BOOTSTRAP).display(),
        String::from_utf8_lossy(&status),
    );
}

/// Build the LLVM `key` names from `fork` and put it at `dir`. The caller holds
/// the key's lock.
fn place(fork: &Path, key: &Key, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
    eprintln!("Building LLVM {key} in {}: nobody on this host has", fork.display());
    let built = build(fork);
    let host = host_triple();
    let partial = dir.with_extension("partial");
    if partial.exists() {
        keystore::remove(&partial);
    }
    for part in KEPT {
        clone_tree(&built.join(&host).join("llvm").join(part), &partial.join(part));
    }
    let lld = built.join(&host).join("lld/bin/lld");
    fs::copy(&lld, partial.join("bin/lld"))
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", lld.display(), partial.join("bin/lld").display()));
    // What was built is what the key names, or it is not that key's: the key
    // refuses a bootstrap the build left holding what no commit does.
    let again = self::key(fork);
    assert!(
        again == *key,
        "the fork's LLVM sources moved while LLVM {key} was being built (they now name {again}); \
         nothing was kept, and the next build makes the one they name"
    );
    let checkout = fork.join(LLVM);
    assert!(checkout.join(".git").exists(), "the LLVM build left no checkout at {}", checkout.display());
    let (built_from, commit) = (git_out(&checkout, &["rev-parse", "HEAD"]), llvm_commit(fork));
    // Bootstrap's `Llvm` step checks the gitlink's commit out before it builds,
    // so a checkout behind it, the key never reads, is moved first.
    assert!(
        built_from.trim() == commit,
        "{} is checked out at {}, and its gitlink names {commit}: bootstrap built the commit checked \
         out, which is not LLVM {key}'s; nothing was kept. `git -C {} submodule update {LLVM}` checks \
         the gitlink's commit out",
        checkout.display(),
        built_from.trim(),
        fork.display(),
    );
    check_out_committed(&checkout, &commit, &crate::libcxx::SOURCES, &partial.join("src"));
    fs::write(partial.join(SOURCE), format!("{key}\n"))
        .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCE).display()));
    read_only(&partial);
    keystore::retire(dir);
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
    fs::remove_dir_all(&built).unwrap_or_else(|e| panic!("remove {}: {e}", built.display()));
}

/// Write `paths` as `commit` holds them, from the repository at `checkout`,
/// under `dest`: through an index of their own and with no sparse pattern, so
/// nothing the checkout holds beside the commit, tracked, ignored or left out,
/// reaches them.
fn check_out_committed(checkout: &Path, commit: &str, paths: &[&str], dest: &Path) {
    fs::create_dir_all(dest).unwrap_or_else(|e| panic!("create {}: {e}", dest.display()));
    let index = toyos_tmpdir::TempDir::new("llvm-runtimes-index");
    let out = Command::new("git")
        .env("GIT_INDEX_FILE", index.join("index"))
        .args(["-c", "core.sparseCheckout=false", "--work-tree"])
        .arg(dest)
        .args(["checkout", commit, "--"])
        .args(paths)
        .current_dir(checkout)
        .output()
        .unwrap_or_else(|e| panic!("run git in {}: {e}", checkout.display()));
    assert!(
        out.status.success(),
        "git checkout {commit} -- {paths:?} into {} in {}: {}",
        dest.display(),
        checkout.display(),
        String::from_utf8_lossy(&out.stderr).trim(),
    );
}

/// Take write permission from every file and directory under `dir`, and from
/// `dir`.
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

/// What the bootstrap build directory `build` holds of an LLVM of its own:
/// bootstrap's LLVM and LLD, and `download-ci-llvm`'s with its downloads,
/// whole or as a stopped removal left them.
pub fn in_tree(build: &Path) -> Vec<PathBuf> {
    let host = build.join(host_triple());
    let mut own: Vec<PathBuf> = ["llvm", "lld", "ci-llvm"].iter().map(|d| host.join(d)).collect();
    let cache = build.join("cache");
    if cache.is_dir() {
        let downloads: BTreeSet<String> = fs::read_dir(&cache)
            .unwrap_or_else(|e| panic!("read {}: {e}", cache.display()))
            .map(|e| e.unwrap_or_else(|e| panic!("read {}: {e}", cache.display())).file_name())
            .map(|name| name.to_string_lossy().trim_end_matches(".swept").to_string())
            .filter(|name| name.starts_with("llvm-"))
            .collect();
        own.extend(downloads.iter().map(|name| cache.join(name)));
    }
    own.retain(|dir| dir.exists() || dir.with_extension("swept").exists());
    own
}

/// Remove [`in_tree`]. The caller holds the lock covering `build`, where
/// nothing builds an LLVM or downloads one any more.
pub fn retire_in_tree(build: &Path) {
    for dir in in_tree(build) {
        eprintln!("Removing {}: builds here link the host's LLVM or none", dir.display());
        keystore::retire(&dir);
    }
}

/// Bootstrap's build of LLVM, clang and LLD in `fork`, into its own build
/// directory, which it returns, under the n2 installed under `root`.
fn build_in_fork(root: &Path, fork: &Path) -> PathBuf {
    let n2 = crate::n2::bin(root);
    // Bootstrap refuses to build with no `ninja` on `PATH`, and CMake's Ninja
    // generator runs the first one there; n2's directory holds nothing but n2.
    let caller = std::env::var_os("PATH").unwrap_or_else(|| panic!("PATH is unset, and the LLVM build finds its tools on it"));
    let path = std::env::join_paths(std::iter::once(n2.clone()).chain(std::env::split_paths(&caller)))
        .unwrap_or_else(|e| panic!("{} cannot lead PATH: {e}", n2.display()));
    let host = host_triple();
    let build_dir = fork.join("build/toyos-llvm");
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host, host_tools()))
        .unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args = ["build", "--config", config, "src/llvm-project/llvm", "src/llvm-project/lld"];
    let (ok, _) = toolchain::x_build_with(fork, &args, "LLVM", |command| {
        clear(command);
        command.env("PATH", path);
    });
    assert!(ok, "the LLVM build in {} failed: its output above says why", fork.display());
    build_dir
}

/// Bootstrap's configuration for an LLVM: the options every compiler build's
/// `[llvm]` names, no host library, and `tools`' compilers, for the host alone.
fn config_text(build_dir: &Path, host: &str, tools: &HostTools) -> String {
    let off: Vec<String> = NO_HOST_LIBRARIES.iter().map(|option| format!("{option} = \"OFF\"")).collect();
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]

[llvm]
{llvm}
build-config = {{ {off} }}

[target.{host}]
cc = "{cc}"
cxx = "{cxx}"
"#,
        build_dir = build_dir.display(),
        llvm = crate::clang::LLVM_CONFIG,
        off = off.join(", "),
        cc = tools.cc.display(),
        cxx = tools.cxx.display(),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::buildlock;
    use crate::compiler::tests::{estate, git, write, LLVM_B};

    /// A scratch directory whose read-only LLVMs are made writable again before
    /// it goes.
    struct Scratch(TempDir);

    impl Scratch {
        fn new(name: &str) -> Self {
            Self(TempDir::new(name))
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            keystore::writable(&self.0);
        }
    }

    /// Bootstrap's stand-in: what its LLVM and LLD builds leave in the build
    /// directory, CMake's tree among them.
    fn fake_build(fork: &Path) -> PathBuf {
        let built = fork.join("build/toyos-llvm");
        let _ = fs::remove_dir_all(&built);
        let install = built.join(host_triple()).join("llvm");
        for tool in ["llvm-config", "clang-22", "llvm-ar"] {
            write(&install.join("bin").join(tool), &format!("the {tool}"));
        }
        std::os::unix::fs::symlink("clang-22", install.join("bin/clang")).unwrap();
        write(&install.join("include/llvm/Config/llvm-config.h"), "#define LLVM_VERSION_MAJOR 22");
        write(&install.join("lib/libLLVMCore.a"), "core");
        write(&install.join("lib/clang/22/include/stddef.h"), "typedef long ptrdiff_t;");
        write(&install.join("build/CMakeCache.txt"), "the build tree");
        write(&built.join(host_triple()).join("lld/bin/lld"), "the lld");
        built
    }

    /// Check `fork`'s LLVM out at the commit `content` names, one commit in every
    /// checkout whatever came before it: what bootstrap leaves once it has built
    /// one. Returns it.
    fn check_out_llvm(fork: &Path, content: &str) -> String {
        let checkout = fork.join(LLVM);
        if !checkout.join(".git").exists() {
            git(&checkout, &["init", "-q"]);
        }
        write(&checkout.join("llvm/CMakeLists.txt"), content);
        for source in crate::libcxx::SOURCES {
            write(&checkout.join(source).join("CMakeLists.txt"), &format!("the {source} of {content}"));
        }
        git(&checkout, &["add", "-A"]);
        let tree = git(&checkout, &["write-tree"]);
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit-tree", &tree, "-m", content])
            .envs([("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z"), ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")])
            .current_dir(&checkout)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let commit = String::from_utf8(out.stdout).unwrap().trim().to_string();
        git(&checkout, &["reset", "-q", "--hard", &commit]);
        commit
    }

    /// [`check_out_llvm`], and that commit recorded as `fork`'s gitlink.
    fn pin_llvm(fork: &Path, content: &str) {
        let commit = check_out_llvm(fork, content);
        git(fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{commit},{LLVM}")]);
        git(fork, &["commit", "-qm", &format!("LLVM {content}")]);
    }

    /// `estate`, every fork's LLVM checked out at the one commit its gitlink
    /// records.
    fn estate_built(scratch: &Path) -> (PathBuf, PathBuf, [PathBuf; 3]) {
        let (primary, rust_dir, forks) = estate(scratch);
        for worktree in &forks {
            pin_llvm(&worktree.join("rust"), "A");
        }
        pin_llvm(&rust_dir, "A");
        (primary, rust_dir, forks)
    }

    /// What `f` panicked with; `expect` if it returned.
    fn refusal(expect: &str, f: impl FnOnce()) -> String {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err(expect);
        refused.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    /// **One LLVM per key, made once and found again**: two worktrees whose
    /// compilers differ and whose LLVM commit is one share one LLVM, and the
    /// second build makes nothing and writes nothing; what is kept is the
    /// install and its LLD, never CMake's tree, and the build directory goes.
    #[test]
    fn two_compilers_of_one_llvm_make_it_once() {
        let scratch = Scratch::new("llvm");
        let (primary, rust_dir, [same, a, b]) = estate_built(&scratch);
        let makes = Cell::new(0);
        let once = |fork: &Path| {
            makes.set(makes.get() + 1);
            assert_eq!(makes.get(), 1, "an LLVM whose key was made was made again");
            fake_build(fork)
        };

        let la = choose(&a, &rust_dir, &a.join("rust"), once);
        assert_eq!(makes.get(), 1);
        assert_eq!(defect(&la.dir), None);
        assert_eq!(fs::read_to_string(la.dir.join("bin/lld")).unwrap(), "the lld");
        assert_eq!(fs::read_link(la.dir.join("bin/clang")).unwrap(), Path::new("clang-22"));
        assert!(la.dir.join("lib/clang/22/include/stddef.h").is_file());
        assert_eq!(fs::read_to_string(la.dir.join("src/libcxx/CMakeLists.txt")).unwrap(), "the libcxx of A", "the runtimes' sources are not the commit's");
        assert!(!la.dir.join("build").exists(), "CMake's tree was kept");
        assert!(!a.join("rust/build/toyos-llvm").exists(), "the build directory outlived the placement");

        let before = snapshot(&store(&rust_dir));
        let lb = choose(&b, &rust_dir, &b.join("rust"), once);
        let primary_s = choose(&primary, &rust_dir, &rust_dir, once);
        let same_s = choose(&same, &rust_dir, &same.join("rust"), once);
        assert_eq!(makes.get(), 1);
        assert!(lb.dir == la.dir && primary_s.dir == la.dir && same_s.dir == la.dir, "one LLVM commit named two LLVMs");
        assert_eq!(snapshot(&store(&rust_dir)), before, "an LLVM was written after it was whole");
    }

    /// **A placed LLVM cannot be written**, through its own path or through a
    /// link bootstrap makes to one of its files, and nothing in it can be
    /// removed, replaced or added.
    #[test]
    fn a_placed_llvm_is_never_written() {
        let scratch = Scratch::new("llvm-read-only");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let dir = choose(&a, &rust_dir, &a.join("rust"), fake_build).dir;
        let stage = scratch.join("stage1-rust-lld");
        fs::hard_link(dir.join("bin/lld"), &stage).unwrap();
        let denied = |what: &str, done: std::io::Result<()>| {
            assert_eq!(done.map_err(|e| e.kind()).err(), Some(std::io::ErrorKind::PermissionDenied), "{what}");
        };
        for file in [dir.join("bin/lld"), stage, dir.join("lib/libLLVMCore.a"), dir.join(SOURCE)] {
            denied(&format!("{} could be written", file.display()), fs::OpenOptions::new().write(true).open(&file).map(drop));
        }
        denied("a placed tool could be removed", fs::remove_file(dir.join("bin/lld")));
        denied("a file could be added to a placed LLVM", fs::write(dir.join("bin/new"), "x"));
        denied("a placed LLVM could be emptied", fs::remove_dir_all(dir.join("include")));
    }

    /// **An LLVM that is not whole is made again, all of it**: one whose
    /// `SOURCE` says it finished and that lost a tool, or a directory it keeps,
    /// is replaced.
    #[test]
    fn an_llvm_that_is_not_whole_is_made_again() {
        let scratch = Scratch::new("llvm-whole");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let makes = Cell::new(0);
        let counted = |fork: &Path| {
            makes.set(makes.get() + 1);
            fake_build(fork)
        };
        let dir = choose(&a, &rust_dir, &a.join("rust"), counted).dir;
        for (lost, made) in [("bin/lld", 2), ("lib", 3), ("src/libcxxabi", 4)] {
            keystore::writable(&dir);
            let lost = dir.join(lost);
            if lost.is_dir() {
                fs::remove_dir_all(&lost).unwrap();
            } else {
                fs::remove_file(&lost).unwrap();
            }
            assert!(defect(&dir).is_some_and(|d| d.contains(&lost.display().to_string())), "{:?}", defect(&dir));
            let again = choose(&a, &rust_dir, &a.join("rust"), counted);
            assert_eq!((makes.get(), defect(&again.dir)), (made, None));
        }
    }

    /// **The key is the LLVM and nothing else**: the same inputs give the same
    /// key; each of the recipe, the configuration (its `[llvm]` and its host),
    /// the host's tools, the committed LLVM gitlink and the committed
    /// `src/bootstrap` moves it; a compiler edit does not.
    #[test]
    fn the_key_moves_with_the_llvm_and_only_with_it() {
        let scratch = Scratch::new("llvm-key");
        let (_primary, rust_dir, [same, a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        let tools = host_tools();
        let config = config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), tools);
        let base = key(&fork);
        assert_eq!(key_of(&fork, RECIPE, &config, &tools.identity), base);
        let linux = config_text(Path::new(KEYED_BUILD_DIR), "x86_64-unknown-linux-gnu", tools);
        for (what, other) in [
            ("the recipe", key_of(&fork, "another recipe", &config, &tools.identity)),
            ("the [llvm]", key_of(&fork, RECIPE, &config.replace("X86", "RISCV;X86"), &tools.identity)),
            ("the host", key_of(&fork, RECIPE, &linux, &tools.identity)),
            ("the host's tools", key_of(&fork, RECIPE, &config, "/usr/bin/gcc\ngcc 14\n")),
        ] {
            assert_ne!(other, base, "{what} did not move the key");
        }

        assert_eq!(key(&fork), key(&rust_dir), "one LLVM commit named two keys");
        assert_eq!(key(&fork), key(&a.join("rust")), "a compiler/ edit moved the LLVM key");
        write(&fork.join("compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "a target"]);
        assert_eq!(key(&fork), base, "a compiler/ commit moved the LLVM key");

        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        git(&fork, &["commit", "-qm", "another LLVM"]);
        let moved = key(&fork);
        assert_ne!(moved, base, "another LLVM commit kept the key");

        write(&fork.join("src/bootstrap/src/core/build_steps/llvm.rs"), "cfg.define(\"LLVM_ENABLE_ZLIB\", \"OFF\");\n");
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "another LLVM step"]);
        assert_ne!(key(&fork), moved, "another LLVM step in bootstrap kept the key");
    }

    /// **The tools the key names are the host's**: the C and C++ compilers the
    /// configuration names by path, and CMake, each by the file it resolves to
    /// and what its `--version` says; on macOS, the SDK.
    #[test]
    fn the_key_names_the_host_s_tools() {
        let tools = host_tools();
        let lines: Vec<&str> = tools.identity.lines().collect();
        for tool in [&tools.cc, &tools.cxx] {
            assert!(tool.is_absolute() && tool.is_file(), "{} is no compiler", tool.display());
            let at = lines.iter().position(|l| Path::new(l) == tool.as_path()).unwrap_or_else(|| panic!("{}", tools.identity));
            assert!(lines[at + 1].contains("version"), "{} said no version: {}", tool.display(), tools.identity);
        }
        assert!(lines.iter().any(|l| l.starts_with("cmake version")), "{}", tools.identity);
        if host_triple().ends_with("apple-darwin") {
            assert!(lines.iter().any(|l| l.ends_with(".sdk") && Path::new(l).is_dir()), "no SDK: {}", tools.identity);
        }
        let config = config_text(Path::new(KEYED_BUILD_DIR), "h", tools);
        let named = format!("[target.h]\ncc = \"{}\"\ncxx = \"{}\"\n", tools.cc.display(), tools.cxx.display());
        assert!(config.contains(&named), "{config}");
    }

    /// **Two SDK versions are two LLVMs**, though both answer one SDK path, as
    /// the unversioned `MacOSX.sdk` a Command Line Tools update moves does.
    #[test]
    #[cfg(target_os = "macos")]
    fn two_sdk_versions_are_two_keys() {
        let scratch = Scratch::new("llvm-sdk");
        let (_primary, _rust_dir, [same, _a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        let [older, newer] = ["26.0", "27.0"].map(|version| {
            let tools = tools_with(|question| match question {
                "--show-sdk-path" => "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk\n".to_string(),
                "--show-sdk-version" => format!("{version}\n"),
                other => panic!("xcrun was asked {other}"),
            });
            key_of(&fork, RECIPE, &config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), &tools), &tools.identity)
        });
        assert_ne!(older, newer, "two SDK versions at one SDK path named one LLVM");
    }

    /// **Every host library LLVM looks for is turned off by name**, in the
    /// `build-config` bootstrap passes to CMake after its own options.
    #[test]
    fn the_llvm_config_turns_every_host_library_off() {
        let config = config_text(Path::new(KEYED_BUILD_DIR), "h", host_tools());
        let off = "build-config = { LLVM_ENABLE_ZLIB = \"OFF\", LLVM_ENABLE_ZSTD = \"OFF\", \
                   LLVM_ENABLE_LIBXML2 = \"OFF\", CLANG_ENABLE_LIBXML2 = \"OFF\", LLVM_ENABLE_LIBEDIT = \"OFF\", \
                   LLVM_ENABLE_LIBPFM = \"OFF\", LLVM_ENABLE_FFI = \"OFF\", LLVM_ENABLE_CURL = \"OFF\", \
                   LLVM_ENABLE_HTTPLIB = \"OFF\", LLVM_ENABLE_ICU = \"OFF\", LLVM_ENABLE_ICONV = \"OFF\", \
                   LLVM_ENABLE_Z3_SOLVER = \"OFF\" }\n";
        assert!(config.contains(&format!("\n{off}")), "{config}");
    }

    /// **A checkout behind its gitlink names the gitlink's LLVM**: the key
    /// reads the gitlink and never the checkout, and the build, which checks the
    /// gitlink's commit out first as bootstrap's `Llvm` step does, stores it.
    #[test]
    fn a_checkout_behind_its_gitlink_is_built_at_the_gitlink() {
        let scratch = Scratch::new("llvm-behind");
        let (_primary, rust_dir, [same, a, _b]) = estate_built(&scratch);
        let (fork, at_gitlink) = (a.join("rust"), same.join("rust"));
        for fork in [&fork, &at_gitlink] {
            pin_llvm(fork, "B");
        }
        check_out_llvm(&fork, "A");
        assert_eq!(key(&fork), key(&at_gitlink), "a checkout behind its gitlink moved the key");
        let updating = |fork: &Path| {
            check_out_llvm(fork, "B");
            fake_build(fork)
        };
        let llvm = choose(&a, &rust_dir, &fork, updating);
        assert_eq!((llvm.dir.clone(), defect(&llvm.dir)), (store(&rust_dir).join(key(&at_gitlink)), None));
    }

    const FORK: &str = "TOYOS_LLVM_TEST_FORK";
    const ROOT: &str = "TOYOS_LLVM_TEST_ROOT";

    /// What a caller's environment may hold that would reach an LLVM build:
    /// flags, compilers, tools, and the SDK and deployment target.
    const AMBIENT: [(&str, &str); 11] = [
        ("CFLAGS", "-O0"),
        ("CXXFLAGS", "-O0"),
        ("LDFLAGS", "-Wl,-no-such-flag"),
        ("CC", "/no/such/cc"),
        ("CXX", "/no/such/c++"),
        ("AR", "/no/such/ar"),
        ("RANLIB", "/no/such/ranlib"),
        ("SDKROOT", "/no/such.sdk"),
        ("MACOSX_DEPLOYMENT_TARGET", "10.9"),
        ("CMAKE", "/no/such/cmake"),
        ("CMAKE_TOOLCHAIN_FILE", "/no/such/toolchain.cmake"),
    ];

    /// **The caller's environment reaches neither the LLVM build nor its key**:
    /// a process holding every [`AMBIENT`] name keys the LLVM as this one does,
    /// and the bootstrap it runs, a script that writes down its environment,
    /// sees nothing but `PATH`, `TMPDIR` and what a shell sets itself, named
    /// here and not read from [`ENVIRONMENT`]; its `PATH` is the caller's with
    /// n2's directory first.
    #[test]
    fn the_caller_s_environment_reaches_neither_the_build_nor_the_key() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("llvm-environment");
        let (_primary, _rust_dir, [same, _a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        write(&fork.join("library/Cargo.lock"), "# lock\n");
        write(&fork.join("x"), "#!/bin/sh\nenv > build/toyos-llvm/environment\n");
        fs::set_permissions(fork.join("x"), fs::Permissions::from_mode(0o755)).unwrap();
        let n2 = crate::n2::tests::installed_stand_in(&scratch);

        let mut rerun = buildlock::tests::rerun("llvm::tests::keyed_and_built");
        let out = rerun.env(FORK, &fork).env(ROOT, &*scratch).envs(AMBIENT).output().unwrap();
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let built = fork.join("build/toyos-llvm");
        assert_eq!(fs::read_to_string(built.join("key")).unwrap(), key(&fork).as_str(), "the caller's environment moved the key");
        let seen = fs::read_to_string(built.join("environment")).unwrap();
        let allowed = ["PATH", "TMPDIR", "BOOTSTRAP_SKIP_TARGET_SANITY", "PWD", "OLDPWD", "SHLVL", "_"];
        for name in seen.lines().filter_map(|l| l.split_once('=')).map(|(name, _)| name) {
            assert!(allowed.contains(&name), "the build saw {name}: {seen}");
        }
        let caller = std::env::var_os("PATH").unwrap();
        let path = std::env::join_paths(std::iter::once(n2).chain(std::env::split_paths(&caller))).unwrap();
        let path = format!("PATH={}", path.to_str().unwrap());
        assert!(seen.lines().any(|l| l == path), "the build's PATH is not {path}: {seen}");
    }

    /// The process [`the_caller_s_environment_reaches_neither_the_build_nor_the_key`]
    /// runs: the key of the fork in [`FORK`] and its build under the n2 of the
    /// root in [`ROOT`], the key written beside what the build wrote.
    #[test]
    #[ignore = "the process the environment test runs; never runs on its own"]
    fn keyed_and_built() {
        let fork = PathBuf::from(std::env::var(FORK).unwrap_or_else(|_| panic!("keyed_and_built ran without {FORK}; it is not a test")));
        let root = PathBuf::from(std::env::var(ROOT).unwrap_or_else(|_| panic!("keyed_and_built ran without {ROOT}; it is not a test")));
        let key = key(&fork);
        let built = build_in_fork(&root, &fork);
        fs::write(built.join("key"), key.as_str()).unwrap();
    }

    /// **A gitlink staged and not committed names no LLVM**: bootstrap checks
    /// out the index's, and the key would name HEAD's.
    #[test]
    fn a_staged_llvm_gitlink_is_refused() {
        let scratch = Scratch::new("llvm-staged");
        let (_primary, _rust_dir, [same, _a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        let said = refusal("a staged gitlink named an LLVM", || {
            key(&fork);
        });
        assert!(said.contains("stages") && said.contains(LLVM_B), "{said}");
    }

    /// **An uncommitted `src/bootstrap` names no LLVM**, not even one already
    /// stored: the key refuses it, and nothing is built or resolved.
    #[test]
    fn an_uncommitted_bootstrap_names_no_llvm() {
        let scratch = Scratch::new("llvm-dirty-key");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let fork = a.join("rust");
        drop(choose(&a, &rust_dir, &fork, fake_build));
        write(&fork.join("src/bootstrap/src/core/build_steps/llvm.rs"), "cfg.define(\"LLVM_ENABLE_ZLIB\", \"ON\");\n");
        let never = |_: &Path| -> PathBuf { panic!("an LLVM was built from a bootstrap no commit holds") };
        let said = refusal("an uncommitted bootstrap edit resolved to the stored LLVM", || {
            choose(&a, &rust_dir, &fork, never);
        });
        assert!(said.contains("src/bootstrap holds changes no commit does"), "{said}");
        let said = refusal("an uncommitted bootstrap edit named an LLVM", || {
            key(&fork);
        });
        assert!(said.contains("src/bootstrap holds changes no commit does"), "{said}");
    }

    /// **Only an LLVM built from what its key names is stored**: not from a
    /// `src/bootstrap` holding what no commit does, which is refused before
    /// anything is built, and after, when the build left it so; not from an
    /// LLVM checkout other than the gitlink's; not when the sources moved while
    /// it was being built.
    #[test]
    fn what_the_key_does_not_name_is_never_stored() {
        let scratch = Scratch::new("llvm-dirt");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let fork = a.join("rust");
        let step = fork.join("src/bootstrap/src/core/build_steps/llvm.rs");
        let never = |_: &Path| -> PathBuf { panic!("an LLVM was built from a bootstrap no commit holds") };

        write(&step, "cfg.define(\"LLVM_ENABLE_ZLIB\", \"OFF\");\n");
        let said = refusal("an uncommitted bootstrap edit was built", || {
            choose(&a, &rust_dir, &fork, never);
        });
        assert!(said.contains("src/bootstrap holds changes no commit does"), "{said}");
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "the step"]);

        let dirtying = |fork: &Path| {
            write(&fork.join("src/bootstrap/src/core/build_steps/llvm.rs"), "cfg.define(\"LLVM_ENABLE_ZSTD\", \"ON\");\n");
            fake_build(fork)
        };
        let said = refusal("an LLVM built while its bootstrap was edited was stored", || {
            choose(&a, &rust_dir, &fork, dirtying);
        });
        assert!(said.contains("src/bootstrap holds changes no commit does"), "{said}");
        git(&fork, &["checkout", "-q", "--", "src/bootstrap"]);

        let lagging = |fork: &Path| {
            check_out_llvm(fork, "A");
            fake_build(fork)
        };
        pin_llvm(&fork, "B");
        let said = refusal("an LLVM checkout other than the gitlink's was stored", || {
            choose(&a, &rust_dir, &fork, lagging);
        });
        assert!(said.contains("is checked out at") && said.contains("submodule update src/llvm-project"), "{said}");
        check_out_llvm(&fork, "B");

        let moving = |fork: &Path| {
            write(&step, "cfg.define(\"LLVM_ENABLE_ZSTD\", \"OFF\");\n");
            git(fork, &["commit", "-qam", "moved while built"]);
            fake_build(fork)
        };
        let said = refusal("an LLVM whose sources moved while it was built was stored", || {
            choose(&a, &rust_dir, &fork, moving);
        });
        assert!(said.contains("moved while LLVM"), "{said}");

        let stored: Vec<_> = fs::read_dir(store(&rust_dir)).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(stored.iter().all(|n| n.to_string_lossy().ends_with(".partial")), "stored: {stored:?}");
    }

    /// **The runtimes' sources are the commit's**: made while the LLVM was
    /// built, an edit to a file the commit holds is refused and nothing is
    /// stored, and a file the checkout ignores is not stored with it.
    #[test]
    fn the_runtimes_sources_are_the_commit_s() {
        let scratch = Scratch::new("llvm-runtimes");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let fork = a.join("rust");
        let checkout = fork.join(LLVM);
        write(&checkout.join(".git/info/exclude"), "*.pyc\n");

        let editing = |fork: &Path| {
            write(&fork.join(LLVM).join("libcxx/CMakeLists.txt"), "an edit no commit holds");
            fake_build(fork)
        };
        let said = refusal("an edit to the runtimes' sources was stored", || {
            choose(&a, &rust_dir, &fork, editing);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        git(&checkout, &["checkout", "-q", "--", "libcxx"]);

        let ignored = |fork: &Path| {
            write(&fork.join(LLVM).join("libcxx/utils/cache.pyc"), "what no commit holds");
            fake_build(fork)
        };
        let dir = choose(&a, &rust_dir, &fork, ignored).dir;
        assert_eq!(fs::read_to_string(dir.join("src/libcxx/CMakeLists.txt")).unwrap(), "the libcxx of A");
        assert!(checkout.join("libcxx/utils/cache.pyc").is_file());
        assert!(!dir.join("src/libcxx/utils").exists(), "a file the checkout ignores was stored");
    }

    const WORKTREE: &str = "TOYOS_LLVM_TEST_WORKTREE";
    const RUST_DIR: &str = "TOYOS_LLVM_TEST_RUST_DIR";
    const ROLE: &str = "TOYOS_LLVM_TEST_ROLE";

    /// The competing process for the tests below: the LLVM the worktree in
    /// [`WORKTREE`] names, held in use until released — or, as `make`, held
    /// while it is being made.
    #[test]
    #[ignore = "the competing process for the tests below; never runs on its own"]
    fn child_role() {
        let worktree = PathBuf::from(std::env::var(WORKTREE).unwrap_or_else(|_| panic!("child_role ran without {WORKTREE}; it is not a test")));
        let rust_dir = PathBuf::from(std::env::var(RUST_DIR).unwrap());
        match std::env::var(ROLE).unwrap().as_str() {
            "use" => {
                let _held = choose(&worktree, &rust_dir, &worktree.join("rust"), fake_build);
                buildlock::tests::hold_until_released();
            }
            "make" => {
                let held = |fork: &Path| {
                    buildlock::tests::hold_until_released();
                    fake_build(fork)
                };
                choose(&worktree, &rust_dir, &worktree.join("rust"), held);
            }
            other => panic!("unknown child role {other}"),
        }
    }

    fn elsewhere(role: &str, worktree: &Path, rust_dir: &Path) -> buildlock::tests::Elsewhere {
        let env = [(ROLE, std::ffi::OsStr::new(role)), (WORKTREE, worktree.as_os_str()), (RUST_DIR, rust_dir.as_os_str())];
        buildlock::tests::Elsewhere::hold("llvm::tests::child_role", &env)
    }

    /// **An LLVM another process is making is waited for, not made again.**
    #[test]
    fn an_llvm_being_made_elsewhere_is_not_made_again() {
        let scratch = Scratch::new("llvm-made-elsewhere");
        let (primary, rust_dir, [_same, _a, b]) = estate_built(&scratch);
        let maker = elsewhere("make", &b, &rust_dir);
        let made = key(&b.join("rust"));
        assert!(buildlock::keyed_idle(&primary, Keyed::Llvm, &made).is_none(), "a sweep could take an LLVM being made");
        maker.release();
        let never = |_: &Path| -> PathBuf { panic!("an LLVM another process made was made here too") };
        assert_eq!(defect(&choose(&b, &rust_dir, &b.join("rust"), never).dir), None);
    }

    /// **A sweep takes an LLVM only once no worktree names it and nobody uses
    /// it**: `a` moves to another LLVM while a process of its own still uses
    /// the first, and then `b` names the first until it moves too.
    #[test]
    fn an_llvm_is_swept_once_nobody_names_or_uses_it() {
        let scratch = Scratch::new("llvm-sweep");
        let (primary, rust_dir, [_same, a, b]) = estate_built(&scratch);
        let user = elsewhere("use", &a, &rust_dir);
        let first = store(&rust_dir).join(key(&a.join("rust")));

        let fork = a.join("rust");
        pin_llvm(&fork, "B");
        let second = choose(&a, &rust_dir, &fork, fake_build);
        assert_ne!(second.dir, first);
        assert!(first.is_dir(), "placing an LLVM swept one still in use");

        keystore::record(&b, Keyed::Llvm, &key(&b.join("rust")));
        user.release();
        let swept = |root: &Path| keystore::sweep(root, Keyed::Llvm, &store(&rust_dir));
        assert_eq!(swept(&primary), Vec::<PathBuf>::new(), "the sweep took an LLVM a worktree names");
        keystore::record(&b, Keyed::Llvm, &key(&fork));
        assert_eq!(swept(&primary), [first], "the sweep kept an LLVM nobody names, or took a named one");
        assert!(second.dir.is_dir());
    }

    /// **A sweep stopped halfway leaves no LLVM that passes for whole**: the
    /// entry is renamed away from its key before anything in it is removed, and
    /// the next sweep takes what the stopped one left.
    #[test]
    fn a_stopped_sweep_leaves_no_llvm_that_passes_for_whole() {
        let scratch = Scratch::new("llvm-sweep-stopped");
        let (primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let dir = choose(&a, &rust_dir, &a.join("rust"), fake_build).dir;
        keystore::forget(&a, Keyed::Llvm);
        let halfway = |path: &Path| {
            keystore::writable(path);
            fs::remove_file(path.join("lib/libLLVMCore.a")).unwrap();
            panic!("stopped");
        };
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            keystore::sweep_by(&primary, Keyed::Llvm, &store(&rust_dir), halfway)
        }));
        assert!(stopped.is_err(), "the stand-in removal was never asked");
        assert!(defect(&dir).is_some(), "a stopped sweep left {} passing for whole", dir.display());
        assert_eq!(keystore::sweep(&primary, Keyed::Llvm, &store(&rust_dir)), [dir.with_extension("swept")]);
    }

    /// **An LLVM a worktree resolved stays once nothing uses it**: its record,
    /// not its use, is what names it.
    #[test]
    fn a_resolved_llvm_is_named_by_its_worktree() {
        let scratch = Scratch::new("llvm-named");
        let (primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        elsewhere("use", &a, &rust_dir).release();
        let dir = store(&rust_dir).join(key(&a.join("rust")));
        assert_eq!(keystore::sweep(&primary, Keyed::Llvm, &store(&rust_dir)), Vec::<PathBuf>::new());
        assert_eq!(defect(&dir), None, "the sweep took an LLVM the worktree that resolved it names");
    }

    /// **An LLVM held for a sysroot build is not recorded**: the record follows
    /// the compiler's, so a worktree whose compiler is the primary's names none.
    #[test]
    fn a_held_llvm_is_not_recorded() {
        let scratch = Scratch::new("llvm-held");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let llvm = held_with(&a, &rust_dir, &a.join("rust"), fake_build);
        assert_eq!(defect(&llvm.dir), None);
        assert_eq!(keystore::recorded(&a, Keyed::Llvm), None, "a held LLVM was recorded");
    }

    /// **A build directory whose compiler links the host's LLVM keeps none of
    /// its own**: bootstrap's LLVM and LLD, `download-ci-llvm`'s and its
    /// downloads go, and what a stopped removal left; the rest stays.
    #[test]
    fn a_build_directory_keeps_no_llvm_of_its_own() {
        let build = TempDir::new("llvm-in-tree");
        let host = build.join(host_triple());
        for file in ["llvm/bin/llvm-config", "lld/bin/lld", "ci-llvm/lib/libLLVM.dylib", "llvm.swept/bin/clang", "stage2/bin/rustc"] {
            write(&host.join(file), "x");
        }
        for file in ["cache/llvm-1111-false/rust-dev.tar.xz", "cache/llvm-2222-false.swept/rust-dev.tar.xz", "cache/2026-07-13/rustc.tar.xz"] {
            write(&build.join(file), "x");
        }
        retire_in_tree(&build);
        for gone in ["llvm", "lld", "ci-llvm", "llvm.swept"] {
            assert!(!host.join(gone).exists(), "{gone} stayed");
        }
        let cache: Vec<_> = fs::read_dir(build.join("cache")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(cache, ["2026-07-13"]);
        assert!(host.join("stage2/bin/rustc").is_file());
    }

    /// Every file under `dir` with its bytes, and every link with its target.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in fs::read_dir(&at).unwrap().flatten() {
                let path = entry.path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.file_type().is_symlink() {
                    out.push((path.clone(), fs::read_link(&path).unwrap().into_os_string().into_encoded_bytes()));
                } else if meta.is_dir() {
                    stack.push(path);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}
