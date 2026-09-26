use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::arch::Arch;
use crate::buildlock;
use crate::buildlock::Scope;
use crate::stamps;
use crate::sysroot::{self, Sysroot, SYSROOT_SOURCES};

/// Whether the primary's compiler needs a bootstrap. `invalidate_hosted`
/// separates "the compiler changed" from "the rustup link is missing": only the
/// first makes the ToyOS-hosted rustc stale, and rebuilding that one costs
/// minutes.
#[derive(Clone, Copy, PartialEq)]
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
///
/// One home for the list, because it is read four ways that must agree — the
/// `stage1-std` cleans in [`full_bootstrap`], [`write_config`]'s bootstrap
/// `target` set, the libraries `src/sysroot.rs` builds and places, and
/// `src/build.rs`'s external fingerprint. A fifth spelling would silently leave
/// one of them building or fingerprinting a different set of targets than the
/// others.
pub const GUEST_TARGETS: [&str; 6] = [
    Arch::X86_64.userland(),
    Arch::X86_64.kernel(),
    Arch::X86_64.loader(),
    Arch::Aarch64.userland(),
    Arch::Aarch64.kernel(),
    Arch::Aarch64.loader(),
];

/// The one ToyOS the hosted rustc (`system.toml`'s `hosted-rustc`) is built to
/// run on.
pub const HOSTED_ARCH: Arch = Arch::X86_64;

/// The primary's compiler, which every sysroot is cloned from and compiled by.
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

/// What an installed toolchain's sysroot was built from, as its publisher
/// recorded it (`src/release.rs`).
fn witness_path(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-sysroot-witness")
}


fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.starts_with('.') && name != "target" {
                collect_sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs" || e == "toml" || e == "h") {
            out.push(path);
        }
    }
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
/// **The one step that provisions it, and every path that can produce a
/// toolchain directory goes through it**: the primary's bootstrap and its
/// staleness rebuild (both upstream of [`ensure`]'s call), a linked worktree
/// adopting the shared one, and a runner that unpacked the published artifact.
/// The fix this replaces was made once, by hand, in a step the rebuild path does
/// not run — so the 2026-08-14 sysroot rebuild recreated `bin/` without it and
/// nothing noticed, and CI, which links its toolchain fresh from the artifact
/// every run, never had it at all.
///
/// **A symlink, and what survives the artifact round-trip is this step rather
/// than the link.** `src/release.rs` excludes it from the tarball for the reason
/// it excludes `lib/rustlib/<host>`: it names a path only the publishing runner
/// has, and a copy would put a 32 MB host binary into a 401 MiB artifact to
/// stand in for a file the consumer can make in a microsecond. `Owner::Installed`
/// makes it, exactly as it makes the host target.
pub(crate) fn provision_toolchain_cargo(stage2: &Path) {
    let at = stage2.join("bin/cargo");
    let _ = fs::remove_file(&at);
    std::os::unix::fs::symlink(host_cargo(), &at).unwrap_or_else(|e| {
        panic!("Failed to symlink {} -> {}: {e}", at.display(), host_cargo().display())
    });
}

/// Refuse a toolchain layout that would make rustup narrate.
///
/// Unconditional and after the step that provisions, because the defect being
/// gated is a provisioning step that silently stopped running: a check that only
/// runs when the step runs asserts nothing about the build that skipped it.
pub(crate) fn assert_toolchain_is_honest(stage2: &Path) {
    let bin = stage2.join("bin");
    let narrated = narrated_binaries(&bin);
    assert!(
        narrated.is_empty(),
        "the toyos toolchain at {} is missing {}, so rustup answers for {} by falling back to \
         another toolchain and narrating it on every invocation.\n\
         provision_toolchain_cargo is the step that puts them there, and it did not.",
        bin.display(),
        narrated.join(" and "),
        if narrated.len() == 1 { "it" } else { "them" },
    );
}

