//! The compiler a worktree's sysroot is cloned from and compiled by: the
//! primary's, or one of its own, content-addressed.
//!
//! **Every worktree builds with the compiler its own fork checkout names.** The
//! primary's `stage2` is built from what the primary's `rust/compiler/` holds,
//! and [`record`] writes which that is. A linked worktree whose fork checkout
//! holds the same `compiler/` ([`source`]) compiles with that one. One whose
//! `compiler/` differs — a new target spec, a codegen change — gets its own:
//! built by bootstrap in its own fork checkout, under that checkout's
//! `build/toyos-compiler/`, and placed at `rust/build/compilers/<key>/`, where
//! the key ([`key`]) is the identity (`src/identity.rs`) of the checkout's
//! `compiler/`, `src/bootstrap/`, `src/tools/`, `src/stage0` and `Cargo.lock`,
//! the key of the LLVM it links, and [`RECIPE`]. Nothing writes that directory after its [`SOURCE`] file exists,
//! and two worktrees naming the same compiler share one copy.
//!
//! **LLVM is the host's, built from `src/llvm-project`** (`src/llvm.rs`), and
//! linked through its `llvm-config`. [`source`] names that LLVM's key, so
//! another LLVM is another compiler, the primary's among them, and an LLVM
//! checkout or a `src/bootstrap` holding what no commit does names none. A
//! worktree records the LLVM its compiler links for as long as it builds with
//! that compiler.
//!
//! **A compiler of a worktree's own never touches what the others build with**:
//! not the primary's `stage2`, not its record, not the machine-global rustup
//! `toyos` link — a sysroot is named by its directory, never by a toolchain
//! name, so no link is made. The global lock is not taken either; nothing of
//! the primary's is read but the LLVM store, under its key's own lock.
//!
//! Locks, in the one order every acquirer takes them: the key's
//! (`buildlock::keyed_*` with [`Keyed::Compiler`]), with this worktree's build
//! lock put down, held shared for as long as a sysroot is being made from it;
//! then, to build, this worktree's exclusively, because its fork build
//! directory is written; then the LLVM key's, held shared while it is linked.
//!
//! A compiler no worktree names any more is removed by `keystore::sweep`, which
//! `--worktree remove` and every placement run: each build records the key it used in its
//! worktree's `target/`, and a key no registered worktree records, that nobody
//! is making or using, goes.
//!
//! What such a worktree's image cannot carry is the ToyOS-hosted rustc: that is
//! the primary's, built from the primary's `compiler/`, so `hosted-rustc` in a
//! worktree building with its own compiler is refused by name
//! (`src/build.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::buildlock::{self, Guard, Held, Keyed};
use crate::keystore;
use crate::sysroot::{clone_tree, git_bytes, git_out, short, tree_identity};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become a compiler and is none of them: the
/// build below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage 2 of compiler/rustc and library, profile compiler, host only, with rust-lld, host linker pinned, LLVM, clang and LLD from the host's LLVM; 5";

/// What a compiler's key is the identity of, in its fork checkout.
const KEYED: [&str; 5] = ["compiler", "src/bootstrap", "src/tools", "src/stage0", "Cargo.lock"];

/// The submodule a compiler is built against by commit: its LLVM, which
/// bootstrap builds from that commit, so its content is never read.
pub(crate) const LLVM: &str = "src/llvm-project";

/// Where a fork checkout builds a compiler of its own.
const BUILD_DIR: &str = "build/toyos-compiler";

/// The file a finished compiler carries last, naming what it was built from. A
/// directory without it is a build that did not finish.
const SOURCE: &str = "SOURCE";

/// A compiler, held in use for as long as this lives.
pub struct Compiler {
    /// Its toolchain directory: `bin/rustc`, `lib/`.
    pub stage2: PathBuf,
    /// The file naming what it was built from.
    record: PathBuf,
    /// Whether it is the primary's, the one the hosted rustc and the rustup
    /// link are built from.
    pub primary: bool,
    _using: Option<Guard>,
}

