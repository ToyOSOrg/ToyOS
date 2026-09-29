use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::arch::Arch;
use crate::store::{self, ABI_TREES};
use crate::sysroot::{self, Sysroot};

/// Which checkout holds the `rust/` submodule and the toolchain built from it.
pub enum Owner {
    Us,
    /// The primary checkout, named so a refusal can point at it.
    Elsewhere(PathBuf),
    /// Nobody in this repository: `rust/build` holds a toolchain that arrived
    /// as an artifact, and there is no `rust/` source to have built it from.
    ///
    /// Read off the disk rather than declared, because a checkout with a
    /// toolchain and no compiler source has exactly one thing it can do with
    /// it, and a flag or an env var saying so could disagree with what is
    /// there. This is how a CI runner gets a sysroot: `x.py` on four cores is
    /// an hour, and the product is 1.1 GB.
    Installed,
}

/// One `rust/` per repository, in the primary checkout, holding the fork's
/// objects and the store every checkout builds into. A linked worktree never
/// initialises its own: git would clone the fork's history again, 913 MiB
/// sharing no objects.
pub fn owner(root: &Path) -> Owner {
    let primary = crate::primary_checkout(root);
    let same = fs::canonicalize(root).map(|r| r == primary).unwrap_or(false);
    if same {
        let installed = !root.join("rust/x.py").exists()
            && root
                .join(format!("rust/build/{}/stage2/bin/rustc", host_triple()))
                .exists();
        return if installed { Owner::Installed } else { Owner::Us };
    }
    assert!(
        primary.join("rust/x.py").exists(),
        "{} is a linked worktree, so the shared rust checkout should be at {}, \
         and there is nothing there.\n\
         A repository laid out with --separate-git-dir cannot be located this way.",
        root.display(),
        primary.join("rust").display()
    );
    Owner::Elsewhere(primary)
}

/// The shared rust checkout: source, `build/`, and the sysroot every worktree
/// compiles against.
pub fn rust_dir(root: &Path) -> PathBuf {
    match owner(root) {
        Owner::Us | Owner::Installed => root.join("rust"),
        Owner::Elsewhere(primary) => primary.join("rust"),
    }
}

/// Of the sysroot's sources, the ones std compiles.
const STD_SOURCES: [&str; 2] = ["toyos-abi/src", "toyos/src"];

/// Every target a guest artifact is built for: ToyOS userland, the kernel's
/// bare-metal target, and the UEFI bootloader.
///
/// One home for the list, because it is read in ways that must agree: the
/// libraries `src/sysroot.rs` builds and places, and `src/build.rs`'s external
/// fingerprint.
pub const GUEST_TARGETS: [&str; 6] = [
    Arch::X86_64.userland(),
    Arch::X86_64.kernel(),
    Arch::X86_64.loader(),
    Arch::Aarch64.userland(),
    Arch::Aarch64.kernel(),
    Arch::Aarch64.loader(),
];

/// Where an installed toolchain is, as `src/release.rs` unpacks it.
pub(crate) fn stage2(rust_dir: &Path) -> PathBuf {
    rust_dir.join(format!("build/{}/stage2", host_triple()))
}

/// Every `toyos-abi`/`toyos` source file a std build under `dep_info` actually
/// compiled, read out of cargo's dep-info rather than out of what was asked for.
fn std_toyos_sources(dep_info: &Path) -> Vec<String> {
    let mut deps = Vec::new();
    collect_dep_info(dep_info, &mut deps);
    let mut found: Vec<String> = deps
        .iter()
        .flat_map(|text| toyos_sources_in_dep_info(text))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

fn collect_dep_info(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_dep_info(&path, out);
        } else if path.extension().is_some_and(|e| e == "d") {
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(text);
            }
        }
    }
}

/// The paths in one dep-info file that name a `toyos-abi/src` or `toyos/src`
/// source. Split out from the filesystem so the gate below has a negative
/// control that is a string literal.
fn toyos_sources_in_dep_info(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split_ascii_whitespace() {
        let word = word.strip_suffix(':').unwrap_or(word);
        if !word.ends_with(".rs") {
            continue;
        }
        if STD_SOURCES.iter().any(|src| word.contains(&format!("/{src}/"))) {
            out.push(word.to_string());
        }
    }
    out
}

