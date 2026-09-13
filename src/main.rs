mod qemu;

use std::env;
use std::path::{Path, PathBuf};
use toyos_build::flags::{self, CARGO_RUN};

/// One prerequisite: any of `any` satisfies it, and `why` is what reaches it.
struct Tool {
    any: &'static [&'static str],
    why: &'static str,
}

/// No build gets past these.
///
/// `cc` is here and is not ours: `rustc` drives every *host* link through it
/// and `rustup` does not install it. Nothing that boots goes near it —
/// `bootloader/`, `kernel/` and `userland/` all set `linker = "toyos-ld"` —
/// which is the distinction "ToyOS needs a C compiler" would destroy.
const REQUIRED: &[Tool] = &[
    Tool { any: &["git"], why: "every build; the image ships what git says is tracked" },
    Tool { any: &["rustup"], why: "the toolchain — install from https://rustup.rs" },
    Tool { any: &["qemu-system-x86_64"], why: "every boot — install QEMU" },
    Tool { any: &["cc"], why: "rustc links every host binary through it; no guest binary" },
];

/// Named, because a list that stops at what is fatal reads as the whole list.
/// Each of these costs one thing when absent rather than the build, so none of
/// them exits.
const ALSO_USED: &[Tool] = &[
    Tool {
        any: &["python3", "python", "py", "python2", "uv"],
        why: "rust/x runs rustc's bootstrap, which is Python — a clean clone and \
              every toolchain change need one",
    },
];

/// Where the OS would find `name`, if anywhere.
///
/// A `PATH` scan and not a `--version` run: it is what `Command::new` does
/// anyway, and one name above must not be executed — asking macOS for `py`
/// opens the Command Line Tools installer, which is why `rust/x` searches
/// `python3` ahead of it.
fn executable_on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(name))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

