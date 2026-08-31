//! Making a linked worktree buildable, and saying why when it cannot be.
//!
//! `git worktree add` alone leaves a tree that does not build and, worse, one
//! that builds *wrongly*: `rust/` comes out an empty stub, so the build system
//! reads it as a missing submodule, clones 913 MiB from the network, bootstraps
//! a second 47 GiB toolchain, and finally points the machine-global rustup
//! `toyos` name at it — taking the toolchain out from under every other
//! checkout. Measured, in that order, on this host.
//!
//! So the shared state stays shared and nothing here copies it: `rust/` is left
//! the stub it was, and [`crate::toolchain::rust_dir`] sends every read to the
//! primary checkout. What this module does is the small remainder — create the
//! worktree, carry over the one file git cannot, and refuse by name when the
//! result would not be usable.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::toolchain;

/// What a worktree's crate target directories reach: 4.1 GiB after
/// `--build-only`, and 23 GiB on the primary checkout, which has run everything.
/// Measured with `du`. The 50 GiB `rust/` is shared and never counted here.
///
/// Refusing at the upper figure plus a little, rather than at the lower one: a
/// build that fills the disk halfway through costs more than a worktree that
/// was never made.
const NEEDED_BYTES: u64 = 25 * 1024 * 1024 * 1024;

pub fn dispatch(root: &Path, args: &[String]) {
    let mut rest = args.iter().skip_while(|a| *a != "--worktree").skip(1);
    let verb = rest.next().map(String::as_str);
    let operand = rest.next().cloned();
    match verb {
        Some("add") => add(root, &path_operand("add", operand)),
        Some("list") => list(root),
        Some("remove") => remove(root, &path_operand("remove", operand)),
        other => panic!(
            "--worktree takes add <path>, list, or remove <path>; got {other:?}"
        ),
    }
}

fn path_operand(verb: &str, operand: Option<String>) -> String {
    let path = operand.unwrap_or_else(|| panic!("--worktree {verb} needs a path"));
    assert!(
        !path.starts_with('-'),
        "--worktree {verb} needs a path, got flag {path:?}"
    );
    path
}

/// Create a worktree and leave it in a state where `cargo run -- --build-only`
/// works.
fn add(root: &Path, path: &str) {
    let path = PathBuf::from(path);
    assert!(!path.exists(), "{} already exists", path.display());
    let name = path
        .file_name()
        .unwrap_or_else(|| panic!("{} has no final component", path.display()))
        .to_string_lossy()
        .to_string();

    // Everything that would make the result unusable, asked before anything is
    // created: a half-made worktree is worse than none, because the next agent
    // finds it and believes it.
    let primary = match toolchain::owner(root) {
        toolchain::Owner::Us | toolchain::Owner::Installed => root.to_path_buf(),
        toolchain::Owner::Elsewhere(p) => p,
    };
    let stage2 = primary.join(format!(
        "rust/build/{}/stage2",
        toolchain::host_triple()
    ));
    assert!(
        stage2.join("bin/rustc").exists(),
        "the shared toolchain does not exist yet ({} is missing).\n\
         Run `cargo run -- --build-only` in {} before making worktrees of it.",
        stage2.display(),
        primary.display()
    );
    let free = free_bytes(path.parent().unwrap_or(Path::new("/")));
    assert!(
        free >= NEEDED_BYTES,
        "{} has {:.1} GiB free and a worktree's target directories reach about \
         {:.0} GiB.\nThe shared toolchain is not copied, but the crate targets are \
         its own.",
        path.parent().unwrap_or(Path::new("/")).display(),
        free as f64 / 1024.0_f64.powi(3),
        NEEDED_BYTES as f64 / 1024.0_f64.powi(3),
    );

    let branch = format!("wt/{name}");
    git(root, &["worktree", "add", "-b", &branch, &path.to_string_lossy(), "main"]);

    // `rust/` is deliberately left the empty stub `git worktree add` made. It is
    // not an oversight the next reader should fix: initialising it is the
    // 913 MiB clone, and a symlink in its place makes git error out of `status`,
    // `diff` and `submodule` alike rather than just ignoring it.

    // The one file git cannot carry: it is gitignored, and a worktree that
    // silently loses the fork redirects would build different code from the
    // checkout it was made from and report the difference as a result.
    let redirects = root.join(".cargo/config.toml");
    if redirects.exists() {
        fs::copy(&redirects, path.join(".cargo/config.toml"))
            .unwrap_or_else(|e| panic!("copy {}: {e}", redirects.display()));
        eprintln!("carried over .cargo/config.toml (fork redirects)");
    }

    eprintln!();
    eprintln!("worktree   {}", path.display());
    eprintln!("branch     {branch}");
    eprintln!("toolchain  {} (shared, not copied)", stage2.display());
    eprintln!();
    eprintln!("Build it with `cargo run -- --build-only` from {}.", path.display());
}

