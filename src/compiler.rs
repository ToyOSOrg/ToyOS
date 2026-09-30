//! The compiler a sysroot is cloned from and compiled by: a product of the
//! store (`src/store.rs`), shared by every checkout whose fork names it, the
//! primary's among them.
//!
//! **Its key** ([`key`]) is [`RECIPE`], the key of the LLVM it links, and the
//! fork's `compiler/`, `src/tools/`, `src/stage0` and `Cargo.lock`. Bootstrap
//! builds it in the fork checkout's [`BUILD_DIR`], for the host alone, against
//! that LLVM (`src/llvm.rs`) through its `llvm-config`; its `stage2` is placed
//! at `compilers/<key>/stage2/`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::llvm;
use crate::store::{self, Kind, Sources};
use crate::sysroot::{clone_tree, Fork};
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become a compiler and is none of them: the
/// build below. Moving it moves every key.
const RECIPE: &str = "bootstrap stage 2 of compiler/rustc, library and src/tools/cargo linked statically, profile compiler, host only, with rust-lld, host linker pinned, LLVM, clang and LLD from the host's LLVM, without the links to its source; 9";

/// What bootstrap links into a `stage2` from the checkout that built it, which
/// a product of the store does not carry (`store::publish`).
const SOURCE_LINKS: [&str; 2] = ["lib/rustlib/src", "lib/rustlib/rustc-src"];

/// What bootstrap compiles for a compiler, as it names them.
const BUILT: [&str; 3] = ["compiler/rustc", "library", "src/tools/cargo"];

/// Where a fork checkout builds its compiler, kept between builds so the next
/// one is incremental.
const BUILD_DIR: &str = "build/toyos-rustc";

/// What the host rustc links its own binaries with, held to one answer: bootstrap
/// otherwise ties it to `lld` for `x86_64-unknown-linux-gnu`.
const HOST_LINKER_PIN: &str = "default-linker-linux-override = \"off\"";

/// A compiler, held in use for as long as this lives.
pub struct Compiler {
    /// Its toolchain directory: `bin/rustc`, `lib/`.
    pub stage2: PathBuf,
    pub key: String,
    _held: store::Held,
}

/// The key of the compiler `sources` name.
pub fn key(sources: &Sources) -> String {
    let parts = [&llvm::key(sources), sources.get("compiler"), sources.get("src/tools"), sources.get("src/stage0"), sources.get("Cargo.lock")];
    store::key(RECIPE, &parts)
}

/// The compiler `sources` name, built from `fork` if nobody has built it, and
/// held in use for as long as the returned value lives. `root` records its key
/// and its LLVM's, so both stay while it builds with them.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Fork, sources: &Sources) -> Compiler {
    choose(root, rust_dir, fork, sources, |dir| build_in_fork(root, rust_dir, dir, sources))
}

/// [`resolve`] with the build that makes a compiler's `stage2` passed in, so a
/// test can stand in for bootstrap: `build` compiles the fork checkout it is
/// given and returns the `stage2` it left there.
fn choose(root: &Path, rust_dir: &Path, fork: &Fork, sources: &Sources, build: impl Fn(&Path) -> PathBuf) -> Compiler {
    store::record(root, Kind::Llvm, &llvm::key(sources));
    let key = key(sources);
    let held = store::get(root, rust_dir, Kind::Compiler, &key, |partial| {
        let checkout = fork.checkout(root);
        fill(root, &checkout.dir, &key, partial, &build);
    });
    Compiler { stage2: held.dir.join("stage2"), key, _held: held }
}

/// Build the compiler `key` names from the fork checkout at `fork` into
/// `partial`.
fn fill(root: &Path, fork: &Path, key: &str, partial: &Path, build: &impl Fn(&Path) -> PathBuf) {
    eprintln!("Building compiler {key} in {}: nobody on this host has", fork.display());
    let stage2 = build(fork);
    clone_tree(&stage2, &partial.join("stage2"));
    for link in SOURCE_LINKS {
        let at = partial.join("stage2").join(link);
        fs::remove_dir_all(&at).unwrap_or_else(|e| panic!("remove {}: {e}", at.display()));
    }
    let again = self::key(&Sources::of(root, fork));
    assert!(
        again == key,
        "the fork's compiler sources moved while compiler {key} was being built (they are now \
         {again}); nothing was kept, and the next build makes the one they name"
    );
    store::assert_built_at_gitlinks(fork, &BUILT, &format!("compiler {key}"));
    if let Some(defect) = toolchain::toolchain_defect(&partial.join("stage2")) {
        panic!("compiler {key} was made, and is not whole: {defect}");
    }
}

