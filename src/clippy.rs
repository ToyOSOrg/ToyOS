//! Default clippy with warnings denied, over the three trees this host lints.
//!
//! `cargo run -- --clippy` runs [`SHAPES`] alone; `cargo run -- --ci
//! host-full` runs the same list as one of its steps, so the local command and
//! the nightly gate cannot verify different sets.
//!
//! Userland is not here: `x86_64-unknown-toyos` is a custom target, and the
//! fork's `toyos` toolchain ships no clippy.

use std::path::Path;
use std::process::Command;

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
/// so the kernel and bootloader run from their own; a `$ADOPTED` token in
/// `after` splices [`ADOPTED`] there.
struct Shape {
    dir: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

/// `--all-targets` on the host workspace only: on the bootloader and kernel a
/// test target links `std`, whose `panic_impl` collides with theirs. The second
/// kernel shape is the feature set every guest boots, whose `cfg`s the default
/// set never sees; the third is the one `--kernel-param` builds, `boot-actuators`
/// without `test-actuators`, whose dead code neither of the others can see.
/// `undocumented_unsafe_blocks` is adopted per area as each area's
/// justifications land.
const SHAPES: &[Shape] = &[
    Shape {
        dir: "",
        before: &["--workspace", "--all-targets", "--keep-going"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", "x86_64-unknown-none"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", "x86_64-unknown-none", "--features", "boot-actuators,test-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "kernel",
        before: &["--target", "x86_64-unknown-none", "--features", "boot-actuators"],
        after: &["$ADOPTED", "-D", "warnings"],
    },
    Shape {
        dir: "bootloader",
        before: &["--target", "x86_64-unknown-uefi"],
        after: &["$ADOPTED", "-W", "clippy::undocumented_unsafe_blocks", "-D", "warnings"],
    },
    Shape {
        dir: "",
        before: &["-p", "toyos-abi", "--all-targets", "--keep-going"],
        after: &["-W", "clippy::undocumented_unsafe_blocks", "-D", "warnings"],
    },
];

impl Shape {
    /// The command as a reader writes it, `$ADOPTED` unexpanded.
    fn line(&self) -> String {
        let mut parts = vec!["cargo clippy".to_string()];
        parts.extend(self.before.iter().map(|s| (*s).to_string()));
        parts.push("--".to_string());
        parts.extend(self.after.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }

    /// The arguments to `cargo clippy`, `$ADOPTED` spliced in — what actually
    /// runs.
    fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = self.before.iter().map(|s| (*s).to_string()).collect();
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
        println!("=== clippy: {scope} — {}", shape.line());
        let status = Command::new("cargo")
            .arg("clippy")
            .args(shape.args())
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
    println!("clippy: {} invocations clean", SHAPES.len());
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
