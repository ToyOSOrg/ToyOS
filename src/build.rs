use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::arch::Arch;
use crate::assets;
use crate::buildlock;
use crate::flags;
use crate::hostws;
use crate::image;
use crate::printer;
use crate::sysroot::{Identity, Stale, Sysroot};
use crate::toolchain;

thread_local! {
    /// Time this worker has spent in a [`Building`].
    ///
    /// A suite worker is also the thread that asks for its boot image, so a
    /// cumulative thread-local clock lets the harness remove a cold build from
    /// the test that happened to ask for it first. A process-wide counter would
    /// subtract another worker's concurrent build instead.
    static ARTIFACT_BUILD_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    /// The test this worker builds for, where it said: [`building_for`].
    static BUILDING_FOR: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Every thread's [`ARTIFACT_BUILD_TIME`], summed, in nanoseconds.
static BUILT_NANOS: AtomicU64 = AtomicU64::new(0);

/// Name the test every build this thread makes from here on is reported as
/// made for.
pub fn building_for(test: &'static str) {
    BUILDING_FOR.set(Some(test));
}

/// How long this process's threads have spent building, summed over them.
pub fn built() -> Duration {
    Duration::from_nanos(BUILT_NANOS.load(Ordering::Relaxed))
}

/// One reading of the artifact-build clock for the current thread.
///
/// Test duration profiles are execution prices, not ownership of a shared
/// cache miss. The raw suite wall clock still includes every build.
#[derive(Clone, Copy)]
pub struct ArtifactBuildMark(Duration, PhantomData<Rc<()>>);

/// Read the current thread's cumulative artifact-build time.
pub fn mark_artifact_build_time() -> ArtifactBuildMark {
    ArtifactBuildMark(ARTIFACT_BUILD_TIME.get(), PhantomData)
}

impl ArtifactBuildMark {
    /// Remove artifact construction since this mark from a raw elapsed time.
    pub fn execution_part(self, raw: Duration) -> Duration {
        let built = ARTIFACT_BUILD_TIME.get().saturating_sub(self.0);
        raw.saturating_sub(built)
    }
}

/// One build, reported as a test is: a `BUILD` line naming it when it starts,
/// and when it ends, unwinding or not, [`printer::outcome`]'s line with how
/// long it took, which is charged to the build clocks. Image creation on a
/// memo hit is deliberately outside this guard: every boot pays that work, so
/// it is part of the test's repeatable execution price.
struct Building {
    what: String,
    began: Instant,
}

impl Building {
    fn start(what: String) -> Self {
        let what = match BUILDING_FOR.get() {
            Some(test) => format!("{what}, for {test}"),
            None => what,
        };
        eprintln!("{}", printer::started("BUILD", &what));
        Self { what, began: Instant::now() }
    }
}

impl Drop for Building {
    fn drop(&mut self) {
        let took = self.began.elapsed();
        ARTIFACT_BUILD_TIME.set(ARTIFACT_BUILD_TIME.get().saturating_add(took));
        BUILT_NANOS.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        let word = if std::thread::panicking() { "FAIL" } else { "BUILT" };
        eprintln!("{}", printer::outcome(word, &self.what, took));
    }
}

// --- Config ---

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SystemConfig {
    #[serde(default)]
    programs: BTreeMap<String, ProgramConfig>,
    #[serde(default)]
    symlinks: BTreeMap<String, String>,
    #[serde(default)]
    assets: Vec<String>,
    /// What `/system/bin/supervisor` starts at boot. Program *keys*, never paths — a path
    /// here is a second spelling of a `[programs]` key and is what let a boot
    /// list smuggle an argument through. Arguments live on the program entry's
    /// `args` instead.
    #[serde(default)]
    boot: BootConfig,
    /// What every program launched out of `/apps` holds. One row for all of
    /// them, because a package directory is writable.
    #[serde(default)]
    apps: AppsConfig,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct BootConfig {
    start: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct AppsConfig {
    /// Connectors, and no `devices` or `syscap` beside it: an installed package
    /// reaches servers and claims no hardware.
    receives: Vec<String>,
}

/// **Unknown fields refused**: a misspelled `starts` would be a row that
/// silently holds no launcher.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct ProgramConfig {
    /// Argv this program is started with, after argv[0].
    args: Vec<String>,
    /// Names the supervisor creates **one machine-wide port** for and endows this program
    /// the *acceptor* of.
    serves: Vec<String>,
    /// Names this program creates a port for **itself**, once per instance, and
    /// hands the connector down to its own children. The supervisor creates nothing and
    /// holds nothing for these — `surface` is the whole of this kind.
    provides: Vec<String>,
    /// Names in this program's namespace, each a *connector*.
    receives: Vec<String>,
    /// Device classes the supervisor mints a claim for and endows.
    devices: Vec<String>,
    /// Rights on the `SysCap` duplicate the supervisor endows this program, by the names
    /// `toyos_manifest::syscap_rights` takes. A handful of rows in the whole
    /// tree declare one.
    syscap: Vec<String>,
    /// The idle slot, granted as partition claims (`toyos_manifest::Program::slots`).
    slots: bool,
    /// A system service: the supervisor starts it with `HOME` at its own `/state/<name>`
    /// and makes that directory, where every other row gets the session's.
    service: bool,
    /// The file-server roles this binary serves, one process each
    /// (`toyos_manifest::Program::roles`).
    roles: Vec<String>,
    /// The supervisor starts it again when it ends (`toyos_manifest::Program::restart`).
    restart: bool,
    /// The rows it may start through the launcher (`toyos_manifest::Program::starts`).
    starts: Vec<String>,
    /// A launch it makes from the machine's session opens a login session
    /// (`toyos_manifest::Program::login`).
    login: bool,
}

/// The crate directory of the program `name`.
fn program_dir(root: &Path, name: &str) -> PathBuf {
    root.join("userland").join(name)
}

fn parse_config(path: &Path) -> SystemConfig {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    toml::from_str(&text)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()))
}

// --- Freshness checking ---

fn stale(crate_dir: &Path, identity: &Identity) -> Option<Stale> {
    let stamp = crate_dir.join("target/.deps-stamp");
    identity.stale(fs::read_to_string(&stamp).ok().as_deref())
}

/// Remove what `stale` names of `crate_dir`'s target directory and stamp it
/// with `identity`. [`Stale::All`] is the guest profile's host half and every
/// guest target's directory: the root's `target/` is also the build system's
/// own, which no sysroot made.
fn clean(crate_dir: &Path, stale: &Stale, identity: &Identity) {
    let target = crate_dir.join("target");
    let all: Vec<&str> = std::iter::once(PROFILE).chain(toolchain::GUEST_TARGETS.map(|t| t.triple())).collect();
    let moved = match stale {
        Stale::All => &all,
        Stale::Targets(moved) => moved,
    };
    for dir in moved.iter().map(|t| target.join(t)).filter(|dir| dir.exists()) {
        eprintln!("external deps changed: cleaning {}", dir.display());
        crate::keystore::remove(&dir);
    }

    fs::create_dir_all(&target).unwrap_or_else(|e| panic!("create {}: {e}", target.display()));
    let stamp = target.join(".deps-stamp");
    fs::write(&stamp, identity.to_string()).unwrap_or_else(|e| panic!("write {}: {e}", stamp.display()));
}

/// Drop what in the target directories the sysroot's moved parts invalidated.
///
/// Deciding and acting under one exclusive section is the whole point. Each of
/// these cleans removes a tree another builder may be compiling into, and
/// cargo's own lock cannot cover it — the lock lives at
/// `target/<profile>/.cargo-lock`, inside what the clean deletes. Two processes
/// that each decided before either acted would still both clean.
fn invalidate_stale(
    lock: &mut buildlock::Held,
    identity: &Identity,
    targets: &[PathBuf],
) {
    lock.act_if(
        "clean crate targets against changed external deps",
        || {
            let work: Vec<(PathBuf, Stale)> =
                targets.iter().filter_map(|dir| stale(dir, identity).map(|s| (dir.clone(), s))).collect();
            (!work.is_empty()).then_some(work)
        },
        |work| {
            for (dir, stale) in work {
                clean(&dir, &stale, identity);
            }
        },
    );
}

/// Which features the build gives a crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Features {
    /// Its manifest's defaults.
    Default,
    /// Its defaults and these, comma-separated.
    With(&'static str),
    /// Any it declares, since the command line picks: the kernel's, which
    /// `--kernel-feature` names.
    AnyDeclared,
}

impl Features {
    /// The `cargo` arguments that give a crate these features.
    pub fn args(self) -> Vec<&'static str> {
        match self {
            Features::Default => vec![],
            Features::With(names) => vec!["--features", names],
            Features::AnyDeclared => vec!["--all-features"],
        }
    }
}

/// What one crate of a config is to [`build`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Built {
    Kernel,
    Bootloader,
    /// A program, built with the others in one `cargo build`.
    Member,
}

/// One crate a config's image is built from.
struct ConfigCrate {
    name: String,
    dir: PathBuf,
    built: Built,
    features: Features,
}

/// Every crate a config's image is built from: the one list the build, its
/// staleness sweep and the licence gate read.
fn config_crates(root: &Path, config: &SystemConfig) -> Vec<ConfigCrate> {
    let mut crates = vec![
        ConfigCrate {
            name: crate::ci::KERNEL.to_string(),
            dir: root.join(crate::ci::KERNEL),
            built: Built::Kernel,
            features: Features::AnyDeclared,
        },
        ConfigCrate {
            name: LOADER.to_string(),
            dir: root.join(LOADER),
            built: Built::Bootloader,
            features: Features::Default,
        },
    ];
    for name in config.programs.keys() {
        crates.push(ConfigCrate {
            name: name.clone(),
            dir: program_dir(root, name),
            built: Built::Member,
            features: Features::Default,
        });
    }
    crates.push(ConfigCrate {
        name: SUPERVISOR_PROGRAM.to_string(),
        dir: program_dir(root, SUPERVISOR_PROGRAM),
        built: Built::Member,
        features: Features::Default,
    });
    crates
}

// --- Cargo helpers ---

/// The profile every guest binary is built with, and the directory cargo puts
/// it in.
///
/// One name, passed to every `cargo build` here and declared by every crate
/// root the image is made of.
pub const PROFILE: &str = "toyos";

/// What every guest `cargo` and `rustc` here runs with: the toolchain directory
/// of the sysroot this checkout's sources name, as `RUSTUP_TOOLCHAIN`, which
/// carries the linker too.
struct GuestEnv {
    /// Owned, so the sysroot is held in use for as long as anything here runs
    /// against it.
    sysroot: Sysroot,
    /// The public key the loader and `/system/bin/update` embed
    /// (`signing::KEY_ENV`): every guest build carries it, so no crate that
    /// names it can be built without it.
    image_key: String,
    /// Whose floor the loader keeps (`signing::FLOOR_ENV`), which follows the key.
    floor_scope: &'static str,
}

impl GuestEnv {
    fn new(sysroot: Sysroot) -> Self {
        Self {
            sysroot,
            image_key: crate::signing::key().public_hex(),
            floor_scope: crate::signing::key().floor_scope().word(),
        }
    }
}

/// `cargo build` in `crate_dir`: the root, for a workspace member `extra_args`
/// names with `-p`, or a crate that keeps its own resolution.
fn cargo_build(
    crate_dir: &Path,
    target: &str,
    extra_args: &[&str],
    env: &GuestEnv,
    extra_env: &[(&str, &str)],
    quiet: bool,
) {
    let mut args = vec!["build", "--target", target, "--profile", PROFILE];
    if quiet {
        args.push("--quiet");
    }
    args.extend_from_slice(extra_args);
    let mut cmd = Command::new("cargo");
    cmd.args(&args)
        .current_dir(crate_dir)
        .env("RUSTUP_TOOLCHAIN", env.sysroot.dir())
        .env_remove("RUSTFLAGS")
        .env(crate::signing::KEY_ENV, &env.image_key)
        .env(crate::signing::FLOOR_ENV, env.floor_scope)
        .env_remove("RUSTC");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // `Command::output()` pipes any stream the builder left unset, so reaching
    // for it is a decision to capture *both* modes, not just the quiet one.
    // Diagnostics must survive a successful build either way: a warning nobody
    // sees is a warning nobody fixes.
    let status = if quiet {
        // The caller owns the terminal (the test harness interleaves this with
        // its own progress), so hold cargo's output until the crate is done and
        // replay it as one block. `--quiet` reduces that block to diagnostics.
        let output = cmd
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap_or_else(|e| {
                panic!("cargo build failed to launch in {}: {e}", crate_dir.display())
            });
        std::io::Write::write_all(&mut std::io::stderr(), &output.stderr).ok();
        output.status
    } else {
        cmd.status().unwrap_or_else(|e| {
            panic!("cargo build failed to launch in {}: {e}", crate_dir.display())
        })
    };
    if !status.success() {
        panic!("cargo build failed in {}", crate_dir.display());
    }
}

// --- Artifact staging ---
//
// A kernel's feature list changes its binary, but cargo keys the artifact path
// on (crate, target, profile) and nothing else, so every config writes and reads
// one path.
//
// The window is not a moment: `build_test_image` builds, then runs the entire
// userland build and root-image assembly, and only then reads the artifact back.
// Seconds to minutes, during which another config's build overwrites it.
//
// So: hold [`buildlock::artifact`] across each build→stage pair, and copy the
// artifact to a name carrying what it is actually keyed by. Readers use the
// staged name, which no other config can overwrite.

/// The staged-artifact key of a kernel built with `features`.
///
/// **Both build paths go through this and that is the whole of the claim** that
/// a test which asks for no feature boots the binary an image ships: the staged
/// file is named for this key, so an equal key is not a similar kernel but the
/// same file. `cargo run --build-only` passes what `kernel_features` made of an
/// empty request; the harness passes `BootOptions::kernel_features` joined.
/// Nothing between them may add a name — which is what `qemu::fold_inert` used
/// to do to every boot in the suite.
fn kernel_key(arch: Arch, features: &str) -> u64 {
    key_hash(&[PROFILE, arch.name(), features])
}

/// The loader's build key: its profile, its architecture, and the key and
/// floor scope it embeds.
fn loader_key(arch: Arch, image_key: &str, floor_scope: &str) -> u64 {
    key_hash(&[PROFILE, arch.name(), image_key, floor_scope])
}

fn key_hash(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    h.finish()
}

/// Copy a just-built artifact to a path carrying its build key, and return that
/// path. Must be called with [`buildlock::artifact`] held, before anything else
/// can rebuild the same crate.
fn stage_artifact(root: &Path, built: &Path, stem: &str, key: u64) -> PathBuf {
    let staged = root.join(format!("target/{stem}-{key:016x}"));
    fs::create_dir_all(root.join("target")).ok();
    fs::copy(built, &staged).unwrap_or_else(|e| {
        panic!("stage {} -> {}: {e}", built.display(), staged.display())
    });
    staged
}

/// The panic message rustc emits beside every checked add. Absent from a binary
/// built with `overflow-checks = false`, because then there is no call site to
/// reference it and the linker's liveness pass drops it — measured on this
/// kernel: present at 3,784,872 bytes with the checks on, gone at 3,296,672
/// with them off.
const OVERFLOW_CHECK_MARKER: &[u8] = b"attempt to add with overflow";