/// Refuse a std whose dep-info under `dep_info` names another checkout's ABI.
///
/// What the builder believed is not evidence; this reads what the compiler was
/// handed. A std built against another worktree's `toyos-abi` still builds,
/// links and boots, and its syscall arguments land at different offsets.
pub(crate) fn assert_std_built_from(root: &Path, dep_info: &Path) {
    let sources = std_toyos_sources(dep_info);
    assert!(
        !sources.is_empty(),
        "the std build under {} names no toyos-abi or toyos source at all, so the check that \
         it was built from this worktree cannot answer. Cargo's dep-info moved.",
        dep_info.display(),
    );
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // Resolved: the shared checkout reaches a worktree's trees through links.
    let resolved = |p: &String| fs::canonicalize(p).unwrap_or_else(|e| panic!("resolve {p}, which std compiled: {e}"));
    let foreign: Vec<&String> = sources.iter().filter(|p| !resolved(p).starts_with(&root)).collect();
    assert!(
        foreign.is_empty(),
        "std was compiled against {} sources that are not this worktree's:\n  {}",
        foreign.len(),
        foreign.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("\n  "),
    );
}

/// What an installed toolchain's sysroot was built from, as its publisher
/// recorded it (`src/release.rs`).
fn witness_path(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-sysroot-witness")
}

/// The lines of a witness belonging to `trees`.
fn witness_subset(text: &str, trees: &[&str]) -> String {
    text.lines()
        .filter(|l| trees.iter().any(|t| l.starts_with(&format!("{t}:"))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Which of [`ABI_TREES`] this worktree disagrees with the sysroot about.
fn differing_trees(recorded: Option<&str>, current: &str) -> String {
    let Some(recorded) = recorded else {
        return "nothing recorded what the sysroot was built from".to_string();
    };
    let names: Vec<&str> = ABI_TREES
        .iter()
        .copied()
        .filter(|t| witness_subset(recorded, &[t]) != witness_subset(current, &[t]))
        .collect();
    if names.is_empty() {
        return "the record is malformed".to_string();
    }
    names.join(", ")
}

pub(crate) fn rustup_home() -> Option<PathBuf> {
    std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".rustup")))
}

/// Where the machine-global `toyos` rustup toolchain currently points.
pub(crate) fn rustup_link() -> Option<PathBuf> {
    fs::read_link(rustup_home()?.join("toolchains/toyos")).ok()
}

/// The binaries rustup proxies out of a linked toolchain's `bin/`.
///
/// A name that is not there is a name rustup answers for by falling back to
/// another toolchain and narrating it, two `info:` lines per invocation — about
/// 8-10 invocations in a build and 249 in a full `cargo test`.
const TOOLCHAIN_BINARIES: [&str; 2] = ["rustc", "cargo"];

/// Which of [`TOOLCHAIN_BINARIES`] a toolchain's `bin/` would make rustup
/// narrate a fallback for.
///
/// `exists` and not `read_dir`, because it follows the link: a `cargo` symlink
/// left by another machine — an artifact publisher's, say — dangles here, and a
/// dangling proxy is a narrated fallback exactly as an absent one is.
fn narrated_binaries(bin: &Path) -> Vec<&'static str> {
    TOOLCHAIN_BINARIES.into_iter().filter(|name| !bin.join(name).exists()).collect()
}

/// Why the toolchain at `stage2` is not whole, if it is not: a binary rustup
/// would narrate a fallback for, no linker for the guest targets, or no C
/// toolchain (`src/clang.rs`).
///
/// The one definition of whole: [`assert_toolchain_is_honest`] refuses by it,
/// and a sysroot is finished only by it (`src/sysroot.rs`).
pub(crate) fn toolchain_defect(stage2: &Path) -> Option<String> {
    let bin = stage2.join("bin");
    let narrated = narrated_binaries(&bin);
    if !narrated.is_empty() {
        return Some(format!(
            "the toyos toolchain at {} is missing {}, so rustup answers for {} by falling back to \
             another toolchain and narrating it on every invocation.\n\
             The compiler build puts them there (`src/compiler.rs`), and it did not.",
            bin.display(),
            narrated.join(" and "),
            if narrated.len() == 1 { "it" } else { "them" },
        ));
    }
    // Every guest target names `rust-lld` and rustc looks for it here, so a
    // toolchain without it is refused here, by name, rather than at the first link.
    let lld = rust_lld(stage2);
    if !lld.is_file() {
        return Some(format!(
            "the toyos toolchain at {} carries no {}, the linker every guest target names: \
             bootstrap puts it there when its configuration says `lld = true`, and it did not",
            stage2.display(),
            lld.display(),
        ));
    }
    crate::clang::defect(stage2)
}

