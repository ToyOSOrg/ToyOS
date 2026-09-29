//! The LLVM every host compiler links, with its clang and LLD, content-addressed:
//! one per key on this host, shared by every compiler build that names it.
//!
//! **An LLVM is a function of its key** ([`key`]): the `src/llvm-project` commit
//! the fork checkout's gitlink names (`compiler::llvm_commit`, which refuses a
//! checkout holding what no commit does and a gitlink staged and not committed),
//! the tree of its `src/bootstrap`, the bootstrap configuration below, [`RECIPE`],
//! and the host's C and C++ compilers. `rust/build/llvm/<key>/` in the primary is
//! bootstrap's install of that LLVM and its clang, with its LLD in `bin/` beside
//! `llvm-config`, made by whichever build first needs it ([`resolve`]), and
//! stored only when it was built from what the key names. Its files are
//! read-only once its [`SOURCE`] file exists, so a tool bootstrap hard-links
//! into a stage is not written through the link. Every compiler build, the
//! primary's and a worktree's own, names it as the host's `llvm-config` with
//! `llvm-has-rust-patches`, so bootstrap builds no LLVM and takes LLD from beside
//! it as `rust-lld`; `clang::provision` copies its clang. Once a build directory's
//! compiler is built against it, the LLVM that directory built itself goes
//! ([`retire_in_tree`]).
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

use crate::buildlock::{self, Guard, Keyed};
use crate::compiler::{llvm_commit, LLVM};
use crate::keystore;
use crate::sysroot::{clone_tree, git_bytes, git_out, short};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become an LLVM and is none of the other
/// parts: the build's targets and what is kept of it. Moving it moves every key.
const RECIPE: &str = "bootstrap build of src/llvm-project/llvm and src/llvm-project/lld; the install's bin, \
                      include and lib, and lld in bin, read-only; 2";

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

/// The build directory the key's configuration names: any one builds the same
/// LLVM.
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

/// The key of the LLVM `fork` names.
pub fn key(fork: &Path) -> String {
    key_of(fork, RECIPE, &config_text(Path::new(KEYED_BUILD_DIR), &host_triple()), cc_identity())
}

fn key_of(fork: &Path, recipe: &str, config: &str, cc: &str) -> String {
    let bootstrap = git_out(fork, &["rev-parse", &format!("HEAD:{BOOTSTRAP}")]);
    short([recipe, config, &llvm_commit(fork), bootstrap.trim(), cc].join("\n\0\n").as_bytes())
}

/// The host's C and C++ compilers as bootstrap's `cc` finds them, the ones that
/// build LLVM: each one's resolved path and everything its `--version` says.
fn cc_identity() -> &'static str {
    static IDENTITY: OnceLock<String> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        let host = host_triple();
        let mut identity = String::new();
        for (var, default) in [("CC", "cc"), ("CXX", "c++")] {
            let named = [format!("{var}_{host}"), format!("{var}_{}", host.replace('-', "_")), format!("HOST_{var}"), var.into()]
                .iter()
                .find_map(|v| std::env::var(v).ok())
                .unwrap_or_else(|| default.into());
            let found = if named.contains('/') {
                PathBuf::from(&named)
            } else {
                let path = std::env::var_os("PATH").unwrap_or_default();
                std::env::split_paths(&path)
                    .map(|dir| dir.join(&named))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| panic!("no `{named}` on PATH, and bootstrap builds LLVM with it"))
            };
            let path = fs::canonicalize(&found).unwrap_or_else(|e| panic!("resolve {}: {e}", found.display()));
            let out = Command::new(&path)
                .arg("--version")
                .output()
                .unwrap_or_else(|e| panic!("run {} --version: {e}", path.display()));
            assert!(out.status.success(), "{} --version failed: {}", path.display(), String::from_utf8_lossy(&out.stderr));
            identity += &format!(
                "{var} {}\n{}{}\n",
                path.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        identity
    })
}