impl Compiler {
    /// The primary's `stage2`.
    pub fn primary(rust_dir: &Path) -> Self {
        Self { stage2: toolchain::stage2(rust_dir), record: primary_record(rust_dir), primary: true, _using: None }
    }

    /// The compiler as a sysroot's key sees it: the source it was built from,
    /// and the driver that build left, so a rebuild of the same source is a new
    /// compiler too.
    pub fn identity(&self) -> String {
        let source = fs::read_to_string(&self.record).unwrap_or_else(|_| {
            panic!(
                "{} is missing, so no sysroot can say which compiler it was built with.\n\
                 The primary checkout writes the primary's: run `cargo run -- --build-only` there once.",
                self.record.display(),
            )
        });
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
        format!("{} {} {} {mtime}", source.trim(), driver.file_name().to_string_lossy(), meta.len())
    }
}

/// The primary's record of which `compiler/` its `stage2` was built from.
pub(crate) fn primary_record(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-compiler")
}

/// Every compiler of a worktree's own on this host.
pub fn compilers_dir(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/compilers")
}

/// [`compiler_source`] and the key of the LLVM it links.
pub fn source(checkout: &Path) -> String {
    format!("{} llvm {}", compiler_source(checkout), crate::llvm::key(checkout))
}

/// What `checkout`'s `compiler/` is: its commit's tree, and whatever the working
/// tree changes in it — an edit, or a file git does not track yet, which is
/// what a new target spec is before its commit.
fn compiler_source(checkout: &Path) -> String {
    let tree = git_out(checkout, &["rev-parse", "HEAD:compiler"]);
    let mut local = git_bytes(checkout, &["diff", "HEAD", "--", "compiler"]);
    let untracked = git_bytes(checkout, &["ls-files", "-z", "--others", "--exclude-standard", "--", "compiler"]);
    for name in untracked.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let path = checkout.join(String::from_utf8_lossy(name).as_ref());
        local.extend_from_slice(name);
        local.push(0);
        local.extend(fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())));
        local.push(0);
    }
    if local.is_empty() {
        tree.trim().to_string()
    } else {
        format!("{} with local changes {}", tree.trim(), short(&local))
    }
}

/// Record which compiler the primary's `stage2` is. The primary calls this
/// after a toolchain build.
pub fn record(rust_dir: &Path) {
    let at = primary_record(rust_dir);
    let want = source(rust_dir);
    if fs::read_to_string(&at).ok().as_deref() != Some(want.as_str()) {
        fs::write(&at, &want).unwrap_or_else(|e| panic!("write {}: {e}", at.display()));
    }
}

/// Remove the record of which compiler the primary's `stage2` is. The primary
/// calls this before a toolchain build.
pub fn forget(rust_dir: &Path) {
    let at = primary_record(rust_dir);
    match fs::remove_file(&at) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove {}: {e}", at.display()),
        _ => {}
    }
}

/// Whether the primary's `stage2` is the compiler `checkout`'s `compiler/`
/// names — `Err` when nothing records which compiler that is.
///
/// **The source's content, never its files' times**: a checkout that rewrites a
/// file with the bytes it had is no new compiler.
fn primary_is(rust_dir: &Path, checkout: &Path) -> Result<bool, std::io::Error> {
    let names = source(checkout);
    Ok(fs::read_to_string(primary_record(rust_dir))?.trim() == names)
}

/// Whether the primary's `stage2` is built from what its own `rust/compiler/`
/// holds: false until a bootstrap has finished and [`record`]ed it.
pub fn primary_is_current(rust_dir: &Path) -> bool {
    match primary_is(rust_dir, rust_dir) {
        Ok(current) => current,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("read {}: {e}", primary_record(rust_dir).display()),
    }
}

/// The key of the compiler `fork`'s sources name: their content, so committing
/// what was built as local changes names the same compiler.
pub fn key(fork: &Path) -> String {
    let parts = [RECIPE.to_string(), tree_identity(fork, &KEYED), crate::llvm::key(fork)];
    short(parts.join("\n\0\n").as_bytes())
}