/// Refuse a toolchain that is not whole ([`toolchain_defect`]).
///
/// Unconditional and after the step that provisions, because the defect being
/// gated is a provisioning step that silently stopped running: a check that only
/// runs when the step runs asserts nothing about the build that skipped it.
pub(crate) fn assert_toolchain_is_honest(stage2: &Path) {
    if let Some(defect) = toolchain_defect(stage2) {
        panic!("{defect}");
    }
}

/// The sysroot this checkout's sources name, made if nobody has made it
/// (`src/sysroot.rs`), and held in use for as long as the returned value lives.
///
/// Every checkout makes what its own sources name, into the store; nothing is
/// rebuilt in place. The primary's build alone points the rustup `toyos`
/// toolchain at what it built ([`link`]).
pub fn ensure(root: &Path) -> Sysroot {
    let rust_dir = rust_dir(root);
    match owner(root) {
        Owner::Installed => {
            check_installed_toolchain(root, &rust_dir);
            Sysroot::installed(stage2(&rust_dir))
        }
        Owner::Elsewhere(_) => sysroot::ensure(root, &rust_dir),
        Owner::Us => {
            let sysroot = sysroot::ensure(root, &rust_dir);
            let home = rustup_home().expect("a rustup home");
            link(&home, &rust_dir, &sysroot.dir);
            sysroot
        }
    }
}

/// Point the rustup `toyos` toolchain at `sysroot`, through the one stable path
/// it ever names, [`store::current`]: that link is replaced by a rename, so no
/// `rustc` run through rustup ever finds the name dangling, and rustup's own
/// is made once.
fn link(rustup_home: &Path, rust_dir: &Path, sysroot: &Path) {
    let current = store::current(rust_dir);
    let target = sysroot.strip_prefix(current.parent().expect("the link has a parent")).unwrap_or(sysroot);
    swap_link(target, &current);
    swap_link(&current, &rustup_home.join("toolchains/toyos"));
}

/// Make `at` a link to `to`, by a rename over whatever `at` was.
pub(crate) fn swap_link(to: &Path, at: &Path) {
    if fs::read_link(at).is_ok_and(|now| now == to) {
        return;
    }
    let dir = at.parent().expect("a link has a parent");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let fresh = at.with_extension(format!("{}.new", std::process::id()));
    let _ = fs::remove_file(&fresh);
    std::os::unix::fs::symlink(to, &fresh).unwrap_or_else(|e| panic!("link {} -> {}: {e}", fresh.display(), to.display()));
    fs::rename(&fresh, at).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", fresh.display(), at.display()));
}

/// Everything a checkout may do with a toolchain it did not build: check that
/// it is the one this tree needs, and say what to do when it is not.
///
/// No amount of source here can rebuild a sysroot without `rust/`, so there is
/// nothing to decide and the answer is always to publish a toolchain built from
/// these sources. Its std fork is pinned by the release tag, which is a function
/// of `rust` (`src/release.rs`).
fn check_installed_toolchain(root: &Path, rust_dir: &Path) {
    let stage2 = stage2(rust_dir);
    let linked = rustup_link();
    assert!(
        linked.as_deref() == Some(stage2.as_path()),
        "the rustup toolchain `toyos` points at {}, not at the installed toolchain at {}.\n\
         Link it: rustup toolchain link toyos {}",
        linked.map_or_else(|| "nothing".to_string(), |p| p.display().to_string()),
        stage2.display(),
        stage2.display(),
    );

    assert_toolchain_is_honest(&stage2);

    let want = sysroot::witness(root);
    let recorded = fs::read_to_string(witness_path(rust_dir)).ok();
    assert!(
        recorded.as_deref() == Some(want.as_str()),
        "this checkout and the installed toolchain at {} disagree about {}, so a build \
         here would link its kernel against another tree's struct layouts.\n\
         Publish a toolchain built from these sources and install that one instead.",
        stage2.display(),
        differing_trees(recorded.as_deref(), &want),
    );
}