/// Refuse to build a kernel whose target has hardware float.
///
/// The kernel's entry saves the user machine state at the ring transition and
/// nowhere else, which is sound only because kernel code cannot disturb it:
/// the FP/SIMD registers may be left dirty for a whole kernel excursion because
/// nothing in the kernel reads or writes them. That rests on the target spec —
/// `RustcAbi::Softfloat` and `+soft-float` in
/// `rust/compiler/rustc_target/src/spec/targets/x86_64_unknown_none.rs`, and
/// `aarch64_unknown_none_softfloat.rs`'s `-fp-armv8,-neon` — and an edit
/// turning it off would make every bracket in the kernel insufficient without
/// changing a byte of `kernel/`.
///
/// Asked of the compiler rather than of the manifest, and once per process and
/// architecture: it is a property of the toolchain rather than of any one image.
fn assert_kernel_is_softfloat(env: &GuestEnv, arch: Arch) {
    static CHECKED: std::sync::Mutex<BTreeSet<Arch>> = std::sync::Mutex::new(BTreeSet::new());
    let mut checked = CHECKED.lock().expect("a softfloat check panicked");
    if checked.contains(&arch) {
        return;
    }
    let out = Command::new("rustc")
        .args(["--print", "cfg", "--target", arch.kernel()])
        .env("RUSTUP_TOOLCHAIN", env.sysroot.dir())
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC")
        .output()
        .expect("rustc --print cfg failed to launch");
    assert!(out.status.success(), "rustc --print cfg failed for the kernel target");
    let cfg = String::from_utf8_lossy(&out.stdout);
    let has = |feature: &str| cfg.lines().any(|l| l == format!(r#"target_feature="{feature}""#));
    match arch {
        Arch::X86_64 => {
            assert!(
                has("x87"),
                "the kernel target no longer reports x87, so `arch::fpu`'s FXSAVE64 image is \
                 not the state this machine has:\n{cfg}"
            );
            assert!(
                !has("sse"),
                "the kernel target has hardware float, so kernel code may now clobber the user \
                 machine state between the entry's save and its restore:\n{cfg}"
            );
        }
        Arch::Aarch64 => assert!(
            !has("neon") && !has("fp-armv8"),
            "the kernel target has FP/SIMD, so kernel code may now clobber the user machine \
             state between the entry's save and its restore:\n{cfg}"
        ),
    }
    checked.insert(arch);
}

/// Whether `haystack` contains `needle` as a contiguous subslice.
///
/// The one form every artifact search here uses. `filter` on the first byte and
/// then `starts_with` rather than `windows(needle.len()).any(|w| w == needle)`:
/// `windows` builds and compares a slice at every offset, while this compares
/// past the first byte only where the first byte matched — the difference that
/// mattered on a multi-megabyte kernel scanned once per build. An empty needle
/// is contained by everything, which the first-byte guard returns directly.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    let Some(&first) = needle.first() else { return true };
    haystack
        .iter()
        .enumerate()
        .filter(|&(_, &b)| b == first)
        .any(|(at, _)| haystack[at..].starts_with(needle))
}

/// Refuse to build an image whose kernel does not carry its overflow checks.
///
/// [`PROFILE`] states them and `--release` is gone from this build system, so
/// the way they can still be lost is somebody editing `[profile.toyos]`. This
/// asks the artifact rather than the manifest.
fn assert_overflow_checked(what: &str, image: &[u8]) {
    let found = contains_subslice(image, OVERFLOW_CHECK_MARKER);
    assert!(
        found,
        "the {what} was built without overflow checks: nothing in {} bytes references \
         {:?}. `[profile.toyos]` states `overflow-checks = true` in every crate root the \
         image is made of; something has stopped being true.",
        image.len(),
        core::str::from_utf8(OVERFLOW_CHECK_MARKER).unwrap()
    );
}

// --- Shared root-image assembly ---

/// Build all programs from a config and assemble the ROOT image.
/// The one program the kernel starts, in **every** image whatever `[programs]`
/// says. It reads the manifest below and starts what that names, so a ROOT image
/// without it is a machine with a kernel and no userland at all.
const SUPERVISOR_PROGRAM: &str = "supervisor";

/// Names `/system/bin/supervisor` serves itself.
///
/// The supervisor is in every image and is no `[programs]` key, so these have no
/// declaration to come from. They travel in the manifest so the supervisor creates
/// exactly the ports the build-time gate counted as provided — one producer,
/// rather than a constant here and a string in the supervisor.
///
/// Not `launcher`: a row holds it by its `starts`, badged with its row, and no
/// row receives it.
const SUPERVISOR_SERVED: &[&str] = &[toyos_swap::PORT, "power"];

/// Who may hold the two authorities that change what the machine runs:
/// the swap port, [`toyos_swap::HOLDER`] and nothing else — no other
/// `[programs]` row and never `[apps]` — and the idle slot,
/// [`toyos_update::slots::HOLDER`] alone.
///
/// **Checked on every manifest rendered, not only on the committed configs**,
/// because the holder of the one can replace any service's binary and the
/// holder of the other writes the image the machine boots next.
fn held_by_their_holders_alone(config: &SystemConfig) -> Result<(), String> {
    for (name, program) in &config.programs {
        if name != toyos_swap::HOLDER && program.receives.iter().any(|r| r == toyos_swap::PORT) {
            return Err(format!(
                "`{name}` receives `{}`, which only `{}` may hold",
                toyos_swap::PORT,
                toyos_swap::HOLDER
            ));
        }
        if name != toyos_update::slots::HOLDER && program.slots {
            return Err(format!("`{name}` asks for `slots`, which only `{}` may hold", toyos_update::slots::HOLDER));
        }
    }
    if config.apps.receives.iter().any(|r| r == toyos_swap::PORT) {
        return Err(format!("`[apps] receives` names `{}`, which only `{}` may hold", toyos_swap::PORT, toyos_swap::HOLDER));
    }
    Ok(())
}

/// What a row may start through the launcher, and which rows open a login
/// session (`toyos_manifest::launch`).
///
/// **Checked on every manifest rendered**: a `starts` entry is `/apps` or a
/// declared row the supervisor can start more than once, so not one that
/// serves a port or a file-server role, whose acceptors a launch would take and
/// a second one find gone.
fn starts_name_what_a_launch_can_start(config: &SystemConfig) -> Result<(), String> {
    for (name, program) in &config.programs {
        for key in &program.starts {
            if key == toyos_manifest::launch::APPS {
                continue;
            }
            let Some(target) = config.programs.get(key) else {
                return Err(format!("`{name}` starts `{key}`, which is not declared"));
            };
            if !target.serves.is_empty() || !target.roles.is_empty() {
                return Err(format!("`{name}` starts `{key}`, which serves ports a launch would take for good"));
            }
        }
    }
    Ok(())
}

/// The resolved config as the records `/system/bin/supervisor` reads.
///
/// The format, the renderer and the parser are `toyos-manifest/`, whose
/// round-trip test is what makes "what the build writes is what the supervisor reads" a
/// fact rather than two hand-matched implementations.
fn render_manifest(config: &SystemConfig) -> Vec<u8> {
    for gate in [held_by_their_holders_alone, starts_name_what_a_launch_can_start] {
        if let Err(why) = gate(config) {
            panic!("system.toml cannot be rendered as a manifest: {why}");
        }
    }
    let mut names: Vec<&String> = config.programs.keys().collect();
    names.sort();
    let manifest = toyos_manifest::Manifest {
        programs: names
            .iter()
            .map(|name| {
                let cfg = &config.programs[*name];
                toyos_manifest::Program {
                    name: (*name).clone(),
                    path: format!("/system/bin/{name}"),
                    args: cfg.args.clone(),
                    serves: cfg.serves.clone(),
                    provides: cfg.provides.clone(),
                    receives: cfg.receives.clone(),
                    devices: cfg.devices.clone(),
                    syscap: cfg.syscap.clone(),
                    slots: cfg.slots,
                    service: cfg.service,
                    roles: cfg.roles.clone(),
                    restart: cfg.restart,
                    starts: cfg.starts.clone(),
                    login: cfg.login,
                }
            })
            .collect(),
        supervisor_serves: SUPERVISOR_SERVED.iter().map(|s| (*s).to_string()).collect(),
        apps: config.apps.receives.clone(),
        start: config.boot.start.clone(),
    };
    toyos_manifest::render(&manifest)
        .unwrap_or_else(|e| panic!("system.toml cannot be rendered as a manifest: {e:?}"))
}

/// Which build `root`'s tree makes for `arch` against the sysroot whose key
/// for it is `toolchain`, as `/system/etc/os-release` records it, read with
/// gitoxide (`issues/the-build-runs-host-tools-outside-rust-and-qemu.md`,
/// row 21). Untracked files are dirty whatever `status.showUntrackedFiles`
/// says; submodules are not read, because the fork's state is the toolchain
/// key's.
fn release(root: &Path, toolchain: &crate::keystore::Key, arch: Arch) -> toyos_osrelease::Release {
    let repo = gix::open(root).unwrap_or_else(|e| panic!("{} is no git checkout: {e}", root.display()));
    let head = repo.head_commit().unwrap_or_else(|e| panic!("{}'s HEAD names no commit: {e}", root.display()));
    let commit = toyos_osrelease::Hex::parse(&head.id.to_string()).expect("a SHA-1 commit is forty hex digits");
    let time = head.time().unwrap_or_else(|e| panic!("commit {} names no committer time: {e}", head.id));
    let committed = u64::try_from(time.seconds)
        .unwrap_or_else(|_| panic!("commit {} was committed before 1970: {}", head.id, time.seconds));
    // Set whole: the platform `status` makes has no walk at all for a
    // checkout configured to show no untracked files.
    let walk = repo
        .dirwalk_options()
        .unwrap_or_else(|e| panic!("the status of {}: {e}", root.display()))
        .emit_untracked(gix::dir::walk::EmissionMode::CollapseDirectory);
    let changes = repo
        .status(gix::progress::Discard)
        .and_then(|status| {
            status
                .index_worktree_options_mut(|options| options.dirwalk_options = Some(walk))
                .index_worktree_submodules(None)
                .into_iter(None)
        })
        .unwrap_or_else(|e| panic!("the status of {}: {e}", root.display()));
    // An index entry whose stat alone moved, and an ignored file the walk
    // passed, summarise to nothing.
    let dirty = changes.map(|item| item.unwrap_or_else(|e| panic!("the status of {}: {e}", root.display()))).any(
        |item| match item {
            gix::status::Item::IndexWorktree(change) => change.summary().is_some(),
            gix::status::Item::TreeIndex(_) => true,
        },
    );
    toyos_osrelease::Release {
        commit,
        tree: if dirty { toyos_osrelease::Tree::Dirty } else { toyos_osrelease::Tree::Clean },
        toolchain: toyos_osrelease::Hex::parse(toolchain.as_str()).expect("a key is sixteen hex digits"),
        arch: match arch {
            Arch::X86_64 => toyos_osrelease::Arch::X86_64,
            Arch::Aarch64 => toyos_osrelease::Arch::Aarch64,
        },
        committed,
    }
}

fn build_and_assemble(
    root: &Path,
    config: &SystemConfig,
    env: &GuestEnv,
    extra_files: &[(String, Vec<u8>)],
    quiet: bool,
    arch: Arch,
) -> Vec<u8> {
    // Before anything is built, so a file edited during the build is not
    // credited to the state it was read in.
    let release = release(root, env.sysroot.identity.of_target(arch.userland()), arch);
    let mut root_files: Vec<(String, Vec<u8>)> =
        vec![(toyos_osrelease::PATH.to_string(), release.to_string().into_bytes())];
    build_programs(root, config, env, quiet, arch, &mut root_files);
    root_files.push((toyos_manifest::PATH.to_string(), render_manifest(config)));

    if !config.assets.is_empty() {
        let programs: BTreeSet<&str> = config.programs.keys().map(String::as_str).collect();
        root_files.extend(assets::collect(&config.assets, &programs));
    }

    // Extra files (test binaries, shared libs)
    for (name, data) in extra_files {
        root_files.push((name.clone(), data.clone()));
    }

    let symlinks: Vec<(String, String)> = config.symlinks.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let programs: BTreeSet<&str> = config.programs.keys().map(String::as_str).collect();
    // Targets are inventoried beside the files: `bin/ls -> /system/bin/ghost` reaches a
    // program as surely as a file would, and the files alone walk past it.
    let targets: Vec<String> =
        symlinks.iter().map(|(_, to)| symlink_target_name(to).to_string()).collect();
    let mut names: Vec<&str> = root_files.iter().map(|(name, _)| name.as_str()).collect();
    names.extend(targets.iter().map(String::as_str));
    if let Err(why) = unnamed_program(&names, &programs, &config.boot.start) {
        panic!("{why}");
    }

    image::create_root_image(&root_files, &symlinks, quiet)
}

/// The programs an architecture cannot build yet, each with why. Such a program
/// is left off that architecture's ROOT, said by name at build time; its row
/// goes when its reason does.
const NOT_YET_BUILT: &[(Arch, &str, &str)] = &[
    (Arch::Aarch64, "calc", TOOLKIT_FORKS),
    (Arch::Aarch64, "snake", TOOLKIT_FORKS),
    (
        Arch::Aarch64,
        "doom",
        "softbuffer's toyos fork stops it \
         (issues/the-toolkit-forks-resolve-an-x86-only-toyos-window.md); its C compiles for \
         AArch64 with the toolchain's clang",
    ),
];

const TOOLKIT_FORKS: &str = "softbuffer's and winit's toyos forks resolve the published \
     toyos-window 0.2.0, whose framebuffer is x86-64 only \
     (issues/the-toolkit-forks-resolve-an-x86-only-toyos-window.md)";

/// Why `arch`'s userland leaves `program` out, if it does.
fn not_built_for(arch: Arch, program: &str) -> Option<&'static str> {
    NOT_YET_BUILT.iter().find(|(a, name, _)| *a == arch && *name == program).map(|(_, _, why)| *why)
}

/// Build `config`'s programs and the supervisor for `arch`, and add each to `root_files`.
fn build_programs(
    root: &Path,
    config: &SystemConfig,
    env: &GuestEnv,
    quiet: bool,
    arch: Arch,
    root_files: &mut Vec<(String, Vec<u8>)>,
) {
    let target = arch.userland();

    let programs: Vec<ConfigCrate> = config_crates(root, config)
        .into_iter()
        .filter(|c| c.built == Built::Member)
        .filter(|c| match not_built_for(arch, &c.name) {
            Some(why) => {
                eprintln!("{}: not built for {}, and not on this ROOT: {why}", c.name, arch.name());
                false
            }
            None => true,
        })
        .collect();
    for c in &programs {
        assert!(
            c.dir.join("Cargo.toml").exists(),
            "Program '{}' crate not found at {}",
            c.name,
            c.dir.display()
        );
    }
    let workspace_packages: Vec<&str> = programs.iter().map(|c| c.name.as_str()).collect();

    let ws_target = root.join(format!("target/{target}/{PROFILE}"));

    // Every userland crate that compiles C compiles it with the toolchain's
    // clang against libc's C sysroot.
    let cc_env = crate::clang::CSysroot::of(env.sysroot.dir(), arch).cc_env();
    let cc_env: Vec<(&str, &str)> = cc_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    // Build and read under one hold, exactly as `build_toyos_bins` does and for
    // the same reason: a program's path is keyed on (crate, target, profile)
    // alone, so every config in this run writes and reads the same
    // `target/.../toybox`. Cargo's own lock orders the two *builds* and
    // says nothing about a read between them — `ioapic_topology` died on
    // `Failed to read binary for toybox` while another worker's config was
    // relinking it, and was green the moment it was re-run alone.
    let _artifact = buildlock::artifact(root);
    if !workspace_packages.is_empty() {
        let mut extra: Vec<&str> = Vec::new();
        for pkg in &workspace_packages {
            extra.push("-p");
            extra.push(pkg);
        }
        cargo_build(root, target, &extra, env, &cc_env, quiet);
    }

    for c in &programs {
        let name = &c.name;
        let data =
            fs::read(ws_target.join(name)).unwrap_or_else(|_| panic!("Failed to read binary for {name}"));
        root_files.push((format!("bin/{name}"), data));
    }
}

/// What `tests/common/qemu.rs` prefixes every binary it injects with.
const HARNESS_PREFIXES: [&str; 2] = ["bin/test_rs_", "bin/test_c_"];

/// Where a symlink's target sits on ROOT, which mounts at `/system`; a target
/// outside that mount comes back whole and reaches no name in this image.
fn symlink_target_name(to: &str) -> &str {
    to.strip_prefix("/system/").unwrap_or(to)
}

/// The converse of the crate assertion above: a `bin/` entry no name reaches.
/// A name buys authority — `/system/bin/supervisor` builds a `[programs]` row's namespace and
/// device claims — and a harness binary has none, holding only what its spawner
/// moved in, so it is legal exactly when the config starts something that could
/// spawn it. **An inventory over the `bin/` namespace, not a reachability
/// proof**, which `the_check_does_not_reach_a_spawner_that_never_spawns` asserts.
fn unnamed_program(
    names: &[&str],
    programs: &BTreeSet<&str>,
    start: &[String],
) -> Result<(), String> {
    for name in names {
        let Some(program) = name.strip_prefix("bin/") else { continue };
        if program == SUPERVISOR_PROGRAM || programs.contains(program) {
            continue;
        }
        if HARNESS_PREFIXES.iter().any(|p| name.starts_with(p)) {
            // A start list of names no row declares runs nothing, so it is empty.
            if !start.iter().any(|s| programs.contains(s.as_str())) {
                return Err(format!(
                    "the image carries {name} and this config's `[boot] start` names no \
                     `[programs]` row, so nothing runs that could spawn it — a harness binary \
                     is endowed by the process that starts it and by nothing else"
                ));
            }
            continue;
        }
        return Err(format!(
            "the image carries {name} and no `[programs]` row names it, so `/system/bin/supervisor` can build \
             it no namespace and no device claim; add a row or take the file out"
        ));
    }
    Ok(())
}

// --- Public API ---

/// The file every boot config is called, in whichever directory holds it.
const CONFIG: &str = "system.toml";

/// Everything one image is built from: the config, the kernel's feature list,
/// and the parameters that kernel is armed with.
pub struct Plan {
    pub arch: Arch,
    pub config: PathBuf,
    pub features: Vec<String>,
    pub params: Vec<String>,
    /// The version the image's signed header names.
    pub version: u64,
    /// Room for a second slot, or none: a machine that cannot update itself.
    pub second: Option<image::SecondSlot>,
}

impl Plan {
    pub fn new(arch: Arch, config: &Path, features: &[&str], params: &[&str]) -> Self {
        Self {
            arch,
            config: config.to_path_buf(),
            features: features.iter().map(|f| (*f).to_string()).collect(),
            params: params.iter().map(|p| (*p).to_string()).collect(),
            version: image::version_now(),
            second: None,
        }
    }
}

/// The plan a `cargo run` command line names. [`build`] consumes one and
/// computes none of it; the harness builds its own with [`Plan::new`].
///
/// **`--kernel-param` and `--kernel-feature` are orthogonal to the boot mode**:
/// the config decides which programs start and these decide what the kernel is,
/// so every mode reaches every parameter and no mode implies one.
pub fn plan_for(root: &Path, boot: &Boot, debug: bool, args: &[String]) -> Plan {
    let owned = |flag| -> Vec<String> {
        flags::CARGO_RUN.values(args, flag).into_iter().map(String::from).collect()
    };
    let feature = owned(&flags::KERNEL_FEATURE);
    let param = owned(&flags::KERNEL_PARAM);
    // Both refuse an unknown name by name, and both run here rather than in the
    // build: a misspelling is the command line's and has to come back before
    // anything waits on a lock.
    let features = kernel_features(root, debug, &feature, &param);
    check_params(root, &param);
    let arch = arch_for(args);
    Plan {
        arch,
        config: boot.config.clone(),
        features: features.split(',').filter(|f| !f.is_empty()).map(Into::into).collect(),
        params: param,
        version: image::version_now(),
        second: None,
    }
}

/// The architecture a `cargo run` command line names with `--arch`: x86-64
/// when it names none, because that is the one whose userland boots to a
/// desktop until the port's userland stage lands.
pub fn arch_for(args: &[String]) -> Arch {
    match flags::CARGO_RUN.value(args, &flags::ARCH) {
        Some(name) => Arch::parse(name).unwrap_or_else(|why| {
            eprintln!("Error: --arch {why}");
            std::process::exit(2);
        }),
        None => Arch::X86_64,
    }
}

/// Which boot the image being built is for: the directory holding its config,
/// and the artifact that directory's name gives it.
///
/// Every mode is a constructor over the one representation, because a mode is
/// only ever a different directory — the kernel and the bootloader in a diag
/// image are byte-identical to the shipping one's, which a `#[cfg]` could not
/// have given us.
pub struct Boot {
    config: PathBuf,
    image: PathBuf,
    /// Which of the two build sequences writes it. They are not one function
    /// yet — `issues/two-sequences-build-one-image.md` — and until they
    /// are, this is what keeps each artifact to a single writer.
    case: bool,
}