fn check_prerequisites(root: &Path) {
    fn absent(tools: &'static [Tool]) -> Vec<&'static Tool> {
        tools.iter().filter(|t| !t.any.iter().any(|n| executable_on_path(n))).collect()
    }

    for tool in absent(ALSO_USED) {
        eprintln!("Note: no {} — {}", tool.any.join(" or "), tool.why);
    }

    let missing = absent(REQUIRED);
    if !missing.is_empty() {
        eprintln!("Error: missing required tools:");
        for tool in &missing {
            eprintln!("  - {} ({})", tool.any.join(" or "), tool.why);
        }
        std::process::exit(1);
    }

    // The one prerequisite whose *version* decides verdicts rather than whether
    // anything runs at all, so a scan of `PATH` cannot ask it.
    // `toyos_build::ci` carries why this is a note here and a red in CI.
    if let Some(note) = toyos_build::ci::qemu_version_note(root) {
        eprintln!("{note}");
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    match flags::check(&args) {
        flags::Outcome::Proceed => {}
        flags::Outcome::Help(message) => {
            println!("{message}");
            return;
        }
        flags::Outcome::Refuse(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
    let asked = |flag: &flags::Flag| CARGO_RUN.present(&args, flag);

    // The landing protocol, and the command it replaced — **before
    // `check_prerequisites`**, because none of these builds anything and the
    // runner that runs `--abi-split-check` has no QEMU on it. They are git, a
    // push, and a refusal.
    if asked(&flags::LAND) {
        toyos_build::pr::dispatch_retired_land();
    }
    if asked(&flags::PR) {
        toyos_build::pr::dispatch_pr(&root, &args);
        return;
    }
    if asked(&flags::SYNC) {
        toyos_build::pr::dispatch_sync(&root);
        return;
    }
    if asked(&flags::ABI_SPLIT_CHECK) {
        toyos_build::pr::dispatch_abi_check(&root, &args);
        return;
    }
    // The published crates' rule, and the list the publish workflow reads. Here
    // for the same reason: git and five manifests, on a runner with no QEMU.
    if asked(&flags::SDK_VERSION_CHECK) {
        toyos_build::sdkversion::dispatch_check(&root, &args);
        return;
    }
    if asked(&flags::SDK_VERSIONS) {
        toyos_build::sdkversion::dispatch_versions(&root);
        return;
    }
    // Here for the same reason: it reads twelve files a sharded run left and
    // writes one, and it is meant to be run on the machine holding them —
    // which, since the run that produces them is CI's, is a runner with no
    // QEMU.
    if asked(&flags::MERGE_DURATIONS) {
        toyos_build::durations::dispatch(&root, &args);
        return;
    }
    // Runs the `host` job's clippy over three targets, so a branch verifies the
    // gate's own claim before a push. Here for the same reason as the two below:
    // it shells to `cargo clippy` and the runner that runs it has no QEMU.
    if asked(&flags::CLIPPY) {
        toyos_build::clippy::dispatch(&root);
        return;
    }
    // Reads two directories and two files and prints. Here for the same reason
    // again, and for one more: the question it answers — "is this red known,
    // and on what?" — is asked while a build is broken as often as while one
    // works.
    if asked(&flags::KNOWN_RED) {
        toyos_build::redlist::dispatch(&root, &args);
        return;
    }
    // Asks `gh`, not the toolchain, so it runs on the bare `ubuntu-latest`
    // runner the nightly schedule gives it — no QEMU, no ToyOS toolchain.
    // Same reason as the two above: before `check_prerequisites`.
    if asked(&flags::MERGE_HEALTH) {
        toyos_build::mergehealth::dispatch(&root, &args);
        return;
    }
    // Reads lockfiles and cargo's own checkouts, nothing else: the half of a
    // "zero callers" ABI sweep a monorepo grep cannot see.
    if asked(&flags::ABI_CALLERS) {
        toyos_build::forkcheck::dispatch_callers(&root, &args);
        return;
    }

    check_prerequisites(&root);
    env::set_current_dir(&root).expect("Failed to cd to project root");

    let debug = asked(&flags::DEBUG);
    let build_only = asked(&flags::BUILD_ONLY);
    let dump_audio = asked(&flags::DUMP_AUDIO);
    let rebuild_toolchain = asked(&flags::REBUILD_TOOLCHAIN);
    let claim_sysroot = asked(&flags::CLAIM_SYSROOT);
    if let Some(budget) = CARGO_RUN.value(&args, &flags::HOST_BUILDS) {
        toyos_build::buildlock::set_host_builds(
            budget.parse().unwrap_or_else(|_| panic!("--host-builds: {budget:?} is not a budget")),
        );
    }
    let smp = parse_smp(&args);
    let profile = parse_profile(&args);
    let mute = asked(&flags::MUTE);
    // A machine with no serial port has the framebuffer and nothing else, and
    // the kernel stops painting it the moment userland claims it. `--diag-boot`
    // builds the image that never does; `--console-boot` builds the one that
    // claims it deliberately and puts a shell there, having first copied the
    // boot log into its scrollback.
    let diag = asked(&flags::DIAG_BOOT);
    let console = asked(&flags::CONSOLE_BOOT);
    assert!(!(diag && console), "--diag-boot and --console-boot are two images; build one");
    // `--boot-config <dir>` builds the `system.toml` in that directory.
    // **A flashed image carries no actuator**, so only the kernel's own boot
    // parameters are admitted beside it, and every flag it cannot combine with
    // is refused by name.
    let boot_config = CARGO_RUN.value(&args, &flags::BOOT_CONFIG);
    if let Some(dir) = boot_config {
        for (other, flag) in [
            (diag, &flags::DIAG_BOOT),
            (console, &flags::CONSOLE_BOOT),
            (rebuild_toolchain, &flags::REBUILD_TOOLCHAIN),
        ] {
            assert!(!other, "--boot-config {dir} cannot be combined with {}", flag.name);
        }
        assert!(build_only, "--boot-config {dir} builds an image; pass --build-only");
        let params: Vec<String> =
            CARGO_RUN.values(&args, &flags::KERNEL_PARAM).into_iter().map(String::from).collect();
        toyos_build::build::flashable_params(&root, &params)
            .unwrap_or_else(|refusal| panic!("{refusal}"));
    }
    let boot = match (boot_config, diag, console) {
        (Some(dir), _, _) => {
            toyos_build::build::Boot::case(&root, dir).unwrap_or_else(|refusal| panic!("{refusal}"))
        }
        (None, true, _) => toyos_build::build::Boot::diag(&root),
        (None, _, true) => toyos_build::build::Boot::console(&root),
        _ => toyos_build::build::Boot::shipped(&root),
    };
    assert!(
        !(dump_audio && profile == qemu::Profile::Metal),
        "--dump-audio needs virtio-sound, which --metal-sim removes"
    );
    assert!(
        !(mute && profile != qemu::Profile::Metal),
        "--mute only means anything under --metal-sim; the others need their console"
    );

    if asked(&flags::REGEN_FONT) {
        toyos_build::assets::regen_panic_font(&root);
        return;
    }

    if asked(&flags::REGEN_WALLPAPER) {
        toyos_build::wallpaper::regen(&root);
        return;
    }

    if let Some(bank) = CARGO_RUN.value(&args, &flags::REGEN_SOUNDFONT) {
        toyos_build::soundfont::regen(&root, Path::new(bank));
        return;
    }

    if asked(&flags::WORKTREE) {
        toyos_build::worktree::dispatch(&root, &args);
        return;
    }

    // On demand and nowhere else: it asks GitHub for every fork branch head, so
    // neither `cargo test` nor `--land` may reach it.
    if asked(&flags::CHECK_FORKS) {
        toyos_build::forkcheck::dispatch(&root);
        return;
    }

    // Only where the submodules belong. In a linked worktree `rust/` is an empty
    // stub and initialising it clones the whole rust history again, into a git
    // directory of its own that shares no objects with the one beside it.
    if matches!(toyos_build::toolchain::owner(&root), toyos_build::toolchain::Owner::Us) {
        toyos_build::ensure_submodules(&root);
    }

    // Toolchain included: `build` holds the build lock across both, so no other
    // agent's clean or bootstrap can land between the two.
    let plan = toyos_build::build::plan_for(&root, &boot, debug, &args);
    let image = toyos_build::build::build(&root, boot, rebuild_toolchain, claim_sysroot, &plan);
    println!("Build finished.");
    println!("Boot image: {}", image.display());

    if !build_only {
        qemu::launch(&qemu::Options { debug, dump_audio, profile, smp, mute, image });
    }
}

/// `--gop` swaps virtio-gpu for a firmware framebuffer; `--metal-sim` goes
/// further and removes every virtio device, which is what the target laptop
/// actually presents. `--metal-sim --mute` additionally takes the 16550 away.
fn parse_profile(args: &[String]) -> qemu::Profile {
    let gop = CARGO_RUN.present(args, &flags::GOP);
    let metal = CARGO_RUN.present(args, &flags::METAL_SIM);
    match (gop, metal) {
        (_, true) => qemu::Profile::Metal,
        (true, false) => qemu::Profile::Gop,
        (false, false) => qemu::Profile::Virtio,
    }
}

/// `--smp N` sets the QEMU core count (default 8). `--smp 1` is the
/// single-CPU case the audio spec treats as first-class.
fn parse_smp(args: &[String]) -> u32 {
    let Some(value) = CARGO_RUN.value(args, &flags::SMP) else {
        return 8;
    };
    let smp: u32 = value.parse().unwrap_or_else(|_| panic!("invalid --smp value: {value:?}"));
    assert!(smp >= 1, "--smp must be at least 1");
    smp
}