/// Run bootstrap in the fork checkout `rust_dir`, streaming its output where it
/// was going anyway and keeping a copy, with both the checkout's lockfiles put
/// back as they were when this returns, however the build that holds them
/// ends.
///
/// Bootstrap re-locks them against this worktree's `toyos-abi` and `toyos`;
/// putting them back keeps the checkout clean, and a key read from it the one
/// that was built.
///
/// `.status()` was enough while the only question was the exit code. It is not
/// enough for the question [`refuse_on_compile_error`] asks, which is what the
/// failure *was*.
pub(crate) fn x_build(rust_dir: &Path, args: &[&str], what: &str) -> (bool, Vec<String>) {
    x_build_with(rust_dir, args, what, |_| {})
}

/// [`x_build`], with bootstrap's environment what `environment` makes of this
/// process's.
pub(crate) fn x_build_with(
    rust_dir: &Path,
    args: &[&str],
    what: &str,
    environment: impl FnOnce(&mut Command),
) -> (bool, Vec<String>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{Arc, Mutex};

    let _locks = [
        Restore::holding(&rust_dir.join("Cargo.lock")),
        Restore::holding(&rust_dir.join("library/Cargo.lock")),
    ];
    // Two literals and not one variable: `src/sourcegate::every_binary_the_host_runs_is_declared`
    // reads the argument, and a name assembled at run time is a name nobody declared.
    let (x, mut command) = if rust_dir.join("x").exists() {
        ("./x", Command::new("./x"))
    } else {
        ("./x.py", Command::new("./x.py"))
    };
    environment(&mut command);
    let mut child = command
        .args(args)
        .env("BOOTSTRAP_SKIP_TARGET_SANITY", "1")
        .current_dir(rust_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to run {x} for {what}: {e}"));

    // One log in the order the two streams produced it, because the error and
    // the `--> file:line` under it are what a reader needs together.
    let log = Arc::new(Mutex::new(Vec::new()));
    let pump = |stream: Box<dyn Read + Send>, to_stderr: bool, log: Arc<Mutex<Vec<String>>>| {
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if to_stderr {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }
                log.lock().expect("the log outlives both pumps").push(line);
            }
        })
    };
    let out = pump(Box::new(child.stdout.take().expect("piped")), false, Arc::clone(&log));
    let err = pump(Box::new(child.stderr.take().expect("piped")), true, Arc::clone(&log));
    let status = child.wait().unwrap_or_else(|e| panic!("waiting for {x} {what}: {e}"));
    out.join().expect("the stdout pump");
    err.join().expect("the stderr pump");

    let log = Arc::try_unwrap(log).expect("both pumps are joined").into_inner();
    (status.success(), log.expect("no pump panicked while holding it"))
}

/// A file put back to its bytes when this drops, however the scope ends, by a
/// sibling renamed over it: a restore that fails leaves the file as it was.
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
        let mut sibling = self.path.clone().into_os_string();
        sibling.push(".restore");
        let sibling = PathBuf::from(sibling);
        fs::write(&sibling, &self.bytes).unwrap_or_else(|e| panic!("write {}: {e}", sibling.display()));
        fs::rename(&sibling, &self.path).unwrap_or_else(|e| panic!("restore {}: {e}", self.path.display()));
    }
}

/// Where a compile error starts in an `x build` log, if there is one.
///
/// Both bootstrap callers let a non-zero `x build` through when the artifacts
/// they need are on disk, because rustdoc for ToyOS does not link and never
/// has. That allowance used to be *anything at all*, as long as a `rustc` from
/// some earlier build was still there — so run `31370078581` compiled std with
/// `error[E0433]`, took the allowance, and died 83 seconds and 260 lines later
/// at a missing file. The reported failure was the consequence.
///
/// A compile error cannot be a link failure, so it cannot be the thing that
/// allowance is for.
fn compile_error_at(log: &[String]) -> Option<usize> {
    log.iter().position(|l| {
        let l = l.trim_start();
        l.starts_with("error[") || l.starts_with("error: could not compile")
    })
}