impl Boot {
    /// **The one naming rule**: the artifact is named after the directory
    /// holding the config, and the shipped config sits at the root and keeps
    /// the unsuffixed name.
    ///
    /// A directory outside the repository has no name under that rule, and the
    /// fallback would be the shipped artifact's — so it is refused here rather
    /// than given one.
    fn at(root: &Path, dir: &Path, case: bool) -> Result<Self, String> {
        let real =
            |at: &Path| fs::canonicalize(at).map_err(|e| format!("{} — {e}", at.display()));
        let (root, dir) = (real(root)?, real(dir)?);
        let Ok(under) = dir.strip_prefix(&root) else {
            return Err(format!(
                "{} is not in this repository, and the harness cannot boot a case outside it",
                dir.display()
            ));
        };
        let image = match under.file_name() {
            Some(name) => format!("target/bootable-{}.img", name.to_string_lossy()),
            None => "target/bootable.img".to_string(),
        };
        Ok(Self { config: dir.join(CONFIG), image: PathBuf::from(image), case })
    }

    /// The image `arch`'s build of this boot writes. x86-64 keeps the name
    /// every flashing step already reads; every other architecture's carries its
    /// name, so two architectures' builds of one config never share a file.
    pub fn image_for(&self, arch: Arch) -> PathBuf {
        match arch {
            Arch::X86_64 => self.image.clone(),
            Arch::Aarch64 => self.image.with_extension(format!("{}.img", arch.name())),
        }
    }

    /// The three modes' directories are the repository's own, so a refusal from
    /// [`Boot::at`] on one of them is a broken checkout and not a command line.
    fn mode(root: &Path, dir: &Path) -> Self {
        Self::at(root, dir, false).unwrap_or_else(|why| panic!("{why}"))
    }

    pub fn shipped(root: &Path) -> Self {
        Self::mode(root, root)
    }

    /// The config declares no `devices`, so nothing started there claims the
    /// framebuffer and the kernel's last boot checkpoint stays on screen.
    pub fn diag(root: &Path) -> Self {
        Self::mode(root, &root.join("diag"))
    }

    /// `/system/bin/console` claims the framebuffer and runs the shell on it.
    /// Claiming the screen is what stops the boot checkpoints painting, so a
    /// machine that wedges before userland is readable in this mode and in no
    /// other.
    pub fn console(root: &Path) -> Self {
        Self::mode(root, &root.join("console"))
    }

    /// One case directory named on the command line, holding its own
    /// [`CONFIG`], and built by [`build_test_image`].
    ///
    /// **A mode's own directory is refused.** Those three artifacts are written
    /// by [`build`]'s own sequence, and an image with two writers is an image
    /// whose contents depend on which command last ran.
    pub fn case(root: &Path, asked: &str) -> Result<Self, String> {
        Self::case_at(root, asked).map_err(|why| format!("--boot-config {asked}: {why}"))
    }

    /// [`Boot::case`] without its own name in front of every refusal.
    fn case_at(root: &Path, asked: &str) -> Result<Self, String> {
        let at = Path::new(asked);
        let asked_at = if at.is_absolute() { at.to_path_buf() } else { root.join(at) };
        let dir = fs::canonicalize(&asked_at)
            .map_err(|e| format!("{} — {e}", asked_at.display()))?;
        if !dir.join(CONFIG).is_file() {
            return Err(format!("{} holds no {CONFIG}", dir.display()));
        }
        for (mode, instead) in [
            ("", "--build-only with no config names the shipped one"),
            ("diag", "--diag-boot builds it"),
            ("console", "--console-boot builds it"),
        ] {
            let owned = fs::canonicalize(root.join(mode)).map_err(|e| e.to_string())?;
            if owned == dir {
                return Err(format!("{instead}, and one artifact keeps one writer"));
            }
        }
        Self::at(root, &dir, true)
    }
}

/// What the three modes' images are built from, besides std: every crate
/// [`config_crates`] names for one of them with the features the build gives
/// it, and the asset directories their configs copy onto ROOT. A case's config
/// is a test image and is not here.
pub struct Shipped {
    pub crates: BTreeSet<(PathBuf, Features)>,
    /// The directories of `crates` but the kernel and the loader: every
    /// program an image runs.
    pub programs: BTreeSet<PathBuf>,
    pub assets: BTreeSet<PathBuf>,
}

/// [`Shipped`], read out of the modes' configs the way [`build`] reads them.
pub fn shipped(root: &Path) -> Shipped {
    let mut crates = BTreeSet::new();
    let mut programs = BTreeSet::new();
    let mut assets = BTreeSet::new();
    for boot in [Boot::shipped(root), Boot::diag(root), Boot::console(root)] {
        let config = parse_config(&boot.config);
        for c in config_crates(root, &config) {
            if c.built == Built::Member {
                programs.insert(c.dir.clone());
            }
            crates.insert((c.dir, c.features));
        }
        assets.extend(config.assets.iter().map(|dir| root.join(dir)));
    }
    Shipped { crates, programs, assets }
}

/// The parameters an image built for flashing may carry: the kernel's own boot
/// parameters (`kernel/src/params.rs`) and nothing else.
///
/// **A flashed image carries no actuator.** An actuator arms an instrument in
/// the test kernel, and what goes on a stick is the shipping kernel — so an
/// actuator name is refused here by name rather than reaching
/// [`build_test_image`]'s assert, which would answer about kernel features.
///
/// **Every valued parameter is cleared here by name.**
pub fn flashable_params(root: &Path, asked: &[String]) -> Result<(), String> {
    let own = declared_params(root);
    for name in asked {
        if !own.contains(name) && !is_valued_param(name) {
            return Err(format!(
                "--kernel-param {name} beside --boot-config: {name} is not one of the kernel's \
                 boot parameters {own:?} or one carrying a value ({}), and a flashed image \
                 carries no actuator",
                valued_params().join(", ")
            ));
        }
    }
    Ok(())
}

/// The cargo feature list this build's kernel is compiled with, as one comma-
/// separated argument.
///
/// **Every name the caller asked for is checked against `kernel/Cargo.toml`,
/// and an unknown one stops the build by name**, as does a control or a name in
/// [`KERNEL_CARRIES`]: neither is a kernel build. Read from the manifest rather
/// than listed here, so the check cannot drift from what cargo would accept —
/// and, more to the point, so that deleting a feature takes its own command
/// lines down with it. That is what a temporary feature needs: once one is
/// deleted, an invocation still asking for it fails saying so instead of
/// quietly producing a kernel with no diagnostic in it, which is the same
/// image and a different machine.
///
/// Cargo would refuse an unknown feature too — after the build lock, the
/// toolchain check and the userland build, and with `kernel` in the message
/// rather than the flag the user typed. This runs before any of them.
fn kernel_features(
    root: &Path,
    debug: bool,
    requested: &[String],
    params: &[String],
) -> String {
    let mut features: Vec<&str> = Vec::new();
    if debug {
        features.push(DEBUG_KERNEL_BUILD);
    }
    // A parameter names an actuator or one of the kernel's own boot parameters,
    // and only the first needs a kernel compiled with them.
    let own = declared_params(root);
    if params.iter().any(|p| !own.contains(p) && !is_valued_param(p)) {
        features.push("boot-actuators");
    }
    if !requested.is_empty() {
        let declared = declared_kernel_features(root);
        for name in requested {
            assert!(
                declared.contains(name),
                "--kernel-feature {name}: the kernel declares no such feature.\n\
                 Features it declares: {}.\n\
                 Every actuator is now a --kernel-param; `cargo run -- --kernel-param --help` \
                 lists them.",
                declared.join(", ")
            );
            assert!(
                !crate::ci::CONTROLS.iter().any(|c| c.feature == name)
                    && !KERNEL_CARRIES.contains(&name.as_str()),
                "--kernel-feature {name}: a model's negative control or a name the kernel \
                 declares for another package's build of its sources, and never a kernel build"
            );
            features.push(name);
        }
    }
    features.join(",")
}

/// Every `--kernel-param` checked against what `kernel/src/actuator.rs` and
/// `kernel/src/params.rs` declare.
///
/// Refused here as well as in the kernel, and before any lock, so that deleting
/// an actuator takes its stale command lines down with it instead of quietly
/// producing an image that arms nothing — the same rule `--kernel-feature` runs
/// on, one layer further in.
fn check_params(root: &Path, params: &[String]) {
    if params.is_empty() {
        return;
    }
    let declared = declared_actuators(root);
    let own = declared_params(root);
    for name in params {
        assert!(
            declared.contains(name) || own.contains(name) || is_valued_param(name),
            "--kernel-param {name}: the kernel declares no such actuator or boot parameter.\n\
             Actuators it declares: {}.\n\
             Boot parameters it declares: {}.\n\
             Boot parameters carrying a value: {}.",
            declared.join(", "),
            own.join(", "),
            valued_params().join(", "),
        );
    }
}

/// The boot parameters that carry a value after their name, each beside the
/// path `kernel/src/params.rs`'s `claims` matches it by.
///
/// **Not read out of `PARAMS`, because they are not in it**: a flag is a name
/// the kernel matches whole, and these are prefixes it matches with
/// `starts_with`. The path is here so the two lists can be checked against each
/// other — a prefix only one of them knows is an image the other refuses.
const VALUED_PARAMS: &[(&str, &str)] = &[
    ("toyos_blackbox::PARAM", toyos_blackbox::PARAM),
    ("toyos_tco::DEADLINE_PARAM", toyos_tco::DEADLINE_PARAM),
    // The loader's words about the slot it booted, appended as it appends the
    // black box's.
    ("toyos_abi::boot::SLOT_PARAM", toyos_abi::boot::SLOT_PARAM),
    ("toyos_abi::boot::SLOT_REFUSED_PARAM", toyos_abi::boot::SLOT_REFUSED_PARAM),
];

/// The names in [`VALUED_PARAMS`], for a refusal that says what it would have
/// taken.
pub fn valued_params() -> Vec<String> {
    VALUED_PARAMS.iter().map(|(_, name)| (*name).to_string()).collect()
}

/// Whether `param` is one of [`VALUED_PARAMS`] with its value after it.
pub fn is_valued_param(param: &str) -> bool {
    VALUED_PARAMS.iter().any(|(_, prefix)| param.starts_with(prefix))
}

/// Every prefix `kernel/src/params.rs`'s `claims` matches with `starts_with`,
/// as the constant paths it names them by.
///
/// The gate's own reading of the kernel, so the two lists of valued parameters
/// can be asserted equal; nothing in a build needs it.
///
/// Anchored on the function and closed on the first line that ends it, so a
/// reflow still reads and a declaration this cannot find is empty rather than
/// guessed.
#[cfg(test)]
fn prefixes_claimed(text: &str) -> Vec<String> {
    let Some((_, body)) = text.split_once("pub fn claims") else { return Vec::new() };
    let Some((body, _)) = body.split_once("\n}") else { return Vec::new() };
    body.match_indices("starts_with(")
        .filter_map(|(at, marker)| {
            let rest = &body[at + marker.len()..];
            rest.split_once(')').map(|(path, _)| path.trim().to_string())
        })
        .collect()
}

/// The boot parameters the kernel itself answers to, off `kernel/src/params.rs`.
pub fn declared_params(root: &Path) -> Vec<String> {
    let path = root.join("kernel/src/params.rs");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    let names = params_of(&text);
    assert!(!names.is_empty(), "{} declares no boot parameters", path.display());
    names
}

/// Every name in `PARAMS`: anchored on the name and closed on `];`, so a reflow still reads and an unfound declaration is empty rather than guessed.
///
/// A name is a string literal there or `toyos_tco::PARAM`, the one constant the
/// bootloader reads the same parameter out of. A row spelled any other way
/// resolves to nothing, which the count below refuses rather than passing on
/// the names it did resolve.
fn params_of(text: &str) -> Vec<String> {
    let Some((_, body)) = text.split_once("pub const PARAMS") else { return Vec::new() };
    let Some((body, _)) = body.split_once("];") else { return Vec::new() };
    let mut names: Vec<String> = body.split('"').skip(1).step_by(2).map(str::to_string).collect();
    names.extend(body.matches("toyos_tco::PARAM").map(|_| toyos_tco::PARAM.to_string()));
    assert!(
        names.len() == rows_of(body),
        "`PARAMS` lists {} row(s) and this reads {} name(s) out of them: {names:?}",
        rows_of(body),
        names.len()
    );
    names
}

/// How many `(name, flag)` rows the list holds: an opening paren at the slice's
/// own depth, so a paren inside a row is part of that row and not another.
///
/// The value's `&[`, not the type's, which is the last one before the rows.
fn rows_of(body: &str) -> usize {
    let Some((_, rows)) = body.rsplit_once("&[") else { return 0 };
    let mut depth = 0usize;
    let mut count = 0;
    for c in rows.chars() {
        match c {
            '(' => {
                if depth == 0 {
                    count += 1;
                }
                depth += 1;
            }
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    count
}

#[derive(Deserialize)]
struct KernelManifest {
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}

fn declared_kernel_features(root: &Path) -> Vec<String> {
    let path = root.join("kernel/Cargo.toml");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    let manifest: KernelManifest = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()));
    manifest.features.into_keys().collect()
}

/// The features the kernel declares for another package's build of its
/// sources: `loom`, which `kernel-loom` turns on, and `protocol-port`, which
/// `kernel-sim` does.
const KERNEL_CARRIES: &[&str] = &["loom", "protocol-port"];

/// The kernel every test that needs an actuator boots: all of them compiled in
/// and none of them armed, plus the `SYS_DEBUG` number.
///
/// **One list, so one build.** `kernel_key` is over the joined string, so a
/// second spelling of this set would be a second kernel and nothing would say
/// so.
pub const TEST_KERNEL: &[&str] = &["boot-actuators", "test-actuators"];

/// Kernel builds the ordinary test suite is allowed to make.
pub const TEST_SUITE_KERNEL_BUILDS: [&str; 3] =
    ["", "boot-actuators,test-actuators", MASK_WINDOWS_KERNEL[0]];

/// The shipping kernel with the windows' instrument and nothing else, for
/// [`SCHED_CHECK_KERNEL`]'s reason: one spelling, so one build.
pub const MASK_WINDOWS_KERNEL: &[&str] = &["mask-windows"];

/// The scheduler core's own asserts, compiled in.
///
/// One name, for [`TEST_KERNEL`]'s reason — a second spelling is a second
/// kernel and nothing would say so.
pub const SCHED_CHECK_KERNEL: &[&str] = &["sched-check"];

/// The kernel build used only by the harness's interactive debugger.
pub const DEBUG_KERNEL_BUILD: &str = "debug-wait";

/// Whether the test harness's current mode declares this kernel build.
pub fn harness_kernel_build_is_declared(features: &str, debug_wait: bool) -> bool {
    if debug_wait {
        features == DEBUG_KERNEL_BUILD
    } else {
        TEST_SUITE_KERNEL_BUILDS.contains(&features)
    }
}

/// The manifest bytes and the symlink table `config` renders to, for a reader
/// that judges the finished ROOT against what the config asked for.
pub fn manifest_and_symlinks(config: &Path) -> (Vec<u8>, Vec<(String, String)>) {
    let config = parse_config(config);
    let symlinks = config.symlinks.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    (render_manifest(&config), symlinks)
}

/// Every actuator `kernel/src/actuator.rs` declares, read out of the file that
/// declares them.
///
/// Read rather than listed here for `declared_kernel_features`' reason, one
/// layer in: deleting an actuator has to take its own command lines and its own
/// `BootOptions` with it, rather than leaving a name that quietly arms nothing.
/// The kernel's own parser refuses an unknown token as well, so this is the
/// early half of a two-sided answer and not the only one.
pub fn declared_actuators(root: &Path) -> Vec<String> {
    let path = root.join("kernel/src/actuator.rs");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    let body = text
        .split_once("\nactuators! {\n")
        .expect("kernel/src/actuator.rs has no `actuators!` block")
        .1;
    let body = body.split_once("\n}\n").expect("the `actuators!` block does not end").0;
    let names: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("///"))
        .filter_map(|line| line.split_once(" = \"")?.1.strip_suffix("\";").map(str::to_string))
        .collect();
    assert!(!names.is_empty(), "{} declares no actuators", path.display());
    names
}

/// Refuse a kernel that does not carry exactly the `names` its feature set
/// says it does: none of them in the shipping kernel, all of them in the one
/// built `with`, and no opinion about any other build.
///
/// **Both directions, because one of them is a spelling of `true`.** The
/// kernel that must carry every name is what says the search can find one.
/// `what` names them in the refusal and `why` is the sentence under it.
fn assert_names_match_features<S: AsRef<str> + std::fmt::Debug>(
    features: &str,
    kernel: &[u8],
    with: &[&str],
    names: &[S],
    what: &str,
    why: &str,
) {
    let want = match features {
        "" => false,
        f if f == with.join(",") => true,
        _ => return,
    };
    let named = |name: &&S| contains_subslice(kernel, name.as_ref().as_bytes());
    let wrong: Vec<&S> = names.iter().filter(|n| named(n) != want).collect();
    assert!(
        wrong.is_empty(),
        "the {} kernel {} {} of the {} {what}: {wrong:?}.\n{why}",
        if want { with.join(",") } else { "shipping".to_string() },
        if want { "is missing" } else { "names" },
        wrong.len(),
        names.len(),
    );
}

/// Refuse to write an image whose kernel does not carry exactly the actuators
/// its feature set says it does.
///
/// [`assert_overflow_checked`]'s shape and for its reason: the property is
/// about the artifact, so the artifact is what is asked. Two builds quietly
/// becoming one build with test hooks in it is the failure mode this exists
/// for, and a convention nothing enforces is not a bar.
///
/// A shipping kernel must name none of them; the test kernel must name all of
/// them — measured on the two binaries this build produced when the tree
/// declared 47 of them: 0 of 47 at 3,829,440 bytes and 47 of 47 at 4,247,272.
/// The count is read off the file, so adding one moves the assertion and not
/// this sentence.
///
/// A kernel built with no features is the shipping one, because that is what
/// "shipping" means here: `--kernel-feature`, `--kernel-param` and `--debug`
/// each say out loud that this image is not one.
fn assert_actuators_match_features(root: &Path, features: &str, kernel: &[u8]) {
    assert_names_match_features(
        features,
        kernel,
        TEST_KERNEL,
        &declared_actuators(root),
        "actuators `kernel/src/actuator.rs` declares",
        "Everything under that file belongs to a kernel built with `boot-actuators`, and an \
         image that ships must not be able to be told to break.",
    );
}

/// `arch::syscall::syscall_entry`'s v0-mangled path, less the crate
/// disambiguator that stands in front of it.
const SYSCALL_ENTRY_SYMBOL: &str = "6kernel4arch6x86_647syscall13syscall_entry";