/// Bootstrap's build of the compiler in `fork`, into [`BUILD_DIR`], against the
/// LLVM `sources` name, and the `stage2` it made, with the fork's own cargo and
/// the clang every toolchain directory carries.
fn build_in_fork(root: &Path, rust_dir: &Path, fork: &Path, sources: &Sources) -> PathBuf {
    crate::ensure_submodule(fork, "library/backtrace");
    let llvm = llvm::resolve(root, rust_dir, fork, sources);
    let host = host_triple();
    let build_dir = fork.join(BUILD_DIR);
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host, &llvm.dir)).unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args: Vec<&str> = ["build", "--stage", "2", "--config", config, "--warnings", "warn"].into_iter().chain(BUILT).collect();
    let (ok, log) = toolchain::x_build(fork, &args, "the compiler");
    toolchain::refuse_on_compile_error(&log, "the compiler");
    assert!(ok, "the compiler build in {} failed, and nothing in its output was a compile error", fork.display());
    let stage2 = build_dir.join(&host).join("stage2");
    assert!(stage2.join("bin/rustc").is_file(), "the compiler build left no {}", stage2.join("bin/rustc").display());
    let tools: Vec<PathBuf> = fs::read_dir(build_dir.join(&host))
        .unwrap_or_else(|e| panic!("read {}: {e}", build_dir.join(&host).display()))
        .map(|e| e.unwrap_or_else(|e| panic!("read the build directory: {e}")).path().join("cargo"))
        .filter(|cargo| cargo.parent().is_some_and(|d| d.to_string_lossy().ends_with("-tools-bin")) && cargo.is_file())
        .collect();
    let [cargo] = tools.as_slice() else { panic!("the compiler build left cargo at {tools:?}, not one place") };
    // Removed first: a link left there would be copied through, onto what it names.
    match fs::remove_file(stage2.join("bin/cargo")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove {}: {e}", stage2.join("bin/cargo").display()),
        _ => {}
    }
    fs::copy(cargo, stage2.join("bin/cargo")).unwrap_or_else(|e| panic!("copy {} into {}: {e}", cargo.display(), stage2.display()));
    crate::clang::provision(&stage2, &llvm.dir);
    toolchain::assert_toolchain_is_honest(&stage2);
    stage2
}

