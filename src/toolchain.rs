use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::arch::Arch;
use crate::buildlock;
use crate::buildlock::Scope;
use crate::sysroot::{self, Sysroot, SYSROOT_SOURCES};

/// Whether the primary's compiler needs a bootstrap. `invalidate_hosted`
/// separates "the compiler changed" from "its `rustc` does not run": only the
/// first makes the ToyOS-hosted rustc stale, and rebuilding that one costs
/// minutes.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Bootstrap {
    invalidate_hosted: bool,
}

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

/// One `rust/` per repository, in the primary checkout, and every worktree
/// compiles against it.
///
/// Not a policy — an affordance. A second checkout of that submodule is a
/// 913 MiB clone (git gives a linked worktree its own, sharing no objects), and
/// a second `build/` beside it is 47 GiB. `git worktree add` leaves `rust/` an
/// empty stub, and leaving it empty is what keeps `git status` clean: git
/// refuses a symlink where a gitlink belongs, and errors out of every command
/// rather than just that one.
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

/// The one ToyOS the hosted rustc (`system.toml`'s `hosted-rustc`) is built to
/// run on.
pub const HOSTED_ARCH: Arch = Arch::X86_64;

/// The primary's compiler, which every sysroot is cloned from and compiled by.
pub(crate) fn stage2(rust_dir: &Path) -> PathBuf {
    rust_dir.join(format!("build/{}/stage2", host_triple()))
}

/// The ToyOS-hosted rustc's toolchain directory, beside the primary's compiler.
fn hosted_stage2(rust_dir: &Path) -> PathBuf {
    rust_dir.join(format!("build/{}/stage2", HOSTED_ARCH.userland()))
}

/// Whether the primary builds the hosted rustc: a build whose config ships it
/// `asked`, and `rustc` is not there or `stamp`, which says it is this
/// compiler's, is not.
fn hosted_rustc_owed(asked: bool, stamp: &Path, rustc: &Path) -> bool {
    asked && (!stamp.exists() || !rustc.exists())
}

