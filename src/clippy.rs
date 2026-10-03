//! Default clippy with warnings denied, over the three trees this host lints.
//!
//! `cargo run -- --clippy` runs [`SHAPES`] alone; `cargo run -- --ci
//! host` runs the same list as one of its steps, so the local command and
//! the merge gate cannot verify different sets.
//!
//! Userland is not here: its targets are the fork's own, and the fork's
//! `toyos` toolchain ships no clippy. The kernel and the bootloader are linted
//! for every architecture.

use std::path::Path;
use std::process::Command;

use crate::arch::Arch;
use crate::ci::{KERNEL_HOST_TARGET, KERNEL_MANIFEST};

/// The pedantic/nursery lints adopted one at a time, each on a measured finding
/// (`issues/build/clippy-stage-two-is-lints-one-at-a-time.md`).
const ADOPTED: &[&str] = &[
    "clippy::checked_conversions",
    "clippy::default_trait_access",
    "clippy::manual_midpoint",
    "clippy::redundant_clone",
    "clippy::unchecked_time_subtraction",
    "clippy::unnecessary_semicolon",
];

/// One `cargo clippy`. `dir` is relative to the repository root and empty for
/// the root itself — `.cargo/config.toml` is found from the working directory,
/// so the kernel and bootloader run from their own; a `$ADOPTED` token splices
/// [`ADOPTED`] there, a `$CONTROLS` token [`control_features`], and a
/// `$KERNEL_CONTROLS` token those of them whose package is `kernel`.
struct Shape {
    dir: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

/// Every `--kernel-feature` instrument with the default heap band and
/// `pass-spin`'s own hold: the other band and hold arms exclude these.
const INSTRUMENTS: &str = "debug-wait,sched-check,sched-tripwire,heap-tripwire,heap-sweep,\
                           pass-spin,stack-witness,switch-witness,switch-witness-mutate-frame,\
                           switch-witness-mutate-rsp,df-witness,df-witness-mutate,\
                           entry-df-unclean,mask-windows";

/// [`INSTRUMENTS`] less the direction-flag three, which are x86-64's alone.
const AARCH64_INSTRUMENTS: &str = "debug-wait,sched-check,sched-tripwire,heap-tripwire,heap-sweep,\
                                   pass-spin,stack-witness,switch-witness,\
                                   switch-witness-mutate-frame,switch-witness-mutate-rsp,\
                                   mask-windows";

const UNCONTROLLED: &[&str] = &["kernel/sched-tripwire"];

/// Every model's negative control and [`UNCONTROLLED`], as `package/feature`.
fn control_features() -> Vec<String> {
    let controls = crate::ci::CONTROLS.iter().map(|c| format!("{}/{}", c.krate, c.feature));
    controls.chain(UNCONTROLLED.iter().map(|f| (*f).to_string())).collect()
}

/// `--all-targets` on the host workspace only: on the bootloader and kernel a
/// test target links `std`, whose `panic_impl` collides with theirs. The second
/// kernel shape is the feature set every guest boots, whose `cfg`s the default
/// set never sees; the third is the one `--kernel-param` builds, `boot-actuators`
/// without `test-actuators`, whose dead code neither of the others can see.
/// `undocumented_unsafe_blocks` is adopted per area as each area's
/// justifications land. `toyos-xhci` has a shape of its own because the
/// workspace run builds it only with `toyos-xhci-sim`'s `flaws`, never as the
/// kernel does. `kernel-loom` has one because `victim-retires-mid-probe`'s
/// test arm excludes `no-preempt-guard`, which `$CONTROLS` turns on beside it.
/// The kernel's library, no member of the host workspace, is linted on the host
/// from its own manifest: its tests as `--ci host` runs them, and again with
/// `$KERNEL_CONTROLS`.
const SHAPES: &[Shape] = &[
    Shape {
        dir: "",
        before: &["--workspace", "--all-targets", "--keep-going"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &["--workspace", "--all-targets", "--keep-going", "--features", "$CONTROLS"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &[
            "--manifest-path",
            KERNEL_MANIFEST,
            "--target-dir",
            KERNEL_HOST_TARGET,
            "--lib",
            "--tests",
            "--features",
            "sched-check",
        ],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &[
            "--manifest-path",
            KERNEL_MANIFEST,
            "--target-dir",
            KERNEL_HOST_TARGET,
            "--lib",
            "--tests",
            "--features",
            "sched-check,protocol-port",
            "--features",
            "$KERNEL_CONTROLS",
        ],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel()],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel(), "--features", "boot-actuators,test-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel(), "--features", "boot-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel(), "--features", INSTRUMENTS],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel(), "--features", "heap-band-notail,heap-lockspin"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::X86_64.kernel(), "--features", "heap-band-nohead"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::Aarch64.kernel()],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::Aarch64.kernel(), "--features", "boot-actuators,test-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::Aarch64.kernel(), "--features", "boot-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", Arch::Aarch64.kernel(), "--features", AARCH64_INSTRUMENTS],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "bootloader",
        before: &["--target", Arch::X86_64.loader()],
        after: &["$ADOPTED", "-W", "clippy::undocumented_unsafe_blocks", "-D", "warnings"],
    },
    Shape {
        dir: "bootloader",
        before: &["--target", Arch::Aarch64.loader()],
        after: &["$ADOPTED", "-W", "clippy::undocumented_unsafe_blocks", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &["-p", "toyos-xhci", "--all-targets"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &[
            "-p",
            "kernel-loom",
            "--test",
            "loom_mailbox",
            "--features",
            "victim-retires-mid-probe",
        ],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    // Its own target directory: cargo keeps one check of a unit, so a unit
    // linted under two sets of lints is checked again by each, every run, and
    // so is every crate depending on it.
    Shape {
        dir: "",
        before: &["-p", "toyos-abi", "--all-targets", "--keep-going", "--target-dir", "target/clippy-abi"],
        after: &["-W", "clippy::undocumented_unsafe_blocks", "-D", "warnings"],
    },
];

/// The bare targets the kernel and bootloader shapes lint against, which
/// `rustup target add` installs: every architecture's.
pub const BARE_TARGETS: [&str; 4] =
    [Arch::X86_64.kernel(), Arch::X86_64.loader(), Arch::Aarch64.kernel(), Arch::Aarch64.loader()];

impl Shape {
    /// The command as a reader writes it, its tokens unexpanded.
    fn line(&self) -> String {
        let mut parts = vec!["cargo clippy".to_string()];
        parts.extend(self.before.iter().map(|s| (*s).to_string()));
        parts.push("--".to_string());
        parts.extend(self.after.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }

    /// The arguments to `cargo clippy`, every token spliced in — what actually
    /// runs.
    fn args(&self) -> Vec<String> {
        let kernel = format!("{}/", crate::ci::KERNEL);
        let mut args: Vec<String> = self
            .before
            .iter()
            .map(|s| match *s {
                "$CONTROLS" => control_features().join(","),
                "$KERNEL_CONTROLS" => {
                    let ours = control_features().into_iter().filter(|f| f.starts_with(&kernel));
                    ours.collect::<Vec<_>>().join(",")
                }
                arg => arg.to_string(),
            })
            .collect();
        args.push("--".to_string());
        for token in self.after {
            if *token == "$ADOPTED" {
                for lint in ADOPTED {
                    args.push("-W".to_string());
                    args.push((*lint).to_string());
                }
            } else {
                args.push((*token).to_string());
            }
        }
        args
    }
}

/// Run every shape, keeping going across them, and name the ones that found
/// warnings or failed to run.
pub fn run(root: &Path) -> Vec<String> {
    let mut failed = Vec::new();
    for shape in SHAPES {
        let scope = if shape.dir.is_empty() { "workspace root" } else { shape.dir };
        eprintln!("=== clippy: {scope} — {}", shape.line());
        let status = Command::new("cargo")
            .arg("clippy")
            .args(shape.args())
            // The loader will not compile without the key it embeds; a
            // throwaway one, since nothing linted here is signed.
            .env(crate::signing::KEY_ENV, crate::signing::key().public_hex())
            .env(crate::signing::FLOOR_ENV, crate::signing::key().floor_scope().word())
            .current_dir(root.join(shape.dir))
            .status()
            .unwrap_or_else(|e| panic!("running cargo clippy in {scope}: {e}"));
        if !status.success() {
            failed.push(shape.line());
        }
    }
    failed
}

/// `cargo run -- --clippy`: exit non-zero if any shape reports a finding, so the
/// whole set is one green/red answer.
pub fn dispatch(root: &Path) {
    let failed = run(root);
    if !failed.is_empty() {
        eprintln!("clippy: {} of {} invocation(s) found warnings:", failed.len(), SHAPES.len());
        for line in &failed {
            eprintln!("  {line}");
        }
        std::process::exit(1);
    }
    eprintln!("clippy: {} invocations clean", SHAPES.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$ADOPTED` is spliced where the token sits and nowhere else.
    #[test]
    fn the_adopted_set_expands_into_the_shapes_that_name_it() {
        let workspace = &SHAPES[0];
        assert!(workspace.args().windows(2).any(|w| w == ["-W", "clippy::redundant_clone"]));
        let abi = SHAPES.last().unwrap();
        assert!(!abi.args().iter().any(|a| a == "clippy::redundant_clone"));
        assert!(abi.args().iter().any(|a| a == "clippy::undocumented_unsafe_blocks"));
        assert!(!abi.args().iter().any(|a| a == "$ADOPTED"));
    }
}