fn list(root: &Path) {
    let primary = match toolchain::owner(root) {
        toolchain::Owner::Us | toolchain::Owner::Installed => root.to_path_buf(),
        toolchain::Owner::Elsewhere(p) => p,
    };
    eprintln!("toolchain owner  {}", primary.display());
    eprintln!(
        "rustup toyos     {}",
        fs::read_link(
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".rustup/toolchains/toyos")
        )
        .map_or_else(|_| "unlinked".to_string(), |p| p.display().to_string())
    );
    eprintln!();
    let trees = survey(root, true);
    for tree in &trees {
        let branch = if tree.branch.is_empty() { "(detached)" } else { &tree.branch };
        let note = match (tree.primary, tree.landed) {
            (true, _) => "  primary",
            (_, true) => "  landed — reclaimable",
            _ => "",
        };
        eprintln!(
            "{:<44} {:<26} {:>9} in {:>2} target dir(s){note}",
            tree.path.display(),
            branch,
            gib(tree.bytes),
            tree.targets,
        );
    }
    eprintln!();
    eprintln!(
        "{} worktree(s), {} of build caches; the shared toolchain is not counted",
        trees.len(),
        gib(trees.iter().map(|t| t.bytes).sum()),
    );
    if let Some(line) = reclaim_line(&trees) {
        eprintln!("{line}");
    }
}

/// One worktree, and the two facts that decide whether it should still exist.
///
/// **Nothing ever reclaimed one**, and `add`'s disk check was the whole of what
/// this subject had — a refusal is the last notice rather than the first. A
/// worktree whose branch has landed has no reason to hold its build caches, and
/// neither its size nor whether its branch is in `origin/main` is anything
/// `git worktree list` says.
pub struct Tree {
    pub path: PathBuf,
    /// Empty for a detached worktree.
    pub branch: String,
    /// What its build caches hold. The shared `rust/` is never counted.
    pub bytes: u64,
    pub targets: usize,
    /// The checkout that owns `rust/`, the rustup link and `main`. Never
    /// reclaimable whatever its branch says.
    pub primary: bool,
    /// Its branch is already in `origin/main`.
    pub landed: bool,
}

/// Every worktree of `root`.
///
/// `all_sizes` walks every worktree's caches, which is a metadata walk of tens
/// of gigabytes and takes seconds; `false` walks only the ones that could be
/// given back, which is the only size `--sync` prints.
pub fn survey(root: &Path, all_sizes: bool) -> Vec<Tree> {
    let mut trees = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = String::new();
    let listing = capture(root, &["worktree", "list", "--porcelain"]);
    for line in listing.lines() {
        if let Some(next) = line.strip_prefix("worktree ") {
            if let Some(done) = path.replace(PathBuf::from(next)) {
                let first = trees.is_empty();
                trees.push(measure(root, done, std::mem::take(&mut branch), first, all_sizes));
            }
        } else if let Some(name) = line.strip_prefix("branch ") {
            branch = name.trim_start_matches("refs/heads/").to_string();
        }
    }
    if let Some(done) = path {
        let first = trees.is_empty();
        trees.push(measure(root, done, branch, first, all_sizes));
    }
    trees
}

/// What could be given back, or nothing to say.
///
/// `--sync` reports this as well as `list`, because `--sync` runs at the moment
/// a branch lands, which is the moment its worktree stops having a reason to
/// exist.
pub fn reclaim_line(trees: &[Tree]) -> Option<String> {
    let done: Vec<&Tree> = trees.iter().filter(|t| !t.primary && t.landed).collect();
    if done.is_empty() {
        return None;
    }
    Some(format!(
        "{} worktree(s) hold {} on branches already in origin/main: {}\n\
         `cargo run -- --worktree remove <path>` gives each back, and refuses one carrying \
         uncommitted work.",
        done.len(),
        gib(done.iter().map(|t| t.bytes).sum()),
        done.iter().map(|t| t.path.display().to_string()).collect::<Vec<_>>().join(", "),
    ))
}

