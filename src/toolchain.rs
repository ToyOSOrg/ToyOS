use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::arch::Arch;
use crate::buildlock;
use crate::sysroot::{self, Sysroot, SYSROOT_SOURCES};

/// Which checkout holds the `rust/` submodule.
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

/// One `rust/` submodule per repository, in the primary checkout: a linked
/// worktree's `rust/` is a git worktree of it (`sysroot::fork_checkout`).
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

/// Of the sysroot's sources, the crates std links, which its dep-info names
/// by the path cargo resolved; `sdk/std` it names through the fork.
const STD_SOURCES: [&str; 2] = ["toyos-abi/src", "toyos/src"];

/// Every target a guest artifact is built for: ToyOS userland, the kernel's
/// bare-metal target, and the UEFI bootloader.
pub const GUEST_TARGETS: [GuestTarget; 6] = [
    GuestTarget { arch: Arch::X86_64, role: Role::Userland },
    GuestTarget { arch: Arch::X86_64, role: Role::Kernel },
    GuestTarget { arch: Arch::X86_64, role: Role::Loader },
    GuestTarget { arch: Arch::Aarch64, role: Role::Userland },
    GuestTarget { arch: Arch::Aarch64, role: Role::Kernel },
    GuestTarget { arch: Arch::Aarch64, role: Role::Loader },
];

/// What a guest target's artifacts are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Userland,
    Kernel,
    Loader,
}

/// One of the [`GUEST_TARGETS`].
#[derive(Clone, Copy, Debug)]
pub struct GuestTarget {
    pub arch: Arch,
    pub role: Role,
}

impl GuestTarget {
    pub const fn triple(self) -> &'static str {
        match self.role {
            Role::Userland => self.arch.userland(),
            Role::Kernel => self.arch.kernel(),
            Role::Loader => self.arch.loader(),
        }
    }
}

/// The toolchain directory of a checkout whose toolchain arrived as an artifact
/// ([`Owner::Installed`], `src/release.rs`).
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

/// The text of every dep-info file under `dir`, none if there is no `dir`; one
/// that cannot be read is refused, since its paths would go undecided.
fn collect_dep_info(dir: &Path, out: &mut Vec<String>) {
    match fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("read {}: {e}", dir.display()),
        Ok(_) => dep_info_under(dir, out),
    }
}

fn dep_info_under(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).path();
        if path.is_dir() {
            dep_info_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "d") {
            out.push(fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())));
        }
    }
}

/// Every path one dep-info file names: what was built, and what it read.
fn paths_in_dep_info(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .flat_map(str::split_ascii_whitespace)
        .map(|word| word.strip_suffix(':').unwrap_or(word))
}

/// The paths in one dep-info file that name a `toyos-abi/src` or `toyos/src`
/// source. Split out from the filesystem so the gate below has a negative
/// control that is a string literal.
fn toyos_sources_in_dep_info(text: &str) -> Vec<String> {
    paths_in_dep_info(text)
        .filter(|path| path.ends_with(".rs") && STD_SOURCES.iter().any(|src| path.contains(&format!("/{src}/"))))
        .map(str::to_string)
        .collect()
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
    let foreign: Vec<&String> =
        sources.iter().filter(|p| !Path::new(p).starts_with(&root)).collect();
    assert!(
        foreign.is_empty(),
        "std was compiled against {} sources that are not this worktree's:\n  {}\n\
         `library/std` names them as `../../../`, so the fork checkout that built it is not \
         this worktree's own.",
        foreign.len(),
        foreign.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("\n  "),
    );
}