/// `cld`, which `arch::entry::ring3_naked_asm` puts first in every Ring 0 entry.
const CLD: u8 = 0xfc;
/// `mov gs:[disp32], rsp` less its displacement: the `gs` override, `REX.W`,
/// the opcode, and the ModRM/SIB pair that says `rsp` against an absolute
/// `disp32`.
const SAVE_RSP_TO_GS: [u8; 5] = [0x65, 0x48, 0x89, 0x24, 0x25];
/// `mov rsp, gs:[disp32]` less its displacement.
const LOAD_RSP_FROM_GS: [u8; 5] = [0x65, 0x48, 0x8b, 0x24, 0x25];
/// Bytes either `mov` occupies, displacement included.
const MOV_RSP_GS_LEN: usize = 9;

/// A kernel's `syscall_entry` from its first instruction to the end of the
/// segment it is in, found at its symbol.
fn syscall_entry_bytes(kernel: &[u8]) -> Result<&[u8], String> {
    use toyos_elf::header::{ProgramHeader, PT_LOAD};

    let (syms, strs) =
        toyos_symbols::locate(kernel).ok_or("the kernel has no readable `.symtab`")?;
    let table = toyos_elf::SymTab::new(syms, strs);
    let mut entries = table.defined().filter(|&(i, sym)| {
        sym.kind() == toyos_elf::sym::STT_FUNC && table.name(i).ends_with(SYSCALL_ENTRY_SYMBOL)
    });
    let (Some((_, entry)), None) = (entries.next(), entries.next()) else {
        return Err(format!(
            "the kernel's `.symtab` does not name exactly one function `…{SYSCALL_ENTRY_SYMBOL}`"
        ));
    };
    let header = toyos_elf::FileHeader::parse(kernel).map_err(|e| format!("{e:?}"))?;
    let extent = toyos_elf::Layout::parse(kernel, header.machine).map_err(|e| format!("{e}"))?.extent();
    let value = entry
        .address(extent)
        .map(|at| extent.min() + at.get())
        .ok_or("the kernel's `syscall_entry` lies outside its own image")?;
    let segments = header.program_headers(kernel).map_err(|e| format!("{e:?}"))?;
    (0..header.phnum as usize)
        .filter_map(|i| ProgramHeader::parse(segments, i))
        .filter(|segment| segment.kind == PT_LOAD)
        .find_map(|segment| {
            let within = value.checked_sub(segment.vaddr)?;
            let left = segment.filesz.checked_sub(within).filter(|&left| left != 0)?;
            toyos_symbols::file_range(kernel, segment.offset.checked_add(within)?, left)
        })
        .ok_or_else(|| format!("no `PT_LOAD` holds `syscall_entry` at {value:#x} in the file"))
}

/// Whether an entry switches to the kernel's `rsp` in the instruction after it
/// saves the user's, and the bytes that stand between the two where it does
/// not.
///
/// An entry that does not open `cld`, save is one this cannot read, and is
/// refused for both kernels rather than passed for either.
fn entry_window(entry: &[u8]) -> Result<Result<(), &[u8]>, String> {
    let after_save = 1 + MOV_RSP_GS_LEN;
    if entry.first() != Some(&CLD) || !entry.get(1..).is_some_and(|e| e.starts_with(&SAVE_RSP_TO_GS))
    {
        return Err(format!(
            "`syscall_entry` does not open `cld`, `mov gs:[…], rsp`: it opens {:02x?}",
            &entry[..entry.len().min(after_save)],
        ));
    }
    let rest = entry.get(after_save..).unwrap_or_default();
    if rest.starts_with(&LOAD_RSP_FROM_GS) {
        return Ok(Ok(()));
    }
    // Up to the switch where one follows within what a hold could take, and a fixed span where none does.
    let between = rest
        .windows(LOAD_RSP_FROM_GS.len())
        .take(256)
        .position(|w| w == LOAD_RSP_FROM_GS)
        .unwrap_or(rest.len().min(64));
    Ok(Err(&rest[..between]))
}

/// Refuse to write a shipping image whose `syscall_entry` does anything between
/// saving the user's `rsp` and switching to the kernel's.
///
/// Read at the entry's own symbol, the shipping kernel's first three
/// instructions are `cld`, the save and the switch, with nothing between.
fn judge_entry_window(features: &str, kernel: &[u8]) -> Result<(), String> {
    if !features.is_empty() {
        return Ok(());
    }
    match entry_window(syscall_entry_bytes(kernel)?)? {
        Ok(()) => Ok(()),
        Err(between) => Err(format!(
            "the shipping kernel's `syscall_entry` does not switch to the kernel's `rsp` in the \
             instruction after it saves the user's; between them stand {between:02x?}.\nEvery \
             instruction there runs at CPL 0 on a user's stack, and an image that ships must \
             not be able to be asked to stop there."
        )),
    }
}

fn assert_entry_window_matches_features(features: &str, kernel: &[u8]) {
    if let Err(refusal) = judge_entry_window(features, kernel) {
        panic!("{refusal}");
    }
}

/// The scheduler core's `feature = "check"` instruments, by their own text, and
/// the two kernels that must disagree about carrying them.
///
/// Every one of these is a `#[cfg(feature = "sched-check")]` site in `kernel::sched`.
/// Two are asserts from `invariants::check_cpu` — invariant T's armed-timer
/// bound and the container-versus-state-word agreement. The third is the
/// pass-cost report, which is a *measurement*
/// and not an assert: a pass's elapsed time includes any interval a hypervisor
/// took the CPU away, so it is recorded rather than panicked over. Their format
/// strings are the only part of the check build with a literal the linker keeps,
/// which is what makes the artifact answerable at all — and the report's literal
/// is kept out of the shipping kernel by nothing but dead-code elimination,
/// which the `want == false` direction below is what checks.
const SCHED_CHECK_LITERALS: [&str; 3] = [
    "sched-check pass-costs cpu=",
    "invariant T: cpu",
    "disagrees with its state word",
];

/// Refuse to write an image whose scheduler instruments do not match the
/// feature set that decides whether they exist.
///
/// [`assert_actuators_match_features`]'s shape and its reason: the property is
/// about the artifact, so the artifact is what is asked, and a convention
/// nothing enforces is not a bar.
///
/// Measured on the two binaries this build produces: 0 of 3 in the shipping
/// kernel, 3 of 3 in the `sched-check` one.
fn assert_sched_check_matches_features(features: &str, kernel: &[u8]) {
    assert_names_match_features(
        features,
        kernel,
        SCHED_CHECK_KERNEL,
        &SCHED_CHECK_LITERALS,
        "scheduler check instruments",
        "`sched-check` compiles the scheduler core's instruments in, so a build that carries the \
         feature and not the instruments is a check build in name only.",
    );
}

/// The loader's package.
const LOADER: &str = "bootloader";

/// Build the kernel with `features`, comma-separated.
fn build_kernel(root: &Path, arch: Arch, features: &str, env: &GuestEnv, quiet: bool) {
    let mut args = vec!["-p", crate::ci::KERNEL];
    if !features.is_empty() {
        args.extend(["--features", features]);
    }
    cargo_build(root, arch.kernel(), &args, env, &[], quiet);
}

/// Stage the freshly built kernel under its feature key, read it back, and run
/// every artifact assertion against it — returning the certified bytes.
///
/// **Both build paths route their kernel through here, which is the whole of
/// the guarantee that they certify the same set.** This stage→read→assert
/// sequence was hand-matched between [`build`] and [`build_test_image`], so an
/// assertion added to one certified only that path's kernel while the other
/// shipped uncertified. There is now one place to add an assertion, and it is
/// the kernel of every image this build system produces that gets it. The
/// caller has already run `cargo_build` on the kernel crate and must hold
/// [`buildlock::artifact`], since the stage below copies the shared cargo path.
/// Stage the loader `arch`'s build just wrote, under the key that names it.
/// The caller holds [`buildlock::artifact`], as [`stage_and_certify_kernel`]'s does.
fn stage_loader(root: &Path, arch: Arch, env: &GuestEnv) -> PathBuf {
    stage_artifact(
        root,
        &root.join(format!("target/{}/{PROFILE}/bootloader.efi", arch.loader())),
        &format!("bootloader-{}.efi", arch.name()),
        loader_key(arch, &env.image_key, env.floor_scope),
    )
}

fn stage_and_certify_kernel(root: &Path, features: &str, env: &GuestEnv, arch: Arch) -> Vec<u8> {
    let staged = stage_artifact(
        root,
        &root.join(format!("target/{}/{PROFILE}/kernel", arch.kernel())),
        &format!("kernel-{}", arch.name()),
        kernel_key(arch, features),
    );
    let bytes = fs::read(&staged).expect("Failed to read staged kernel");
    assert_overflow_checked("kernel", &bytes);
    assert_actuators_match_features(root, features, &bytes);
    match arch {
        Arch::X86_64 => {
            assert_entry_window_matches_features(features, &bytes);
        }
        // Taking an exception to EL1 sets `PSTATE.SP`, so its handler's first
        // instruction already runs on `SP_EL1`: no instruction runs at EL1 on a
        // user's stack, and there is no window to judge.
        Arch::Aarch64 => {}
    }
    assert_sched_check_matches_features(features, &bytes);
    assert_kernel_is_softfloat(env, arch);
    bytes
}

/// Full build: kernel, bootloader, all programs, boot image. Returns the image.
pub fn build(root: &Path, boot: Boot, plan: &Plan) -> PathBuf {
    // Every lock below is `build_test_image`'s own, and the flags it cannot
    // combine with were refused before any of them.
    if boot.case {
        let bytes = build_test_image(root, plan, false, &[]);
        let image_path = root.join(boot.image_for(plan.arch));
        fs::write(&image_path, bytes)
            .unwrap_or_else(|e| panic!("write {}: {e}", image_path.display()));
        return image_path;
    }

    let (kernel_bytes, bl_bytes, root_bytes) = shipped_parts(root, &boot, plan);
    let key = said_key(plan);
    // A machine this image is flashed onto updates itself, so it carries
    // the second slot an update is written to, with room for a ROOT twice
    // this one's size.
    let second = image::SecondSlot { root_bytes: 2 * root_bytes.len() as u64 };
    let disk_bytes = image::create_boot_image(
        plan.arch,
        &kernel_bytes,
        &bl_bytes,
        &root_bytes,
        &plan.params.join(","),
        image::Signing { key, version: plan.version },
        Some(second),
    );
    let image_path = root.join(boot.image_for(plan.arch));
    fs::write(&image_path, disk_bytes).expect("Failed to write image");

    let nvme_path = root.join("target/nvme.img");
    if !nvme_path.exists() {
        create_sparse(&nvme_path, 1024 * 1024 * 1024);
    }

    image_path
}

/// The image `ssh <machine> update` takes, of the boot `boot` names, written
/// to `out`: the same kernel, parameter and ROOT [`build`] would put in a
/// slot, signed with this run's key at the plan's version.
pub fn build_update(root: &Path, boot: &Boot, plan: &Plan, out: &Path) {
    assert!(!boot.case, "an update image is built from a mode's config, and a case's image is a test's");
    let (kernel_bytes, _, root_bytes) = shipped_parts(root, boot, plan);
    let key = said_key(plan);
    let bytes = image::update_image(
        &kernel_bytes,
        &root_bytes,
        &plan.params.join(","),
        image::Signing { key, version: plan.version },
    );
    fs::write(out, bytes).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
}

/// This run's key, said with whose it is and the version it signs.
fn said_key(plan: &Plan) -> &'static crate::signing::Key {
    let key = crate::signing::key();
    eprintln!(
        "Signed with {} {} at version {}",
        match key.whose() {
            crate::signing::Whose::Owner(_) => "the owner's key",
            crate::signing::Whose::Throwaway => "this checkout's throwaway key",
        },
        key.fingerprint(),
        plan.version
    );
    key
}

/// The kernel, the loader and ROOT a mode's image is made of.
fn shipped_parts(root: &Path, boot: &Boot, plan: &Plan) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let kernel_features = plan.features.join(",");
    let arch = plan.arch;

    // Held until the last staged artifact has been read back, so no clean of
    // this worktree's crate targets can land inside this build.
    let mut lock = buildlock::shared(root, "build");
    let config = parse_config(&boot.config);
    let env = GuestEnv::new(toolchain::ensure(root, &mut lock));

    invalidate_stale(&mut lock, &env.sysroot.identity, &[root.to_path_buf()]);

    // Same lock-and-stage as `build_test_image`: `cargo run --build-only` and
    // `cargo test` share these paths, so this races the harness too. The kernel
    // is staged and certified through the same [`stage_and_certify_kernel`] that
    // path uses, so neither can grow an assertion the other lacks.
    let (kernel_bytes, bl_art) = {
        let _artifact = buildlock::artifact(root);
        // One after the other: cargo holds one lock on the target directory.
        build_kernel(root, arch, &kernel_features, &env, false);
        cargo_build(root, arch.loader(), &["-p", LOADER], &env, &[], false);
        (
            stage_and_certify_kernel(root, &kernel_features, &env, arch),
            stage_loader(root, arch, &env),
        )
    };

    let root_bytes = build_and_assemble(root, &config, &env, &[], false, arch);

    let bl_bytes = fs::read(&bl_art).expect("Failed to read staged bootloader");
    (kernel_bytes, bl_bytes, root_bytes)
}

/// Create an empty disk image the guest sees at full size and the host pays
/// nothing for until something is written. A materialized image caps how big
/// a device the tests may present, and device *size* is a shape dimension:
/// an index sized per device block is invisible on a small disk and fatal on
/// a real one.
///
/// Designates the result, because every caller here is making a scratch disk
/// for a guest that expects a working `/apps` and `/home`, and the kernel will
/// not format an undesignated one. Leaving it to the call sites would mean two
/// places to forget; forgetting is not silent (the boot says so and both paths
/// are volatile) but it is not worth the chance.
pub fn create_sparse(path: &Path, len: u64) {
    let file = fs::File::create(path)
        .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    file.set_len(len)
        .unwrap_or_else(|e| panic!("set_len {} on {}: {e}", len, path.display()));
    designate_for_format(path, len);
}

/// Partition this image as a DATA disk and stamp the designation on that
/// partition, so the kernel is allowed to format it.
///
/// The kernel never formats a volume that does not carry the stamp, which is
/// what stops it taking the disk of any machine it is booted on. So a throwaway
/// image has to say so, and this is the whole of the test harness's opt-in:
/// **data on a scratch file, not a build flag.** The kernel binary and the
/// code path are identical either way — `probe` runs the same match
/// on metal as it does here — so the configuration under test is the
/// configuration that ships, which a `#[cfg]` could not have given us.
///
/// Only ever called on a file this build system just created. It is a
/// destructive write by construction: on a device with anything on it, this
/// overwrites the partition table.
pub fn designate_for_format(path: &Path, len: u64) {
    image::designate_data_disk(path, len);
}

/// One part of a boot image, built once per key for the life of this process.
///
/// A `cargo test` run boots ~76 machines, and most of those boots ask for an
/// image some earlier boot already built; the three `cargo` invocations then
/// take ~1.4 s between them to answer "nothing changed". In memory and never on
/// disk, so a run gets one answer for the tree it started against and the next
/// run asks cargo again.
///
/// Per part rather than per image, because a part is what a key can be true of:
/// the kernel is its feature set, the ROOT image is
/// its config and the caller's extra files. That is the same split
/// [`stage_artifact`] already writes into the artifact names, and it is what
/// makes this affordable — a full run boots a handful of kernels, and builds
/// each ROOT image once for its config and the test binaries a task carries.
///
/// What it does not see is a source edit that lands mid-run. A run is a
/// measurement of one tree, so that is the behaviour wanted either way; a run
/// that *starts* after a kernel edit still rebuilds every variant it uses.
struct Memo(std::sync::Mutex<BTreeMap<u64, Arc<Vec<u8>>>>);

impl Memo {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(BTreeMap::new()))
    }

    fn get(&self, key: u64) -> Option<Arc<Vec<u8>>> {
        self.0.lock().expect("a build panicked holding the artifact memo").get(&key).cloned()
    }

    /// The lock is deliberately not held across `make`: a build that panics
    /// under it would poison the memo, and every later boot would then fail on
    /// the poison instead of on whatever went wrong with it.
    fn get_or_build(&self, key: u64, make: impl FnOnce() -> Vec<u8>) -> Arc<Vec<u8>> {
        if let Some(hit) = self.get(key) {
            return hit;
        }
        let made = Arc::new(make());
        self.0
            .lock()
            .expect("a build panicked holding the artifact memo")
            .insert(key, Arc::clone(&made));
        made
    }
}

static KERNEL: Memo = Memo::new();
static BOOTLOADER: Memo = Memo::new();
static ROOT_IMAGE: Memo = Memo::new();

