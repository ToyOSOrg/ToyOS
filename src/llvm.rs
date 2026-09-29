//! The LLVM every host compiler links, with its clang and LLD, content-addressed:
//! one per key on this host, shared by every compiler build that names it.
//!
//! **An LLVM is a function of its key** ([`key`]): the `src/llvm-project` commit
//! the fork checkout's gitlink names (`compiler::llvm_commit`, which refuses a
//! checkout holding what no commit does), `clang::LLVM_CONFIG`, [`RECIPE`] and
//! the host. `rust/build/llvm/<key>/` in the primary is bootstrap's install of
//! that LLVM and its clang, with its LLD in `bin/` beside `llvm-config`, made by
//! whichever build first needs it ([`resolve`]) and written by nothing after its
//! [`SOURCE`] file exists: bootstrap hard-links its tools into the stages it
//! assembles, so a stage's tool is never written in place. Every compiler build,
//! the primary's and a worktree's own, names it as the host's `llvm-config` with
//! `llvm-has-rust-patches`, so bootstrap builds no LLVM and takes LLD from beside
//! it as `rust-lld`; `clang::provision` copies its clang.
//!
//! Its lock (`buildlock::keyed_*` with [`Keyed::Llvm`]) is taken inside the
//! worktree or global lock that covers the fork build directory its maker
//! writes, `build/toyos-llvm/`, which is removed once the LLVM is placed.
//!
//! An LLVM no worktree names any more is removed by [`sweep`], which
//! `--worktree remove` and every placement run: each build that resolves one
//! records its key in its worktree's `target/`, and a key no registered worktree
//! records, that nobody is making or using, goes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Guard, Keyed};
use crate::sysroot::{clone_tree, git_out, short};
use crate::toolchain::{self, host_triple};

/// What changes how a key's commit becomes an LLVM and is none of the other
/// parts: the build below. Moving it moves every key.
const RECIPE: &str = "bootstrap build of src/llvm-project/llvm and src/llvm-project/lld, profile compiler, \
                      host only; the install's bin, include and lib, and lld in bin; 1";

/// What of bootstrap's install an LLVM keeps: `build/` beside them is CMake's
/// tree, which nothing reads once the install is made.
const KEPT: [&str; 3] = ["bin", "include", "lib"];

/// What a compiler build and `clang::provision` read of an LLVM.
const TOOLS: [&str; 4] = ["bin/llvm-config", "bin/lld", "bin/clang", "bin/llvm-ar"];

/// The file a finished LLVM carries last, naming its key. A directory without
/// it is a build that did not finish.
const SOURCE: &str = "SOURCE";

/// Where each build records the key of the LLVM it resolved, for [`sweep`].
const RECORD: &str = "target/toyos-llvm-key";

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
    key_of(&crate::compiler::llvm_commit(fork), crate::clang::LLVM_CONFIG, &host_triple())
}