/// The LLVM `fork` names, made if nobody on this host has made it, and held in
/// use for as long as the returned value lives. `root` records its key.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Path) -> Llvm {
    choose(root, rust_dir, fork, build_in_fork)
}

/// [`resolve`] with the build that makes an LLVM passed in, so a test can stand
/// in for bootstrap: `build` builds in the fork checkout it is given and returns
/// the build directory, holding `<host>/llvm` and `<host>/lld`.
fn choose(root: &Path, rust_dir: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Llvm {
    let key = key(fork);
    let dir = store(rust_dir).join(&key);
    keystore::record(root, Keyed::Llvm, &key);
    let mut placed = false;
    let using = buildlock::keyed_made(
        root,
        Keyed::Llvm,
        &key,
        || defect(&dir),
        || {
            place(fork, &key, &dir, &build);
            placed = true;
        },
    );
    if placed {
        for gone in keystore::sweep(root, Keyed::Llvm, &store(rust_dir)) {
            eprintln!("Removed LLVM {}: no worktree names it", gone.display());
        }
    }
    Llvm { dir, _using: using }
}

/// Why `dir` is not a finished LLVM, if it is not.
fn defect(dir: &Path) -> Option<String> {
    if !dir.join(SOURCE).is_file() {
        return Some(format!("{} carries no {SOURCE}", dir.display()));
    }
    let kept = KEPT.iter().map(|k| dir.join(k)).filter(|p| !p.is_dir());
    let tools = TOOLS.iter().map(|t| dir.join(t)).filter(|p| !p.is_file());
    let gone: Vec<String> = kept.chain(tools).map(|p| p.display().to_string()).collect();
    (!gone.is_empty()).then(|| format!("{} carries no {}", dir.display(), gone.join(", ")))
}

/// Refuse to store what `fork`'s bootstrap builds while its `src/bootstrap`
/// holds changes no commit does: the key names the committed tree.
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
fn place(fork: &Path, key: &str, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
    refuse_uncommitted_bootstrap(fork);
    eprintln!("Building LLVM {key} in {}: nobody on this host has", fork.display());
    let built = build(fork);
    let host = host_triple();
    let partial = dir.with_extension("partial");
    if partial.exists() {
        fs::remove_dir_all(&partial).unwrap_or_else(|e| panic!("remove {}: {e}", partial.display()));
    }
    for part in KEPT {
        clone_tree(&built.join(&host).join("llvm").join(part), &partial.join(part));
    }
    let lld = built.join(&host).join("lld/bin/lld");
    fs::copy(&lld, partial.join("bin/lld"))
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", lld.display(), partial.join("bin/lld").display()));
    // What was built is what the key names, or it is not that key's.
    refuse_uncommitted_bootstrap(fork);
    let again = self::key(fork);
    assert!(
        again == key,
        "the fork's LLVM sources moved while LLVM {key} was being built (they now name {again}); \
         nothing was kept, and the next build makes the one they name"
    );
    let checkout = fork.join(LLVM);
    assert!(checkout.join(".git").exists(), "the LLVM build left no checkout at {}", checkout.display());
    let (built_from, commit) = (git_out(&checkout, &["rev-parse", "HEAD"]), llvm_commit(fork));
    assert!(
        built_from.trim() == commit,
        "{} is checked out at {}, and its gitlink names {commit}: bootstrap built the commit checked \
         out, which is not LLVM {key}'s; nothing was kept",
        checkout.display(),
        built_from.trim(),
    );
    fs::write(partial.join(SOURCE), format!("{key}\n"))
        .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCE).display()));
    read_only(&partial);
    keystore::retire(dir);
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
    fs::remove_dir_all(&built).unwrap_or_else(|e| panic!("remove {}: {e}", built.display()));
}

/// Take write permission from every file under `dir`.
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
}

/// Remove what the bootstrap build directory `build` holds of an LLVM of its
/// own: bootstrap's LLVM and LLD, and `download-ci-llvm`'s with its downloads.
/// The caller holds the lock covering `build`, whose compiler was just built
/// against the store.
pub fn retire_in_tree(build: &Path) {
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
    for dir in own {
        if dir.exists() {
            eprintln!("Removing {}: the compiler built here links the host's LLVM", dir.display());
        }
        keystore::retire(&dir);
    }
}