fn measure(
    root: &Path,
    path: PathBuf,
    branch: String,
    primary: bool,
    all_sizes: bool,
) -> Tree {
    let landed = !primary
        && !branch.is_empty()
        && ok(root, &["merge-base", "--is-ancestor", &branch, "origin/main"]);
    let mut bytes = 0;
    let mut targets = 0;
    if all_sizes || landed {
        caches(&path, &mut bytes, &mut targets);
    }
    Tree { path, branch, bytes, targets, primary, landed }
}

/// Directories a survey never enters: the shared toolchain and git's own store.
const NOT_OURS: &[&str] = &["rust", ".git"];

/// Ten `target/` directories per worktree is the design and not an accident —
/// `Cargo.toml`'s `exclude` list keeps five cross-compiled crates out of the
/// host workspace and each guest fixture resolves on its own — so `cargo clean`
/// at the root reaches exactly one of them and a count is worth printing.
fn caches(dir: &Path, bytes: &mut u64, targets: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if NOT_OURS.contains(&name.as_ref()) {
            continue;
        }
        if name == "target" {
            *bytes += bytes_under(&path);
            *targets += 1;
            continue;
        }
        caches(&path, bytes, targets);
    }
}

fn bytes_under(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let mut total = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        total += if meta.is_dir() { bytes_under(&path) } else { meta.len() };
    }
    total
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / 1024.0_f64.powi(3))
}

/// Remove a worktree and the branch it was made with.
///
/// Deliberately not `--force`: a worktree with uncommitted work in it is a
/// refusal, because the work in a worktree is the only copy of itself.
fn remove(root: &Path, path: &str) {
    git(root, &["worktree", "remove", path]);
    eprintln!("removed {path}; its branch is still there, and `git branch -d` will say if it is unmerged");
}

fn free_bytes(dir: &Path) -> u64 {
    let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())
        .unwrap_or_else(|_| panic!("{} has an embedded NUL", dir.display()));
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid, NUL-terminated C string and `buf` is a
    // `libc::statvfs` the kernel fills in whole or leaves at the `zeroed()`
    // above; a non-zero return is checked before anything reads it.
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut buf) };
    assert!(rc == 0, "statvfs {}: {}", dir.display(), std::io::Error::last_os_error());
    buf.f_bavail as u64 * buf.f_frsize as u64
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("run git: {e}"));
    assert!(status.success(), "git {args:?} failed");
}

/// git's answer, for a question rather than an action.
fn capture(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run git: {e}"));
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether git says yes. A non-zero exit is the answer here, never a failure.
fn ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_refused_as_a_worktree_path_by_name() {
        let args = ["toyos-build", "--worktree", "add", "--help"].map(String::from);
        let panic = std::panic::catch_unwind(|| dispatch(Path::new("/not-used"), &args))
            .expect_err("a flag is not a worktree path");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("--help"), "the refusal must name the bad argument: {message}");
    }

    fn tree(path: &str, primary: bool, landed: bool, bytes: u64) -> Tree {
        Tree {
            path: PathBuf::from(path),
            branch: String::from("wt/x"),
            bytes,
            targets: 10,
            primary,
            landed,
        }
    }

    /// **The primary checkout sits on `main`**, which is an ancestor of
    /// `origin/main` by construction, so a rule that offered back every landed
    /// worktree would offer back the one holding `rust/` and the rustup link.
    #[test]
    fn only_a_landed_worktree_that_is_not_the_primary_is_offered_back() {
        assert!(reclaim_line(&[tree("/primary", true, true, 4 << 30)]).is_none());
        assert!(reclaim_line(&[tree("/live", false, false, 8 << 30)]).is_none());
        let line = reclaim_line(&[
            tree("/primary", true, true, 4 << 30),
            tree("/gone", false, true, 2 << 30),
            tree("/live", false, false, 8 << 30),
        ])
        .expect("a landed worktree that is not the primary is reclaimable");
        assert!(line.contains("/gone"), "{line}");
        assert!(!line.contains("/live"), "{line}");
        assert!(!line.contains("/primary"), "{line}");
        assert!(line.contains("2.0 GiB"), "the offer has to say what it is worth: {line}");
    }
}