/// Stop on the first real error rather than on what it goes on to break.
pub(crate) fn refuse_on_compile_error(log: &[String], what: &str) {
    let Some(at) = compile_error_at(log) else { return };
    let end = (at + 6).min(log.len());
    panic!("{what} did not compile:\n{}", log[at..end].join("\n"));
}

/// The linker every guest target names, as the toolchain at `toolchain` carries
/// it: `lib/rustlib/<host>/bin/rust-lld`, where rustc itself looks for it.
pub fn rust_lld(toolchain: &Path) -> PathBuf {
    toolchain.join("lib/rustlib").join(host_triple()).join("bin/rust-lld")
}

/// The host triple, asked of rustc once per process.
///
/// Every path built from it calls this, so an uncached one spent about seven
/// `rustc --version --verbose` spawns per build call — 0.118 s each, measured.
pub fn host_triple() -> String {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        let output = Command::new("rustc")
            .args(["--version", "--verbose"])
            .output()
            .expect("Failed to run rustc");
        let text = String::from_utf8(output.stdout).unwrap();
        text.lines()
            .find(|l| l.starts_with("host:"))
            .map(|l| l.strip_prefix("host: ").unwrap().to_string())
            .expect("Could not determine host triple")
    })
    .clone()
}


#[cfg(test)]
mod tests {
    use super::*;
    use toyos_tmpdir::TempDir;

    /// The file a fork checkout carries for [`a_fake_bootstrap`] to act in it.
    const FAKE: &str = "FAKE_BOOTSTRAP";