/// What the ROOT image is a function of: the config naming the programs, the
/// architecture and whether they are built at all, the key its
/// `/system/bin/update` embeds, and the files the caller adds to it. Hashed whole: a key over the test binaries'
/// names and lengths would call two different builds of one binary the same
/// image.
fn root_image_key(plan: &Plan, image_key: &str, extra_files: &[(String, Vec<u8>)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    plan.config.hash(&mut h);
    plan.arch.hash(&mut h);
    image_key.hash(&mut h);
    for (name, data) in extra_files {
        name.hash(&mut h);
        data.hash(&mut h);
    }
    h.finish()
}

/// Build a test image from a system.toml config. Returns the raw disk image bytes.
/// The caller writes these to a temp file for QEMU.
///
/// The image itself is never memoized, only the three parts it is made of:
/// [`image::create_boot_image`] mints a fresh partition GUID per call and writes
/// it into both the GPT and the ESP.
pub fn build_test_image(
    root: &Path,
    plan: &Plan,
    quiet: bool,
    extra_files: &[(String, Vec<u8>)],
) -> Vec<u8> {
    let parts = build_test_parts(root, plan, quiet, extra_files);
    image::create_boot_image(
        plan.arch,
        &parts.kernel,
        &parts.bootloader,
        &parts.root,
        &plan.params.join(","),
        image::Signing { key: crate::signing::key(), version: plan.version },
        plan.second,
    )
}

/// The three parts one image is made of, each memoized for this process.
pub struct Parts {
    pub kernel: Arc<Vec<u8>>,
    pub bootloader: Arc<Vec<u8>>,
    pub root: Arc<Vec<u8>>,
}

/// [`Parts`] for a plan: built, or this process's memo of them.
pub fn build_test_parts(
    root: &Path,
    plan: &Plan,
    quiet: bool,
    extra_files: &[(String, Vec<u8>)],
) -> Parts {
    let config_path = plan.config.as_path();
    let (kernel_features, kernel_params) = (&plan.features, &plan.params);
    let config = parse_config(config_path);
    let features = kernel_features.join(",");
    // **The kernel is not keyed on this and that is the whole change.** A
    // parameter picks which actuator the one test kernel arms, so 45 builds
    // became two; keying the image on it is what keeps two boots asking for
    // different actuators from sharing one disk.
    let own = declared_params(root);
    assert!(
        kernel_params.iter().all(|p| own.contains(p) || is_valued_param(p))
            || kernel_features.iter().eq(TEST_KERNEL.iter().copied()),
        "a boot asking for {kernel_params:?} must boot the test kernel, not {kernel_features:?}"
    );
    let arch = plan.arch;
    let kernel_key = kernel_key(arch, &features);
    // The loader and ROOT (whose `/system/bin/update` embeds it) are each a
    // function of the key this process signs with.
    let image_key = crate::signing::key().public_hex();
    let bl_key = loader_key(arch, &image_key, crate::signing::key().floor_scope().word());
    let root_image_key = root_image_key(plan, &image_key, extra_files);

    // Nothing left to build, so nothing for the lock, the toolchain check or the
    // staleness sweep to protect.
    let memo = (KERNEL.get(kernel_key), BOOTLOADER.get(bl_key), ROOT_IMAGE.get(root_image_key));
    if let (Some(kernel), Some(bootloader), Some(root)) = memo {
        return Parts { kernel, bootloader, root };
    }

    // A cache miss is shared setup, not a property of whichever test happened
    // to be first on this shard. Keep it on a separate clock until all missing
    // memo parts have been constructed. The fresh per-boot image below remains
    // outside the charge because every execution needs one.
    let missing = [
        memo.0.is_none().then(|| format!("kernel {features}").trim_end().to_string()),
        memo.1.is_none().then(|| "loader".to_string()),
        memo.2.is_none().then(|| {
            format!("ROOT of {}", hostws::rel(root, config_path.parent().unwrap_or(config_path)))
        }),
    ];
    let missing: Vec<String> = missing.into_iter().flatten().collect();
    let building = Building::start(format!("{} {}", arch.name(), missing.join(", ")));

    // Held to the end of the function: the staged artifacts below are read
    // back after the userland build, and a clean landing in between is the
    // same defect as one landing mid-compile.
    let mut lock = buildlock::shared(root, "test image");
    let env = GuestEnv::new(toolchain::ensure(root, &mut lock));

    invalidate_stale(&mut lock, &env.sysroot.identity, &[root.to_path_buf()]);

    // Build and stage under one lock, released before `build_and_assemble`.
    // Releasing it there is deliberate and required: that build takes its own
    // `buildlock::artifact` across its build→read window (it reads shared cargo
    // paths too), so holding this one across the long userland build would
    // deadlock the process against itself — and the staged copies below are
    // already immune to another config's rebuild.
    let (kernel_bytes, bl_bytes) = {
        let _artifact = buildlock::artifact(root);
        let kernel = KERNEL.get_or_build(kernel_key, || {
            build_kernel(root, arch, &features, &env, quiet);
            stage_and_certify_kernel(root, &features, &env, arch)
        });
        let bl = BOOTLOADER.get_or_build(bl_key, || {
            cargo_build(root, arch.loader(), &["-p", LOADER], &env, &[], quiet);
            fs::read(stage_loader(root, arch, &env)).expect("Failed to read staged bootloader")
        });
        (kernel, bl)
    };

    let root_bytes = ROOT_IMAGE.get_or_build(root_image_key, || {
        build_and_assemble(root, &config, &env, extra_files, quiet, arch)
    });

    drop(building);

    Parts { kernel: kernel_bytes, bootloader: bl_bytes, root: root_bytes }
}

/// The host binaries the network judges drive, built here rather than inside a
/// test: a judge's price is its exchange and not a compile.
///
/// Each of these keeps its own `Cargo.lock` and is excluded from the
/// workspace. That is what makes them possible: they exist to be
/// a *second* implementation, and a second implementation's dependency graph is
/// not the harness's to resolve.
pub fn build_host_judges(root: &Path, quiet: bool) {
    for (dir, _) in HOST_JUDGES {
        let _building = Building::start(format!("the host's {dir}"));
        let at = root.join(dir);
        let mut cmd = Command::new("cargo");
        cmd.args(["build", "--release"]);
        if quiet {
            cmd.arg("--quiet");
        }
        let status = cmd
            .current_dir(&at)
            .env_remove("RUSTUP_TOOLCHAIN")
            .env_remove("RUSTC")
            .env_remove("RUSTFLAGS")
            .status()
            .unwrap_or_else(|e| panic!("cargo failed to launch in {}: {e}", at.display()));
        assert!(status.success(), "{dir} did not build");
    }
}

/// One host judge: where its crate is, and the binary that crate builds. Named
/// rather than indexed, because a row inserted anywhere but the end would
/// silently repoint every accessor below.
type Judge = (&'static str, &'static str);

const SSH_CLIENT: Judge = ("tests/ssh-client-host", "toyos_ssh");

const HOST_JUDGES: [Judge; 1] = [SSH_CLIENT];

/// Copy to `to` the binary the build leaves for the program
/// `name`: the bytes a swap sends a running machine in place of the ones its
/// image carries. Read under the artifact lock, as every image build reads it.
pub fn copy_guest_program(root: &Path, arch: Arch, name: &str, to: &Path) -> Result<(), String> {
    let from = root.join(format!("target/{}/{PROFILE}/{name}", arch.userland()));
    let _artifact = buildlock::artifact(root);
    fs::copy(&from, to)
        .map(|_| ())
        .map_err(|e| format!("{} to {}: {e}", from.display(), to.display()))
}

/// The harness's SSH client — the only thing in this tree that speaks the
/// protocol from the other side of `userland/sshserver`.
pub fn ssh_client_host(root: &Path) -> PathBuf {
    host_judge(root, SSH_CLIENT)
}

fn host_judge(root: &Path, (dir, bin): Judge) -> PathBuf {
    root.join(dir).join("target/release").join(bin)
}

/// Build all binaries in a multi-binary crate. Returns vec of (binary_name, bytes).
/// Also builds any cdylib subcrates and includes their .so files.
///
/// **The test binaries are enumerated from `src/bin`, never from the target
/// directory**: cargo does not remove a binary when its source is deleted, so a
/// target-directory scan keeps shipping a renamed or merged test from an artifact
/// nothing in the tree can produce any more — into the ROOT image, into the test list,
/// and over the name of whatever gets it next.
pub fn build_toyos_bins(root: &Path, arch: Arch, crate_path: &Path, quiet: bool) -> Vec<(String, Vec<u8>)> {
    let _building = Building::start(format!("{} binaries of {}", arch.name(), hostws::rel(root, crate_path)));
    let mut targets = vec![crate_path.to_path_buf()];
    for entry in fs::read_dir(crate_path).into_iter().flatten().flatten() {
        let sub_path = entry.path();
        if sub_path.is_dir() && sub_path.join("Cargo.toml").exists() {
            targets.push(sub_path);
        }
    }
    let build = TestBuild::begin(root, arch, "test binaries", &targets);

    let (target, env) = (build.target, &build.env);

    let mut results = Vec::new();

    // Build cdylib subcrates first
    let mut lib_search_dirs = Vec::new();
    for entry in fs::read_dir(crate_path).unwrap() {
        let entry = entry.unwrap();
        let sub_path = entry.path();
        if !sub_path.is_dir() {
            continue;
        }
        let cargo_toml = sub_path.join("Cargo.toml");
        if !cargo_toml.exists() {
            continue;
        }
        let toml_text = fs::read_to_string(&cargo_toml).unwrap();
        if !toml_text.contains("cdylib") {
            continue;
        }

        let lib_name = sub_path.file_name().unwrap().to_str().unwrap();
        if !quiet {
            eprintln!("[build] Building cdylib subcrate: {lib_name}");
        }
        cargo_build(&sub_path, target, &[], env, &[], quiet);

        let lib_out = sub_path.join(format!("target/{target}/{PROFILE}"));
        lib_search_dirs.push(lib_out.clone());

        for so_entry in fs::read_dir(&lib_out).unwrap() {
            let so_entry = so_entry.unwrap();
            let name = so_entry.file_name().to_str().unwrap().to_string();
            if name.ends_with(".so") {
                let path = so_entry.path();
                let data = fs::read(&path)
                    .unwrap_or_else(|e| panic!("read the cdylib {}: {e}", path.display()));
                results.push((name, data));
            }
        }
    }

    // Build test binaries — pass -L flags for cdylib .so locations
    let mut link_flags = String::new();
    for dir in &lib_search_dirs {
        link_flags.push_str(&format!("-L {} ", dir.display()));
    }
    let extra_env: Vec<(&str, &str)> = if link_flags.is_empty() {
        vec![]
    } else {
        vec![("RUSTFLAGS", link_flags.trim_end())]
    };
    cargo_build(crate_path, target, &["--bins"], env, &extra_env, quiet);

    let bin_dir = crate_path.join(format!("target/{target}/{PROFILE}"));
    let bin_src = crate_path.join("src/bin");
    if bin_src.exists() {
        for entry in fs::read_dir(&bin_src).unwrap() {
            let entry = entry.unwrap();
            let name = entry
                .file_name()
                .to_str()
                .unwrap()
                .strip_suffix(".rs")
                .unwrap()
                .to_string();
            let binary = bin_dir.join(&name);
            if binary.exists() {
                let data = fs::read(&binary)
                    .unwrap_or_else(|e| panic!("read the test binary {}: {e}", binary.display()));
                results.push((name, data));
            }
        }
    }

    results
}

/// What building test binaries starts from, held until the read of what was built is done.
///
/// Every build→read pair is under one hold, for the reason the "Artifact
/// staging" section above gives: cargo keys an artifact path on (crate, target,
/// profile), so a second `cargo test` in this tree writes the very `.so` and
/// test binaries this one reads back. Between the `read_dir` and the `read`
/// that was enough to kill a run outright — four concurrent suites, one dead on
/// `Result::unwrap()` on a `NotFound` naming no file.
struct TestBuild {
    target: &'static str,
    env: GuestEnv,
    _lock: buildlock::Held,
    _artifact: buildlock::Guard,
}

impl TestBuild {
    fn begin(root: &Path, arch: Arch, what: &str, stale_targets: &[PathBuf]) -> Self {
        let mut lock = buildlock::shared(root, what);
        let env = GuestEnv::new(toolchain::ensure(root, &mut lock));
        invalidate_stale(&mut lock, &env.sysroot.identity, stale_targets);
        let artifact = buildlock::artifact(root);
        TestBuild { target: arch.userland(), env, _lock: lock, _artifact: artifact }
    }
}

/// The one binary `name` of the crate at `crate_path`, built for `arch`: for an
/// architecture the crate's other binaries do not all build for.
pub fn build_toyos_bin(root: &Path, arch: Arch, crate_path: &Path, name: &str, quiet: bool) -> Vec<u8> {
    let _building = Building::start(format!("{} {name} of {}", arch.name(), hostws::rel(root, crate_path)));
    let build = TestBuild::begin(root, arch, "a test binary", &[crate_path.to_path_buf()]);
    let (target, env) = (build.target, &build.env);
    cargo_build(crate_path, target, &["--bin", name], env, &[], quiet);
    let binary = crate_path.join(format!("target/{target}/{PROFILE}/{name}"));
    fs::read(&binary).unwrap_or_else(|e| panic!("read the test binary {}: {e}", binary.display()))
}

// --- Internal helpers ---

#[cfg(test)]
mod tests {
    use super::*;

    /// **The release a tree records is its commit, at that commit's own time,
    /// and dirty from the first file that is not that commit's**: an
    /// untracked file counts, and so does an edit to a tracked one, staged or
    /// not, whatever the checkout's own status shows. The time is the
    /// committer's, in a zone not UTC's, and never the author's or the host
    /// clock's.
    #[test]
    fn a_tree_records_its_commit_its_commit_time_and_whether_it_is_dirty() {
        let (_dir, _origin, work) = crate::gitfixture::repo("release");
        fs::write(work.join("f"), "next\n").unwrap();
        crate::gitfixture::sh(&work, &["add", "f"]);
        let committed = Command::new("git")
            .args(["commit", "-qm", "next"])
            .env("GIT_AUTHOR_DATE", "@1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1791089159 +0200")
            .current_dir(&work)
            .status()
            .unwrap();
        assert!(committed.success());
        // The ref as `git` wrote it, for an oracle that is not gitoxide.
        let head = fs::read_to_string(work.join(".git/refs/heads/wt")).unwrap();
        let key = crate::keystore::Key::parse("0123456789abcdef").unwrap();

        let clean = release(&work, &key, Arch::Aarch64);
        assert_eq!(clean.commit.as_str(), head.trim());
        assert_eq!(clean.committed, 1_791_089_159);
        assert_eq!(clean.tree, toyos_osrelease::Tree::Clean);
        assert_eq!(clean.toolchain.as_str(), key.as_str());
        assert_eq!(clean.arch, toyos_osrelease::Arch::Aarch64);

        fs::write(work.join("untracked"), "x").unwrap();
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Dirty);
        fs::remove_file(work.join("untracked")).unwrap();
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Clean);
        fs::write(work.join("f"), "edited\n").unwrap();
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Dirty);
        // An ignored file is the build's own output, not a change to the tree.
        fs::write(work.join("f"), "next\n").unwrap();
        fs::create_dir(work.join("target")).unwrap();
        fs::write(work.join("target/out"), "x").unwrap();
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Clean);
        // A checkout that hides untracked files from its own status.
        crate::gitfixture::sh(&work, &["config", "status.showUntrackedFiles", "no"]);
        fs::write(work.join("untracked"), "x").unwrap();
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Dirty);
        fs::remove_file(work.join("untracked")).unwrap();
        // A change staged, with the files as staged: only HEAD's tree differs.
        fs::write(work.join("f"), "staged\n").unwrap();
        crate::gitfixture::sh(&work, &["add", "f"]);
        assert_eq!(release(&work, &key, Arch::Aarch64).tree, toyos_osrelease::Tree::Dirty);
    }

    /// `console` is reached by `console/system.toml` alone and the supervisor by no
    /// `[programs]` row, so a reader that drops a mode or the supervisor loses one.
    #[test]
    fn every_modes_crates_and_the_supervisor_ship() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let shipped = shipped(root);
        for (dir, features) in [
            ("userland/console", Features::Default),
            ("userland/supervisor", Features::Default),
            ("kernel", Features::AnyDeclared),
            ("bootloader", Features::Default),
        ] {
            assert!(
                shipped.crates.contains(&(root.join(dir), features)),
                "{dir} with {features:?} is not among {:?}",
                shipped.crates
            );
        }
        let (kernel, loader) = (root.join("kernel"), root.join("bootloader"));
        let programs = shipped.crates.iter().map(|(dir, _)| dir.clone());
        assert_eq!(shipped.programs, programs.filter(|dir| *dir != kernel && *dir != loader).collect());
    }

    /// **Moved libraries take what was built for their targets, and nothing
    /// else**: an ABI edit moves the userland targets' libraries, so the
    /// kernels and the loaders stay and only `target/<userland triple>` goes; a
    /// fork edit moves every target's, so the kernels and the loaders go too.
    /// The host half the same compiler built stays either way. A moved
    /// compiler, or a stamp that names none, takes that half too, and never
    /// what the build system itself is built into.
    #[test]
    fn moved_libraries_take_what_was_built_for_them_and_a_moved_compiler_all() {
        let root = toyos_tmpdir::TempDir::new("moved-libraries");
        let file = |path: PathBuf| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "built").unwrap();
            path
        };
        let built = |name: &str, triple: fn(Arch) -> &'static str| {
            Arch::ALL.map(|arch| file(root.join(format!("target/{}/{PROFILE}/{name}", triple(arch)))))
        };
        let (kernels, loaders, programs) =
            (built("kernel", Arch::kernel), built("bootloader.efi", Arch::loader), built("supervisor", Arch::userland));
        let host = file(root.join(format!("target/{PROFILE}/deps/libproc-1.dylib")));
        let own = file(root.join("target/debug/toyos-build"));

        let before = Identity::of_parts("compiler", "freestanding", "toyos");
        fs::write(root.join("target/.deps-stamp"), before.to_string()).unwrap();
        assert_eq!(stale(&root, &before), None, "stale against its own stamp");
        let moved = |identity: &Identity, want: Stale| {
            let found = stale(&root, identity);
            assert_eq!(found, Some(want));
            clean(&root, &found.unwrap(), identity);
            assert_eq!(stale(&root, identity), None, "not stamped");
        };

        let mut toyos: Vec<&'static str> = Arch::ALL.map(Arch::userland).into();
        toyos.sort();
        moved(&Identity::of_parts("compiler", "freestanding", "toyos, edited"), Stale::Targets(toyos));
        for kept in kernels.iter().chain(&loaders).chain([&host, &own]) {
            assert!(kept.is_file(), "{} went, and nothing it was built from moved", kept.display());
        }
        for gone in &programs {
            assert!(!gone.exists(), "{} survived its target's libraries moving", gone.display());
        }

        let mut all: Vec<&'static str> = Arch::ALL.into_iter().flat_map(|arch| [arch.userland(), arch.kernel(), arch.loader()]).collect();
        all.sort();
        let fork_edit = Identity::of_parts("compiler", "freestanding, edited", "toyos on the edited fork");
        moved(&fork_edit, Stale::Targets(all));
        for gone in kernels.iter().chain(&loaders) {
            assert!(!gone.exists(), "{} survived its target's libraries moving", gone.display());
        }
        assert!(host.is_file() && own.is_file(), "the compiler did not move, and what it built for the host went");

        let rebuilt = built("kernel", Arch::kernel);
        moved(&Identity::of_parts("another compiler", "freestanding, edited", "toyos on the edited fork"), Stale::All);
        for gone in rebuilt.iter().chain([&host]) {
            assert!(!gone.exists(), "{} survived the compiler that built it moving", gone.display());
        }
        assert!(own.is_file(), "a moved guest compiler took the build system's own target");
        fs::write(root.join("target/.deps-stamp"), "sysroot:/a/stamp/naming/no/compiler").unwrap();
        assert_eq!(stale(&root, &fork_edit), Some(Stale::All), "a stamp naming no compiler was trusted");
    }

    /// **A stamp that cannot be written stops the build**: one left behind would
    /// call what the clean took current, or owe a clean every build after.
    #[test]
    fn a_stamp_that_cannot_be_written_panics() {
        let root = toyos_tmpdir::TempDir::new("unwritable-stamp");
        // A directory, not a mode: root writes through any mode, and nobody
        // writes a file over a directory.
        let stamp = root.join("target/.deps-stamp");
        fs::create_dir_all(&stamp).unwrap();
        let identity = Identity::of_parts("compiler", "freestanding", "toyos");
        let failed = std::panic::catch_unwind(|| clean(&root, &Stale::Targets(vec![]), &identity));
        let refusal = failed.expect_err("a stamp that was not written was taken for written");
        let refusal = refusal.downcast_ref::<String>().expect("a formatted panic");
        assert!(refusal.starts_with(&format!("write {}", stamp.display())), "{refusal}");
    }

    #[test]
    fn an_artifact_build_is_not_part_of_a_test_execution_price() {
        let before = mark_artifact_build_time();
        ARTIFACT_BUILD_TIME.set(
            ARTIFACT_BUILD_TIME
                .get()
                .saturating_add(Duration::from_millis(70)),
        );
        assert_eq!(
            before.execution_part(Duration::from_millis(83)),
            Duration::from_millis(13)
        );

        let after = mark_artifact_build_time();
        assert_eq!(
            after.execution_part(Duration::from_millis(13)),
            Duration::from_millis(13)
        );

        // A coarse build clock must not underflow a very short failed outcome.
        ARTIFACT_BUILD_TIME.set(
            ARTIFACT_BUILD_TIME
                .get()
                .saturating_add(Duration::from_millis(70)),
        );
        assert_eq!(
            after.execution_part(Duration::from_millis(13)),
            Duration::ZERO
        );
    }

    /// A test that asks for no kernel feature boots the binary an image ships.
    ///
    /// **That claim is a file, not a resemblance.** A kernel is staged under
    /// [`kernel_key`] and read back from there, so the two paths agreeing about
    /// the key means one artifact — and the day something re-inserts a name
    /// between `BootOptions::kernel_features` and the build, this goes red.
    /// Until 2026-08-10 something did: `qemu::fold_inert` prepended
    /// `test-actuators` to every boot in the suite, so no test had ever booted
    /// the shipping kernel and nothing in the tree could have said so.
    ///
    /// The third assertion is the negative control: a key that ignored its
    /// features would satisfy the first two and certify nothing.
    #[test]
    fn a_boot_that_asks_for_no_feature_gets_the_shipping_kernel() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let shipping = kernel_features(root, false, &[], &[]);
        assert_eq!(shipping, "", "`cargo run` asks the kernel for {shipping:?}, not nothing");
        let harness = <&[&str]>::default().join(",");
        assert_eq!(
            kernel_key(Arch::X86_64, &shipping),
            kernel_key(Arch::X86_64, &harness),
            "a featureless boot and the shipping build stage different kernels"
        );
        assert_ne!(
            kernel_key(Arch::X86_64, &shipping),
            kernel_key(Arch::X86_64, "test-actuators"),
            "the key ignores the features, so it cannot tell two kernels apart"
        );
    }

    /// Interactive debug mode deliberately builds one variant the ordinary
    /// suite does not, and the mode bit must not become a blanket exemption.
    #[test]
    fn debug_mode_declares_only_its_debug_kernel() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let debug = kernel_features(root, true, &[], &[]);
        assert_eq!(debug, DEBUG_KERNEL_BUILD);
        assert!(!TEST_SUITE_KERNEL_BUILDS.contains(&debug.as_str()));
        assert!(harness_kernel_build_is_declared(&debug, true));
        assert!(!harness_kernel_build_is_declared(&debug, false));
        assert!(!harness_kernel_build_is_declared(
            "boot-actuators,test-actuators,debug-wait",
            true
        ));
        for suite_build in TEST_SUITE_KERNEL_BUILDS {
            assert!(harness_kernel_build_is_declared(suite_build, false));
            assert!(!harness_kernel_build_is_declared(suite_build, true));
        }
    }

    /// **The two lists of valued parameters are one list.** A prefix
    /// `kernel/src/params.rs`'s `claims` matches and this file does not know is
    /// a name the pre-flash gate refuses; one this file clears and `claims` does
    /// not match writes an image `actuator::init` panics on. Both are read from
    /// the kernel's own source, so neither can be satisfied by editing this
    /// test.
    #[test]
    fn a_valued_parameter_is_one_the_kernel_claims_by_prefix() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = fs::read_to_string(root.join("kernel/src/params.rs")).expect("params.rs");
        let mut claimed = prefixes_claimed(&text);
        claimed.sort();
        let mut declared: Vec<String> =
            VALUED_PARAMS.iter().map(|(path, _)| (*path).to_string()).collect();
        declared.sort();
        assert_eq!(
            claimed, declared,
            "`params::claims` matches {claimed:?} by prefix and `VALUED_PARAMS` names {declared:?}"
        );

        // Anchored on the function and closed on it: a body it cannot find
        // reads as nothing rather than as the rest of the file.
        assert!(prefixes_claimed("fn other(t: &str) { t.starts_with(a::B) }").is_empty());
        assert_eq!(
            prefixes_claimed("pub fn claims(t: &str) -> bool {\n    t.starts_with( a::B )\n}\n"),
            vec!["a::B".to_string()]
        );
    }

    /// A valued parameter passes the gate a stick is written behind, and a name
    /// that is neither a parameter nor a valued one is refused.
    #[test]
    fn the_pre_flash_gate_clears_a_valued_parameter_and_refuses_an_actuator() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(flashable_params(root, &[format!("{}0x1000", toyos_blackbox::PARAM)]), Ok(()));
        assert!(flashable_params(root, &["wedge-before-reset".to_string()]).is_err());
    }

    #[test]
    fn params_read_the_kernels_own_list() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let params = declared_params(root);
        assert!(params.contains(&"watchdog".to_string()), "{params:?}");
        assert!(
            params.iter().all(|p| p.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')),
            "a parameter name is ASCII `[a-z-]`, and these are not: {params:?}"
        );
        let actuators = declared_actuators(root);
        let both: Vec<&String> = params.iter().filter(|p| actuators.contains(p)).collect();
        assert!(both.is_empty(), "declared as both a parameter and an actuator: {both:?}");

        assert_eq!(
            params_of("pub const PARAMS: &[(&str, &AtomicBool)] = &[\n    (\"a\", &A),\n];"),
            ["a"],
            "the scan reads the declaration this kernel has",
        );
        assert_eq!(
            params_of("pub const PARAMS:\n    &[(&str, &AtomicBool)] =\n    &[(\"a\", &A), (\"b\", &B)];"),
            ["a", "b"],
            "and the same declaration reflowed",
        );
        assert!(params_of("static PARAMS: u8 = 0;").is_empty(), "a declaration it cannot read");
        // The one path it resolves, and a path it does not.
        assert_eq!(
            params_of("pub const PARAMS: &[(&str, &AtomicBool)] = &[(toyos_tco::PARAM, &W)];"),
            vec![toyos_tco::PARAM.to_string()]
        );
        // A paren inside a row is that row's, not another row's.
        assert_eq!(
            params_of("pub const PARAMS: &[(&str, &AtomicBool)] = &[(\"a (one)\", &A)];"),
            vec!["a (one)".to_string()]
        );
        // A row it cannot read is refused rather than dropped, which the count
        // is for: without it the two-row table below would read as one name.
        for unreadable in [
            "pub const PARAMS: &[(&str, &AtomicBool)] = &[(other::NAME, &W)];",
            "pub const PARAMS: &[(&str, &AtomicBool)] = &[(\"a\", &A), (other::NAME, &W)];",
        ] {
            assert!(
                std::panic::catch_unwind(|| params_of(unreadable)).is_err(),
                "a row this cannot read passed: {unreadable}"
            );
        }
    }

    /// **An actuator is a boot parameter and never a kernel build.**
    ///
    /// A name that reappears in `kernel/Cargo.toml` is a 46th kernel, and the
    /// suite would build it without anything saying so — which is the state
    /// collapsing seven per-actuator features into `test-actuators` got out of.
    /// The two lists are read from the two files that declare them, so neither
    /// can be satisfied by editing this test.
    #[test]
    fn no_actuator_is_also_a_cargo_feature() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let actuators = declared_actuators(root);
        let features = declared_kernel_features(root);
        let both: Vec<&String> = actuators.iter().filter(|a| features.contains(a)).collect();
        assert!(both.is_empty(), "declared as both an actuator and a kernel feature: {both:?}");
        assert!(
            actuators.iter().all(|a| a.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
            "an actuator name is ASCII `[a-z0-9-]`, and these are not: {actuators:?}"
        );
    }

    /// Every kernel feature that is a kernel build of its own, each beside what
    /// earned it one. Every other feature the kernel declares is a control a row
    /// of `crate::ci::CONTROLS` runs, or one of [`KERNEL_CARRIES`].
    const KERNEL_BUILDS: &[&str] = &[
        "boot-actuators",
        "debug-wait",
        // Does this kernel reach a pass, a trap or a syscall with the
        // direction flag set. No gate clears `DF` and
        // `compiler_builtins::mem::memmove` sets it across three `rep`
        // string operations, so before `arch::entry`'s `cld` it could be
        // — and every `rep movs`/`rep stos` after that writes backwards.
        // Its own build: a `pushfq` and a test on three hot paths, and
        // the negative control for that `cld` in both directions.
        "df-witness",
        // Its control: a `std` one instruction before the reader that
        // must refuse it. The witness fired zero times on an unclean
        // kernel, which is a fact about where the flag reaches and would
        // be indistinguishable from a broken reader without this.
        "df-witness-mutate",
        // The kernel this tree had before `arch::entry`'s `cld`: the
        // instruction gone and `DF` back out of the `SYSCALL` mask, so a
        // build carrying it inherits a set direction flag from whatever
        // it interrupted. The negative control for that fix, and its own
        // build because the defect is in a `naked_asm!` body on every
        // ring transition, where a boot parameter would have to be a
        // branch.
        "entry-df-unclean",
        // The two band shapes that separate the two readings
        // `heap-tripwire`'s own result left standing — the bands absorb
        // a bounded overrun, or they displace every allocation and the
        // victim moved. No band can be zero-width and keep its
        // placement, because the padding *is* the displacement, so the
        // separation is per side: `notail` leaves the slack past a
        // payload byte-for-byte what an unbanded build has, `nohead`
        // puts the payload at the bottom of its own chunk. They are one
        // experiment in two arms and refuse to build together.
        "heap-band-nohead",
        "heap-band-notail",
        // The sweep's lock hold without the sweep. `heap-sweep` and
        // `sched-tripwire` both multiply this class and both spend time
        // on the pass path; only the sweep also holds `dlmalloc`'s lock
        // while it does. This arm and `pass-spin` below spend one
        // `HOLD_NS` with and without that lock, which is the one
        // variable nobody has varied.
        "heap-lockspin",
        // The sweep that reads every live band rather than only the
        // ones a `dealloc` reaches. Its own build for `heap-tripwire`'s
        // reason twice over: the walk takes `dlmalloc`'s lock on the
        // pass path, which nothing shipping may do.
        "heap-sweep",
        // `sched-tripwire`'s twin one layer down: a band of known bytes
        // on each side of every heap allocation, read back at `dealloc`
        // and — for the running task's kernel stack — at every pass. It
        // earns a build of its own because the bands change what
        // `GlobalAlloc::alloc` returns, which no boot parameter can
        // reach: an allocation minted under one arm and freed under the
        // other is a miscomputed base address. No suite builds it, so a
        // full run pays nothing and a boot storm asks for it by name.
        "heap-tripwire",
        "mask-windows",
        // `heap-lockspin`'s other arm: the same visit to the pass path,
        // for the same span, without the allocator's lock.
        "pass-spin",
        "sched-check",
        // The stray-write tripwire on the per-CPU `CpuSched` record: a
        // byte shadow taken and compared at both ends of the driver's
        // exclusive region, plus a walk of its three containers. It
        // earns a build of its own because what it watches cannot be
        // reached from a boot parameter — the shadow's subject is a
        // whole record and the walk's is a container, and both are
        // decided at compile time by a cargo feature, the same wall
        // `sched-check` is behind. No suite builds it: it is
        // not in `TEST_SUITE_KERNEL_BUILDS`, so a full run pays nothing
        // for it and a boot storm asks for it by name.
        "sched-tripwire",
        // The two comparisons that ask who else is standing on a task's
        // kernel stack: the words a Ring 3 entry takes its stack from,
        // against the running task's own top, at every pass; and the one
        // driver field this class has been caught changing inside a
        // single call. Its own build because both halves are readers on
        // hot paths, so it has to be in both arms of any comparison.
        "stack-witness",
        // The one window this class has never measured: the eight words
        // `context_switch` pops, copied at `check_switch_frame` and
        // compared from inside the switch, one instruction before the
        // first `pop`, against the stack pointer the machine is standing
        // on. Its own build for `stack-witness`'s reason and one more —
        // the compare is a `call`, so the frame has to have been proven
        // to be inside a real stack, which is why it turns that feature
        // on. Its two mutation controls sit beside it, each staging one
        // arm of what it watches.
        "switch-witness",
        "switch-witness-mutate-frame",
        "switch-witness-mutate-rsp",
        "test-actuators",
    ];

    /// Features of a host workspace member that choose a build's world rather
    /// than revert a decision: loom's atomics, a crate's `std` and its default
    /// set, the toolchain's `core` and `rustc-dep-of-std`, libc's `std-runtime`,
    /// the signer's `sign`, and `flaws`, which compiles what a simulator's
    /// negative gates select at run time.
    const NOT_A_CONTROL: &[&str] = &[
        "core",
        "default",
        "flaws",
        "loom",
        "rustc-dep-of-std",
        "sign",
        "std",
        "std-runtime",
    ];

    /// The features a kernel build may still carry, and the whole list.
    ///
    /// **The gate on the count.** Each name in [`KERNEL_BUILDS`] is a kernel
    /// `cargo test` may build beside the two, so adding one is a decision to pay
    /// the ~6.9 s of wall clock and ~29.6 s of CPU measured for one extra kernel
    /// build per full run after any kernel edit — and `boot-actuators` exists so
    /// that the answer is almost always a parameter instead.
    #[test]
    fn the_kernel_declares_only_the_builds_that_earned_one() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let controls: BTreeSet<&str> = crate::ci::CONTROLS.iter().map(|c| c.feature).collect();
        let mut builds: Vec<String> = declared_kernel_features(root)
            .into_iter()
            .filter(|f| !controls.contains(f.as_str()) && !KERNEL_CARRIES.contains(&f.as_str()))
            .collect();
        builds.sort();
        assert_eq!(
            builds, KERNEL_BUILDS,
            "the kernel declares a feature that is neither a build this list accounts for nor a \
             control `src/ci.rs` runs"
        );
    }

    /// The kernel depends on no libc (root `CLAUDE.md`, Dependencies), and
    /// `Cargo.lock` cannot show it: a lock records every platform's edges and
    /// none of their `cfg`s.
    #[test]
    fn the_kernel_resolves_no_libc_for_either_target() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for arch in Arch::ALL {
            let out = Command::new("cargo")
                .args(["tree", "--locked", "--all-features", "-e", "normal,no-proc-macro"])
                .args(["--prefix", "none", "--format", "{p}", "--target", arch.kernel()])
                .args(["-p", crate::ci::KERNEL])
                .current_dir(root)
                .output()
                .expect("cargo tree failed to launch");
            let tree = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && tree.starts_with("kernel "),
                "cargo tree --target {} exited {} and printed no kernel: {}",
                arch.kernel(),
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !tree.lines().any(|package| package.starts_with("libc ")),
                "the {} kernel resolves libc:\n{tree}",
                arch.kernel()
            );
        }
    }

    /// Every negative control the tree declares: every feature of the kernel's
    /// manifest but its builds and [`KERNEL_CARRIES`], and every feature of every
    /// host workspace member's but [`NOT_A_CONTROL`].
    ///
    /// **Every manifest, and no list of the crates that hold a model.** A model
    /// of memory orderings, of the process table's interleavings, of a simulated
    /// machine's policy or of an allocator's isolation each declares its controls
    /// beside its own decisions, and a list of those crates is one a new model
    /// can be left off — its control then declared and run nowhere, silently.
    /// Each file's own comment beside a name carries the argument for it.
    fn declared_model_controls(root: &Path) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for dir in std::iter::once(crate::ci::KERNEL.to_string()).chain(crate::hostws::host_members(root)) {
            let path = root.join(&dir).join("Cargo.toml");
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
            let parsed: KernelManifest = toml::from_str(&text)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()));
            let skip: &[&[&str]] = if dir == crate::ci::KERNEL {
                &[KERNEL_BUILDS, KERNEL_CARRIES]
            } else {
                &[NOT_A_CONTROL]
            };
            out.extend(
                parsed.features.into_keys().filter(|name| !skip.iter().any(|s| s.contains(&name.as_str()))),
            );
        }
        out
    }

    /// **A control nobody runs is a control nobody has shown can fail.** Every
    /// name [`declared_model_controls`] finds is a row of `crate::ci::CONTROLS`,
    /// which `cargo run -- --ci host` runs, and every row names a declared
    /// control — or a new control can be declared and run nowhere, silently.
    #[test]
    fn every_model_control_is_run() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let run: BTreeSet<String> =
            crate::ci::CONTROLS.iter().map(|c| c.feature.to_string()).collect();
        assert_eq!(
            declared_model_controls(root), run,
            "a model's negative control is declared and not in src/ci.rs's CONTROLS, or a row \
             there names a feature no model crate declares"
        );
    }

    /// `test-actuators` is one name and pulls in nothing.
    ///
    /// The seven it replaced were seven kernel builds differing only in which
    /// unreachable `SYS_DEBUG` arm they carried. Re-introducing one as an implied
    /// feature rebuilds that, silently, and only this notices.
    #[test]
    fn the_actuator_umbrella_is_a_leaf() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = fs::read_to_string(root.join("kernel/Cargo.toml")).expect("read the manifest");
        let manifest: KernelManifest = toml::from_str(&text).expect("parse the manifest");
        let implied = manifest
            .features
            .get("test-actuators")
            .expect("the kernel declares no `test-actuators`");
        assert!(
            implied.is_empty(),
            "`test-actuators` implies {implied:?}, so it is several kernel builds again"
        );
    }

    /// **An architecture leaves out only what the shipped config builds**, and
    /// each reason names the issue file that owns it.
    #[test]
    fn every_program_an_architecture_leaves_out_is_one_the_shipped_config_builds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let programs = parse_config(&Boot::shipped(root).config).programs;
        for (arch, name, why) in NOT_YET_BUILT {
            assert!(programs.contains_key(*name), "{name} is left out for {arch:?} and the shipped config builds no such program");
            assert_eq!(not_built_for(*arch, name), Some(*why));
            let issue = why.split("issues/").nth(1).map(|rest| rest.split(')').next().unwrap_or(rest));
            let issue = issue.unwrap_or_else(|| panic!("{name}'s reason names no issue file: {why}"));
            assert!(root.join("issues").join(issue).is_file(), "{name}'s reason cites issues/{issue}, which does not exist");
        }
        assert_eq!(not_built_for(Arch::X86_64, "calc"), None, "x86-64 builds every program");
    }

    /// No image this repository ships starts sshserver.
    ///
    /// It listens on every interface and authenticates against a file that is
    /// absent on a fresh install, so on a default boot it would be a port that
    /// accepts connections and refuses all of them. Whoever wants it runs
    /// `/system/bin/sshserver` themselves. It stays in `[programs]` — the gate is on what
    /// the supervisor starts, not on the binary being present.
    #[test]
    fn no_shipped_boot_config_starts_sshserver() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for boot in [Boot::shipped(root), Boot::diag(root), Boot::console(root)] {
            let config = boot.config.clone();
            let start = parse_config(&config).boot.start;
            assert!(
                !start.iter().any(|p| p == "sshserver"),
                "{} starts sshserver: {start:?}",
                config.display(),
            );
        }
    }

    /// The plan a case build hands the builder carries the config and the
    /// parameters asked for, and each case builds to an artifact of its own.
    #[test]
    fn a_case_plan_carries_the_config_and_the_parameters() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let argv: Vec<String> =
            ["--build-only", "--boot-config", "tests/jobcase", "--kernel-param", "watchdog"]
                .iter()
                .map(|w| (*w).to_string())
                .collect();
        let asked = flags::CARGO_RUN
            .value(&argv, &flags::BOOT_CONFIG)
            .expect("the argv carries one");
        let boot = Boot::case(root, asked).unwrap_or_else(|why| panic!("{why}"));
        let plan = plan_for(root, &boot, false, &argv);
        assert_eq!(plan.config, root.join("tests/jobcase").join(CONFIG));
        assert_eq!(plan.params, ["watchdog"]);
        assert!(plan.features.is_empty(), "{:?}", plan.features);
        assert_eq!(boot.image, Path::new("target/bootable-jobcase.img"));

        // Every config this tree holds builds to an artifact of its own,
        // reached by the constructor its directory decides — and a case that
        // will not construct reds here rather than falling back to a mode.
        let mut images = BTreeSet::new();
        for at in ALL_CONFIGS {
            let dir = Path::new(at).parent().expect("a config sits in a directory");
            let boot = match dir.to_string_lossy().as_ref() {
                "" => Boot::shipped(root),
                "diag" => Boot::diag(root),
                "console" => Boot::console(root),
                case => Boot::case(root, case).unwrap_or_else(|why| panic!("{at}: {why}")),
            };
            assert_eq!(boot.config, root.join(at), "{at}");
            assert!(images.insert(boot.image), "{at} shares an image with another config");
        }
        for mode in [Boot::shipped(root), Boot::diag(root), Boot::console(root)] {
            assert!(images.contains(&mode.image), "{}", mode.image.display());
        }
    }

    /// A directory that does not hold a config, and the three a mode already
    /// writes, are refused by name rather than reaching `parse_config` or
    /// giving one artifact two writers.
    #[test]
    fn a_boot_config_that_is_not_a_case_is_refused() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let refused = |asked: &str| match Boot::case(root, asked) {
            Err(why) => why,
            Ok(boot) => panic!("{asked} was admitted, building {}", boot.image.display()),
        };
        for asked in ["tests/nosuchcase", "/nowhere-at-all", "Cargo.toml"] {
            assert!(refused(asked).starts_with("--boot-config"), "{asked}");
        }
        assert!(refused("src").contains("holds no system.toml"));
        for (asked, flag) in
            [(".", "--build-only"), ("diag", "--diag-boot"), ("console", "--console-boot")]
        {
            let refusal = refused(asked);
            assert!(refusal.contains(flag), "{asked}: {refusal}");
            assert!(refusal.contains("one artifact keeps one writer"), "{asked}: {refusal}");
        }

        // A case outside the tree has no name under the one naming rule, and
        // the fallback was the shipped artifact's: measured, it rewrote it.
        let outside = toyos_tmpdir::TempDir::new("outside");
        std::fs::copy(root.join("tests/jobcase").join(CONFIG), outside.join(CONFIG))
            .expect("a config to point at");
        let refusal = refused(&outside.to_string_lossy());
        assert!(refusal.contains("not in this repository"), "{refusal}");
    }

    /// **Every config with a `[boot] start` runs `/system/bin/logkeeper`, `logkeeper` always holds
    /// `logread`, and nothing outside the cursor readers ever does.**
    ///
    /// The kernel writes no file — `/system/bin/logkeeper` owns `/log` and reads records off
    /// a cursor — so a boot config that does not start `logkeeper` is an image whose
    /// log partition stays empty for the whole of that boot — and on
    /// the machine this subsystem exists for, a T14 with no serial port, that is
    /// the boot with no record of itself anywhere. A config added later fails
    /// the first clause **by default**, which is the direction this bound has to
    /// fail in.
    ///
    /// The rest is the capability half: `logread` is
    /// `Rights::LOG | Rights::WAIT` on a `SysCap` duplicate, which is authority
    /// over every record every CPU wrote, and a right with no caller is a
    /// capability handed out for a plan. Two programs read a cursor —
    /// `/system/bin/logkeeper`, which writes the file, and `test-runner`, which runs the
    /// conservation gates inside itself. `logkeeper` always does, since that is its
    /// whole job; `test-runner` does where an estate runs such a gate and not
    /// where it runs none. `/system/bin/console` is the near miss: it *could* show this boot's
    /// records live off a cursor, and holds the right only once something reads one.
    ///
    /// It reads the **parsed** `ProgramConfig` and never the file text: a grep
    /// over the TOML would pass on a row that is commented out and on a key
    /// `serde` never saw.
    #[test]
    fn every_boot_config_runs_logkeeper() {
        const READERS: &[&str] = &["logkeeper", "test-runner"];
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for config in ALL_CONFIGS {
            let parsed = parse_config(&root.join(config));
            assert!(
                parsed.boot.start.iter().any(|p| p == "logkeeper"),
                "{config} declares `[boot] start = {:?}` and no `logkeeper` in it, so this image's \
                 /log is empty for the whole boot",
                parsed.boot.start,
            );
            assert!(
                parsed.programs.contains_key("logkeeper"),
                "{config} starts `logkeeper` and has no `[programs.logkeeper]` row to say what it holds",
            );
            for (name, program) in &parsed.programs {
                let holds = program.syscap.iter().any(|s| s == "logread");
                if name == "logkeeper" {
                    assert!(holds, "{config}: `logkeeper` writes the file and must read the cursor");
                    continue;
                }
                assert!(
                    !holds || READERS.contains(&name.as_str()),
                    "{config}: `{name}` holds `logread`, and the only programs that read a \
                     cursor are {READERS:?}",
                );
            }
        }
    }

    /// **An image a user boots serves no log on the network.** `logkeeper` answers
    /// `toyos_logstream::PORT` to whoever connects, with nothing to authenticate
    /// them, once it holds a `netstack` connector: the test configs that read the
    /// stream give it one, and these do not.
    #[test]
    fn no_shipped_image_serves_the_log_on_the_network() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for config in ALL_CONFIGS.iter().filter(|config| !config.starts_with("tests/")) {
            let parsed = parse_config(&root.join(config));
            let logkeeper = parsed.programs.get("logkeeper").expect("every config runs logkeeper");
            assert!(
                logkeeper.receives.is_empty(),
                "{config}: `logkeeper` receives {:?}, and a `netstack` connector is what serves this \
                 machine's log to anyone on its network",
                logkeeper.receives,
            );
        }
    }

    /// Every config renders, so a row the manifest refuses — one that serves a
    /// port and is not marked `service` — reds here rather than at a build.
    #[test]
    fn every_config_renders_its_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for config in ALL_CONFIGS {
            let parsed = parse_config(&root.join(config));
            let rendered = std::panic::catch_unwind(|| render_manifest(&parsed));
            assert!(rendered.is_ok(), "{config} does not render; the panic above says why");
        }
    }

    /// One prefix and no other, so a doc naming `/etc/logkeeper` or `/apps/logkeeper` is a
    /// token the filter below drops and an assertion that reds.
    const LOG_DOC_BIN: &str = "/system/bin/";

    /// `Rights::LOG`'s doc names its holders, which is a claim about these
    /// manifests and rots on its own: `/system/bin/console` stood in it for the whole
    /// time no boot config gave it a `logread` row.
    #[test]
    fn the_log_right_doc_names_exactly_the_manifests_holders() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut every_program = BTreeSet::new();
        let mut holders = BTreeSet::new();
        for config in ALL_CONFIGS {
            for (name, program) in parse_config(&root.join(config)).programs {
                if program.syscap.iter().any(|s| s == "logread") {
                    holders.insert(name.clone());
                }
                every_program.insert(name);
            }
        }
        let handle = root.join("toyos-abi/src/handle.rs");
        let source = fs::read_to_string(&handle).expect("toyos-abi/src/handle.rs");
        // Only a backticked token that is a program name somewhere is a holder
        // claim: `SYS_LOG_READ` and `/log` are in the same block and are not.
        let named: BTreeSet<String> = doc_block(&source, "pub const LOG: Rights")
            .split('`')
            .skip(1)
            .step_by(2)
            .map(|token| token.strip_prefix(LOG_DOC_BIN).unwrap_or(token).to_string())
            .filter(|token| every_program.contains(token))
            .collect();
        assert_eq!(
            named,
            holders,
            "`Rights::LOG`'s doc in {} names {named:?} as holders, and the boot configs give \
             `logread` to {holders:?}",
            handle.display(),
        );
    }

    /// The `///` lines directly above the one `item` starts, newest first.
    fn doc_block(source: &str, item: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.trim_start().starts_with(item))
            .unwrap_or_else(|| panic!("no `{item}` in the source"));
        lines[..at]
            .iter()
            .rev()
            .map_while(|line| line.trim_start().strip_prefix("///"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every `system.toml` this repository builds an image from.
    /// `every_shipped_boot_config_is_covered` asserts this equals what a walk of
    /// the tree finds, so a config added without a gate row reds rather than
    /// slipping through uncovered.
    const ALL_CONFIGS: &[&str] = &[
        "system.toml",
        "diag/system.toml",
        "console/system.toml",
        "tests/acpicase/system.toml",
        "tests/jobcase/system.toml",
        "tests/lanleasecase/system.toml",
        "tests/lantalkcase/system.toml",
        "tests/latencycase/system.toml",
        "tests/logstallcase/system.toml",
        "tests/metalcase/system.toml",
        "tests/metaldevicecase/system.toml",
        "tests/netcase/system.toml",
        "tests/panelcase/system.toml",
        "tests/proctreecase/system.toml",
        "tests/testcases/system.toml",
        "tests/virtjobcase/system.toml",
        "tests/virtpaniccase/system.toml",
        "tests/virtrebootcase/system.toml",
        "tests/virtsmpcase/system.toml",
    ];

    fn load(cfg: &str) -> SystemConfig {
        parse_config(&Path::new(env!("CARGO_MANIFEST_DIR")).join(cfg))
    }

    /// Every name a program `receives` must be served or provided by some
    /// program in the *same* config, or served by the supervisor. The build-time form of
    /// "a client cannot name a service the system does not have", and the gate
    /// with the sharpest teeth: no guest, no mutated tree.
    fn receives_have_providers(cfg: &SystemConfig) -> Result<(), String> {
        let mut providers: Vec<&str> = SUPERVISOR_SERVED.to_vec();
        for prog in cfg.programs.values() {
            providers.extend(prog.serves.iter().map(String::as_str));
            providers.extend(prog.provides.iter().map(String::as_str));
        }
        for (name, prog) in &cfg.programs {
            for r in &prog.receives {
                if !providers.contains(&r.as_str()) {
                    return Err(format!(
                        "program `{name}` receives `{r}`, which no program serves or provides"
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn every_receives_names_a_provider() {
        for cfg in ALL_CONFIGS {
            receives_have_providers(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let bad: SystemConfig =
            toml::from_str("[programs.client]\nreceives = [\"ghost\"]\n").unwrap();
        assert!(receives_have_providers(&bad).is_err());
    }

    /// The swap port reaches `swap` and nothing else, and the idle slot
    /// `update` and nothing else, in every committed config and in a config
    /// that tries any other door.
    #[test]
    fn only_their_holders_may_hold_the_swap_port_and_the_slots() {
        for cfg in ALL_CONFIGS {
            held_by_their_holders_alone(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let ok: SystemConfig =
            toml::from_str("[programs.swap]\nreceives = [\"swap\"]\n[programs.update]\nslots = true\n").unwrap();
        assert!(held_by_their_holders_alone(&ok).is_ok());
        let shell: SystemConfig =
            toml::from_str("[programs.shell]\nreceives = [\"swap\"]\n").unwrap();
        assert!(held_by_their_holders_alone(&shell).is_err());
        let sshserver: SystemConfig =
            toml::from_str("[programs.sshserver]\nreceives = [\"netstack\", \"swap\"]\n").unwrap();
        assert!(held_by_their_holders_alone(&sshserver).is_err());
        let apps: SystemConfig = toml::from_str("[apps]\nreceives = [\"swap\"]\n").unwrap();
        assert!(held_by_their_holders_alone(&apps).is_err());
        let slots: SystemConfig = toml::from_str("[programs.shell]\nslots = true\n").unwrap();
        assert!(held_by_their_holders_alone(&slots).is_err());
    }

    /// Every committed config passes, and each refusal has a config that
    /// takes it and nothing else.
    #[test]
    fn a_row_starts_only_what_a_launch_can_start() {
        for cfg in ALL_CONFIGS {
            starts_name_what_a_launch_can_start(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let gate = |toml: &str| starts_name_what_a_launch_can_start(&toml::from_str(toml).unwrap());
        assert!(gate("[programs.shell]\nstarts = [\"toybox\", \"/apps\"]\nlogin = true\n[programs.toybox]\n").is_ok());
        assert!(gate("[programs.shell]\nstarts = [\"ghost\"]\n").is_err());
        assert!(gate("[programs.shell]\nstarts = [\"apps\"]\n").is_err());
        assert!(gate("[programs.shell]\nstarts = [\"compositor\"]\n[programs.compositor]\nserves = [\"compositor\"]\n").is_err());
        assert!(gate("[programs.shell]\nstarts = [\"fileserver\"]\n[programs.fileserver]\nroles = [\"data\"]\n").is_err());
        assert!(toml::from_str::<SystemConfig>("[programs.shell]\nstart = [\"toybox\"]\n").is_err());
    }

    /// `[apps] receives` is narrower than a program's: a `provides` name is one
    /// port per instance and nobody makes one for a package, so naming one here
    /// is a namespace the supervisor cannot build.
    fn apps_receive_a_served_name(cfg: &SystemConfig) -> Result<(), String> {
        let mut served: Vec<&str> = SUPERVISOR_SERVED.to_vec();
        for prog in cfg.programs.values() {
            served.extend(prog.serves.iter().map(String::as_str));
        }
        for name in &cfg.apps.receives {
            if !served.contains(&name.as_str()) {
                return Err(format!("`[apps] receives` names `{name}`, which no program serves"));
            }
        }
        Ok(())
    }

    #[test]
    fn an_installed_app_receives_only_names_the_supervisor_holds() {
        for cfg in ALL_CONFIGS {
            apps_receive_a_served_name(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let provided: SystemConfig = toml::from_str(
            "[apps]\nreceives = [\"surface\"]\n\
             [programs.terminal]\nprovides = [\"surface\"]\n",
        )
        .unwrap();
        assert!(apps_receive_a_served_name(&provided).is_err());
        let ghost: SystemConfig =
            toml::from_str("[apps]\nreceives = [\"ghost\"]\n").unwrap();
        assert!(apps_receive_a_served_name(&ghost).is_err());
    }

    /// A `serves` name is one port machine-wide; a `provides` name is one port
    /// per instance. A name declared both ways is a config where the supervisor makes a
    /// port nobody accepts from while the real one is made elsewhere.
    fn provides_disjoint_from_serves(cfg: &SystemConfig) -> Result<(), String> {
        let serves: Vec<&str> = cfg
            .programs
            .values()
            .flat_map(|p| p.serves.iter().map(String::as_str))
            .collect();
        for (name, prog) in &cfg.programs {
            for p in &prog.provides {
                if serves.contains(&p.as_str()) {
                    return Err(format!(
                        "`{p}` is both a serves name and `{name}`'s provides name"
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_provides_name_is_never_also_a_serves_name() {
        for cfg in ALL_CONFIGS {
            provides_disjoint_from_serves(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let bad: SystemConfig = toml::from_str(
            "[programs.a]\nserves = [\"x\"]\n[programs.b]\nprovides = [\"x\"]\n",
        )
        .unwrap();
        assert!(provides_disjoint_from_serves(&bad).is_err());
    }

    /// The supervisor mints one claim per device, so a config naming one twice starts a
    /// program with a hole where its claim should be.
    fn one_claimant_per_device(cfg: &SystemConfig) -> Result<(), String> {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, prog) in &cfg.programs {
            for d in &prog.devices {
                if let Some(prev) = seen.insert(d, name) {
                    return Err(format!(
                        "device `{d}` is claimed by both `{prev}` and `{name}`; the second \
                         claim is refused at boot"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Not the capability boundary — `kernel/src/pcidev`'s slot reservation is,
    /// and this compares `system.toml` strings.
    #[test]
    fn every_device_class_has_at_most_one_claimant() {
        for cfg in ALL_CONFIGS {
            one_claimant_per_device(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let bad: SystemConfig = toml::from_str(
            "[programs.a]\ndevices = [\"framebuffer\"]\n\
             [programs.b]\ndevices = [\"framebuffer\"]\n",
        )
        .unwrap();
        assert!(one_claimant_per_device(&bad).is_err());
    }

    /// netstack's actuator that only its Intel driver answers, spelled here and
    /// held to netstack's own declaration by
    /// [`netstack_declares_the_flag_this_gate_spells`].
    const EXIT_WITH_LEASE: &str = "--exit-with-lease";

    /// netstack's main module, which is where both halves of this gate's spelling
    /// live: nothing links the two crates, so the build system reads the source.
    fn netstack_source() -> (std::path::PathBuf, String) {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("userland/netstack/src/main.rs");
        let text = std::fs::read_to_string(&at).expect("netstack's main module");
        (at, text)
    }

    /// Nothing links the two crates: netstack is a userland binary and this is the
    /// build system, so the flags both ends spell are held to netstack's own
    /// declarations by reading its source.
    #[test]
    fn netstack_declares_the_flag_this_gate_spells() {
        let (at, source) = netstack_source();
        assert!(
            crate::bootlog::declares(&source, &format!("\"{EXIT_WITH_LEASE}\"")),
            "{} declares no constant equal to \"{EXIT_WITH_LEASE}\"",
            at.display()
        );
    }

    /// The four hex digits after `key` on this line.
    fn hex_after(line: &str, key: &str) -> Option<String> {
        let at = line.find(key)? + key.len();
        let digits: String = line[at..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        (digits.len() == 4).then_some(digits)
    }

    /// The device entries netstack opens with the driver that has §10.2.4.4's `ICS`,
    /// read out of netstack's own `CARDS` rather than guessed from a vendor id: each
    /// row spells an id and the constructor that takes it on one line.
    ///
    /// **The scan reaches that one spelling and no other**, so every
    /// `Card::intel` row it saw has to have yielded an id — a table written
    /// another way reds here instead of narrowing this gate to nothing.
    fn netstack_intel_cards(source: &str) -> Vec<String> {
        let mut cards = Vec::new();
        let mut rows = 0;
        for line in source.lines() {
            if !line.contains("Card::intel") {
                continue;
            }
            rows += 1;
            if let (Some(vendor), Some(device)) =
                (hex_after(line, "vendor: 0x"), hex_after(line, "device: 0x"))
            {
                cards.push(format!("pci:{vendor}:{device}"));
            }
        }
        assert_eq!(
            cards.len(),
            rows,
            "netstack names `Card::intel` on {rows} line(s) and an id was read off {}; its `CARDS` \
             table is spelled in a way this gate does not reach",
            cards.len()
        );
        cards
    }

    /// netstack's Intel-only actuators — `--exit-with-lease` reports the Intel
    /// driver's bring-up beside the lease — and virtio's driver has none, so a
    /// boot config that arms one on a card netstack opens with any other driver is
    /// a boot that panics instead of answering the question it was built for.
    fn an_armed_intel_actuator_claims_a_card_the_driver_opens(
        cfg: &SystemConfig,
        cards: &[String],
    ) -> Result<(), String> {
        for (name, prog) in &cfg.programs {
            if !prog.args.iter().any(|arg| arg == EXIT_WITH_LEASE) {
                continue;
            }
            if !prog.devices.iter().any(|d| cards.contains(d)) {
                return Err(format!(
                    "`{name}` is armed with `{EXIT_WITH_LEASE}` and claims {:?}, none of which \
                     is one of the {cards:?} netstack opens with that driver",
                    prog.devices
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn every_armed_intel_actuator_claims_a_card_the_driver_opens() {
        let (_, source) = netstack_source();
        let cards = netstack_intel_cards(&source);
        assert!(!cards.is_empty(), "netstack's `CARDS` names no card its Intel driver opens");
        let mut armed = 0;
        for cfg in ALL_CONFIGS {
            let config = load(cfg);
            armed += config
                .programs
                .values()
                .filter(|p| p.args.iter().any(|arg| arg == EXIT_WITH_LEASE))
                .count();
            an_armed_intel_actuator_claims_a_card_the_driver_opens(&config, &cards)
                .unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        // A walk that reached no armed program passes on having found nothing,
        // which is the one way this gate can rot while every config still loads.
        assert!(armed > 0, "no shipped boot config arms `{EXIT_WITH_LEASE}` at all");
        let armed_on = |device: &str, args: &str| {
            let cfg: SystemConfig = toml::from_str(&format!(
                "[programs.netstack]\ndevices = [\"{device}\"]\nargs = [{args}]\n"
            ))
            .unwrap();
            an_armed_intel_actuator_claims_a_card_the_driver_opens(&cfg, &cards)
        };
        // The card netstack drives with the other driver, and an Intel function it
        // drives with none: a vendor id is not what gives a part an `ICS` or a
        // PHY behind `MDIC`.
        let flag = format!("\"{EXIT_WITH_LEASE}\"");
        assert!(armed_on("pci:1af4:1041", &flag).is_err());
        assert!(armed_on("pci:8086:1502", &flag).is_err());
        assert!(armed_on(&cards[0], &flag).is_ok());
    }

    /// A device name the ABI does not know renders fine and leaves the supervisor with a
    /// `devices` entry it cannot mint — a dead machine for a typo, where this is
    /// a red in milliseconds. Same for a `syscap` right.
    ///
    /// The ABI's own parser, not a copy of it: a `pci:<vendor>:<device>` entry
    /// names a function and a class name names a class, and this is the same
    /// `DeviceRequest::parse` the supervisor and the kernel read the entry with.
    fn names_only_real_capabilities(cfg: &SystemConfig) -> Result<(), String> {
        for (name, prog) in &cfg.programs {
            for device in &prog.devices {
                if toyos_manifest::DeviceRequest::parse(device).is_none() {
                    return Err(format!("`{name}` names device `{device}`, which is not one"));
                }
            }
            toyos_manifest::syscap_rights(&prog.syscap)
                .map_err(|e| format!("`{name}`: {e}"))?;
        }
        Ok(())
    }

    #[test]
    fn every_declared_capability_is_one_the_abi_has() {
        for cfg in ALL_CONFIGS {
            names_only_real_capabilities(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let bad_class: SystemConfig =
            toml::from_str("[programs.a]\ndevices = [\"gpu\"]\n").unwrap();
        assert!(names_only_real_capabilities(&bad_class).is_err());
        // A PCI entry that names no function is the same defect one level down,
        // and the one a hand-written config is most likely to make.
        let bad_function: SystemConfig =
            toml::from_str("[programs.a]\ndevices = [\"pci:1af4\"]\n").unwrap();
        assert!(names_only_real_capabilities(&bad_function).is_err());
        let bad_right: SystemConfig =
            toml::from_str("[programs.a]\nsyscap = [\"root\"]\n").unwrap();
        assert!(names_only_real_capabilities(&bad_right).is_err());
    }

    fn claims_no_device(cfg: &SystemConfig) -> Result<(), String> {
        for (name, prog) in &cfg.programs {
            if !prog.devices.is_empty() {
                return Err(format!("program `{name}` claims {:?}", prog.devices));
            }
        }
        Ok(())
    }

    /// The diagnostic image's whole reason for existing: nothing in it can claim
    /// the framebuffer, so the kernel's boot log stays on the panel. `/system/bin/supervisor`
    /// is in every image and could reach a device, so the property becomes "the
    /// config declares no `devices`" — checkable here for the first time.
    #[test]
    fn no_diag_program_claims_the_screen() {
        claims_no_device(&load("diag/system.toml"))
            .unwrap_or_else(|e| panic!("diag/system.toml: {e}"));
        let bad: SystemConfig =
            toml::from_str("[programs.x]\ndevices = [\"framebuffer\"]\n").unwrap();
        assert!(claims_no_device(&bad).is_err());
    }

    /// `[boot] start` names program keys, so a typo is a build error rather than
    /// a refusal `/system/bin/supervisor` reports at boot.
    fn started_programs_are_declared(cfg: &SystemConfig) -> Result<(), String> {
        for name in &cfg.boot.start {
            if !cfg.programs.contains_key(name) {
                return Err(format!("[boot] start names `{name}`, not a [programs] key"));
            }
        }
        Ok(())
    }

    #[test]
    fn every_started_program_is_declared() {
        for cfg in ALL_CONFIGS {
            started_programs_are_declared(&load(cfg)).unwrap_or_else(|e| panic!("{cfg}: {e}"));
        }
        let bad: SystemConfig = toml::from_str("[boot]\nstart = [\"ghost\"]\n").unwrap();
        assert!(started_programs_are_declared(&bad).is_err());
    }

    fn declared(names: &[&str]) -> BTreeSet<&'static str> {
        names.iter().map(|n| Box::leak(n.to_string().into_boxed_str()) as &str).collect()
    }

    /// The converse of the row above: what the image carries and no name reaches.
    #[test]
    fn a_bin_entry_no_name_reaches_is_refused() {
        let programs = declared(&["shell"]);
        let started = ["shell".to_string()];
        assert!(unnamed_program(
            &["bin/supervisor", "bin/shell", "lib/libtls_lib.so", "etc/system.manifest"],
            &programs,
            &started,
        )
        .is_ok());
        assert!(unnamed_program(&["bin/test_rs_window_child"], &programs, &started).is_ok());

        let why = unnamed_program(&["bin/ghost"], &programs, &started).unwrap_err();
        assert!(why.contains("bin/ghost") && why.contains("no `[programs]` row"), "{why}");
        let why = unnamed_program(&["bin/test_rs_window_child"], &programs, &[]).unwrap_err();
        assert!(why.contains("nothing runs that could spawn it"), "{why}");
        // A start list that names only what no row declares runs nothing, so it
        // is the empty list and not a spawner.
        let ghosts = ["ghost".to_string()];
        let why = unnamed_program(&["bin/test_rs_window_child"], &programs, &ghosts).unwrap_err();
        assert!(why.contains("names no `[programs]` row"), "{why}");
        // The other half of the symlink closure: the target as the inventory
        // names it, then judged like any other name.
        assert_eq!(symlink_target_name("/system/bin/toybox"), "bin/toybox");
        let linked = symlink_target_name("/system/bin/ghost");
        assert!(unnamed_program(&[linked], &programs, &started).is_err());
    }

    /// **What the check does not reach, as an assertion and not a sentence.** It
    /// reads names, so a config starting a program that never spawns anything
    /// passes it; closing that needs reachability, which no manifest name
    /// carries. The day the check learns it, this reds.
    #[test]
    fn the_check_does_not_reach_a_spawner_that_never_spawns() {
        // `logkeeper` spawns nothing in any config this tree ships.
        let programs = declared(&["logkeeper"]);
        assert!(
            unnamed_program(&["bin/test_rs_window_child"], &programs, &["logkeeper".to_string()])
                .is_ok(),
            "the scan now reaches whether the spawner spawns; correct this test and its header"
        );
    }

    fn walk_configs(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                let skip = matches!(name.to_str(), Some("target") | Some("rust"))
                    || name.to_string_lossy().starts_with('.');
                if !skip {
                    walk_configs(&path, root, out);
                }
            } else if name == "system.toml" {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// `ALL_CONFIGS` is the list the gates above iterate; a config added without
    /// a row would leave a hole in that coverage. Assert the list is exactly
    /// what a walk of the tree finds, so it cannot silently drift.
    #[test]
    fn every_shipped_boot_config_is_covered() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        walk_configs(root, root, &mut found);
        found.sort();
        let mut expected: Vec<String> = ALL_CONFIGS.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(found, expected);
    }

    /// `cld`, then `mov gs:[0x18], rsp`: how every entry judged here opens.
    const ENTRY_OPENS: [u8; 10] = [0xfc, 0x65, 0x48, 0x89, 0x24, 0x25, 0x18, 0, 0, 0];
    /// `mov rsp, gs:[0x10]`.
    const SWITCH: [u8; 9] = [0x65, 0x48, 0x8b, 0x24, 0x25, 0x10, 0, 0, 0];
    const LABELLED_HOLD: [u8; 84] = [
        0x65, 0x48, 0xf7, 0x04, 0x25, 0x18, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x84,
        0x41, 0x00, 0x00, 0x00, 0x65, 0xf0, 0x48, 0x83, 0x0c, 0x25, 0x18, 0x01, 0x00, 0x00, 0x02,
        0xf3, 0x90, 0x65, 0x48, 0xf7, 0x04, 0x25, 0x18, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x0f, 0x84, 0x21, 0x00, 0x00, 0x00, 0x65, 0xf0, 0x48, 0x81, 0x2c, 0x25, 0x18, 0x01, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x0f, 0x83, 0xd7, 0xff, 0xff, 0xff, 0x65, 0x48, 0xc7, 0x04,
        0x25, 0x18, 0x01, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    ];
    /// The same hold respelled through the local labels `77:` and `78:`, which
    /// put no name in `.strtab` and make each jump a short one.
    const LOCAL_LABEL_HOLD: [u8; 72] = [
        0x65, 0x48, 0xf7, 0x04, 0x25, 0x18, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x74, 0x39,
        0x65, 0xf0, 0x48, 0x83, 0x0c, 0x25, 0x18, 0x01, 0x00, 0x00, 0x02, 0xf3, 0x90, 0x65, 0x48,
        0xf7, 0x04, 0x25, 0x18, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x74, 0x1d, 0x65, 0xf0,
        0x48, 0x81, 0x2c, 0x25, 0x18, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x73, 0xdf, 0x65,
        0x48, 0xc7, 0x04, 0x25, 0x18, 0x01, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    ];

    /// A PIE of one `PT_LOAD` over the whole file, whose `.symtab` names one
    /// function per `names` entry, each at `entry`'s address.
    fn kernel_with_entry(names: &[&str], entry: &[u8]) -> Vec<u8> {
        const TEXT: u64 = 0x200;
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 24];
        for name in names {
            symtab.extend((strtab.len() as u32).to_le_bytes());
            // `STB_GLOBAL`, `STT_FUNC`; `st_other`; defined in section 1.
            symtab.extend([0x12, 0, 1, 0]);
            symtab.extend(TEXT.to_le_bytes());
            symtab.extend((entry.len() as u64).to_le_bytes());
            strtab.extend(name.as_bytes());
            strtab.push(0);
        }
        let mut file = vec![0u8; TEXT as usize];
        file.extend(entry);
        let symtab_at = file.len() as u64;
        file.extend(&symtab);
        let strtab_at = file.len() as u64;
        file.extend(&strtab);
        let shoff = file.len() as u64;

        let put = |file: &mut Vec<u8>, at: usize, bytes: &[u8]| {
            file[at..at + bytes.len()].copy_from_slice(bytes)
        };
        // `e_ident`, `ET_DYN`, `EM_X86_64`, then the two tables.
        put(&mut file, 0, &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        put(&mut file, 16, &3u16.to_le_bytes());
        put(&mut file, 18, &62u16.to_le_bytes());
        put(&mut file, 32, &64u64.to_le_bytes());
        put(&mut file, 40, &shoff.to_le_bytes());
        put(&mut file, 54, &56u16.to_le_bytes());
        put(&mut file, 56, &1u16.to_le_bytes());
        put(&mut file, 58, &64u16.to_le_bytes());
        put(&mut file, 60, &3u16.to_le_bytes());
        // The `PT_LOAD`: file offset 0 at address 0, up to the section headers.
        put(&mut file, 64, &1u32.to_le_bytes());
        put(&mut file, 64 + 32, &shoff.to_le_bytes());
        put(&mut file, 64 + 40, &shoff.to_le_bytes());

        // The null section, `.symtab` linked to section 2, and its `.strtab`.
        let mut sections = vec![0u8; 3 * 64];
        for (index, kind, at, len, link) in [
            (1usize, 2u32, symtab_at, symtab.len() as u64, 2u32),
            (2, 3, strtab_at, strtab.len() as u64, 0),
        ] {
            let base = index * 64;
            sections[base + 4..base + 8].copy_from_slice(&kind.to_le_bytes());
            sections[base + 24..base + 32].copy_from_slice(&at.to_le_bytes());
            sections[base + 32..base + 40].copy_from_slice(&len.to_le_bytes());
            sections[base + 40..base + 44].copy_from_slice(&link.to_le_bytes());
            sections[base + 56..base + 64].copy_from_slice(&24u64.to_le_bytes());
        }
        file.extend(sections);
        file
    }

    const ENTRY_NAME: &str = "_RNvNtNtNtCs2TF9wDo3GXK_6kernel4arch6x86_647syscall13syscall_entry";

    fn entry_with(between: &[u8]) -> Vec<u8> {
        [&ENTRY_OPENS[..], between, &SWITCH[..], &[0x90; 32][..]].concat()
    }

    #[test]
    fn a_clean_entry_is_the_shipping_kernels() {
        let kernel = kernel_with_entry(&[ENTRY_NAME], &entry_with(&[]));
        assert_eq!(judge_entry_window("", &kernel), Ok(()));
    }

    #[test]
    fn a_hold_is_refused_in_a_shipping_entry_however_its_labels_are_spelled() {
        for hold in [&LABELLED_HOLD[..], &LOCAL_LABEL_HOLD[..]] {
            let kernel = kernel_with_entry(&[ENTRY_NAME], &entry_with(hold));
            let refusal = judge_entry_window("", &kernel).unwrap_err();
            assert!(refusal.contains(&format!("between them stand {hold:02x?}")), "{refusal}");
        }
    }

    #[test]
    fn a_hold_in_front_of_the_save_is_an_entry_the_judge_refuses_to_read() {
        let entry = [&[0xfc, 0xf3, 0x90][..], &ENTRY_OPENS[1..], &SWITCH[..]].concat();
        let kernel = kernel_with_entry(&[ENTRY_NAME], &entry);
        let refusal = judge_entry_window("", &kernel).unwrap_err();
        assert!(refusal.contains("does not open `cld`"), "{refusal}");
    }

    #[test]
    fn a_kernel_that_does_not_name_exactly_one_entry_is_refused() {
        let clean = entry_with(&[]);
        for names in [&["_RNvCs1_6kernel4main"][..], &[ENTRY_NAME, ENTRY_NAME][..]] {
            let refusal = judge_entry_window("", &kernel_with_entry(names, &clean)).unwrap_err();
            assert!(refusal.contains("does not name exactly one function"), "{refusal}");
        }
        assert!(judge_entry_window("", b"not an ELF").unwrap_err().contains("`.symtab`"));
    }

    #[test]
    fn a_kernel_of_any_other_feature_set_is_not_judged() {
        for features in [SCHED_CHECK_KERNEL, TEST_KERNEL] {
            assert_eq!(judge_entry_window(&features.join(","), b"not an ELF"), Ok(()));
        }
    }
}