/// Refuse a freestanding target's libraries whose dep-info under `dep_info`
/// names no source of the std fork `fork`, or any file of the worktree `root`
/// outside it: their key reads nothing else of it but the manifests std's
/// lockfile resolves (`src/sysroot.rs`), so a sysroot of another worktree would
/// carry them unchanged. Every path is decided as the filesystem resolves it,
/// `..` and symlinks followed, and one that resolves to nothing is refused.
pub(crate) fn assert_std_reads_no_worktree(root: &Path, fork: &Path, dep_info: &Path) {
    let resolve = |path: &Path| {
        fs::canonicalize(path)
            .unwrap_or_else(|e| panic!("resolve {}, checking the std build under {}: {e}", path.display(), dep_info.display()))
    };
    let (root, fork) = (resolve(root), resolve(fork));
    let mut deps = Vec::new();
    collect_dep_info(dep_info, &mut deps);
    // Bootstrap's cargo runs rustc in the fork, so a relative path is the fork's.
    let read: BTreeSet<PathBuf> =
        deps.iter().flat_map(|text| paths_in_dep_info(text)).map(|path| resolve(&fork.join(path))).collect();
    assert!(
        read.iter().any(|path| path.starts_with(fork.join("library"))),
        "the freestanding libraries under {} name no source of the fork {} at all, so the check that \
         they read nothing else of the worktree cannot answer. Cargo's dep-info moved.",
        dep_info.display(),
        fork.display(),
    );
    let worktree: Vec<String> = read
        .iter()
        .filter(|path| path.starts_with(&root) && !path.starts_with(&fork))
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        worktree.is_empty(),
        "the freestanding libraries under {} read {} files of the worktree {} outside its fork {}, \
         and their key reads none of its sources:\n  {}",
        dep_info.display(),
        worktree.len(),
        root.display(),
        fork.display(),
        worktree.join("\n  "),
    );
}

/// What an installed toolchain's sysroot was built from, as its install
/// recorded it (`src/release.rs`).
pub(crate) fn witness_path(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-sysroot-witness")
}

/// The `TOOLCHAIN` an installed toolchain was installed with (`src/release.rs`).
pub(crate) fn manifest_path(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/TOOLCHAIN")
}


