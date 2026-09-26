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
//! `compiler/`, `src/bootstrap/`, `src/stage0` and `Cargo.lock`, with
//! [`RECIPE`]. Nothing writes that directory after its [`SOURCE`] file exists,
//! and two worktrees naming the same compiler share one copy.
//!
//! **A compiler of a worktree's own never touches what the others build with**:
//! not the primary's `stage2`, not its record, not the machine-global rustup
//! `toyos` link — a sysroot is named by its directory, never by a toolchain
//! name, so no link is made. The global lock is not taken either; nothing of
//! the primary's is read.
//!
//! Locks, in the one order every acquirer takes them: the key's
//! (`buildlock::keyed_*` with [`Keyed::Compiler`]), with this worktree's build
//! lock put down, held shared for as long as a sysroot is being made from it;
//! then, to build, this worktree's exclusively, because its fork build
//! directory is written.
//!
//! A compiler no worktree names any more is removed by [`sweep`], which
//! `--worktree remove` runs: each build records the key it used in its
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
use crate::sysroot::{clone_tree, git_bytes, git_out, short, tree_identity, Restore};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become a compiler and is none of them: the
/// build below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage 2 of compiler/rustc and library, profile compiler, host only; 1";

/// What a compiler's key is the identity of, in its fork checkout.
const KEYED: [&str; 4] = ["compiler", "src/bootstrap", "src/stage0", "Cargo.lock"];

/// The file a finished compiler carries last, naming what it was built from. A
/// directory without it is a build that did not finish.
const SOURCE: &str = "SOURCE";

/// Where each build records the key of the compiler of its own it used, for
/// [`sweep`]. Absent while a worktree builds with the primary's.
const RECORD: &str = "target/toyos-compiler-key";

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
fn primary_record(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-compiler")
}

/// Every compiler of a worktree's own on this host.
pub fn compilers_dir(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/compilers")
}

/// What `checkout`'s `compiler/` is: its commit's tree, and whatever the working
/// tree changes in it — an edit, or a file git does not track yet, which is
/// what a new target spec is before its commit.
pub fn source(checkout: &Path) -> String {
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
/// after a toolchain build, and when the record is missing — its compiler stamp
/// has just said `stage2` is built from what its `rust/` holds.
pub fn record(rust_dir: &Path) {
    let at = primary_record(rust_dir);
    let want = source(rust_dir);
    if fs::read_to_string(&at).ok().as_deref() != Some(want.as_str()) {
        fs::write(&at, &want).unwrap_or_else(|e| panic!("write {}: {e}", at.display()));
    }
}

/// The key of the compiler `fork`'s sources name: their content, so committing
/// what was built as local changes names the same compiler.
pub fn key(fork: &Path) -> String {
    let parts = [RECIPE.to_string(), tree_identity(fork, &KEYED)];
    short(parts.join("\n\0\n").as_bytes())
}

/// The compiler `root`'s fork checkout at `fork` names: the primary's where its
/// `compiler/` is the one the primary's was built from, and otherwise its own,
/// built if nobody has built it, held in use for as long as the returned value
/// lives.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Path, lock: &mut Held) -> Compiler {
    lock.without_shared(|| choose(root, rust_dir, fork, build_in_fork))
}

/// [`resolve`] with the build that makes a compiler's `stage2` passed in, so a
/// test can stand in for bootstrap: `build` compiles the fork checkout it is
/// given and returns the `stage2` it left there.
fn choose(root: &Path, rust_dir: &Path, fork: &Path, build: impl Fn(&Path) -> PathBuf) -> Compiler {
    let recorded = root.join(RECORD);
    if fork == rust_dir {
        return Compiler::primary(rust_dir);
    }
    let built_from = fs::read_to_string(primary_record(rust_dir)).unwrap_or_default();
    if built_from.trim() == source(fork) {
        let _ = fs::remove_file(&recorded);
        return Compiler::primary(rust_dir);
    }
    let key = key(fork);
    let dir = compilers_dir(rust_dir).join(&key);
    fs::create_dir_all(recorded.parent().expect("a file under target/")).ok();
    fs::write(&recorded, &key).unwrap_or_else(|e| panic!("write {}: {e}", recorded.display()));
    let using = loop {
        let using = buildlock::keyed_using(root, Keyed::Compiler, &key);
        if dir.join(SOURCE).is_file() {
            break using;
        }
        drop(using);
        let _building = buildlock::keyed_building(root, Keyed::Compiler, &key);
        if !dir.join(SOURCE).is_file() {
            place(root, fork, &key, &dir, &build);
        }
    };
    Compiler { stage2: dir.join("stage2"), record: dir.join(SOURCE), primary: false, _using: Some(using) }
}