/// Ensure the toolchain is up to date, and return the sysroot this checkout's
/// sources name — made if nobody has made it (`src/sysroot.rs`).
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
pub fn ensure(root: &Path, force_rebuild: bool, lock: &mut buildlock::Held) -> Sysroot {
    let rust_dir = rust_dir(root);
    let stamps_dir = root.join("target/stamps");
    fs::create_dir_all(&stamps_dir).ok();

    // Needed as the cross-linker for bootstrap and for every build.
    let owner = owner(root);
    let ld_src = root.join("toyos-ld/src");
    let ld_stamp = stamps_dir.join("linker.stamp");
    let shipped = installed_toyos_ld(root, &rust_dir, &owner);
    lock.act_if(
        Scope::Worktree,
        "build toyos-ld",
        || {
            (stamps::dir_changed(&ld_src, &ld_stamp) || !toyos_ld_binary(root).exists())
                .then_some(())
        },
        |()| {
            match &shipped {
                Some(from) => {
                    eprintln!("toyos-ld: the installed toolchain's, {}", from.display());
                    install_toyos_ld(from, &toyos_ld_binary(root));
                }
                None => {
                    eprintln!("Building toyos-ld...");
                    build_toyos_ld(root);
                }
            }
            stamps::write_dir_stamp(&ld_src, &ld_stamp);
        },
    );
    if matches!(owner, Owner::Us) {
        record_ld_witness(root, &rust_dir);
    }

    // Used as a host tool by doom's build.rs.
    let cc_src = root.join("toyos-cc/src");
    let cc_inc = root.join("toyos-cc/include");
    let cc_stamp = stamps_dir.join("toyos-cc.stamp");
    let cc_inc_stamp = stamps_dir.join("toyos-cc-include.stamp");
    lock.act_if(
        Scope::Worktree,
        "build toyos-cc",
        || {
            (stamps::dir_changed(&cc_src, &cc_stamp)
                || stamps::dir_changed(&cc_inc, &cc_inc_stamp)
                || !toyos_cc_binary(root).exists())
            .then_some(())
        },
        |()| {
            eprintln!("Building toyos-cc...");
            build_toyos_cc(root);
            stamps::write_dir_stamp(&cc_src, &cc_stamp);
            stamps::write_dir_stamp(&cc_inc, &cc_inc_stamp);
        },
    );


    match owner {
        Owner::Elsewhere(primary) => {
            assert!(
                !force_rebuild,
                "--rebuild-toolchain would replace the compiler at {}, which every worktree of \
                 this repository builds with.\nRun it in {}.",
                stage2(&rust_dir).display(),
                primary.display()
            );
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
            check_installed_toolchain(root, &rust_dir, force_rebuild);
            return Sysroot::installed(stage2(&rust_dir));
        }
        Owner::Us => {}
    }

    let compiler_stamp = stamps_dir.join("compiler.stamp");
    let hosted_stamp = stamps_dir.join("hosted-rustc.stamp");
    lock.act_if(
        Scope::Global,
        "build the rust toolchain",
        || {
            let toolchain_exists = Command::new("rustup")
                .args(["run", "toyos", "rustc", "--version"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if stamps::dir_changed(&rust_dir.join("compiler"), &compiler_stamp) || force_rebuild {
                Some(Bootstrap { invalidate_hosted: true })
            } else if !toolchain_exists {
                Some(Bootstrap { invalidate_hosted: false })
            } else {
                None
            }
        },
        |kind| {
            eprintln!("Building full toolchain (this takes a while on first run)...");
            full_bootstrap(root, &rust_dir);
            stamps::write_dir_stamp(&rust_dir.join("compiler"), &compiler_stamp);
            crate::compiler::record(&rust_dir);
            if kind.invalidate_hosted {
                let _ = fs::remove_file(&hosted_stamp);
            }
        },
    );
    // The compiler stamp above has just said `stage2` is built from what `rust/`
    // holds, so a missing record is written from it.
    lock.act_if(
        Scope::Global,
        "record which compiler the toolchain is",
        || (!rust_dir.join("build/toyos-compiler").exists()).then_some(()),
        |()| crate::compiler::record(&rust_dir),
    );

    let hosted_rustc = rust_dir.join(format!("build/{}/stage2/bin/rustc", HOSTED_ARCH.userland()));
    lock.act_if(
        Scope::Global,
        "build the ToyOS-hosted rustc",
        || (!hosted_stamp.exists() || !hosted_rustc.exists()).then_some(()),
        |()| {
            build_hosted_rustc(&rust_dir, &toyos_ld_binary(root));
            assert!(hosted_rustc.exists(), "Failed to build hosted rustc");
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

    // After both bootstrap steps above, because either of them recreates `bin/`
    // and a fix that lives upstream of a rebuild is a fix that rots. Every
    // sysroot clones this `bin/`, so it is where the cargo is provisioned.
    lock.act_if(
        Scope::Global,
        "give the toyos toolchain its own cargo",
        || cargo_link_stale(&stage2).then_some(()),
        |()| provision_toolchain_cargo(&stage2),
    );
    assert_toolchain_is_honest(&stage2);

    // The hosted rustc's own sysroot needs the host target proc-macros compile
    // against.
    lock.act_if(
        Scope::Global,
        "add the host target to the ToyOS sysroot",
        || host_target_missing(&rust_dir).then_some(()),
        |()| link_host_target(&rust_dir),
    );

    sysroot::ensure(root, &rust_dir, lock)
}

/// Everything a checkout may do with a toolchain it did not build: check that
/// it is the one this tree needs, and say what to do when it is not.
///
/// No amount of source here can rebuild a sysroot without `rust/`, so there is
/// nothing to decide and the answer is always to publish a toolchain built from
/// these sources. Its std fork is pinned by the release tag, which is a function
/// of `rust` (`src/release.rs`).
fn check_installed_toolchain(root: &Path, rust_dir: &Path, force_rebuild: bool) {
    let stage2 = stage2(rust_dir);
    assert!(
        !force_rebuild,
        "there is no `rust/` source in {}, so --rebuild-toolchain has nothing to build from.\n\
         The toolchain at {} arrived as an artifact; rebuild it where it is published.",
        root.display(),
        stage2.display(),
    );
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
    // This is CI's whole share of the cargo provisioning — it links its
    // toolchain fresh from the published artifact on every run, so nothing
    // upstream of the download can have put one there.
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
         Publish a toolchain built from these sources and install that one instead.",
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

/// Run bootstrap, streaming its output where it was going anyway and keeping a
/// copy.
///
/// `.status()` was enough while the only question was the exit code. It is not
/// enough for the question [`refuse_on_compile_error`] asks, which is what the
/// failure *was*.
pub(crate) fn x_build(rust_dir: &Path, args: &[&str], what: &str) -> (bool, Vec<String>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{Arc, Mutex};

    // Two literals and not one variable: `src/sourcegate::every_binary_the_host_runs_is_declared`
    // reads the argument, and a name assembled at run time is a name nobody declared.
    let (x, mut command) = if rust_dir.join("x").exists() {
        ("./x", Command::new("./x"))
    } else {
        ("./x.py", Command::new("./x.py"))
    };
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

fn full_bootstrap(root: &Path, rust_dir: &Path) {
    let toyos_ld = toyos_ld_binary(root);

    // Ensure library/backtrace is checked out — std depends on it.
    // Other rust submodules (llvm, docs, cargo) are handled by bootstrap on demand.
    crate::ensure_submodule(rust_dir, "library/backtrace");

    // Write bootstrap.toml — ToyOS as target only, not host (fast rebuilds)
    let host = host_triple();
    write_config(rust_dir, &host, &toyos_ld, false);

    // Clean cached std for all ToyOS targets so bootstrap picks up compiler changes
    // (e.g. target spec changes like default_uwtable that affect codegen).
    for target in GUEST_TARGETS {
        let stage1_std = rust_dir.join(format!("build/{host}/stage1-std/{target}"));
        if stage1_std.exists() {
            fs::remove_dir_all(&stage1_std).ok();
        }
    }

    let build = ["build", "--stage", "2", "--warnings", "warn"];
    let (ok, log) = x_build(rust_dir, &build, "the toolchain");

    if !ok {
        refuse_on_compile_error(&log, "the toolchain");
        // rustdoc for ToyOS may fail to link; rustc may not be missing.
        let stage2 = rust_dir.join(format!("build/{host}/stage2"));
        assert!(
            stage2.join("bin/rustc").exists(),
            "the toolchain build failed and {} is not there.\n\
             Nothing in its output was a compile error, so this is a link or a bootstrap \
             failure — the last lines above are the whole of what it said.",
            stage2.join("bin/rustc").display()
        );
        tolerated_failure(&log, "the toolchain build");
    }
    for arch in Arch::ALL {
        assert_std_built_from(
            root,
            &rust_dir.join(format!("build/{host}/stage1-std/{}", arch.userland())),
        );
    }
}

fn build_hosted_rustc(rust_dir: &Path, toyos_ld: &Path) {
    eprintln!("Building ToyOS-hosted rustc...");
    write_config(rust_dir, &host_triple(), toyos_ld, true);

    let (ok, log) =
        x_build(rust_dir, &["build", "--stage", "2", "--warnings", "warn"], "the hosted rustc");
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
    // No config restore needed — full_bootstrap writes the
    // cross-only config before they run, so the next non-hosted build
    // always starts with the correct config regardless of what's on disk.
}

fn write_config(rust_dir: &Path, host: &str, toyos_ld: &Path, with_hosted_rustc: bool) {
    let linker = toyos_ld.display();
    let host_line = if with_hosted_rustc {
        format!("host = [\"{host}\", \"{}\"]", HOSTED_ARCH.userland())
    } else {
        format!("host = [\"{host}\"]")
    };
    let codegen_backends = if with_hosted_rustc {
        "\ncodegen-backends = [\"cranelift\"]"
    } else {
        ""
    };
    let targets = std::iter::once(host)
        .chain(GUEST_TARGETS)
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let userland: String = Arch::ALL
        .iter()
        .map(|arch| {
            let backends = if *arch == HOSTED_ARCH { codegen_backends } else { "" };
            format!("[target.{}]\nlinker = \"{linker}\"{backends}\n\n", arch.userland())
        })
        .collect();
    let config = format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
{host_line}
target = [{targets}]

[rust]
incremental = true
lld = false

{userland}"#
    );
    fs::write(rust_dir.join("bootstrap.toml"), config).unwrap();
}

/// Path to the host toyos-ld binary (stable location, never wiped by sysroot rebuilds).
///
/// The workspace root's `target/`, not `toyos-ld/target/`: `toyos-ld` is a
/// member of the host workspace (root `Cargo.toml`), and a member has no target
/// directory of its own — `src/hostws.rs::target_dir` is the same answer for
/// the crates `src/build.rs` asks about generically.
pub fn toyos_ld_binary(root: &Path) -> PathBuf {
    let host = host_triple();
    root.join(format!("target/{host}/release/toyos-ld"))
}

fn build_toyos_ld(root: &Path) {
    let host = host_triple();
    let status = Command::new("cargo")
        .args(["build", "--release", "--target", &host])
        .current_dir(root.join("toyos-ld"))
        .status()
        .expect("Failed to build toyos-ld");
    assert!(status.success(), "toyos-ld build failed");
}

/// Where `src/release.rs` puts the linker in the tarball, so the consumer that
/// unpacks it finds one beside `rustc`.
fn shipped_toyos_ld(rust_dir: &Path) -> PathBuf {
    rust_dir.join(format!("build/{}/stage2/bin/toyos-ld", host_triple()))
}

/// What `toyos-ld/` hashes to, so the publisher and the installer read one
/// function of the same bytes.
fn ld_witness(root: &Path) -> String {
    let mut files = Vec::new();
    collect_sources(&root.join("toyos-ld"), &mut files);
    files.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in files {
        let data = fs::read(&path).unwrap_or_else(|e| panic!("witness {}: {e}", path.display()));
        path.strip_prefix(root).unwrap_or(&path).hash(&mut hasher);
        data.hash(&mut hasher);
    }
    format!("{:016x}\n", hasher.finish())
}

fn ld_witness_path(rust_dir: &Path) -> PathBuf {
    rust_dir.join("build/toyos-ld-witness")
}

/// Record which sources the linker beside the sysroot is, for the tar to carry.
fn record_ld_witness(root: &Path, rust_dir: &Path) {
    let want = ld_witness(root);
    let at = ld_witness_path(rust_dir);
    if fs::read_to_string(&at).ok().as_deref() == Some(want.as_str()) {
        return;
    }
    fs::create_dir_all(rust_dir.join("build")).ok();
    fs::write(&at, want).unwrap_or_else(|e| panic!("write {}: {e}", at.display()));
}

/// The install path's whole decision: a checkout with no `rust/` source did not
/// build the compiler and does not build the linker either — where the toolchain
/// shipped one and the witness beside it is this tree's. The release tag is a
/// function of four trees and `toyos-ld` is not among them, so a shipped linker
/// whose witness differs is older than these sources.
fn choose_shipped_toyos_ld(at: PathBuf, recorded: Option<&str>, sources: &str) -> Option<PathBuf> {
    (recorded == Some(sources) && at.exists()).then_some(at)
}

/// [`choose_shipped_toyos_ld`] over this machine, for the one owner that can
/// have a shipped linker at all.
fn installed_toyos_ld(root: &Path, rust_dir: &Path, owner: &Owner) -> Option<PathBuf> {
    if !matches!(owner, Owner::Installed) {
        return None;
    }
    let recorded = fs::read_to_string(ld_witness_path(rust_dir)).ok();
    choose_shipped_toyos_ld(shipped_toyos_ld(rust_dir), recorded.as_deref(), &ld_witness(root))
}

/// Put it where every build looks for a linker, so nothing downstream knows
/// which it is.
fn install_toyos_ld(from: &Path, to: &Path) {
    fs::create_dir_all(to.parent().expect("the linker has a directory")).ok();
    fs::copy(from, to)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
}

/// Path to the host toyos-cc binary. In the workspace root's `target/`, for
/// [`toyos_ld_binary`]'s reason.
pub fn toyos_cc_binary(root: &Path) -> PathBuf {
    let host = host_triple();
    root.join(format!("target/{host}/release/toyos-cc"))
}

fn build_toyos_cc(root: &Path) {
    let host = host_triple();
    let status = Command::new("cargo")
        .args(["build", "--release", "--target", &host])
        .current_dir(root.join("toyos-cc"))
        .status()
        .expect("Failed to build toyos-cc");
    assert!(status.success(), "toyos-cc build failed");
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

/// PATH with toyos-ld's build directory prepended, so rustc finds it for linking.
pub fn path_with_toyos_ld(root: &Path) -> String {
    let ld_dir = toyos_ld_binary(root).parent().expect("the linker has a directory").to_path_buf();
    match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", ld_dir.display()),
        Err(_) => ld_dir.display().to_string(),
    }
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

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("toyos-toolchain-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// **The judge, and the partial fix it must not pass**: taking any shipped
    /// binary takes one built from sources the release tag does not key on.
    #[test]
    fn the_shipped_linker_is_taken_only_where_it_is_this_tree_s() {
        let dir = scratch("shipped-ld");
        let at = dir.join("toyos-ld");
        fs::write(&at, b"a linker\n").unwrap();

        assert_eq!(
            choose_shipped_toyos_ld(at.clone(), Some("beef\n"), "beef\n"),
            Some(at.clone()),
            "a shipped linker whose witness is this tree's is the one to link through"
        );
        assert_eq!(
            choose_shipped_toyos_ld(at.clone(), Some("f00d\n"), "beef\n"),
            None,
            "a shipped linker built from other sources is not this tree's"
        );
        assert_eq!(
            choose_shipped_toyos_ld(at, None, "beef\n"),
            None,
            "a toolchain that shipped no witness cannot say what its linker is"
        );
        assert_eq!(
            choose_shipped_toyos_ld(dir.join("absent"), Some("beef\n"), "beef\n"),
            None,
            "a toolchain that shipped no linker leaves this checkout to build one"
        );
    }

    /// **The layout that makes rustup narrate, as a decision.**
    ///
    /// The toolchain was given a cargo once, by hand, in a step the rebuild path
    /// does not run; the 2026-08-14 sysroot rebuild recreated `bin/` without it
    /// and nothing noticed, because nothing asked. This is the asking —
    /// [`assert_toolchain_is_honest`] is this function over the real `bin/`.
    #[test]
    fn a_toolchain_bin_without_cargo_is_one_rustup_narrates() {
        let stage2 =
            std::env::temp_dir().join(format!("toyos-toolchain-layout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&stage2);
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
        assert_toolchain_is_honest(&stage2);
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
            "error: linking with `toyos-ld` failed: exit status: 1",
            "  |",
            "  = note: rust-lld: error: undefined symbol: __rust_probestack",
            "Build completed unsuccessfully in 0:00:41",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(compile_error_at(&log), None);
    }

}