/// The LLVM commit `fork` builds against: the one its `HEAD` records, refused
/// when its index stages another, because bootstrap checks out the index's. An
/// LLVM change is a commit there and a gitlink here, so a checkout holding
/// anything no commit does is refused rather than named by its commit.
pub(crate) fn llvm_commit(fork: &Path) -> String {
    let checkout = fork.join(LLVM);
    // Exactly what bootstrap's LLVM stamp hashes beyond the commit; the untracked
    // cache spares each call a walk of the whole tree.
    let status = ["-c", "core.untrackedCache=true", "status", "--porcelain", "--untracked-files=normal"];
    let edited = checkout.join(".git").exists() && !git_bytes(&checkout, &status).is_empty();
    assert!(
        !edited,
        "{} holds changes no commit does, and a compiler is keyed on the commit its gitlink \
         names: commit them there and record that commit in {}",
        checkout.display(),
        fork.display(),
    );
    let recorded = git_out(fork, &["ls-tree", "HEAD", LLVM]);
    let committed = match recorded.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["160000", "commit", sha, _] => sha.to_string(),
        _ => panic!("{} records no {LLVM} gitlink: `git ls-tree HEAD {LLVM}` said {recorded:?}", fork.display()),
    };
    let indexed = git_out(fork, &["ls-files", "--stage", LLVM]);
    let staged = match indexed.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["160000", sha, "0", _] => sha.to_string(),
        _ => panic!("{} indexes no {LLVM} gitlink: `git ls-files --stage {LLVM}` said {indexed:?}", fork.display()),
    };
    assert!(
        staged == committed,
        "{} stages {LLVM} at {staged}, and its HEAD records {committed}: bootstrap builds the one \
         staged, and nothing is keyed on what no commit holds; commit the gitlink, or unstage it",
        fork.display(),
    );
    committed
}

/// The compiler `root`'s fork checkout at `fork` names: the primary's where its
/// `compiler/` is the one the primary's was built from, and otherwise its own,
/// built if nobody has built it, held in use for as long as the returned value
/// lives.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Path, lock: &mut Held) -> Compiler {
    lock.without_shared(|| choose(root, rust_dir, fork, |fork| build_in_fork(root, rust_dir, fork)))
}