fn key_of(commit: &str, config: &str, host: &str) -> String {
    short([RECIPE, config, host, commit].join("\n\0\n").as_bytes())
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
    let recorded = root.join(RECORD);
    fs::create_dir_all(recorded.parent().expect("a file under target/")).ok();
    fs::write(&recorded, &key).unwrap_or_else(|e| panic!("write {}: {e}", recorded.display()));
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
        for gone in sweep(root, rust_dir) {
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
    let gone: Vec<String> =
        TOOLS.iter().map(|t| dir.join(t)).filter(|p| !p.is_file()).map(|p| p.display().to_string()).collect();
    (!gone.is_empty()).then(|| format!("{} carries no {}", dir.display(), gone.join(", ")))
}

/// Build the LLVM `key` names from `fork` and put it at `dir`. The caller holds
/// the key's lock.
fn place(fork: &Path, key: &str, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
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
    // The commit the key named is the one built, or this is not that key's.
    let again = self::key(fork);
    assert!(
        again == key,
        "the fork's LLVM commit moved while LLVM {key} was being built (it now names {again}); \
         nothing was kept, and the next build makes the one it names"
    );
    fs::write(partial.join(SOURCE), format!("{key}\n"))
        .unwrap_or_else(|e| panic!("write {}: {e}", partial.join(SOURCE).display()));
    if dir.exists() {
        fs::remove_dir_all(dir).unwrap_or_else(|e| panic!("remove {}: {e}", dir.display()));
    }
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
    fs::remove_dir_all(&built).unwrap_or_else(|e| panic!("remove {}: {e}", built.display()));
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

/// The key of the LLVM `root`'s last build resolved, if it resolved one.
fn recorded_key(root: &Path) -> Option<String> {
    fs::read_to_string(root.join(RECORD)).ok().map(|k| k.trim().to_string())
}

/// Remove every LLVM no registered worktree records and nobody is making or
/// using, and every half-built one nobody is making. Returns what went.
pub fn sweep(root: &Path, rust_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(store(rust_dir)) else { return Vec::new() };
    let named: std::collections::BTreeSet<String> = git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter_map(|w| recorded_key(Path::new(w)))
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
        let Some(_idle) = buildlock::keyed_idle(root, Keyed::Llvm, &key) else { continue };
        let path = entry.path();
        fs::remove_dir_all(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        removed.push(path);
    }
    removed
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use toyos_tmpdir::TempDir;

    use super::*;
    use crate::compiler::tests::{estate, git, write, LLVM_A, LLVM_B};
    use crate::compiler::LLVM;

    /// Bootstrap's stand-in: what its LLVM and LLD builds leave in the build
    /// directory, CMake's tree among them.
    fn fake_build(fork: &Path) -> PathBuf {
        let built = fork.join("build/toyos-llvm");
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

    /// **One LLVM per key, made once and found again**: two worktrees whose
    /// compilers differ and whose LLVM commit is one share one LLVM, and the
    /// second build makes nothing and writes nothing; what is kept is the
    /// install and its LLD, never CMake's tree, and the build directory goes.
    #[test]
    fn two_compilers_of_one_llvm_make_it_once() {
        let scratch = TempDir::new("llvm");
        let (primary, rust_dir, [same, a, b]) = estate(&scratch);
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
        assert!(host_lines(&la.dir).contains(&format!("{}/bin/llvm-config", la.dir.display())));

        let before = snapshot(&store(&rust_dir));
        let lb = choose(&b, &rust_dir, &b.join("rust"), once);
        let primary_s = choose(&primary, &rust_dir, &rust_dir, once);
        let same_s = choose(&same, &rust_dir, &same.join("rust"), once);
        assert_eq!(makes.get(), 1);
        assert!(lb.dir == la.dir && primary_s.dir == la.dir && same_s.dir == la.dir, "one LLVM commit named two LLVMs");
        assert_eq!(snapshot(&store(&rust_dir)), before, "an LLVM was written after it was whole");
    }

    /// **An LLVM that is not whole is made again, all of it**: one whose
    /// `SOURCE` says it finished and that lost a tool is replaced.
    #[test]
    fn an_llvm_that_is_not_whole_is_made_again() {
        let scratch = TempDir::new("llvm-whole");
        let (_primary, rust_dir, [_same, a, _b]) = estate(&scratch);
        let makes = Cell::new(0);
        let counted = |fork: &Path| {
            makes.set(makes.get() + 1);
            fake_build(fork)
        };
        let dir = choose(&a, &rust_dir, &a.join("rust"), counted).dir;
        fs::remove_file(dir.join("bin/lld")).unwrap();
        assert!(defect(&dir).is_some_and(|d| d.contains("bin/lld")), "{:?}", defect(&dir));
        let again = choose(&a, &rust_dir, &a.join("rust"), counted);
        assert_eq!((makes.get(), defect(&again.dir)), (2, None));
    }

    /// **The key is the LLVM and nothing else**: the same inputs give the same
    /// key, each of the commit, the config and the host moves it, and so does
    /// the recipe through its constant; a compiler edit or a gitlink staged and
    /// not committed does not, and a committed gitlink does.
    #[test]
    fn the_key_moves_with_the_llvm_and_only_with_it() {
        let base = key_of(LLVM_A, crate::clang::LLVM_CONFIG, "aarch64-apple-darwin");
        assert_eq!(key_of(LLVM_A, crate::clang::LLVM_CONFIG, "aarch64-apple-darwin"), base);
        for (what, other) in [
            ("the commit", key_of(LLVM_B, crate::clang::LLVM_CONFIG, "aarch64-apple-darwin")),
            ("the config", key_of(LLVM_A, &crate::clang::LLVM_CONFIG.replace("X86", "RISCV;X86"), "aarch64-apple-darwin")),
            ("the host", key_of(LLVM_A, crate::clang::LLVM_CONFIG, "x86_64-unknown-linux-gnu")),
        ] {
            assert_ne!(other, base, "{what} did not move the key");
        }

        let scratch = TempDir::new("llvm-key");
        let (_primary, rust_dir, [same, a, _b]) = estate(&scratch);
        let fork = same.join("rust");
        assert_eq!(key(&fork), key(&rust_dir), "one LLVM commit named two keys");
        assert_eq!(key(&fork), key(&a.join("rust")), "a compiler/ edit moved the LLVM key");
        write(&fork.join("compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        let before = key(&fork);
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        assert_eq!(key(&fork), before, "a gitlink staged and not committed moved the key; bootstrap builds HEAD's");
        git(&fork, &["commit", "-qm", "another LLVM"]);
        assert_ne!(key(&fork), before, "another LLVM commit kept the key");
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
        let (primary, rust_dir, [_same, _a, b]) = estate(&scratch);
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
        let (primary, rust_dir, [_same, a, b]) = estate(&scratch);
        let user = elsewhere("use", &a, &rust_dir);
        let first = store(&rust_dir).join(key(&a.join("rust")));

        let fork = a.join("rust");
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        git(&fork, &["commit", "-qm", "another LLVM"]);
        let second = choose(&a, &rust_dir, &fork, fake_build);
        assert_ne!(second.dir, first);
        assert!(first.is_dir(), "placing an LLVM swept one still in use");

        fs::create_dir_all(b.join("target")).unwrap();
        fs::write(b.join(RECORD), key(&b.join("rust"))).unwrap();
        user.release();
        assert_eq!(sweep(&primary, &rust_dir), Vec::<PathBuf>::new(), "the sweep took an LLVM a worktree names");
        fs::write(b.join(RECORD), key(&fork)).unwrap();
        assert_eq!(sweep(&primary, &rust_dir), [first], "the sweep kept an LLVM nobody names, or took a named one");
        assert!(second.dir.is_dir());
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