/// Build the compiler `key` names from `fork` and put it at `dir`. The caller
/// holds the key's lock.
fn place(root: &Path, fork: &Path, key: &str, dir: &Path, build: &impl Fn(&Path) -> PathBuf) {
    let what = format!("building compiler {key}");
    let _worktree = buildlock::worktree_exclusive(root, &what);
    eprintln!("Building compiler {key} in {}: its compiler/ is not the one the primary's was built from", fork.display());
    let stage2 = build(fork);
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
/// and the `stage2` it made, with the cargo every toolchain directory carries.
fn build_in_fork(fork: &Path) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let host = host_triple();
    let build_dir = fork.join("build/toyos-compiler");
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host)).unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    // Bootstrap re-locks both lockfiles to this worktree's `toyos-abi` and
    // `toyos`; the fork's own are put back, so the checkout stays clean and the
    // key stays the one it was built for.
    let _locks = (Restore::holding(&fork.join("Cargo.lock")), Restore::holding(&fork.join("library/Cargo.lock")));
    let args = ["build", "--stage", "2", "--config", config, "--warnings", "warn", "compiler/rustc", "library"];
    let (ok, log) = toolchain::x_build(fork, &args, "the compiler");
    toolchain::refuse_on_compile_error(&log, "the compiler");
    assert!(ok, "the compiler build in {} failed, and nothing in its output was a compile error", fork.display());
    let stage2 = build_dir.join(&host).join("stage2");
    assert!(stage2.join("bin/rustc").is_file(), "the compiler build left no {}", stage2.join("bin/rustc").display());
    toolchain::provision_toolchain_cargo(&stage2);
    toolchain::assert_toolchain_is_honest(&stage2);
    stage2
}

/// Bootstrap's configuration for a compiler of a worktree's own: the primary's
/// `profile` and options, for the host alone, since every guest target's
/// libraries are the sysroot's to build.
fn config_text(build_dir: &Path, host: &str) -> String {
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]

[rust]
incremental = true
lld = false
"#,
        build_dir = build_dir.display(),
    )
}

/// The key of the compiler of its own `root`'s last build used, if it used one.
pub fn recorded_key(root: &Path) -> Option<String> {
    fs::read_to_string(root.join(RECORD)).ok().map(|k| k.trim().to_string())
}

/// Remove every compiler no registered worktree records and nobody is making
/// or using, and every half-built one nobody is making. Returns what went.
pub fn sweep(root: &Path) -> Vec<PathBuf> {
    let dir = compilers_dir(&toolchain::rust_dir(root));
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
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
        let Some(_idle) = buildlock::keyed_idle(root, Keyed::Compiler, &key) else { continue };
        let path = entry.path();
        fs::remove_dir_all(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        removed.push(path);
    }
    removed
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::process::Command;

    use super::*;

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

    /// A primary whose `rust` pins fork commit `C0` and has built a compiler
    /// from it, and three linked worktrees: `same` pins `C0`, `a` and `b` each
    /// pin a commit whose `compiler/` is its own.
    fn estate() -> (PathBuf, PathBuf, [PathBuf; 3]) {
        let base = std::env::temp_dir().join(format!("toyos-compiler-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(&base).unwrap();

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
        let (primary, rust_dir, [same, a, b]) = estate();
        let before = snapshot(&rust_dir.join("build"));
        let link = toolchain::rustup_link();
        let builds = Cell::new(0);
        let fake = |fork: &Path| {
            builds.set(builds.get() + 1);
            let stage2 = fork.join("build/toyos-compiler/stage2");
            let spec = fs::read_to_string(fork.join("compiler/rustc_target/src/lib.rs")).unwrap();
            write(&stage2.join("bin/rustc"), &format!("a rustc knowing {spec}"));
            write(&stage2.join("lib/librustc_driver-1.dylib"), &spec);
            stage2
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
        assert_eq!((again.stage2.clone(), builds.get()), (ca.stage2.clone(), 2));
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
        assert_eq!((committed.stage2.clone(), builds.get()), (ca2.stage2.clone(), 3), "a commit rebuilt the compiler");
        git(&a.join("rust"), &["checkout", "-q", &pinned]);

        // The primary's own: nothing under its `build/` but `compilers/` moved.
        let after: Vec<_> = snapshot(&rust_dir.join("build"))
            .into_iter()
            .filter(|(p, _)| !p.starts_with(compilers_dir(&rust_dir)))
            .collect();
        assert_eq!(after, before, "the primary's stage2 or its record was written");
        assert_eq!(git(&rust_dir, &["rev-parse", "HEAD"]), git(&primary, &["rev-parse", "HEAD:rust"]));
        assert_eq!(toolchain::rustup_link(), link, "the machine-global toyos link moved");

        // A sweep takes the compiler nobody names, and only that one.
        let orphan = ca2.stage2.parent().unwrap().to_path_buf();
        drop((mine, ca, cb, again, ca2, committed));
        let kept = choose(&a, &rust_dir, &a.join("rust"), fake);
        assert_eq!(sweep(&primary), [orphan], "the sweep took a compiler a worktree names, or left one nobody does");
        assert!(kept.stage2.is_dir());
    }
}