/// Bootstrap's configuration for a compiler: for the host alone, since every
/// guest target's libraries are the sysroot's to build, linking the LLVM at
/// `llvm`.
fn config_text(build_dir: &Path, host: &str, llvm: &Path) -> String {
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]
cargo-native-static = true

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
        pin = HOST_LINKER_PIN,
        external = llvm::host_lines(llvm),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::store::tests::{behind_a_staged_gitlink, estate, git, refusal, write, LLVM_B};

    /// Bootstrap's stand-in: a `stage2` that says which target spec it knows,
    /// with what every toolchain directory carries.
    fn fake_build(fork: &Path) -> PathBuf {
        let stage2 = fork.join(BUILD_DIR).join("stage2");
        let spec = fs::read_to_string(fork.join("compiler/rustc_target/src/lib.rs")).unwrap();
        write(&stage2.join("bin/rustc"), &format!("a rustc knowing {spec}"));
        write(&stage2.join("lib/librustc_driver-1.dylib"), &spec);
        let lld = toolchain::rust_lld(&stage2);
        for tool in ["rust-lld", "llvm-ar", "clang", "ld.lld"] {
            write(&lld.with_file_name(tool), tool);
        }
        write(&lld.parent().unwrap().parent().unwrap().join("lib/clang/22/include/stddef.h"), "stddef");
        write(&stage2.join("bin/cargo"), "cargo");
        for link in SOURCE_LINKS {
            let at = stage2.join(link).join("rust");
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&at);
            std::os::unix::fs::symlink(fork, &at).unwrap();
        }
        stage2
    }

    fn sources(root: &Path) -> Sources {
        Sources::of(root, &root.join("rust"))
    }

    /// **One compiler per key, whoever asks**: the primary and a worktree whose
    /// fork names the same `compiler/` share one; two worktrees with different
    /// compilers build side by side, each once, and find theirs again; an
    /// untracked file in `compiler/` is a new compiler and committing it is not
    /// another.
    #[test]
    fn one_compiler_per_key_whoever_asks() {
        let e = estate("compiler");
        let builds = Cell::new(0);
        let counted = |fork: &Path| {
            builds.set(builds.get() + 1);
            fake_build(fork)
        };
        let primary = choose(&e.primary, &e.rust_dir, &Fork::Checkout(e.rust_dir.clone()), &sources(&e.primary), counted);
        let same = choose(&e.same, &e.rust_dir, &Fork::Checkout(e.same.join("rust")), &sources(&e.same), counted);
        assert_eq!((same.stage2, builds.get()), (primary.stage2.clone(), 1), "one compiler/ built two compilers");
        assert!(SOURCE_LINKS.iter().all(|l| !primary.stage2.join(l).exists()), "a compiler names the checkout that built it");

        let ca = choose(&e.a, &e.rust_dir, &Fork::Checkout(e.a.join("rust")), &sources(&e.a), counted);
        let cb = choose(&e.b, &e.rust_dir, &Fork::Checkout(e.b.join("rust")), &sources(&e.b), counted);
        assert_eq!(builds.get(), 3);
        assert_ne!(ca.stage2, cb.stage2, "two compilers were given one directory");
        assert!(fs::read_to_string(ca.stage2.join("bin/rustc")).unwrap().contains("aarch64"));
        assert!(fs::read_to_string(cb.stage2.join("bin/rustc")).unwrap().contains("riscv"));
        let again = choose(&e.a, &e.rust_dir, &Fork::Checkout(e.a.join("rust")), &sources(&e.a), counted);
        assert_eq!((again.stage2, builds.get()), (ca.stage2.clone(), 3), "a placed compiler was built again");

        let fork = e.a.join("rust");
        write(&fork.join("compiler/rustc_target/src/new_target.rs"), "pub fn t() {}\n");
        let untracked = choose(&e.a, &e.rust_dir, &Fork::Checkout(fork.clone()), &sources(&e.a), counted);
        assert_ne!(untracked.stage2, ca.stage2, "an untracked target spec kept the old compiler");
        git(&fork, &["add", "-A"]);
        git(&fork, &["commit", "-qm", "the target, committed"]);
        let committed = choose(&e.a, &e.rust_dir, &Fork::Checkout(fork), &sources(&e.a), counted);
        assert_eq!((committed.stage2, builds.get()), (untracked.stage2, 4), "a commit rebuilt the compiler");
        assert_eq!(store::recorded(&e.a, Kind::Llvm), Some(llvm::key(&sources(&e.a))), "the LLVM a compiler links went unrecorded");
    }

    /// **The key is content, never files' times**, and every source a compiler
    /// is built from moves it: its tools by content, its LLVM by commit.
    #[test]
    fn the_key_is_what_the_compiler_is_built_from() {
        let e = estate("compiler-key");
        let fork = e.same.join("rust");
        let before = key(&sources(&e.same));
        let spec = fork.join("compiler/rustc_target/src/lib.rs");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(&spec).unwrap().set_modified(later).unwrap();
        assert_eq!(key(&sources(&e.same)), before, "a file's time moved the key");
        write(&fork.join("src/tools/lld-wrapper/src/main.rs"), "fn main() { 1; }\n");
        let tools = key(&sources(&e.same));
        assert_ne!(tools, before, "a tool's source did not move the key");
        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{}", llvm::LLVM)]);
        assert_ne!(key(&sources(&e.same)), tools, "another LLVM gitlink did not move the key");
        write(&fork.join("library/std/src/lib.rs"), "pub fn z() {}\n");
        let std = key(&sources(&e.same));
        git(&fork, &["checkout", "-q", "--", "library"]);
        assert_eq!(key(&sources(&e.same)), std, "a std edit moved the compiler's key");
    }

    /// **A compiler whose sources moved while it was built is never placed.**
    #[test]
    fn a_compiler_whose_sources_moved_is_never_placed() {
        let e = estate("compiler-moved");
        let fork = e.a.join("rust");
        let moving = |fork: &Path| {
            write(&fork.join("compiler/rustc_target/src/lib.rs"), "pub fn targets() { moved() }\n");
            fake_build(fork)
        };
        let said = refusal("a compiler whose sources moved was placed", || {
            choose(&e.a, &e.rust_dir, &Fork::Checkout(fork.clone()), &sources(&e.a), moving);
        });
        assert!(said.contains("moved while compiler"), "{said}");
        let placed: Vec<_> = fs::read_dir(Kind::Compiler.dir(&e.rust_dir)).into_iter().flatten().flatten().map(|e| e.file_name()).collect();
        assert!(placed.is_empty(), "placed: {placed:?}");
    }

    /// **A compiler built from a submodule at another commit than its gitlink
    /// is never placed**, `library/backtrace` or the `src/tools/cargo` it
    /// ships: bootstrap leaves either at `HEAD`'s gitlink under a staged one,
    /// and builds it.
    #[test]
    fn a_compiler_built_off_a_submodule_s_gitlink_is_never_placed() {
        for path in ["library/backtrace", "src/tools/cargo"] {
            let e = estate("compiler-gitlink");
            let fork = e.a.join("rust");
            let (head, staged) = behind_a_staged_gitlink(&fork, path);
            let said = refusal(&format!("a compiler built from a {path} its gitlink does not name was placed"), || {
                choose(&e.a, &e.rust_dir, &Fork::Checkout(fork.clone()), &sources(&e.a), fake_build);
            });
            assert!(said.contains(&format!("{} is at {head}, and its gitlink names {staged}", fork.join(path).display())), "{said}");
            let placed: Vec<_> = fs::read_dir(Kind::Compiler.dir(&e.rust_dir)).into_iter().flatten().flatten().map(|e| e.file_name()).collect();
            assert!(placed.is_empty(), "placed: {placed:?}");
        }
    }
}