/// [`resolve`] with the build that makes a compiler's `stage2` passed in, so a
/// test can stand in for bootstrap: `build` compiles the fork checkout it is
/// given and returns the `stage2` it left there.
fn choose(root: &Path, rust_dir: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Compiler {
    if fork == rust_dir {
        return Compiler::primary(rust_dir);
    }
    let names_primary = primary_is(rust_dir, fork).unwrap_or_else(|e| {
        panic!(
            "{} cannot be read ({e}), so nothing says which compiler the primary's stage2 is, \
             and no worktree can know whether it names that one.\n\
             The primary checkout writes it: run `cargo run -- --build-only` there once.",
            primary_record(rust_dir).display(),
        )
    });
    // The LLVM record follows the compiler's: a compiler links the LLVM its key
    // names, and the primary's is recorded by the primary.
    if names_primary {
        keystore::forget(root, Keyed::Compiler);
        keystore::forget(root, Keyed::Llvm);
        let build = fork.join(BUILD_DIR);
        if !crate::llvm::in_tree(&build).is_empty() {
            let _worktree = buildlock::worktree_exclusive(root, "removing the LLVM its compiler build built");
            crate::llvm::retire_in_tree(&build);
        }
        return Compiler::primary(rust_dir);
    }
    let key = key(fork);
    let dir = compilers_dir(rust_dir).join(&key);
    keystore::record(root, Keyed::Llvm, &crate::llvm::key(fork));
    let using = keystore::made(
        root,
        Keyed::Compiler,
        &compilers_dir(rust_dir),
        &key,
        || (!dir.join(SOURCE).is_file()).then(|| format!("{} carries no {SOURCE}", dir.display())),
        || place(root, fork, &key, &dir, &build),
    );
    Compiler { stage2: dir.join("stage2"), record: dir.join(SOURCE), primary: false, _using: Some(using) }
}

/// Build the compiler `key` names from `fork` and put it at `dir`. The caller
/// holds the key's lock.
fn place(root: &Path, fork: &Path, key: &str, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
    let what = format!("building compiler {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
    eprintln!("Building compiler {key} in {}: its compiler/ is not the one the primary's was built from", fork.display());
    let stage2 = build(fork);
    crate::llvm::retire_in_tree(&fork.join(BUILD_DIR));
    let partial = dir.with_extension("partial");
    if partial.exists() {
        fs::remove_dir_all(&partial).unwrap_or_else(|e| panic!("remove {}: {e}", partial.display()));
    }
    clone_tree(&stage2, &partial.join("stage2"));
    // The sources the key named are the ones built, or this is not that key's.
    let again = self::key(fork);
    assert!(
        again == key,
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
fn build_in_fork(root: &Path, rust_dir: &Path, fork: &Path) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let llvm = crate::llvm::resolve(root, rust_dir, fork);
    let host = host_triple();
    let build_dir = fork.join(BUILD_DIR);
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host, &llvm.dir)).unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args = ["build", "--stage", "2", "--config", config, "--warnings", "warn", "compiler/rustc", "library"];
    let (ok, log) = toolchain::x_build(fork, &args, "the compiler");
    toolchain::refuse_on_compile_error(&log, "the compiler");
    assert!(ok, "the compiler build in {} failed, and nothing in its output was a compile error", fork.display());
    let stage2 = build_dir.join(&host).join("stage2");
    assert!(stage2.join("bin/rustc").is_file(), "the compiler build left no {}", stage2.join("bin/rustc").display());
    toolchain::provision_toolchain_cargo(&stage2);
    crate::clang::provision(&stage2, &llvm.dir);
    toolchain::assert_toolchain_is_honest(&stage2);
    stage2
}

/// Bootstrap's configuration for a compiler of a worktree's own: the primary's
/// `profile` and options, for the host alone, since every guest target's
/// libraries are the sysroot's to build, linking the LLVM at `llvm`.
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

[target.{host}]
{pin}
{external}
"#,
        build_dir = build_dir.display(),
        llvm = crate::clang::LLVM_CONFIG,
        pin = toolchain::HOST_LINKER_PIN,
        external = crate::llvm::host_lines(llvm),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use toyos_tmpdir::TempDir;
    use std::process::Command;

    use super::*;

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
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

    pub(crate) fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Every file under `dir` with its bytes, for "nothing here changed".
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in fs::read_dir(&at).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// The LLVM commits the fixtures' forks record. Nothing reads their content.
    pub(crate) const LLVM_A: &str = "1111111111111111111111111111111111111111";
    pub(crate) const LLVM_B: &str = "2222222222222222222222222222222222222222";

    /// A primary whose `rust` pins fork commit `C0` and has built a compiler
    /// from it, and three linked worktrees: `same` pins `C0`, `a` and `b` each
    /// pin a commit whose `compiler/` is its own.
    pub(crate) fn estate(scratch: &Path) -> (PathBuf, PathBuf, [PathBuf; 3]) {
        let base = fs::canonicalize(scratch).unwrap();

        let fork = base.join("fork-src");
        fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() {}\n");
        write(&fork.join("src/bootstrap/src/lib.rs"), "fn main() {}\n");
        write(&fork.join("src/stage0"), "compiler_version=beta\n");
        write(&fork.join("Cargo.lock"), "# lock\n");
        write(&fork.join("library/std/src/lib.rs"), "pub fn a() {}\n");
        write(&fork.join(".gitignore"), "/build\n");
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
        write(&toolchain::stage2(&rust_dir).join("bin/rustc"), "the primary's rustc");
        write(&toolchain::stage2(&rust_dir).join("lib/librustc_driver-0.dylib"), "the primary's driver");
        record(&rust_dir);

        let mut linked = Vec::new();
        for (name, pin) in [("same", c0.as_str()), ("a", pins[0].as_str()), ("b", pins[1].as_str())] {
            let wt = base.join(name);
            git(&primary, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
            let _ = fs::remove_dir(wt.join("rust"));
            git(&rust_dir, &["worktree", "add", "-q", "--detach", wt.join("rust").to_str().unwrap(), pin]);
            linked.push(wt);
        }
        (primary, rust_dir, linked.try_into().unwrap())
    }

    /// **Two worktrees with different compilers build side by side, and the
    /// primary's toolchain is untouched by either.** Each gets its own compiler
    /// at its own key, built once and found again; one that names the primary's
    /// `compiler/` builds nothing and gets the primary's; the primary's `stage2`,
    /// its record and the rustup `toyos` link are byte-for-byte what they were;
    /// a sweep takes a compiler only once no worktree names it.
    #[test]
    fn worktrees_with_different_compilers_coexist_and_the_primary_s_is_untouched() {
        let scratch = TempDir::new("compiler");
        let (primary, rust_dir, [same, a, b]) = estate(&scratch);
        let before = snapshot(&rust_dir.join("build"));
        let link = toolchain::rustup_link();
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            fake_build(fork)
        };

        let mine = choose(&same, &rust_dir, &same.join("rust"), fake);
        assert!(mine.primary && mine.stage2 == toolchain::stage2(&rust_dir));
        assert_eq!(builds.get(), 0, "a worktree naming the primary's compiler built one");

        let ca = choose(&a, &rust_dir, &a.join("rust"), fake);
        let cb = choose(&b, &rust_dir, &b.join("rust"), fake);
        assert_eq!(builds.get(), 2);
        assert!(!ca.primary && !cb.primary);
        assert_ne!(ca.stage2, cb.stage2, "two compilers were given one directory");
        assert!(ca.stage2.starts_with(compilers_dir(&rust_dir)) && cb.stage2.starts_with(compilers_dir(&rust_dir)));
        assert!(fs::read_to_string(ca.stage2.join("bin/rustc")).unwrap().contains("aarch64"));
        assert!(fs::read_to_string(cb.stage2.join("bin/rustc")).unwrap().contains("riscv"));
        assert_ne!(ca.identity(), cb.identity());
        assert_ne!(ca.identity(), Compiler::primary(&rust_dir).identity());

        // Found again, not rebuilt; and still both there.
        let again = choose(&a, &rust_dir, &a.join("rust"), fake);
        assert_eq!((again.stage2, builds.get()), (ca.stage2.clone(), 2));
        assert!(ca.stage2.join("bin/rustc").is_file() && cb.stage2.join("bin/rustc").is_file());

        // An uncommitted file in `compiler/` is a new compiler too, and
        // committing it is not another one.
        let pinned = git(&a.join("rust"), &["rev-parse", "HEAD"]);
        write(&a.join("rust/compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        let ca2 = choose(&a, &rust_dir, &a.join("rust"), fake);
        assert_ne!(ca2.stage2, ca.stage2, "an untracked target spec kept the old compiler");
        git(&a.join("rust"), &["add", "-A"]);
        git(&a.join("rust"), &["commit", "-qm", "the target, committed"]);
        let committed = choose(&a, &rust_dir, &a.join("rust"), fake);
        assert_eq!((committed.stage2, builds.get()), (ca2.stage2.clone(), 3), "a commit rebuilt the compiler");
        git(&a.join("rust"), &["checkout", "-q", &pinned]);

        // The primary's own: nothing under its `build/` but `compilers/` moved.
        let after: Vec<_> = snapshot(&rust_dir.join("build"))
            .into_iter()
            .filter(|(p, _)| !p.starts_with(compilers_dir(&rust_dir)))
            .collect();
        assert_eq!(after, before, "the primary's stage2 or its record was written");
        assert_eq!(git(&rust_dir, &["rev-parse", "HEAD"]), git(&primary, &["rev-parse", "HEAD:rust"]));
        assert_eq!(toolchain::rustup_link(), link, "the machine-global toyos link moved");

        // A sweep takes the compiler nobody names, and only that one — and
        // not while it is still in use, though nobody names it any more.
        let spec = a.join("rust/compiler/rustc_target/src/orphan.rs");
        write(&spec, "pub fn o() {}\n");
        let orphan = compilers_dir(&rust_dir).join(key(&a.join("rust")));
        let user = chosen_elsewhere(&a, &rust_dir);
        fs::remove_file(&spec).unwrap();
        let kept = choose(&a, &rust_dir, &a.join("rust"), fake);
        let sweep = |root: &Path, rust_dir: &Path| keystore::sweep(root, Keyed::Compiler, &compilers_dir(rust_dir));
        assert_eq!(sweep(&primary, &rust_dir), Vec::<PathBuf>::new(), "the sweep took a compiler still in use");
        assert!(orphan.is_dir() && ca2.stage2.is_dir());
        user.release();
        drop(cb);
        assert_eq!(sweep(&primary, &rust_dir), [orphan], "the sweep took a compiler a worktree names, or left one nobody does");
        assert!(kept.stage2.is_dir() && ca2.stage2.is_dir());

        // A placement sweeps too: the compiler an edit replaces goes once nobody
        // uses it, and the one another worktree names stays.
        let spec = a.join("rust/compiler/rustc_target/src/another.rs");
        write(&spec, "pub fn u() {}\n");
        let replaced = compilers_dir(&rust_dir).join(key(&a.join("rust")));
        chosen_elsewhere(&a, &rust_dir).release();
        let named = choose(&b, &rust_dir, &b.join("rust"), fake).stage2;
        write(&spec, "pub fn v() {}\n");
        let ca3 = choose(&a, &rust_dir, &a.join("rust"), fake);
        assert!(!replaced.exists(), "placing a compiler left the one it replaced, which nobody names");
        assert!(ca3.stage2.is_dir() && named.is_dir());
    }

    /// **A worktree names the LLVM of the compiler it builds with, and only
    /// that one**: one it placed or found placed, never one it has gone back to
    /// the primary's from; and its build directory keeps no LLVM of its own
    /// either way.
    #[test]
    fn the_llvm_record_follows_the_compiler_in_use() {
        let scratch = TempDir::new("compiler-llvm-record");
        let (primary, rust_dir, [_same, a, _b]) = estate(&scratch);
        let fork = a.join("rust");
        let store = crate::llvm::store(&rust_dir);
        let llvm = store.join(crate::llvm::key(&fork));
        let sweep = || keystore::sweep(&primary, Keyed::Llvm, &store);

        let own = fork.join(BUILD_DIR).join(host_triple()).join("llvm");
        write(&own.join("bin/llvm-config"), "the build directory's own");
        drop(choose(&a, &rust_dir, &fork, fake_build));
        assert!(!own.exists(), "a compiler built against the store left the LLVM its build directory built");
        fs::create_dir_all(&llvm).unwrap();
        assert_eq!(sweep(), Vec::<PathBuf>::new(), "the LLVM of a worktree's own compiler was swept");

        keystore::forget(&a, Keyed::Llvm);
        let never = |_: &Path| -> PathBuf { panic!("a placed compiler was built again") };
        drop(choose(&a, &rust_dir, &fork, never));
        assert_eq!(keystore::recorded(&a, Keyed::Llvm), Some(crate::llvm::key(&fork)), "a placed compiler's LLVM went unrecorded");

        git(&fork, &["checkout", "-q", &git(&rust_dir, &["rev-parse", "HEAD"])]);
        write(&own.join("bin/llvm-config"), "the build directory's own, from before the store");
        assert!(choose(&a, &rust_dir, &fork, never).primary);
        assert_eq!(sweep(), [llvm], "the LLVM of a compiler the worktree no longer builds with stayed");
        assert!(!own.exists(), "a worktree back on the primary's compiler kept the LLVM its build directory built");
    }

    const WORKTREE: &str = "TOYOS_COMPILER_TEST_WORKTREE";
    const RUST_DIR: &str = "TOYOS_COMPILER_TEST_RUST_DIR";

    /// The competing process for the test above: the compiler the worktree in
    /// [`WORKTREE`] names, chosen and held in use until released.
    #[test]
    #[ignore = "the competing process for the test above; never runs on its own"]
    fn child_role() {
        let worktree = std::env::var(WORKTREE).unwrap_or_else(|_| panic!("child_role ran without {WORKTREE}; it is not a test"));
        let worktree = PathBuf::from(worktree);
        let rust_dir = PathBuf::from(std::env::var(RUST_DIR).unwrap());
        let chosen = choose(&worktree, &rust_dir, &worktree.join("rust"), fake_build);
        assert!(!chosen.primary, "the holder was given the primary's compiler, which holds no key");
        buildlock::tests::hold_until_released();
    }

    /// The compiler `worktree` names, made if nobody has and held in use by a
    /// process of its own.
    fn chosen_elsewhere(worktree: &Path, rust_dir: &Path) -> buildlock::tests::Elsewhere {
        let env = [(WORKTREE, worktree.as_os_str()), (RUST_DIR, rust_dir.as_os_str())];
        buildlock::tests::Elsewhere::hold("compiler::tests::child_role", &env)
    }

    /// Bootstrap's stand-in: a `stage2` that says which target spec it knows.
    fn fake_build(fork: &Path) -> PathBuf {
        let stage2 = fork.join("build/toyos-compiler/stage2");
        let spec = fs::read_to_string(fork.join("compiler/rustc_target/src/lib.rs")).unwrap();
        write(&stage2.join("bin/rustc"), &format!("a rustc knowing {spec}"));
        write(&stage2.join("lib/librustc_driver-1.dylib"), &spec);
        stage2
    }

    /// **A primary with no record of its compiler is refused by name**, never
    /// read as "no compiler": that reading made every worktree build its own.
    #[test]
    fn a_missing_primary_record_is_refused_and_builds_nothing() {
        let scratch = TempDir::new("compiler-record");
        let (_primary, rust_dir, [same, _, _]) = estate(&scratch);
        fs::remove_file(primary_record(&rust_dir)).unwrap();
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            fork.join("build/toyos-compiler/stage2")
        };
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            choose(&same, &rust_dir, &same.join("rust"), fake);
        }));
        let why = refused.expect_err("a worktree resolved a compiler with no primary record");
        let why = why.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(why.contains("toyos-compiler cannot be read"), "{why}");
        assert_eq!(builds.get(), 0, "a missing record built a compiler");
    }

    /// **The primary bootstraps when its `compiler/` holds other content, and
    /// never because its files' times moved**: every file rewritten with its own
    /// bytes is the compiler just recorded.
    #[test]
    fn only_the_compiler_s_content_makes_the_primary_bootstrap() {
        let scratch = TempDir::new("compiler-current");
        let (_primary, rust_dir, _) = estate(&scratch);
        assert!(primary_is_current(&rust_dir), "the compiler just recorded is not current");

        let files = snapshot(&rust_dir.join("compiler"));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        for (file, bytes) in &files {
            fs::write(file, bytes).unwrap();
            fs::File::options().write(true).open(file).unwrap().set_modified(later).unwrap();
            assert_eq!(fs::metadata(file).unwrap().modified().unwrap(), later);
        }
        assert!(!files.is_empty());
        assert!(
            primary_is_current(&rust_dir),
            "every file of compiler/ was rewritten with its own bytes, and the primary would bootstrap"
        );

        let spec = rust_dir.join("compiler/rustc_target/src/lib.rs");
        let held = fs::read(&spec).unwrap();
        write(&spec, "pub fn targets() { riscv() }\n");
        assert!(!primary_is_current(&rust_dir), "a change to compiler/ kept the old stage2");
        fs::write(&spec, held).unwrap();
        assert!(primary_is_current(&rust_dir), "compiler/ put back is not the compiler recorded");

        write(&rust_dir.join("compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        assert!(!primary_is_current(&rust_dir), "an untracked file in compiler/ kept the old stage2");
        fs::remove_file(rust_dir.join("compiler/rustc_target/src/new_target.rs")).unwrap();

        fs::remove_file(primary_record(&rust_dir)).unwrap();
        assert!(!primary_is_current(&rust_dir), "a stage2 nothing recorded was taken for current");
    }

    /// Write the primary's record as it was written before its compiler linked
    /// the host's LLVM: its `compiler/` and its LLVM commit.
    pub(crate) fn record_before_the_store(rust_dir: &Path) {
        fs::write(primary_record(rust_dir), format!("{} llvm {}", compiler_source(rust_dir), llvm_commit(rust_dir))).unwrap();
    }

    /// `fork`'s LLVM checked out at a commit of its own.
    pub(crate) fn llvm_checkout(fork: &Path) -> PathBuf {
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
    /// not track, names no compiler of a worktree's.
    #[test]
    fn an_uncommitted_llvm_edit_is_refused() {
        let scratch = TempDir::new("compiler-llvm-edit");
        let (_primary, rust_dir, [same, _, _]) = estate(&scratch);
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
            choose(&same, &rust_dir, &fork, fake);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        git(&llvm, &["commit", "-qam", "the edit"]);
        assert_eq!(key(&fork), committed, "the key read the submodule's commit rather than the gitlink");

        write(&llvm.join("llvm/lib/IR/Untracked.cpp"), "int untracked;\n");
        let said = refusal("an untracked file in LLVM named a compiler", || {
            choose(&same, &rust_dir, &fork, fake);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        assert_eq!(builds.get(), 0, "an LLVM checkout no commit holds built a compiler");
    }

    /// The primary records no LLVM edit as the commit it is an edit of.
    #[test]
    fn the_primary_records_no_uncommitted_llvm_edit() {
        let scratch = TempDir::new("compiler-llvm-primary");
        let (_primary, rust_dir, _) = estate(&scratch);
        let llvm = llvm_checkout(&rust_dir);
        let before = fs::read_to_string(primary_record(&rust_dir)).unwrap();
        write(&llvm.join("llvm/lib/IR/Core.cpp"), "int core_edited;\n");
        let said = refusal("the primary recorded an uncommitted LLVM edit as its commit", || record(&rust_dir));
        assert!(said.contains("holds changes no commit does"), "{said}");
        assert_eq!(fs::read_to_string(primary_record(&rust_dir)).unwrap(), before);
    }

    /// Every source a compiler is built from moves its key: LLVM by commit,
    /// the tools by content.
    #[test]
    fn llvm_and_the_tools_move_the_key() {
        let scratch = TempDir::new("compiler-key");
        let (_primary, _rust_dir, [same, _, _]) = estate(&scratch);
        let fork = same.join("rust");
        let before = key(&fork);
        write(&fork.join("src/tools/lld-wrapper/src/main.rs"), "fn main() { 1; }\n");
        let tools = key(&fork);
        assert_ne!(tools, before, "a tool's source did not move the key");
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        let said = refusal("a gitlink staged and not committed named a compiler", || {
            key(&fork);
        });
        assert!(said.contains("stages"), "{said}");
        git(&fork, &["commit", "-qm", "another LLVM"]);
        assert_ne!(key(&fork), tools, "another LLVM commit did not move the key");
    }

    /// A primary record naming another LLVM, or none, is another compiler.
    #[test]
    fn another_llvm_is_another_compiler() {
        let scratch = TempDir::new("compiler-llvm");
        let (_primary, rust_dir, [same, _, _]) = estate(&scratch);
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            let stage2 = fork.join("build/toyos-compiler/stage2");
            write(&stage2.join("bin/rustc"), "a rustc");
            write(&stage2.join("lib/librustc_driver-2.dylib"), "a driver");
            stage2
        };
        let fork = same.join("rust");
        assert!(choose(&same, &rust_dir, &fork, fake).primary, "the primary's own compiler/ and LLVM built one");

        let record = primary_record(&rust_dir);
        let recorded = fs::read_to_string(&record).unwrap();
        let (compiler, llvm) = recorded.rsplit_once(" llvm ").expect("the record names its LLVM");
        assert_eq!(llvm, crate::llvm::key(&rust_dir));
        fs::write(&record, compiler).unwrap();
        assert!(!choose(&same, &rust_dir, &fork, fake).primary, "a record naming no LLVM was taken for this one");
        assert_eq!(builds.get(), 1);
        fs::write(&record, &recorded).unwrap();

        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        git(&fork, &["commit", "-qm", "the ToyOS LLVM"]);
        let mine = choose(&same, &rust_dir, &fork, fake);
        assert!(!mine.primary, "a worktree pinning another LLVM took the primary's compiler");
        assert_eq!(builds.get(), 2);
    }
}
