//! The `host` job's clippy, runnable here.
//!
//! `cargo run -- --clippy` is the five invocations CI's `host` job denies
//! warnings on, so a branch verifies before a push the claim that gate checks
//! rather than a narrower `cargo clippy -p <crate>`. The list lives here once;
//! a gate below holds it against `.github/workflows/host-tests.yml` so neither
//! the workflow nor this command can move without the other.

use std::path::Path;
use std::process::Command;

/// The pedantic/nursery lints the workflow names in `$ADOPTED`, in its order.
const ADOPTED: &[&str] = &[
    "clippy::checked_conversions",
    "clippy::default_trait_access",
    "clippy::manual_midpoint",
    "clippy::redundant_clone",
    "clippy::unchecked_time_subtraction",
    "clippy::unnecessary_semicolon",
];

/// One `cargo clippy` the `host` job runs. `dir` is relative to the repository
/// root and empty for the root itself; a `$ADOPTED` token in `after` splices
/// [`ADOPTED`] where the workflow's variable expands.
struct Shape {
    dir: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

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

/// `$ADOPTED`'s value, spelled as the workflow writes it. Only the gate reads
/// it; what runs splices [`ADOPTED`] token by token in [`Shape::args`].
#[cfg(test)]
fn adopted() -> String {
    ADOPTED.iter().map(|l| format!("-W {l}")).collect::<Vec<_>>().join(" ")
}

impl Shape {
    /// The command as the workflow writes it, `$ADOPTED` unexpanded — the string
    /// the gate holds the workflow against.
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

/// Run every shape; exit non-zero if any clippy reports a finding or fails to
/// run, so the whole set is one green/red answer.
pub fn dispatch(root: &Path) {
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
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The workflow's executable text with comment lines dropped, continuation
    /// backslashes removed and whitespace collapsed, so a command the YAML
    /// splits across lines is one substring and prose that merely names a
    /// command is not counted as one. No clippy command starts with `#` or
    /// carries a backslash, so both removals are safe.
    fn flatten(text: &str) -> String {
        text.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join(" ")
            .replace('\\', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every way the workflow and [`SHAPES`] disagree, one line each. A pure
    /// function of the flattened text so the negative control can stage a
    /// workflow that is not on disk.
    fn drift(flat: &str) -> Vec<String> {
        let mut bad = Vec::new();
        let decl = format!("ADOPTED=\"{}\"", adopted());
        if !flat.contains(&decl) {
            bad.push(format!("the workflow's $ADOPTED is no longer `{decl}`"));
        }
        for shape in SHAPES {
            if !flat.contains(&shape.line()) {
                bad.push(format!("the workflow no longer runs `{}`", shape.line()));
            }
        }
        let count = flat.matches("cargo clippy").count();
        if count != SHAPES.len() {
            bad.push(format!(
                "the workflow runs `cargo clippy` {count} time(s); this file lists {}",
                SHAPES.len()
            ));
        }
        bad
    }

    /// `cargo run -- --clippy` runs exactly the `host` job's clippy — the gap
    /// this file closed was a local run that verified a narrower claim than CI.
    #[test]
    fn the_local_clippy_is_the_host_jobs_clippy() {
        let path = repo_root().join(".github/workflows/host-tests.yml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} carries the clippy step: {e}", path.display()));
        let bad = drift(&flatten(&text));
        assert!(
            bad.is_empty(),
            "`cargo run -- --clippy` and the `host` job's clippy have drifted, so the local half \
             verifies a different claim than the gate:\n  {}",
            bad.join("\n  ")
        );
    }

    /// Teeth: the scan refuses each shape of drift it exists to catch.
    #[test]
    fn the_drift_scan_refuses_a_workflow_that_moved() {
        let real =
            flatten(&std::fs::read_to_string(repo_root().join(".github/workflows/host-tests.yml"))
                .unwrap());
        assert!(drift(&real).is_empty());
        // A flag dropped from an invocation: CI now denies a different set.
        assert!(!drift(&real.replace("--all-targets", "")).is_empty());
        // A sixth invocation the local command would not run.
        assert!(!drift(&format!("{real} cargo clippy --workspace")).is_empty());
        // An adopted lint swapped — the class of the #283 miss.
        assert!(!drift(&real.replace("manual_midpoint", "manual_is_multiple_of")).is_empty());
    }

    /// `$ADOPTED` is spliced where the token sits and nowhere else, so what runs
    /// is what the workflow expands.
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