/// Remove the hosted rustc a compiler rebuild left stale, and its `stamp`: no
/// build reads one until a config that ships it asks, and that build makes it
/// anew.
fn forget_hosted_rustc(rust_dir: &Path, stamp: &Path) {
    let gone = |path: &Path, removed: std::io::Result<()>| match removed {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove {}: {e}", path.display()),
        _ => {}
    };
    gone(stamp, fs::remove_file(stamp));
    let stale = hosted_stage2(rust_dir);
    gone(&stale, fs::remove_dir_all(&stale));
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
/// The host's is resolved through `rustc --print sysroot` for the same reason
/// [`link_host_target`] does: it is whatever stable toolchain this machine has,
/// and it is not a path any artifact can know.
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
/// path only the publishing runner has. `Owner::Installed` makes it, exactly as
/// it makes the host target.
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
             bootstrap puts it there when `write_config` says `lld = true`, and it did not",
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

/// Whether the primary's toolchain lacks what bootstrap does not put there:
/// `stage2`'s cargo and clang, or the host target in the hosted rustc's sysroot.
fn incomplete(rust_dir: &Path) -> bool {
    let stage2 = stage2(rust_dir);
    cargo_link_stale(&stage2) || crate::clang::defect(&stage2).is_some() || host_target_missing(rust_dir)
}

/// Give the primary's toolchain what [`incomplete`] finds missing, its clang by
/// `provision_clang`.
fn complete(rust_dir: &Path, provision_clang: impl FnOnce(&Path)) {
    let stage2 = stage2(rust_dir);
    if cargo_link_stale(&stage2) {
        provision_toolchain_cargo(&stage2);
    }
    if crate::clang::defect(&stage2).is_some() {
        provision_clang(&stage2);
    }
    if host_target_missing(rust_dir) {
        link_host_target(rust_dir);
    }
}

/// Run `bootstrap` in the primary's `rust/` against the LLVM at `llvm`, then
/// remove the LLVM its build directory built itself (`llvm::retire_in_tree`)
/// and [`complete`] what it reassembled. Called inside the act that holds the
/// global lock exclusively.
///
/// **In the same hold, because bootstrap recreates `stage2` without its cargo
/// and clang**: a completion under a hold of its own queues behind every sysroot
/// build that takes the lock shared in between, and those last minutes.
fn reassemble(rust_dir: &Path, llvm: &Path, bootstrap: impl FnOnce()) {
    bootstrap();
    crate::llvm::retire_in_tree(&rust_dir.join("build"));
    complete(rust_dir, |stage2| crate::clang::provision(stage2, llvm));
}

/// [`reassemble`] the primary's compiler with `bootstrap`, with nothing recording
/// which compiler `stage2` is until it is whole: a bootstrap that is stopped is
/// run again by the primary, and refused by name in every linked worktree.
fn rebuild_compiler(rust_dir: &Path, llvm: &Path, bootstrap: impl FnOnce()) {
    crate::compiler::forget(rust_dir);
    reassemble(rust_dir, llvm, bootstrap);
    crate::compiler::record(rust_dir);
}

/// What the primary bootstraps: a new compiler when `stage2` is not the one its
/// fork checkout names, and the same one again when its `rustc` does not run.
/// A `stage2` that runs and has no rustup link, as one a runner restored, is
/// linked, not rebuilt.
fn bootstrap(current: bool, runs: bool) -> Option<Bootstrap> {
    if !current {
        Some(Bootstrap { invalidate_hosted: true })
    } else {
        (!runs).then_some(Bootstrap { invalidate_hosted: false })
    }
}

/// Whether the `rustc` in `stage2` runs.
fn runs(stage2: &Path) -> bool {
    Command::new(stage2.join("bin/rustc"))
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Ensure the toolchain is up to date, and return the sysroot this checkout's
/// sources name — made if nobody has made it (`src/sysroot.rs`). The primary
/// builds the ToyOS-hosted rustc only for a build whose config ships it
/// (`hosted_rustc`).
///
/// Every step decides under the caller's shared lock and acts under the
/// exclusive one, so the common answer — nothing to do — costs no
/// serialisation, and two agents cannot both conclude the compiler is stale
/// and both start `x.py build` in the same directory. That pair is what left a
/// half-written `librustc_driver` for cargo to probe, and cargo memoises a
/// failed probe (`issues/build/`).
///
/// The steps are ordered, and each invalidates what it makes stale rather than
/// threading a `rebuilt` flag through: a step that decides for itself still
/// decides correctly when the process before it was killed halfway.
///
/// Only the primary checkout builds the compiler. Every checkout builds the
/// sysroot its own sources name, in its own fork checkout.
///
/// **The lock only covers builds routed through here.** A `./x.py build` typed
/// by hand in `rust/` takes no lock at all, and can still lose the race this
/// serialises: one builder's bootstrap removes and recreates
/// `stage1-std/<target>/dist/deps` while another's `rustc` creates a temp file
/// inside it, and the loser dies compiling `core` with `couldn't create a temp
/// dir: No such file or directory`.
pub fn ensure(root: &Path, lock: &mut buildlock::Held, hosted_rustc: bool) -> Sysroot {
    let stamps_dir = root.join("target/stamps");
    fs::create_dir_all(&stamps_dir).ok();

    let rust_dir = match owner(root) {
        Owner::Elsewhere(primary) => {
            let rust_dir = primary.join("rust");
            assert!(
                stage2(&rust_dir).join("bin/rustc").exists(),
                "there is no compiler to build with: {} does not exist.\n\
                 The primary checkout builds it — run `cargo run -- --build-only` in {} first.",
                stage2(&rust_dir).display(),
                primary.display()
            );
            return sysroot::ensure(root, &rust_dir, lock);
        }
        Owner::Installed => {
            let rust_dir = root.join("rust");
            check_installed_toolchain(root, &rust_dir);
            let release = manifest_path(&rust_dir);
            let release = fs::read_to_string(&release).unwrap_or_else(|e| {
                panic!("{}: {e}; an installed toolchain carries the TOOLCHAIN it was installed with", release.display())
            });
            return Sysroot::installed(stage2(&rust_dir), &release);
        }
        Owner::Us => sysroot::fork_checkout(root, lock),
    };
    let hosted_stamp = stamps_dir.join("hosted-rustc.stamp");
    lock.act_if(
        Scope::Global,
        "build the rust toolchain",
        || bootstrap(crate::compiler::primary_is_current(&rust_dir), runs(&stage2(&rust_dir))),
        |kind| {
            eprintln!("Building full toolchain (this takes a while on first run)...");
            let llvm = crate::llvm::resolve(root, &rust_dir, &rust_dir);
            rebuild_compiler(&rust_dir, &llvm.dir, || full_bootstrap(&rust_dir, &llvm.dir));
            if kind.invalidate_hosted {
                forget_hosted_rustc(&rust_dir, &hosted_stamp);
            }
        },
    );

    let hosted = hosted_stage2(&rust_dir).join("bin/rustc");
    lock.act_if(
        Scope::Global,
        "build the ToyOS-hosted rustc",
        || hosted_rustc_owed(hosted_rustc, &hosted_stamp, &hosted).then_some(()),
        |()| {
            let llvm = crate::llvm::resolve(root, &rust_dir, &rust_dir);
            reassemble(&rust_dir, &llvm.dir, || build_hosted_rustc(&rust_dir, &llvm.dir));
            assert!(hosted.exists(), "Failed to build hosted rustc");
            fs::write(&hosted_stamp, "").unwrap();
        },
    );

    let stage2 = stage2(&rust_dir);
    lock.act_if(
        Scope::Global,
        "link the toyos rustup toolchain",
        || link_stale(&stage2).then_some(()),
        |()| {
            let status = Command::new("rustup")
                .args(["toolchain", "link", "toyos", stage2.to_str().unwrap()])
                .status()
                .unwrap_or_else(|e| panic!("Failed to run rustup: {e}"));
            assert!(status.success(), "rustup toolchain link failed");
        },
    );

    lock.act_if(
        Scope::Global,
        "complete the toyos toolchain",
        || incomplete(&rust_dir).then_some(()),
        |()| complete(&rust_dir, |stage2| crate::clang::provision(stage2, &crate::llvm::resolve(root, &rust_dir, &rust_dir).dir)),
    );
    assert_toolchain_is_honest(&stage2);

    sysroot::ensure(root, &rust_dir, lock)
}

/// Everything a checkout may do with a toolchain it did not build: check that
/// it is the one this tree needs, and say what to do when it is not.
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

    // Recreated rather than shipped: both of these point into whatever stable
    // toolchain this machine has, which is not a path any artifact can know.
    // Its clang is the artifact's own, so this is not `complete`.
    if host_target_missing(rust_dir) {
        link_host_target(rust_dir);
    }
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

/// Whether the `toyos` rustup toolchain points anywhere other than `stage2`.
///
/// `rustup toolchain link` unlinks and recreates the symlink rather than
/// replacing it atomically, so every call opens a window in which
/// `~/.rustup/toolchains/toyos` does not resolve. Any concurrent `rustc` proxy
/// invocation landing in that window dies with `'rustc' is not installed for the
/// custom toolchain 'toyos'` — which reads as a broken toolchain rather than as
/// contention, because a probe run a moment later succeeds.
///
/// This ran unconditionally on every `ensure`, i.e. every build. With five
/// agents building in one tree it cost one of them eleven consecutive
/// `cargo test` invocations over about fifteen minutes, while
/// `RUSTUP_TOOLCHAIN=toyos rustc --version` succeeded 20 out of 20 between the
/// attempts.
///
/// A mismatched or absent link still re-links, so a moved tree or a fresh clone
/// behaves as before; only the no-op case is skipped.
///
/// Reached only from the primary checkout, which is what makes the window above
/// a window of one: a linked worktree that re-linked would point the name at a
/// stage2 nobody else has.
fn link_stale(stage2: &Path) -> bool {
    rustup_link().is_none_or(|current| current != stage2)
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
/// ([`LEAN`]): the build finds that LLVM's `llvm-objcopy` by that name on `PATH`.
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

/// What an `x build` failure the artifact check is willing to tolerate actually
/// said, so that "expected" is a claim the reader can check.
fn tolerated_failure(log: &[String], what: &str) {
    let errors: Vec<&str> =
        log.iter().map(String::as_str).filter(|l| l.trim_start().starts_with("error")).collect();
    eprintln!(
        "Note: {what} exited non-zero and the artifacts it must produce are all there, so this \
         is the ToyOS rustdoc link failure. It reported:\n{}",
        if errors.is_empty() {
            "  (no line beginning `error`)".to_string()
        } else {
            errors.join("\n")
        }
    );
}

/// What the primary's compiler build builds: rustc and the host's libraries,
/// which build scripts and proc macros link. No rustdoc: no build runs one.
const COMPILER_BUILD: [&str; 7] = ["build", "--stage", "2", "--warnings", "warn", "compiler/rustc", "library"];

fn full_bootstrap(rust_dir: &Path, llvm: &Path) {
    // Ensure library/backtrace is checked out — std depends on it.
    // Other rust submodules (llvm, docs, cargo) are handled by bootstrap on demand.
    crate::ensure_submodule(rust_dir, "library/backtrace");

    let host = host_triple();
    write_config(rust_dir, &host, false, llvm);

    // Clean cached std for all ToyOS targets so bootstrap picks up compiler changes
    // (e.g. target spec changes like default_uwtable that affect codegen).
    for target in GUEST_TARGETS {
        let stage1_std = rust_dir.join(format!("build/{host}/stage1-std/{}", target.triple()));
        if stage1_std.exists() {
            fs::remove_dir_all(&stage1_std).ok();
        }
    }

    let (ok, log) = x_build_compiler(rust_dir, &COMPILER_BUILD, "the toolchain", llvm);
    refuse_on_compile_error(&log, "the toolchain");
    assert!(ok, "the toolchain build failed, and nothing in its output was a compile error");
}

fn build_hosted_rustc(rust_dir: &Path, llvm: &Path) {
    eprintln!("Building ToyOS-hosted rustc...");
    let host = host_triple();
    write_config(rust_dir, &host, true, llvm);

    let (ok, log) =
        x_build_compiler(rust_dir, &["build", "--stage", "2", "--warnings", "warn"], "the hosted rustc", llvm);
    refuse_on_compile_error(&log, "the hosted rustc");

    // rustdoc for ToyOS may fail to link; rustc and librustc_driver may not.
    let toyos_stage2 = rust_dir.join(format!("build/{}/stage2", HOSTED_ARCH.userland()));
    assert!(
        toyos_stage2.join("bin/rustc").exists(),
        "the hosted rustc build failed and {} is not there.\n\
         Nothing in its output was a compile error, so this is a link or a bootstrap failure.",
        toyos_stage2.join("bin/rustc").display()
    );
    assert!(
        fs::read_dir(toyos_stage2.join("lib"))
            .map(|d| d.filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with("librustc_driver")))
            .unwrap_or(false),
        "the hosted rustc build failed: librustc_driver*.so is not in {}",
        toyos_stage2.join("lib").display()
    );
    if !ok {
        tolerated_failure(&log, "the hosted rustc build");
    }

    // That build reassembled the host's `stage2` without `rust-lld`
    // (`write_config` says why), so the host-only build runs once more to put
    // it back: everything it would compile is already built.
    write_config(rust_dir, &host, false, llvm);
    let (ok, log) = x_build_compiler(rust_dir, &COMPILER_BUILD, "the toolchain, reassembled", llvm);
    refuse_on_compile_error(&log, "the toolchain, reassembled");
    assert!(
        rust_lld(&stage2(rust_dir)).is_file(),
        "the toolchain's reassembly after the hosted rustc left no {}",
        rust_lld(&stage2(rust_dir)).display()
    );
    if !ok {
        tolerated_failure(&log, "the toolchain's reassembly");
    }
}

/// `bootstrap.toml` for the host-only toolchain, every compiler's
/// (`compiler::config_text`), or with the ToyOS-hosted rustc.
///
/// `lld = true` is what puts `rust-lld` in every stage's sysroot, where rustc
/// finds the linker every guest target names. The hosted rustc's build cannot
/// have it: bootstrap would then build LLD for the ToyOS host from C++, which
/// nothing here can compile yet. Every assemble removes the host's
/// `stage2` first, so [`build_hosted_rustc`] reassembles it under the host-only
/// config after.
///
/// `clang::LLVM_CONFIG` is the `[llvm]` both builds share, and both link the LLVM
/// at `llvm` (`src/llvm.rs`), whose LLD every guest target names by path.
///
/// The host's `default-linker-linux-override` is pinned off because bootstrap
/// otherwise ties it to `lld` for `x86_64-unknown-linux-gnu`, and a host rustc
/// whose build environment flips with the config is rebuilt by each of those
/// two builds.
///
/// The host-only toolchain builds no guest target's libraries: every sysroot
/// builds its own (`src/sysroot.rs`).
fn write_config(rust_dir: &Path, host: &str, with_hosted_rustc: bool, llvm: &Path) {
    let config = if with_hosted_rustc {
        let targets = std::iter::once(host)
            .chain(GUEST_TARGETS.map(GuestTarget::triple))
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"change-id = "ignore"
profile = "compiler"

[build]
host = ["{host}", "{hosted}"]
target = [{targets}]

[llvm]
{llvm}

[rust]
incremental = true
lld = false
{LEAN}

[target.{host}]
{HOST_LINKER_PIN}
{external}

{userland}"#,
            hosted = HOSTED_ARCH.userland(),
            llvm = crate::clang::LLVM_CONFIG,
            external = crate::llvm::host_lines(llvm),
            userland = hosted_targets(llvm),
        )
    } else {
        crate::compiler::config_text(&rust_dir.join("build"), host, llvm)
    };
    fs::write(rust_dir.join("bootstrap.toml"), config).unwrap();
}

/// The `[target]` sections of ToyOS userland in the hosted rustc's build: each
/// links with the LLD of the LLVM at `llvm`, and the one the hosted rustc runs
/// on builds it.
fn hosted_targets(llvm: &Path) -> String {
    Arch::ALL
        .iter()
        .map(|arch| {
            let linker = format!("linker = \"{}\"", llvm.join("bin/lld").display());
            let hosted = if *arch == HOSTED_ARCH {
                // Cranelift because no LLVM is built for a ToyOS host yet, and
                // only for that reason: the hosted rustc carries LLVM once clang
                // and libc++ run on ToyOS, and Cranelift is not where the
                // compiler that builds ToyOS goes.
                //
                // The archiver is that LLVM's too: the compiler's crates carry
                // C built for this target (blake3's assembly), and a host `ar`
                // that indexes only its own object format, as macOS's does,
                // leaves those ELF members out of the index lld pulls from.
                format!(
                    "\nar = \"{}\"\ncodegen-backends = [\"cranelift\"]",
                    llvm.join("bin/llvm-ar").display(),
                )
            } else {
                String::new()
            };
            format!("[target.{}]\n{linker}{hosted}\nrpath = false\n\n", arch.userland())
        })
        .collect()
}

/// What the host rustc links its own binaries with, held to one answer in every
/// `bootstrap.toml` that builds a host compiler: [`write_config`] says why.
pub(crate) const HOST_LINKER_PIN: &str = "default-linker-linux-override = \"off\"";

/// The `[rust]` options every `bootstrap.toml` that builds a compiler shares
/// beyond its profile's: no LLVM tool copied into the compiler's sysroot, since
/// `clang::provision` puts there the ones a build runs; no debuginfo in rustc,
/// which no build reads; and no codegen test, for which bootstrap demands
/// LLVM's `FileCheck` beside `llvm-config` (`src/bootstrap/src/core/sanity.rs`).
pub(crate) const LEAN: &str = "llvm-tools = false\ndebuginfo-level-rustc = 0\ncodegen-tests = false";

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
/// Both [`host_cargo`] and [`link_host_target`] resolve their answer through it
/// for the one reason [`host_cargo`]'s doc gives: it is whatever stable
/// toolchain this machine has, and that is not a path any artifact can name.
fn host_sysroot() -> PathBuf {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("Failed to run rustc");
    let sysroot = String::from_utf8(output.stdout).expect("rustc prints a path");
    PathBuf::from(sysroot.trim())
}

/// Whether the ToyOS sysroot is missing the host target proc-macros compile against.
fn host_target_missing(rust_dir: &Path) -> bool {
    let toyos_sysroot = rust_dir.join(format!("build/{}/stage2/lib/rustlib", HOSTED_ARCH.userland()));
    toyos_sysroot.exists() && !toyos_sysroot.join(host_triple()).exists()
}

fn link_host_target(rust_dir: &Path) {
    let host = host_triple();
    let host_target_dir = rust_dir
        .join(format!("build/{}/stage2/lib/rustlib", HOSTED_ARCH.userland()))
        .join(&host);

    let source = host_sysroot().join("lib/rustlib").join(&host);
    assert!(
        source.exists(),
        "Host target {} not found in stable toolchain at {}",
        host,
        source.display()
    );

    std::os::unix::fs::symlink(&source, &host_target_dir).unwrap_or_else(|e| {
        panic!(
            "Failed to symlink {} -> {}: {}",
            host_target_dir.display(),
            source.display(),
            e
        )
    });
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

    /// Every build links the host's LLVM, copies none of its tools and leaves
    /// rustc without debuginfo; the host-only one builds no guest target, and
    /// in the hosted rustc's each guest links through that LLVM's LLD, named
    /// by path.
    #[test]
    fn every_build_links_the_host_s_llvm_and_names_its_lld_by_path() {
        let rust_dir = TempDir::new("lld-config");
        let llvm = rust_dir.join("build/llvm/k");
        let lld = format!("linker = \"{}\"", llvm.join("bin/lld").display());
        let lean = "\nllvm-tools = false\ndebuginfo-level-rustc = 0\ncodegen-tests = false\n";
        for (hosted, lld_flag) in [(true, "false"), (false, "true")] {
            write_config(&rust_dir, "h", hosted, &llvm);
            let config = fs::read_to_string(rust_dir.join("bootstrap.toml")).unwrap();
            assert!(config.contains(&format!("\n[rust]\nincremental = true\nlld = {lld_flag}{lean}")), "{config}");
            assert_eq!(config.contains(&lld), hosted, "{config}");
            assert!(!config.contains("\"rust-lld\""), "{config}");
            assert_eq!(config.contains("\ntarget = [\"h\"]\n"), !hosted, "{config}");
            let host = format!(
                "[target.h]\ndefault-linker-linux-override = \"off\"\nllvm-config = \"{}/bin/llvm-config\"\nllvm-has-rust-patches = true\n",
                llvm.display()
            );
            assert!(config.contains(&host), "{config}");
            if !hosted {
                let keyed = crate::compiler::config_text(&rust_dir.join("build"), "h", &llvm);
                assert_eq!(config, keyed, "the primary's compiler is built under a configuration its key does not read");
            }
        }
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

    /// **A bootstrap leaves the primary nothing that waits on another
    /// worktree's sysroot build**: the act that reassembles `stage2` completes it
    /// before its exclusive hold ends, so the step after it — run while a
    /// sysroot build holds the lock shared — decides it has nothing to do, and
    /// takes no lock.
    #[test]
    fn a_bootstrap_leaves_nothing_to_wait_on_a_sysroot_build_for() {
        let rust_dir = TempDir::new("no-wait");
        let stage2 = stage2(&rust_dir);
        let llvm = store_llvm(&rust_dir);
        reassemble(&rust_dir, &llvm, || bootstrapped(&rust_dir));
        assert!(
            !incomplete(&rust_dir),
            "a bootstrap let its exclusive hold go with a global step left, which the primary's \
             build then queues for behind every sysroot build"
        );
        assert_eq!(toolchain_defect(&stage2), None, "a bootstrap let its exclusive hold go with stage2 not whole");
    }

    /// The LLVM `clang::provision` reads, in `rust_dir`'s store.
    fn store_llvm(rust_dir: &Path) -> PathBuf {
        let llvm = rust_dir.join("build/llvm/k");
        let files = [
            ("bin/clang", "clang"),
            ("bin/llvm-ar", "llvm-ar"),
            ("bin/llvm-objcopy", "llvm-objcopy"),
            ("lib/clang/22/include/stddef.h", "stddef"),
        ];
        for (file, text) in files {
            fs::create_dir_all(llvm.join(file).parent().unwrap()).unwrap();
            fs::write(llvm.join(file), text).unwrap();
        }
        llvm
    }

    /// What bootstrap leaves in `rust_dir`: `stage2` made again, with `rustc`
    /// and `rust-lld` and none of cargo, clang or an LLVM tool; and the hosted
    /// rustc's sysroot without the host target.
    fn bootstrapped(rust_dir: &Path) {
        let stage2 = stage2(rust_dir);
        let _ = fs::remove_dir_all(&stage2);
        for file in [stage2.join("bin/rustc"), rust_lld(&stage2)] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"").unwrap();
        }
        let hosted = rust_dir.join(format!("build/{}/stage2/lib/rustlib", HOSTED_ARCH.userland()));
        fs::create_dir_all(&hosted).unwrap();
    }

    /// What a build directory holds of an LLVM of its own: bootstrap's LLVM and
    /// LLD.
    fn in_tree_llvm(rust_dir: &Path) -> [PathBuf; 2] {
        let host = rust_dir.join("build").join(host_triple());
        let own = [host.join("llvm"), host.join("lld")];
        for dir in &own {
            fs::create_dir_all(dir.join("bin")).unwrap();
            fs::write(dir.join("bin/tool"), "a tool").unwrap();
        }
        own
    }

    /// **A primary whose compiler is built against the store keeps no LLVM of
    /// its own**: the rebuild that links the store removes the one its build
    /// directory built.
    #[test]
    fn a_rebuilt_compiler_leaves_no_llvm_of_its_own() {
        let scratch = TempDir::new("in-tree-llvm");
        let (_primary, rust_dir, _) = crate::compiler::tests::estate(&scratch);
        let own = in_tree_llvm(&rust_dir);
        let llvm = store_llvm(&rust_dir);
        rebuild_compiler(&rust_dir, &llvm, || bootstrapped(&rust_dir));
        for dir in own {
            assert!(!dir.exists() && !dir.with_extension("swept").exists(), "{} outlived the rebuild", dir.display());
        }
        assert_eq!(toolchain_defect(&stage2(&rust_dir)), None);
    }

    /// **Landing the store moves an existing primary onto it**: a primary whose
    /// record was written before its compiler linked the host's LLVM is not
    /// current, so its next build bootstraps, and that rebuild removes the LLVM
    /// and LLD its build directory built. Its LLVM checkout sitting at a commit
    /// its gitlink does not name changes neither answer.
    #[test]
    fn a_primary_recorded_before_the_store_is_rebuilt_onto_it() {
        let scratch = TempDir::new("store-migration");
        let (_primary, rust_dir, _) = crate::compiler::tests::estate(&scratch);
        crate::compiler::tests::llvm_checkout(&rust_dir);
        assert!(crate::compiler::primary_is_current(&rust_dir));
        crate::compiler::tests::record_before_the_store(&rust_dir);
        let own = in_tree_llvm(&rust_dir);
        let kind = bootstrap(crate::compiler::primary_is_current(&rust_dir), true);
        assert!(kind.is_some_and(|k| k.invalidate_hosted), "a primary recorded before the store was taken for current");
        rebuild_compiler(&rust_dir, &store_llvm(&rust_dir), || bootstrapped(&rust_dir));
        assert!(own.iter().all(|dir| !dir.exists()), "the rebuild kept the LLVM its build directory built");
        assert!(crate::compiler::primary_is_current(&rust_dir), "the rebuild recorded a compiler that is not current");
    }

    /// **A stopped bootstrap is run again**: nothing records which compiler
    /// `stage2` is while one runs, so the primary's next build is not told the
    /// old one is current; and the LLVM the old one linked stays.
    #[test]
    fn a_stopped_bootstrap_leaves_no_record() {
        let rust_dir = TempDir::new("stopped");
        let record = crate::compiler::primary_record(&rust_dir);
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        fs::write(&record, "the compiler before").unwrap();
        let own = in_tree_llvm(&rust_dir);
        let stopped = std::panic::catch_unwind(|| rebuild_compiler(&rust_dir, Path::new("no-llvm"), || panic!("stopped")));
        assert!(stopped.is_err());
        assert!(!record.exists(), "a stopped bootstrap left the record of the compiler before it");
        assert!(own.iter().all(|dir| dir.join("bin/tool").is_file()), "a stopped bootstrap took the LLVM its compiler linked");
    }

    /// **The hosted rustc is built only for a build whose config ships it**,
    /// and then only when the one this compiler built is not there.
    #[test]
    fn the_hosted_rustc_is_built_only_when_a_build_ships_it() {
        let rust_dir = TempDir::new("hosted-owed");
        let stamp = rust_dir.join("stamps/hosted-rustc.stamp");
        let rustc = hosted_stage2(&rust_dir).join("bin/rustc");
        assert!(!hosted_rustc_owed(false, &stamp, &rustc), "a build that ships no hosted rustc built one");
        assert!(hosted_rustc_owed(true, &stamp, &rustc));
        for file in [&stamp, &rustc] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "").unwrap();
        }
        assert!(!hosted_rustc_owed(true, &stamp, &rustc), "a hosted rustc this compiler built was built again");
        fs::remove_file(&stamp).unwrap();
        assert!(hosted_rustc_owed(true, &stamp, &rustc), "a hosted rustc of another compiler was taken");
    }

    /// **A compiler rebuild leaves no hosted rustc of the compiler it
    /// replaced**, nor its stamp; with neither there, that is no error.
    #[test]
    fn a_rebuilt_compiler_leaves_no_hosted_rustc_of_the_one_before() {
        let rust_dir = TempDir::new("hosted-stale");
        let stamp = rust_dir.join("stamps/hosted-rustc.stamp");
        let rustc = hosted_stage2(&rust_dir).join("bin/rustc");
        for file in [&stamp, &rustc] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "").unwrap();
        }
        forget_hosted_rustc(&rust_dir, &stamp);
        assert!(!stamp.exists() && !hosted_stage2(&rust_dir).exists(), "the old compiler's hosted rustc stayed");
        forget_hosted_rustc(&rust_dir, &stamp);
    }

    /// **The primary bootstraps a new compiler exactly when its `stage2` is not
    /// current, and otherwise only when rustup has none.**
    #[test]
    fn the_primary_bootstraps_when_stale_or_missing() {
        let new = Some(Bootstrap { invalidate_hosted: true });
        let again = Some(Bootstrap { invalidate_hosted: false });
        for (current, runs, want) in [
            (true, true, None),
            (true, false, again),
            (false, true, new),
            (false, false, new),
        ] {
            assert_eq!(bootstrap(current, runs), want, "current {current}, runs {runs}");
        }
    }

    /// **What decides a rebuild of a current `stage2` is whether its `rustc`
    /// runs**, not whether rustup names it: one a runner restored has no
    /// rustup link and is linked, not built again.
    #[test]
    fn a_compiler_s_rustc_runs_or_it_is_built_again() {
        let stage2 = TempDir::new("runs");
        let rustc = stage2.join("bin/rustc");
        assert!(!runs(&stage2), "a stage2 with no rustc ran");
        fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        // This test binary refuses `--version`; the host's rustc answers it.
        for (what, ran) in [(std::env::current_exe().unwrap(), false), (host_sysroot().join("bin/rustc"), true)] {
            let _ = fs::remove_file(&rustc);
            std::os::unix::fs::symlink(&what, &rustc).unwrap();
            assert_eq!(runs(&stage2), ran, "{}", what.display());
        }
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
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new("dep-info-unread");
        let mut none = Vec::new();
        collect_dep_info(&temp.join("none"), &mut none);
        assert!(none.is_empty(), "{none:?}");

        let built = temp.join("x86_64-unknown-none");
        let dist = built.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let collect = || std::panic::catch_unwind(|| collect_dep_info(&built, &mut Vec::new()));
        let said = |refused: std::thread::Result<()>, what: &str| {
            *refused.err().unwrap_or_else(|| panic!("{what} was skipped")).downcast::<String>().expect("a formatted refusal")
        };
        let d = dist.join("core.d");
        fs::write(&d, b"libcore.rlib: library/core/src/lib.rs \xff\n").unwrap();
        let refused = said(collect(), "dep-info that is not UTF-8");
        assert!(refused.starts_with(&format!("read {}", d.display())), "{refused}");
        fs::remove_file(&d).unwrap();

        let mode = |bits| fs::set_permissions(&dist, fs::Permissions::from_mode(bits)).unwrap();
        mode(0o000);
        let unread = collect();
        mode(0o755);
        let refused = said(unread, "a directory that cannot be read");
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
