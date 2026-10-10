#[macro_use(eprintln)]
extern crate toyos_build;

mod qemu;

use std::env;
use std::path::{Path, PathBuf};
use toyos_build::arch::Arch;
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
/// `bootloader/`, `kernel/` and `userland/` all link through the toolchain's
/// `rust-lld` — which is the distinction "ToyOS needs a C compiler" would destroy.
const REQUIRED: &[Tool] = &[
    Tool { any: &["git"], why: "every build; the image ships what git says is tracked" },
    Tool { any: &["rustup"], why: "the toolchain — install from https://rustup.rs" },
    Tool { any: &["cc"], why: "rustc links every host binary through it; no guest binary" },
    Tool {
        any: &["cmake"],
        why: "every build keys the host's LLVM on its `--version`, rustc's bootstrap \
              configures LLVM and clang with it, and every sysroot build the C++ runtime; \
              `brew install cmake` on macOS",
    },
    Tool {
        any: &["python3", "python"],
        why: "rust/x runs rustc's bootstrap, which is Python, and the C++ runtime's CMake \
              requires a Python 3 and runs it in every sysroot build",
    },
];

/// Where the OS would find `name`, if anywhere.
///
/// A `PATH` scan and not a `--version` run: it is what `Command::new` does
/// anyway.
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

fn check_prerequisites(root: &Path, arch: Arch) {
    let mut missing: Vec<String> = REQUIRED
        .iter()
        .filter(|t| !t.any.iter().any(|n| executable_on_path(n)))
        .map(|t| format!("{} ({})", t.any.join(" or "), t.why))
        .collect();
    if !executable_on_path(arch.qemu()) {
        missing.push(format!("{} (every {} boot — install QEMU)", arch.qemu(), arch.name()));
    }
    if !missing.is_empty() {
        eprintln!("Error: missing required tools:");
        for tool in &missing {
            eprintln!("  - {tool}");
        }
        std::process::exit(1);
    }

    // The one prerequisite whose *version* decides verdicts rather than whether
    // anything runs at all, so a scan of `PATH` cannot ask it.
    // `toyos_build::ci` carries why this is a note here and a red in CI.
    if let Some(note) = toyos_build::ci::qemu_version_note(root, arch) {
        eprintln!("{note}");
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
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

    // Every CI job, before `check_prerequisites`: the host job's runner has no
    // QEMU, and a guest job names its own instrument rather than being noted at.
    if asked(&flags::CI) {
        toyos_build::ci::dispatch(&root, &args);
        return;
    }
    // Runs the nightly's clippy over three targets, so a branch verifies the
    // gate's own claim before a push. Here for the same reason as the two below:
    // it shells to `cargo clippy` and the runner that runs it has no QEMU.
    if asked(&flags::CLIPPY) {
        toyos_build::clippy::dispatch(&root);
        return;
    }
    // Writes one file outside the checkout and builds nothing.
    if asked(&flags::SIGNING_KEY_NEW) {
        match toyos_build::signing::mint_owner_key() {
            Ok((path, fingerprint)) => println!("The owner's image-signing key is at {} ({fingerprint}).", path.display()),
            Err(why) => {
                eprintln!("Error: {why}");
                std::process::exit(1);
            }
        }
        return;
    }
    // Before anything is built, so a missing key is refused before any lock
    // and no image this run writes is signed by two keys.
    let update_image = CARGO_RUN.value(&args, &flags::UPDATE_IMAGE).map(PathBuf::from);
    if asked(&flags::OWNER_KEY) || update_image.is_some() {
        match toyos_build::signing::use_owner() {
            Ok(key) => eprintln!("Signing with the owner's key {}.", key.fingerprint()),
            Err(why) => {
                eprintln!("Error: {why}");
                std::process::exit(1);
            }
        }
    }
    // Writes the key's package repository and builds nothing.
    if let Some(manifest) = CARGO_RUN.value(&args, &flags::PUBLISH) {
        let key = toyos_build::signing::key();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs();
        let published = toyos_build::publish::repository(&root, key)
            .and_then(|dir| toyos_build::publish::publish(Path::new(manifest), &dir, key, now).map(|p| (dir, p)));
        match published {
            Ok((dir, p)) => println!(
                "Published into {}: root {}, targets {}, timestamp {}, signed by {}.",
                dir.display(),
                p.root,
                p.targets,
                p.timestamp,
                key.fingerprint()
            ),
            Err(why) => {
                eprintln!("Error: {why}");
                std::process::exit(1);
            }
        }
        return;
    }

    let arch = toyos_build::build::arch_for(&args);
    check_prerequisites(&root, arch);
    env::set_current_dir(&root).expect("Failed to cd to project root");

    let debug = asked(&flags::DEBUG);
    let build_only = asked(&flags::BUILD_ONLY);
    let dump_audio = asked(&flags::DUMP_AUDIO);
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

    if let Some(bank) = CARGO_RUN.value(&args, &flags::REGEN_SOUNDFONT) {
        toyos_build::soundfont::regen(&root, Path::new(bank));
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
    if let Some(out) = update_image {
        toyos_build::build::build_update(&root, &boot, &plan, &out);
        println!("Update image: {} (ssh <machine> update < it)", out.display());
        return;
    }
    let image = toyos_build::build::build(&root, boot, &plan);
    eprintln!("Build finished.");
    println!("Boot image: {}", image.display());

    if !build_only {
        qemu::launch(&qemu::Options { arch: plan.arch, debug, dump_audio, profile, smp, mute, image });
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