/// The lines of a witness belonging to `trees`.
fn witness_subset(text: &str, trees: &[&str]) -> String {
    text.lines()
        .filter(|l| trees.iter().any(|t| l.starts_with(t)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Which of [`SYSROOT_SOURCES`] this worktree disagrees with the sysroot about.
fn differing_trees(recorded: Option<&str>, current: &str) -> String {
    let Some(recorded) = recorded else {
        return "nothing recorded what the sysroot was built from".to_string();
    };
    let names: Vec<&str> = SYSROOT_SOURCES
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

/// The `cargo` this machine can lend the `toyos` toolchain.
///
/// **A nightly one if rustup has it, and the host's otherwise — which is what
/// rustup itself falls back to, measured on both machines that matter.** The
/// dev host has a `nightly-<host>` installed and rustup's narration names it
/// (`falling back to ".../nightly-aarch64-apple-darwin/bin/cargo"`, cargo
/// 1.96.0-nightly); every CI runner installs `--profile minimal
/// --default-toolchain stable` and nothing else, so rustup falls back to
/// stable's. Provisioning in that order changes the cargo behind no ToyOS build
/// anywhere: it removes the narration and nothing else.
///
/// The order is not decoration. `-Z` is refused outside the nightly channel, so
/// stable's cargo (1.97.1) and bootstrap's stage0 cargo (1.98.0-beta.2) both
/// refuse the `-Zbuild-std` std type-check `src/CLAUDE.md` documents — measured,
/// both — while the nightly rustup already falls back to accepts it. Picking
/// "the host cargo" flatly would have taken that away from the dev host and
/// called it a cleanup.
///
/// The host's is resolved through `rustc --print sysroot`: it is whatever
/// stable toolchain this machine has, and it is not a path any artifact can
/// know.
fn host_cargo() -> &'static Path {
    static CARGO: OnceLock<PathBuf> = OnceLock::new();
    CARGO.get_or_init(|| {
        if let Some(home) = rustup_home() {
            let nightly = home.join(format!("toolchains/nightly-{}/bin/cargo", host_triple()));
            if nightly.exists() {
                return nightly;
            }
        }
        let cargo = host_sysroot().join("bin/cargo");
        assert!(
            cargo.exists(),
            "there is no cargo at {}, so the toyos toolchain cannot be given one and every \
             cargo invocation under it will narrate a fallback.",
            cargo.display(),
        );
        cargo
    })
}

/// Whether the toolchain's `cargo` is not the one this machine would lend it.
///
/// The question is what the link *points at*, not whether a file is there: a
/// `bin/cargo` that arrived inside the published artifact names a path only the
/// publisher had, and a build that took it for provisioned would keep narrating
/// — or worse, run another platform's binary.
fn cargo_link_stale(stage2: &Path) -> bool {
    fs::read_link(stage2.join("bin/cargo")).ok().as_deref() != Some(host_cargo())
}

/// Put a `cargo` beside the toolchain's `rustc`.
///
/// **A symlink, and what survives the artifact round-trip is this step rather
/// than the link.** `src/release.rs` excludes it from the tarball: it names a
/// path only the publishing runner has. `Owner::Installed` makes it.
pub(crate) fn provision_toolchain_cargo(stage2: &Path) {
    let at = stage2.join("bin/cargo");
    let _ = fs::remove_file(&at);
    std::os::unix::fs::symlink(host_cargo(), &at).unwrap_or_else(|e| {
        panic!("Failed to symlink {} -> {}: {e}", at.display(), host_cargo().display())
    });
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
             provision_toolchain_cargo is the step that puts them there, and it did not.",
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
             bootstrap puts it there when `compiler::config_text` says `lld = true`, and it did not",
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

/// The sysroot this checkout's sources name, held in use: the host's store's
/// (`src/keystore.rs`), made if nobody on the host has made it
/// (`src/sysroot.rs`), or, in a checkout whose toolchain arrived as an
/// artifact, that one.
pub fn ensure(root: &Path, lock: &mut buildlock::Held) -> Sysroot {
    match owner(root) {
        Owner::Us | Owner::Elsewhere(_) => sysroot::ensure(root, &crate::keystore::host(), lock),
        Owner::Installed => {
            let rust_dir = root.join("rust");
            check_installed_toolchain(root, &rust_dir);
            let release = manifest_path(&rust_dir);
            let release = fs::read_to_string(&release).unwrap_or_else(|e| {
                panic!("{}: {e}; an installed toolchain carries the TOOLCHAIN it was installed with", release.display())
            });
            Sysroot::installed(stage2(&rust_dir), &release)
        }
    }
}

/// Everything a checkout may do with a toolchain it did not build: check that
/// it is the one this tree needs, and say what to do when it is not.
fn check_installed_toolchain(root: &Path, rust_dir: &Path) {
    let stage2 = stage2(rust_dir);
    // Recreated rather than shipped: it points into whatever stable toolchain
    // this machine has, which is not a path any artifact can know.
    if cargo_link_stale(&stage2) {
        provision_toolchain_cargo(&stage2);
    }
    assert_toolchain_is_honest(&stage2);

    let want = sysroot::witness(root);
    let recorded = fs::read_to_string(witness_path(rust_dir)).ok();
    assert!(
        recorded.as_deref() == Some(want.as_str()),
        "this checkout and the installed toolchain at {} disagree about {}, so a build \
         here would link its kernel against another tree's struct layouts.\n\
         A runner installs the sysroot its job restored by this tree's key; if that is the one \
         installed, the key reads less than the sysroot is built from (`src/sysroot.rs`).",
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

/// [`x_build`] of a compiler that links the LLVM at `llvm`. On an Apple host
/// rustc strips a Darwin binary by running `rust-objcopy` (`rustc_codegen_ssa`'s
/// `back/link.rs`), the rust workspace strips `lld-wrapper`, and the stage-1
/// sysroot carries no `rust-objcopy` when bootstrap copies none of LLVM's tools
/// (`compiler::config_text`): the build finds that LLVM's `llvm-objcopy` by that name on `PATH`.
pub(crate) fn x_build_compiler(rust_dir: &Path, args: &[&str], what: &str, llvm: &Path) -> (bool, Vec<String>) {
    if !host_triple().ends_with("apple-darwin") {
        return x_build(rust_dir, args, what);
    }
    let strip = toyos_tmpdir::TempDir::new("rust-objcopy");
    let objcopy = llvm.join("bin").join(crate::llvm::APPLE_TOOL);
    std::os::unix::fs::symlink(&objcopy, strip.join("rust-objcopy"))
        .unwrap_or_else(|e| panic!("link {} as rust-objcopy: {e}", objcopy.display()));
    let caller = std::env::var_os("PATH").unwrap_or_else(|| panic!("PATH is unset, and bootstrap finds its tools on it"));
    let path = std::env::join_paths(std::iter::once(strip.to_path_buf()).chain(std::env::split_paths(&caller)))
        .unwrap_or_else(|e| panic!("{} cannot lead PATH: {e}", strip.display()));
    x_build_with(rust_dir, args, what, |command| {
        command.env("PATH", path);
    })
}

/// [`x_build`], with bootstrap's environment what `environment` makes of this
/// process's, less GitHub Actions' `GITHUB_ACTIONS` and `CI`: bootstrap takes
/// `HEAD^1` as the upstream commit whose artifacts to fetch when it sees them,
/// and in this fork that is our own merge, which rust-lang's CI never built.
pub(crate) fn x_build_with(
    rust_dir: &Path,
    args: &[&str],
    what: &str,
    environment: impl FnOnce(&mut Command),
) -> (bool, Vec<String>) {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::{Arc, Mutex};

    let _locks = [
        Restore::holding(&rust_dir.join("Cargo.lock")),
        Restore::holding(&rust_dir.join("library/Cargo.lock")),
    ];
    let x = if rust_dir.join("x").exists() { "./x" } else { "./x.py" };
    let mut command = Command::new(x);
    environment(&mut command);
    command.env_remove("GITHUB_ACTIONS").env_remove("CI");
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
    let pump = |stream: Box<dyn Read + Send>, log: Arc<Mutex<Vec<String>>>| {
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                eprintln!("{line}");
                log.lock().expect("the log outlives both pumps").push(line);
            }
        })
    };
    let out = pump(Box::new(child.stdout.take().expect("piped")), Arc::clone(&log));
    let err = pump(Box::new(child.stderr.take().expect("piped")), Arc::clone(&log));
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
/// `rustc --version --verbose` spawns per build call — 0.118 s each, measured —
/// and they fell inside the windows the build lock now covers.
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

/// The stable host toolchain's sysroot, as `rustc --print sysroot` reports it.
///
/// [`host_cargo`] resolves its answer through it, for the reason its doc gives.
fn host_sysroot() -> PathBuf {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("Failed to run rustc");
    let sysroot = String::from_utf8(output.stdout).expect("rustc prints a path");
    PathBuf::from(sysroot.trim())
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

    /// **A compiler build on an Apple host finds its LLVM's `llvm-objcopy` as
    /// the `rust-objcopy` its stage-1 rustc strips with**, first on its `PATH`;
    /// on any other host it finds none of ours. `./x` here is this test binary,
    /// running [`a_fake_bootstrap_that_strips`].
    #[test]
    fn a_compiler_build_finds_the_llvm_s_objcopy_where_rustc_strips_with_it() {
        let fork = TempDir::new("x-build-strip");
        fs::create_dir_all(fork.join("library")).unwrap();
        for lock in ["Cargo.lock", "library/Cargo.lock"] {
            fs::write(fork.join(lock), "# as committed\n").unwrap();
        }
        fs::write(fork.join(FAKE), "").unwrap();
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), fork.join("x")).unwrap();
        let llvm = fork.join("llvm");
        fs::create_dir_all(llvm.join("bin")).unwrap();
        fs::write(llvm.join("bin").join(crate::llvm::APPLE_TOOL), "the llvm-objcopy").unwrap();

        let args = ["--exact", "toolchain::tests::a_fake_bootstrap_that_strips", "--include-ignored", "--nocapture"];
        let (ok, log) = x_build_compiler(&fork, &args, "a fake bootstrap", &llvm);
        assert!(ok, "the fake bootstrap did not run: {log:?}");
        let found = if host_triple().ends_with("apple-darwin") { "the llvm-objcopy" } else { "none" };
        assert!(log.iter().any(|l| *l == format!("rust-objcopy: {found}")), "{log:?}");
    }

    #[test]
    #[ignore = "the bootstrap `a_compiler_build_finds_the_llvm_s_objcopy_where_rustc_strips_with_it` runs; never runs on its own"]
    fn a_fake_bootstrap_that_strips() {
        assert!(Path::new(FAKE).is_file(), "a_fake_bootstrap_that_strips ran outside a fake fork checkout; it is not a test");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let first = std::env::split_paths(&path).map(|dir| dir.join("rust-objcopy")).find(|tool| tool.exists());
        println!("rust-objcopy: {}", first.map_or("none".to_string(), |tool| fs::read_to_string(tool).unwrap()));
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
    ///
    /// The toolchain was given a cargo once, by hand, in a step the rebuild path
    /// does not run; the 2026-08-14 sysroot rebuild recreated `bin/` without it
    /// and nothing noticed, because nothing asked. This is the asking —
    /// [`assert_toolchain_is_honest`] is this function over the real `bin/`.
    #[test]
    fn a_toolchain_bin_without_cargo_is_one_rustup_narrates() {
        let stage2 = TempDir::new("layout");
        let bin = stage2.join("bin");
        fs::create_dir_all(&bin).unwrap();
        assert_eq!(narrated_binaries(&bin), ["rustc", "cargo"]);

        fs::write(bin.join("rustc"), b"").unwrap();
        assert_eq!(narrated_binaries(&bin), ["cargo"], "the layout every build had until now");
        assert!(cargo_link_stale(&stage2));

        // A link that rode in on the published artifact, naming a path only the
        // publishing runner had. It is *there*, and it is a narrated fallback
        // all the same — which is why the question is what it points at.
        let foreign = Path::new("/a-runner-that-is-not-this-one/bin/cargo");
        std::os::unix::fs::symlink(foreign, bin.join("cargo")).unwrap();
        assert_eq!(narrated_binaries(&bin), ["cargo"], "a dangling proxy is not a cargo");
        assert!(cargo_link_stale(&stage2), "another machine's cargo is not this one's");

        provision_toolchain_cargo(&stage2);
        assert!(narrated_binaries(&bin).is_empty());
        assert!(!cargo_link_stale(&stage2));

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

    /// **Bootstrap never sees GitHub Actions' variables**, whatever its
    /// caller's environment: it takes `HEAD^1`'s artifacts when it does. `./x`
    /// here is this test binary, running [`a_fake_bootstrap_that_reads_ci`].
    #[test]
    fn a_bootstrap_run_sees_no_ci_variables() {
        let fork = TempDir::new("x-build-ci");
        fs::create_dir_all(fork.join("library")).unwrap();
        for lock in ["Cargo.lock", "library/Cargo.lock"] {
            fs::write(fork.join(lock), "# as committed\n").unwrap();
        }
        fs::write(fork.join(FAKE), "").unwrap();
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), fork.join("x")).unwrap();
        let args = ["--exact", "toolchain::tests::a_fake_bootstrap_that_reads_ci", "--include-ignored", "--nocapture"];
        let (ok, log) = x_build_with(&fork, &args, "a fake bootstrap", |command| {
            command.env("GITHUB_ACTIONS", "true").env("CI", "true");
        });
        assert!(ok, "the fake bootstrap did not run: {log:?}");
        assert!(log.iter().any(|l| l == "GITHUB_ACTIONS none, CI none"), "{log:?}");
    }

    #[test]
    #[ignore = "the bootstrap `a_bootstrap_run_sees_no_ci_variables` runs; never runs on its own"]
    fn a_fake_bootstrap_that_reads_ci() {
        assert!(Path::new(FAKE).is_file(), "a_fake_bootstrap_that_reads_ci ran outside a fake fork checkout; it is not a test");
        let read = |name| std::env::var(name).unwrap_or_else(|_| "none".to_string());
        println!("GITHUB_ACTIONS {}, CI {}", read("GITHUB_ACTIONS"), read("CI"));
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

        // std's ToyOS backend is not one of these trees.
        assert!(
            toyos_sources_in_dep_info("library/std/src/sys/pal/../../../../../../sdk/std/sys/pal/mod.rs").is_empty()
        );
    }

    /// **Freestanding libraries whose dep-info names the worktree outside its
    /// fork are refused, however the path is spelt**: their key reads none of
    /// it, so a sysroot of another worktree would carry them. The fork's files
    /// and what lies outside the worktree are what they are built from.
    /// Dep-info that names no source of the fork, or a file that is not there,
    /// cannot be checked and is refused too.
    #[test]
    fn freestanding_libraries_that_read_the_worktree_are_refused() {
        let temp = TempDir::new("freestanding-dep-info");
        let base = fs::canonicalize(&*temp).unwrap();
        let (root, registry) = (base.join("worktree"), base.join("registry/compiler_builtins/src/lib.rs"));
        let fork = root.join("rust");
        let built = fork.join("build/toyos-std/host/stage0-std/x86_64-unknown-none");
        let out = built.join("dist/build/core/1/out");
        let file = |path: &Path| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "read").unwrap();
            path.display().to_string()
        };
        file(&fork.join("library/core/src/lib.rs"));
        file(&fork.join("library/stdarch/crates/core_arch/src/mod.rs"));
        let (registry, rlib, raw) = (file(&registry), file(&built.join("dist/libcore.rlib")), file(&out.join("libcore-1.rlib")));
        // Cargo's dep-info names a file whole; rustc's, a fork source by its path in the fork.
        let arch = "library/core/src/../../stdarch/crates/core_arch/src/mod.rs";
        fs::write(built.join("dist/libcore.d"), format!("{rlib}: {}/{arch} {registry}\n", fork.display())).unwrap();
        let rustc = format!("{raw}: library/core/src/lib.rs {arch}\n\nlibrary/core/src/lib.rs:\n# env-dep:CARGO_PKG_NAME=core\n");
        fs::write(out.join("core-1.d"), rustc).unwrap();
        assert_std_reads_no_worktree(&root, &fork, &built);

        let refusal = |dep_info: &Path, what: &str| {
            let refused = std::panic::catch_unwind(|| assert_std_reads_no_worktree(&root, &fork, dep_info));
            *refused.err().unwrap_or_else(|| panic!("{what} was taken")).downcast::<String>().expect("a formatted refusal")
        };
        let naming = |named: &str| {
            fs::write(built.join("dist/other.d"), format!("{rlib}: {named}\n")).unwrap();
            refusal(&built, &format!("dep-info naming {named}"))
        };
        let system = file(&root.join("system.toml"));
        for read in [file(&root.join("toyos-abi/src/lib.rs")), file(&root.join("userland/libc/src/lib.rs")), system.clone()] {
            let said = naming(&read);
            assert!(said.contains(&read), "{said}");
        }
        let dotted = "library/core/src/../../../../system.toml";
        for spelt in [format!("{}/{dotted}", fork.display()), dotted.to_string()] {
            let said = naming(&spelt);
            assert!(said.contains(&system), "{spelt}: {said}");
        }
        std::os::unix::fs::symlink(&system, fork.join("library/core/src/linked.rs")).unwrap();
        let said = naming("library/core/src/linked.rs");
        assert!(said.contains(&system), "{said}");
        let said = naming("library/core/src/gone.rs");
        assert!(said.starts_with(&format!("resolve {}/library/core/src/gone.rs", fork.display())), "{said}");

        let said = refusal(&built.join("none"), "a target directory without dep-info");
        assert!(said.contains("name no source of the fork"), "{said}");
    }

    /// **Dep-info that cannot be read is refused, never skipped**, since the
    /// paths in it would go undecided; only a directory that is not there
    /// holds none.
    #[test]
    fn dep_info_that_cannot_be_read_is_refused() {
        let temp = TempDir::new("dep-info-unread");
        let mut none = Vec::new();
        collect_dep_info(&temp.join("none"), &mut none);
        assert!(none.is_empty(), "{none:?}");

        let built = temp.join("x86_64-unknown-none");
        let dist = built.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let said = |dir: &Path, what: &str| {
            let refused = std::panic::catch_unwind(|| collect_dep_info(dir, &mut Vec::new()));
            *refused.err().unwrap_or_else(|| panic!("{what} was skipped")).downcast::<String>().expect("a formatted refusal")
        };
        let d = dist.join("core.d");
        fs::write(&d, b"libcore.rlib: library/core/src/lib.rs \xff\n").unwrap();
        let refused = said(&built, "dep-info that is not UTF-8");
        assert!(refused.starts_with(&format!("read {}", d.display())), "{refused}");
        fs::remove_file(&d).unwrap();

        // A file where the directory is, not a mode: root reads through any
        // mode, and nobody lists a file.
        fs::remove_dir(&dist).unwrap();
        fs::write(&dist, b"").unwrap();
        let refused = said(&dist, "a directory that cannot be read");
        assert!(refused.starts_with(&format!("read {}", dist.display())), "{refused}");
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