/// Bootstrap's build of LLVM, clang and LLD in `fork`, into its own build
/// directory, which it returns.
fn build_in_fork(fork: &Path) -> PathBuf {
    let host = host_triple();
    let build_dir = fork.join("build/toyos-llvm");
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host)).unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args = ["build", "--config", config, "src/llvm-project/llvm", "src/llvm-project/lld"];
    let (ok, _) = toolchain::x_build(fork, &args, "LLVM");
    assert!(ok, "the LLVM build in {} failed: its output above says why", fork.display());
    build_dir
}

/// Bootstrap's configuration for an LLVM: the options every compiler build's
/// `[llvm]` names, for the host alone.
fn config_text(build_dir: &Path, host: &str) -> String {
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]

[llvm]
{llvm}
"#,
        build_dir = build_dir.display(),
        llvm = crate::clang::LLVM_CONFIG,
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;

    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::compiler::tests::{estate, git, write, LLVM_B};

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
        let scratch = TempDir::new("llvm");
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
    /// link bootstrap makes to one of its files.
    #[test]
    fn a_placed_llvm_is_never_written() {
        let scratch = TempDir::new("llvm-read-only");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let dir = choose(&a, &rust_dir, &a.join("rust"), fake_build).dir;
        let stage = scratch.join("stage1-rust-lld");
        fs::hard_link(dir.join("bin/lld"), &stage).unwrap();
        for file in [dir.join("bin/lld"), stage, dir.join("lib/libLLVMCore.a"), dir.join(SOURCE)] {
            let written = fs::OpenOptions::new().write(true).open(&file);
            assert_eq!(
                written.map_err(|e| e.kind()).err(),
                Some(std::io::ErrorKind::PermissionDenied),
                "{} could be written",
                file.display()
            );
        }
    }

    /// **An LLVM that is not whole is made again, all of it**: one whose
    /// `SOURCE` says it finished and that lost a tool, or a directory it keeps,
    /// is replaced.
    #[test]
    fn an_llvm_that_is_not_whole_is_made_again() {
        let scratch = TempDir::new("llvm-whole");
        let (_primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let makes = Cell::new(0);
        let counted = |fork: &Path| {
            makes.set(makes.get() + 1);
            fake_build(fork)
        };
        let dir = choose(&a, &rust_dir, &a.join("rust"), counted).dir;
        for (lost, made) in [("bin/lld", 2), ("lib", 3)] {
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
    /// the C compilers, the committed LLVM gitlink and the committed
    /// `src/bootstrap` moves it; a compiler edit does not.
    #[test]
    fn the_key_moves_with_the_llvm_and_only_with_it() {
        let scratch = TempDir::new("llvm-key");
        let (_primary, rust_dir, [same, a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        let config = config_text(Path::new(KEYED_BUILD_DIR), &host_triple());
        let base = key(&fork);
        assert_eq!(key_of(&fork, RECIPE, &config, cc_identity()), base);
        let linux = config_text(Path::new(KEYED_BUILD_DIR), "x86_64-unknown-linux-gnu");
        for (what, other) in [
            ("the recipe", key_of(&fork, "another recipe", &config, cc_identity())),
            ("the [llvm]", key_of(&fork, RECIPE, &config.replace("X86", "RISCV;X86"), cc_identity())),
            ("the host", key_of(&fork, RECIPE, &linux, cc_identity())),
            ("the C compilers", key_of(&fork, RECIPE, &config, "CC /usr/bin/gcc\ngcc 14\n")),
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

    /// **The C compilers the key names are the host's**: each by the path it
    /// resolves to, and what its `--version` says.
    #[test]
    fn the_key_names_the_host_s_c_compilers() {
        let identity = cc_identity();
        for var in ["CC", "CXX"] {
            let line = identity.lines().find(|l| l.starts_with(&format!("{var} "))).unwrap_or_else(|| panic!("{identity}"));
            let path = Path::new(&line[var.len() + 1..]);
            assert!(path.is_absolute() && path.is_file(), "{var} names {}, no compiler", path.display());
        }
        assert!(identity.lines().filter(|l| !l.is_empty()).count() > 2, "no --version output: {identity}");
    }

    /// **A gitlink staged and not committed names no LLVM**: bootstrap checks
    /// out the index's, and the key would name HEAD's.
    #[test]
    fn a_staged_llvm_gitlink_is_refused() {
        let scratch = TempDir::new("llvm-staged");
        let (_primary, _rust_dir, [same, _a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        let said = refusal("a staged gitlink named an LLVM", || {
            key(&fork);
        });
        assert!(said.contains("stages") && said.contains(LLVM_B), "{said}");
    }

    /// **Only an LLVM built from what its key names is stored**: not from a
    /// `src/bootstrap` holding what no commit does, which is refused before
    /// anything is built; not from an LLVM checkout other than the gitlink's;
    /// not when the sources moved while it was being built.
    #[test]
    fn what_the_key_does_not_name_is_never_stored() {
        let scratch = TempDir::new("llvm-dirt");
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

        let lagging = |fork: &Path| {
            check_out_llvm(fork, "A");
            fake_build(fork)
        };
        pin_llvm(&fork, "B");
        let said = refusal("an LLVM checkout other than the gitlink's was stored", || {
            choose(&a, &rust_dir, &fork, lagging);
        });
        assert!(said.contains("is checked out at"), "{said}");
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
        let scratch = TempDir::new("llvm-made-elsewhere");
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
        let scratch = TempDir::new("llvm-sweep");
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

    /// **An LLVM a worktree resolved stays once nothing uses it**: its record,
    /// not its use, is what names it.
    #[test]
    fn a_resolved_llvm_is_named_by_its_worktree() {
        let scratch = TempDir::new("llvm-named");
        let (primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        elsewhere("use", &a, &rust_dir).release();
        let dir = store(&rust_dir).join(key(&a.join("rust")));
        assert_eq!(keystore::sweep(&primary, Keyed::Llvm, &store(&rust_dir)), Vec::<PathBuf>::new());
        assert_eq!(defect(&dir), None, "the sweep took an LLVM the worktree that resolved it names");
    }

    /// **A sweep stopped halfway leaves nothing that passes for an LLVM**: what
    /// it was removing is out of the way before anything in it goes, so the next
    /// build makes it again.
    #[test]
    fn a_stopped_sweep_leaves_no_llvm_that_passes_for_whole() {
        let scratch = TempDir::new("llvm-stopped-sweep");
        let (primary, rust_dir, [_same, a, _b]) = estate_built(&scratch);
        let makes = Cell::new(0);
        let counted = |fork: &Path| {
            makes.set(makes.get() + 1);
            fake_build(fork)
        };
        elsewhere("use", &a, &rust_dir).release();
        let dir = store(&rust_dir).join(key(&a.join("rust")));
        keystore::forget(&a, Keyed::Llvm);
        let stuck = |dir: &Path, mode: u32| {
            fs::set_permissions(dir.join("lib/clang/22/include"), fs::Permissions::from_mode(mode)).unwrap();
        };
        stuck(&dir, 0o555);
        let said = refusal("a sweep removed what cannot be removed", || {
            keystore::sweep(&primary, Keyed::Llvm, &store(&rust_dir));
        });
        let away = dir.with_extension("swept");
        if away.exists() {
            stuck(&away, 0o755);
        } else {
            stuck(&dir, 0o755);
        }
        assert!(said.contains("remove"), "{said}");
        assert!(!dir.exists(), "a stopped sweep left {} behind", dir.display());

        let again = choose(&a, &rust_dir, &a.join("rust"), counted);
        assert_eq!((makes.get(), defect(&again.dir)), (1, None));
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