    /// **Every bootstrap run leaves the fork's lockfiles as it found them**:
    /// `./x` here is this test binary, running [`a_fake_bootstrap`], which
    /// re-locks both and fails.
    #[test]
    fn a_bootstrap_run_leaves_both_lockfiles_as_they_were() {
        let fork = TempDir::new("x-build-locks");
        let locks = [fork.join("Cargo.lock"), fork.join("library/Cargo.lock")];
        fs::create_dir_all(fork.join("library")).unwrap();
        for lock in &locks {
            fs::write(lock, "# as committed\n").unwrap();
        }
        fs::write(fork.join(FAKE), "").unwrap();
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), fork.join("x")).unwrap();

        let args = ["--exact", "toolchain::tests::a_fake_bootstrap", "--include-ignored", "--nocapture"];
        let (ok, log) = x_build(&fork, &args, "a fake bootstrap");
        assert!(!ok, "the fake bootstrap did not run: {log:?}");
        assert!(log.iter().any(|l| l.contains("re-locked both")), "{log:?}");
        for lock in &locks {
            assert_eq!(fs::read_to_string(lock).unwrap(), "# as committed\n", "{} was left re-locked", lock.display());
        }
    }

    #[test]
    #[ignore = "the bootstrap `a_bootstrap_run_leaves_both_lockfiles_as_they_were` runs; never runs on its own"]
    fn a_fake_bootstrap() {
        assert!(Path::new(FAKE).is_file(), "a_fake_bootstrap ran outside a fake fork checkout; it is not a test");
        for lock in ["Cargo.lock", "library/Cargo.lock"] {
            fs::write(lock, "# re-locked to the published toyos-abi\n").unwrap();
        }
        panic!("re-locked both, and failed");
    }

    /// **A restore that cannot write leaves the file as it found it**, and
    /// names what it could not write.
    #[test]
    fn a_restore_that_cannot_write_leaves_the_file_whole() {
        let dir = TempDir::new("restore-fails");
        let lock = dir.join("Cargo.lock");
        fs::write(&lock, "# as committed\n").unwrap();
        let held = Restore::holding(&lock);
        fs::write(&lock, "# re-locked to the published toyos-abi\n").unwrap();
        fs::create_dir(dir.join("Cargo.lock.restore")).unwrap();

        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(held)));
        let refusal = failed.expect_err("a restore that could not write went unsaid");
        let refusal = refusal.downcast_ref::<String>().expect("a formatted panic");
        assert!(refusal.contains("Cargo.lock.restore"), "{refusal}");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "# re-locked to the published toyos-abi\n",
                   "a restore that failed wrote over the file");
    }

    /// **The layout that makes rustup narrate, as a decision.**
    /// [`assert_toolchain_is_honest`] is this function over the real `bin/`.
    #[test]
    fn a_toolchain_bin_without_cargo_is_one_rustup_narrates() {
        let stage2 = TempDir::new("layout");
        let bin = stage2.join("bin");
        fs::create_dir_all(&bin).unwrap();
        assert_eq!(narrated_binaries(&bin), ["rustc", "cargo"]);

        fs::write(bin.join("rustc"), b"").unwrap();
        assert_eq!(narrated_binaries(&bin), ["cargo"]);

        // A link naming a path only another machine had is there, and it is a
        // narrated fallback all the same.
        let foreign = Path::new("/a-runner-that-is-not-this-one/bin/cargo");
        std::os::unix::fs::symlink(foreign, bin.join("cargo")).unwrap();
        assert_eq!(narrated_binaries(&bin), ["cargo"], "a dangling proxy is not a cargo");

        fs::remove_file(bin.join("cargo")).unwrap();
        fs::write(bin.join("cargo"), b"").unwrap();
        assert!(narrated_binaries(&bin).is_empty());

        // Nothing narrates, and the toolchain is still refused: it has no linker.
        let refused = std::panic::catch_unwind(|| assert_toolchain_is_honest(&stage2))
            .expect_err("a toolchain with no rust-lld is refused");
        let said = refused.downcast_ref::<String>().expect("a formatted refusal");
        assert!(said.contains("rust-lld"), "the refusal names the linker: {said}");

        // And with its linker, it is still refused: it has no C compiler, which
        // `clang`'s own tests provision.
        let lld = rust_lld(&stage2);
        fs::create_dir_all(lld.parent().unwrap()).unwrap();
        fs::write(&lld, b"").unwrap();
        let refused = std::panic::catch_unwind(|| assert_toolchain_is_honest(&stage2))
            .expect_err("a toolchain with no clang is refused");
        let said = refused.downcast_ref::<String>().expect("a formatted refusal");
        assert!(said.contains("clang") && !said.contains("rust-lld,"), "the refusal names clang alone: {said}");
    }

    /// **The rustup `toyos` toolchain names one stable path, ever**: the
    /// primary's builds move the link at that path, and rustup's own is made
    /// once and never moved again.
    #[test]
    fn the_rustup_link_names_one_stable_path() {
        let scratch = TempDir::new("rustup-link");
        let (home, rust_dir) = (scratch.join("rustup"), scratch.join("rust"));
        let [one, two] = ["k1", "k2"].map(|k| store::Kind::Sysroot.dir(&rust_dir).join(k));
        let toyos = home.join("toolchains/toyos");
        link(&home, &rust_dir, &one);
        assert_eq!(fs::read_link(&toyos).unwrap(), store::current(&rust_dir));
        assert_eq!(fs::read_link(store::current(&rust_dir)).unwrap(), Path::new("sysroots/k1"));
        let made = fs::symlink_metadata(&toyos).unwrap().modified().unwrap();
        link(&home, &rust_dir, &two);
        assert_eq!(fs::read_link(store::current(&rust_dir)).unwrap(), Path::new("sysroots/k2"));
        assert_eq!(fs::symlink_metadata(&toyos).unwrap().modified().unwrap(), made, "rustup's own link was made again");
        let left: Vec<_> = fs::read_dir(rust_dir.join("build")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, ["toyos"], "a link's replacement was left beside it");
    }

    /// **A std is this worktree's when what it compiled resolves into it**: the
    /// shared checkout reaches a worktree's `toyos-abi` through a link beside
    /// it, and a link to another worktree's is refused.
    #[test]
    fn a_std_compiled_through_links_is_the_worktree_they_resolve_to() {
        let scratch = TempDir::new("std-through-links");
        let [mine, theirs, beside, built] = ["mine", "theirs", "shared", "built"].map(|d| scratch.join(d));
        for worktree in [&mine, &theirs] {
            fs::create_dir_all(worktree.join("toyos-abi/src")).unwrap();
            fs::write(worktree.join("toyos-abi/src/lib.rs"), "pub struct A;\n").unwrap();
        }
        fs::create_dir_all(&beside).unwrap();
        fs::create_dir_all(&built).unwrap();
        let through = beside.join("toyos-abi/src/lib.rs");
        fs::write(built.join("toyos_abi.d"), format!("{}: {}\n", built.join("libtoyos_abi.rlib").display(), through.display())).unwrap();

        std::os::unix::fs::symlink(mine.join("toyos-abi"), beside.join("toyos-abi")).unwrap();
        assert_std_built_from(&mine, &built);
        fs::remove_file(beside.join("toyos-abi")).unwrap();
        std::os::unix::fs::symlink(theirs.join("toyos-abi"), beside.join("toyos-abi")).unwrap();
        let refused = std::panic::catch_unwind(|| assert_std_built_from(&mine, &built));
        let said = refused.expect_err("a std compiled against another worktree's ABI was taken for this one's");
        assert!(said.downcast_ref::<String>().is_some_and(|s| s.contains(&through.display().to_string())));
    }

    /// The negative control is the defect itself: this is verbatim what cargo
    /// wrote for a worktree build before the override existed.
    #[test]
    fn dep_info_names_the_checkout_std_was_really_built_from() {
        let primary = "/Users/jan/Dev/jan/toyos/toyos-abi/src/lib.rs \
                       /Users/jan/Dev/jan/toyos/toyos/src/audio.rs \
                       /Users/jan/Dev/jan/toyos/rust/build/host/stage1-std/out/libcore.rmeta";
        assert_eq!(
            toyos_sources_in_dep_info(primary),
            [
                "/Users/jan/Dev/jan/toyos/toyos-abi/src/lib.rs",
                "/Users/jan/Dev/jan/toyos/toyos/src/audio.rs"
            ],
        );

        // A dep-info line ends in a colon when the file is its own target.
        let target = "/Users/jan/Dev/jan/toyos-endow/toyos-abi/src/lib.rs:";
        assert_eq!(
            toyos_sources_in_dep_info(target),
            ["/Users/jan/Dev/jan/toyos-endow/toyos-abi/src/lib.rs"],
        );

        // `rust/library/std/src/sys/pal/toyos/` is not one of these trees.
        assert!(
            toyos_sources_in_dep_info("/x/rust/library/std/src/sys/pal/toyos/mod.rs").is_empty()
        );
    }

    /// Verbatim from run `31370078581`, the run this check exists because of:
    /// the four warnings are real and were what the error had to be told apart
    /// from, and the allowance took it and reported a missing file 83 seconds
    /// later.
    fn run_31370078581() -> Vec<String> {
        [
            "   Compiling panic_unwind v0.0.0 (/home/runner/work/toyos/toyos/rust/library/panic_unwind)",
            "error[E0433]: cannot find `rtabort` in `crate`",
            "  --> library/std/src/sys/pal/toyos/tls.rs:35:26",
            "   |",
            "35 |         Err(_) => crate::rtabort!(\"no TLS block for a dlopen'd module\"),",
            "   |                          ^^^^^^^ could not find `rtabort` in the crate root",
            "warning: unused import: `IntoInner`",
            "  --> library/std/src/os/fd/owned.rs:24:38",
            "warning: unnecessary `unsafe` block",
            "warning: unused variable: `e`",
            "For more information about this error, try `rustc --explain E0433`.",
            "warning: `std` (lib) generated 4 warnings",
            "error: could not compile `std` (lib) due to 1 previous error; 4 warnings emitted",
            "Build completed unsuccessfully in 0:01:23",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn the_first_real_error_is_the_one_reported() {
        let log = run_31370078581();
        let at = compile_error_at(&log).expect("a compile error");
        assert_eq!(log[at], "error[E0433]: cannot find `rtabort` in `crate`");
        assert!(
            log[at + 1].contains("library/std/src/sys/pal/toyos/tls.rs:35"),
            "the file and line have to come with it, or the report is a name and a shrug"
        );
    }

    /// The negative control is the failure the allowance is *for*: a link
    /// failure has no `error[` and no `could not compile`, so it must not be
    /// mistaken for a compile error and stop a build that has its artifacts.
    #[test]
    fn a_link_failure_is_not_a_compile_error() {
        let log = [
            "error: linking with `rust-lld` failed: exit status: 1",
            "  |",
            "  = note: rust-lld: error: undefined symbol: __rust_probestack",
            "Build completed unsuccessfully in 0:00:41",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(compile_error_at(&log), None);
    }

}
