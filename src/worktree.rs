//! Making a linked worktree buildable, and saying why when it cannot be.
//!
//! `git worktree add` alone leaves a tree that does not build and, worse, one
//! that builds *wrongly*: `rust/` comes out an empty stub, so the build system
//! reads it as a missing submodule, clones 913 MiB from the network, bootstraps
//! a second 47 GiB toolchain, and finally points the machine-global rustup
//! `toyos` name at it — taking the toolchain out from under every other
//! checkout. Measured, in that order, on this host.
//!
//! So the compiler stays the primary's and nothing here copies it:
//! [`crate::toolchain::rust_dir`] sends every compiler read to the primary
//! checkout. `rust/` is left the stub it was until the first build makes it
//! this worktree's own fork checkout — a git worktree of the primary's fork
//! repository, sharing its objects (`src/sysroot.rs`) — which [`remove`] takes
//! away again. What this module does is the small remainder — create the
//! worktree, carry over the one file git cannot, and refuse by name when the
//! result would not be usable.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::buildlock::Keyed;
use crate::flags;
use crate::toolchain;

/// What a worktree's crate target directories reach: 4.1 GiB after
/// `--build-only`, and 23 GiB on the primary checkout, which has run everything;
/// its fork checkout and std build directory add 4.2 GiB. Measured with `du`.
/// The primary's 50 GiB `rust/` is shared and never counted here.
///
/// Refusing at the upper figure plus a little, rather than at the lower one: a
/// build that fills the disk halfway through costs more than a worktree that
/// was never made.
const NEEDED_BYTES: u64 = 25 * 1024 * 1024 * 1024;

pub fn dispatch(root: &Path, args: &[String]) {
    let mut rest = flags::CARGO_RUN.rest(args, &flags::WORKTREE).iter();
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

    let summary = create_worktree(root, &path, &name);

    // `rust/` is deliberately left the empty stub `git worktree add` made: the
    // first build makes it a fork checkout sharing the primary's objects, where
    // `git submodule update` would be the 913 MiB clone, and a symlink in its
    // place makes git error out of `status`, `diff` and `submodule` alike.

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
    eprintln!("branch     {summary}");
    eprintln!("compiler   {} (shared, not copied)", stage2.display());
    eprintln!();
    eprintln!("Build it with `cargo run -- --build-only` from {}.", path.display());
}

/// Never a fetch, never a reset. Every refusal below runs before either
/// branch is touched, so a half-made worktree never sits behind one.
fn create_worktree(root: &Path, path: &Path, name: &str) -> String {
    let branch = format!("wt/{name}");
    let upstream = format!("origin/{branch}");
    let local_exists = ok(root, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]);
    let origin_exists = ok(root, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{upstream}")]);
    let path_str = path.to_string_lossy();
    if local_exists {
        refuse_if_no_commit_beyond_main(
            root,
            &branch,
            &format!("delete it with `git branch -d {branch}` or pick a new name for the worktree."),
        );
        if origin_exists {
            refuse_if_behind_or_diverged(root, &branch, &upstream);
        }
        git(root, &["worktree", "add", &path_str, &branch]);
        format!("{branch} (resumed at {})", short_sha(path, "HEAD"))
    } else if origin_exists {
        refuse_if_no_commit_beyond_main(root, &upstream, "pick a new name for the worktree.");
        git(root, &["worktree", "add", "--track", "-b", &branch, &path_str, &upstream]);
        format!("{branch} (resumed from {upstream} at {})", short_sha(path, "HEAD"))
    } else {
        git(root, &["worktree", "add", "-b", &branch, &path_str, "main"]);
        format!("{branch} (new, from main at {})", short_sha(path, "HEAD"))
    }
}

/// Refuse by name, before anything is created, when `resolve` carries no
/// commit beyond `origin/main` — the same ancestry test [`measure`] uses to
/// call a worktree landed and offer its build caches back.
///
/// `merge-base --is-ancestor` cannot tell a branch that landed apart from one
/// that never diverged from `main` in the first place: both are true of it.
/// The message says only what both share, and never "landed" or "merged" —
/// a resume would otherwise start work from an old tip behind main, or from
/// a branch that never carried any work of its own.
fn refuse_if_no_commit_beyond_main(root: &Path, resolve: &str, hint: &str) {
    assert!(
        !ok(root, &["merge-base", "--is-ancestor", resolve, "origin/main"]),
        "{resolve} carries no commit beyond origin/main; {hint}"
    );
}

/// Refuse by name, before anything is created, when the local `branch` is not
/// at or ahead of its own `upstream`: a resume would otherwise pick the local
/// tip silently, and the push back would refuse for the same reason, later
/// and less clearly.
fn refuse_if_behind_or_diverged(root: &Path, branch: &str, upstream: &str) {
    if ok(root, &["merge-base", "--is-ancestor", upstream, branch]) {
        return;
    }
    let relation = if ok(root, &["merge-base", "--is-ancestor", branch, upstream]) {
        "is behind"
    } else {
        "has diverged from"
    };
    panic!(
        "{branch} ({}) {relation} {upstream} ({}); merge it first.",
        short_sha(root, branch),
        short_sha(root, upstream),
    );
}

fn short_sha(dir: &Path, rev: &str) -> String {
    capture(dir, &["rev-parse", "--short", rev]).trim().to_string()
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
/// Deliberately not `--force`: git refuses a worktree holding tracked changes
/// or untracked files and leaves it registered, because the work in a
/// worktree is the only copy of itself — that refusal stands, and so does the
/// one for its own fork checkout ([`remove_fork_checkout`]).
///
/// **git can unregister a worktree and then fail to delete it**: a path deeper
/// than `PATH_MAX` under an ignored `target/`, or a file created while it
/// walks, and it exits non-zero with the directory still there and no longer
/// a worktree of anything. Once git has let go of it nothing in it is work,
/// so the whole directory goes.
///
/// Then every sysroot, compiler and LLVM no remaining worktree names goes too
/// (`src/sysroot.rs`, `src/compiler.rs`, `src/llvm.rs`).
pub(crate) fn remove(root: &Path, path: &str) {
    let at = root.join(path);
    remove_fork_checkout(root, &at);
    if !ok_loud(root, &["worktree", "remove", path]) {
        assert!(
            !registered(root, &at),
            "git refused to remove {path} and it is still a worktree; what it said above is \
             why. Nothing was deleted."
        );
        remove_tree(&at);
        eprintln!("git unregistered {path} and left its ignored files; deleted them");
    }
    eprintln!("removed {path}; its branch is still there, and `git branch -d` will say if it is unmerged");
    let rust_dir = crate::toolchain::rust_dir(root);
    for (kind, store, what) in [
        (Keyed::Sysroot, crate::sysroot::sysroots_dir(&rust_dir), "sysroot"),
        (Keyed::Compiler, crate::compiler::compilers_dir(&rust_dir), "compiler"),
        (Keyed::Llvm, crate::llvm::store(&rust_dir), "LLVM"),
    ] {
        let swept = crate::keystore::sweep(root, kind, &store);
        if !swept.is_empty() {
            eprintln!("removed {} {what}(s) no worktree names any more", swept.len());
        }
    }
}

/// A linked worktree's own fork checkout (`src/sysroot.rs`'s `fork_checkout`)
/// is a git worktree of the primary's fork repository, which git will not
/// remove a worktree around. It goes first, and only while it holds nothing
/// that is not also somewhere else: no change in its tree, and a `HEAD` some
/// ref of the fork repository reaches.
fn remove_fork_checkout(root: &Path, at: &Path) {
    let fork = at.join("rust");
    if !fork.join(".git").is_file() || !at.join(".git").is_file() {
        return;
    }
    let path = at.display();
    let mine = capture(at, &["status", "--porcelain", "--ignore-submodules=all"]);
    assert!(mine.is_empty(), "{path} holds uncommitted work:\n{mine}Nothing was deleted.");
    let theirs = capture(&fork, &["status", "--porcelain", "--ignore-submodules=none"]);
    assert!(
        theirs.is_empty(),
        "{}'s fork checkout holds uncommitted work:\n{theirs}Nothing was deleted.",
        path
    );
    let head = capture(&fork, &["rev-parse", "HEAD"]);
    let reached = capture(&fork, &["for-each-ref", "--count=1", "--contains", head.trim()]);
    assert!(
        !reached.trim().is_empty(),
        "{}'s fork checkout is at {}, which no branch, tag or remote ref of the fork \
         repository reaches: it is the only copy of those commits. Push them, or name them \
         with a branch, first. Nothing was deleted.",
        path,
        head.trim()
    );
    // Moved out whole first, so a removal a writer interrupts leaves a named
    // directory outside the worktree rather than a half-deleted checkout in it.
    let name = at.file_name().expect("a worktree has a name").to_string_lossy();
    let aside = at.with_file_name(format!(".{name}-rust.removing"));
    fs::rename(&fork, &aside)
        .unwrap_or_else(|e| panic!("move {} to {}: {e}", fork.display(), aside.display()));
    fs::create_dir(&fork).unwrap_or_else(|e| panic!("recreate the stub {}: {e}", fork.display()));
    let primary = crate::primary_checkout(root);
    git(&primary.join("rust"), &["worktree", "prune"]);
    let backtrace = primary.join("rust/library/backtrace");
    if backtrace.join(".git").exists() {
        git(&backtrace, &["worktree", "prune"]);
    }
    remove_tree(&aside);
}

/// Remove `dir` and everything in it, including what appears while it goes.
///
/// A writer on this host — the leftovers are `.DS_Store` files — can put a file
/// into a directory while it is being emptied, so a plain recursive delete finds a directory it has just emptied not empty and
/// stops halfway — the `Directory not empty` git itself dies on. The removal
/// runs again over what is left, at most [`PASSES`] times; a tree still refusing
/// after that has a writer this cannot outrun, and the panic says so.
fn remove_tree(dir: &Path) {
    for pass in 1..=PASSES {
        match fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty && pass < PASSES => {
                eprintln!("{} gained files while it was removed ({e}); removing again", dir.display());
            }
            Err(e) => panic!("remove {}: {e}, after {pass} pass(es)", dir.display()),
        }
    }
}

/// How many times [`remove_tree`] runs over a tree that keeps refusing.
const PASSES: usize = 10;

/// Whether `git worktree list` still names `at`, compared as real paths:
/// git prints its own realpath, `/private/tmp/…` for `/tmp/…`.
fn registered(root: &Path, at: &Path) -> bool {
    let at = fs::canonicalize(at).unwrap_or_else(|_| at.to_path_buf());
    capture(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|listed| fs::canonicalize(listed).unwrap_or_else(|_| PathBuf::from(listed)) == at)
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

/// Whether git did it, with what it said left on stderr for the reader.
fn ok_loud(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("run git: {e}"))
        .success()
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
    use toyos_tmpdir::TempDir;

    /// `--worktree` owns the rest of the command line, so this refusal is the
    /// only one between `--worktree add --help` and a worktree named `--help`.
    #[test]
    fn a_flag_is_refused_as_a_worktree_path_by_name() {
        let args = ["--worktree", "add", "--help"].map(String::from);
        assert!(
            matches!(crate::flags::check(&args), crate::flags::Outcome::Proceed),
            "the command line has to reach this dispatch"
        );
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

    #[test]
    fn a_local_branch_is_resumed_at_its_own_commit_not_mains() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-local");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/foo", "main"]);
        crate::gitfixture::commit(&work, "on-branch", "branch work\n", "branch work");
        git(&work, &["push", "-q", "-u", "origin", "wt/foo"]);
        crate::gitfixture::commit(&work, "on-branch-2", "more branch work\n", "more branch work");
        git(&work, &["checkout", "-q", "main"]);
        crate::gitfixture::commit(&work, "on-main", "main moved on\n", "main moved on");

        let path = dir.join("resumed");
        let summary = create_worktree(&work, &path, "foo");

        assert!(summary.contains("resumed at"), "{summary}");
        assert!(!summary.contains("main"), "{summary}");
        let branch_sha = capture(&work, &["rev-parse", "wt/foo"]);
        let origin_sha = capture(&work, &["rev-parse", "origin/wt/foo"]);
        let worktree_sha = capture(&path, &["rev-parse", "HEAD"]);
        let main_sha = capture(&work, &["rev-parse", "main"]);
        assert_ne!(branch_sha, origin_sha, "the local tip must be ahead of origin's, or this proves nothing");
        assert_eq!(worktree_sha, branch_sha, "must resume at the local branch's own tip, not origin's");
        assert_ne!(worktree_sha, main_sha, "must not have been reset onto main");
    }

    /// A local branch already merged into `origin/main` is refused by name
    /// rather than resumed from its old tip behind main.
    #[test]
    fn a_landed_local_branch_is_refused_not_resumed() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-landed-local");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/foo", "main"]);
        crate::gitfixture::commit(&work, "on-branch", "branch work\n", "branch work");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["merge", "-q", "--no-ff", "wt/foo", "-m", "merge wt/foo"]);
        git(&work, &["push", "-q", "origin", "main"]);

        let path = dir.join("resumed");
        let refused = std::panic::catch_unwind(|| {
            create_worktree(&work, &path, "foo")
        });

        let panic = refused.expect_err("a landed branch must not be resumed");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("wt/foo"), "{message}");
        assert!(!path.exists(), "nothing must be created before the refusal");
    }

    /// A branch made from `main` and never touched carries no commit beyond
    /// `origin/main` either, and `merge-base --is-ancestor` cannot tell it
    /// apart from one that actually landed — refused before anything is
    /// created, for the reason that is true of it, never "landed".
    #[test]
    fn an_untouched_branch_is_refused_not_resumed() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-untouched");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/foo", "main"]);
        git(&work, &["checkout", "-q", "main"]);

        let path = dir.join("resumed");
        let refused = std::panic::catch_unwind(|| create_worktree(&work, &path, "foo"));

        let panic = refused.expect_err("an untouched branch must not be resumed");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("wt/foo"), "{message}");
        assert!(message.contains("carries no commit beyond origin/main"), "{message}");
        assert!(message.contains("git branch -d wt/foo"), "{message}");
        assert!(!message.contains("landed"), "{message}");
        assert!(!message.contains("merged"), "{message}");
        assert!(!path.exists(), "nothing must be created before the refusal");
    }

    /// A local branch behind its own `origin/wt/<name>` is refused rather than
    /// resumed at the stale local tip.
    #[test]
    fn a_local_branch_behind_origin_is_refused() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-behind");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/foo", "main"]);
        crate::gitfixture::commit(&work, "first", "first\n", "first");
        crate::gitfixture::commit(&work, "second", "second\n", "second");
        git(&work, &["push", "-q", "-u", "origin", "wt/foo"]);
        git(&work, &["reset", "-q", "--hard", "HEAD~1"]);

        let path = dir.join("resumed");
        let refused = std::panic::catch_unwind(|| {
            create_worktree(&work, &path, "foo")
        });

        let panic = refused.expect_err("a local branch behind its origin counterpart must not resume");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("wt/foo"), "{message}");
        assert!(message.contains("origin/wt/foo"), "{message}");
        assert!(message.contains("merge it first"), "{message}");
        assert!(!path.exists(), "nothing must be created before the refusal");
    }

    /// A local branch that has diverged from `origin/wt/<name>` — neither is
    /// an ancestor of the other — is refused rather than resumed silently at
    /// either side.
    #[test]
    fn a_local_branch_diverged_from_origin_is_refused() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-diverged");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/foo", "main"]);
        crate::gitfixture::commit(&work, "side", "origin side\n", "origin side");
        git(&work, &["push", "-q", "-u", "origin", "wt/foo"]);
        git(&work, &["reset", "-q", "--hard", "main"]);
        crate::gitfixture::commit(&work, "side", "local side\n", "local side");

        let path = dir.join("resumed");
        let refused = std::panic::catch_unwind(|| {
            create_worktree(&work, &path, "foo")
        });

        let panic = refused.expect_err("a diverged local branch must not resume");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("wt/foo"), "{message}");
        assert!(message.contains("origin/wt/foo"), "{message}");
        assert!(message.contains("merge it first"), "{message}");
        assert!(!path.exists(), "nothing must be created before the refusal");
    }

    #[test]
    fn an_origin_only_branch_is_recreated_tracking_it() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-origin");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/bar", "main"]);
        crate::gitfixture::commit(&work, "on-branch", "branch work\n", "branch work");
        git(&work, &["push", "-q", "-u", "origin", "wt/bar"]);
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt/bar"]);
        crate::gitfixture::commit(&work, "on-main", "main moved on\n", "main moved on");

        let path = dir.join("resumed");
        let summary = create_worktree(&work, &path, "bar");

        assert!(summary.contains("resumed from origin/wt/bar"), "{summary}");
        let origin_sha = capture(&work, &["rev-parse", "origin/wt/bar"]);
        let worktree_sha = capture(&path, &["rev-parse", "HEAD"]);
        let main_sha = capture(&work, &["rev-parse", "main"]);
        assert_eq!(worktree_sha, origin_sha, "must resume at origin's commit");
        assert_ne!(worktree_sha, main_sha, "must not have been reset onto main");
        let tracked = capture(&work, &["rev-parse", "wt/bar@{upstream}"]);
        assert_eq!(tracked, origin_sha, "the new local branch must track origin/wt/bar");
    }

    /// A branch already merged into `origin/main` and reachable only as a
    /// stale `origin/wt/<name>` (its GitHub head long deleted, this checkout
    /// never fetched to notice) is refused rather than recreated behind main.
    #[test]
    fn a_landed_origin_only_branch_is_refused() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-landed-origin");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        git(&work, &["checkout", "-qb", "wt/bar", "main"]);
        crate::gitfixture::commit(&work, "on-branch", "branch work\n", "branch work");
        git(&work, &["push", "-q", "-u", "origin", "wt/bar"]);
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["merge", "-q", "--ff-only", "wt/bar"]);
        git(&work, &["branch", "-qD", "wt/bar"]);
        git(&work, &["push", "-q", "origin", "main"]);

        let path = dir.join("resumed");
        let refused = std::panic::catch_unwind(|| {
            create_worktree(&work, &path, "bar")
        });

        let panic = refused.expect_err("a landed origin-only branch must not be resumed");
        let message = panic.downcast::<String>().expect("the refusal is formatted");
        assert!(message.contains("origin/wt/bar"), "{message}");
        assert!(!path.exists(), "nothing must be created before the refusal");
    }

    #[test]
    fn with_neither_branch_it_starts_fresh_from_main() {
        let (dir, _origin, work) = crate::gitfixture::repo("wtresume-fresh");
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["branch", "-qD", "wt"]);
        crate::gitfixture::commit(&work, "on-main", "main moved on\n", "main moved on");

        let path = dir.join("resumed");
        let summary = create_worktree(&work, &path, "baz");

        assert!(summary.contains("new, from main"), "{summary}");
        let worktree_sha = capture(&path, &["rev-parse", "HEAD"]);
        let main_sha = capture(&work, &["rev-parse", "main"]);
        assert_eq!(worktree_sha, main_sha);
    }

    /// A linked worktree of a fresh repository whose `.gitignore` names `target/`,
    /// both in the directory that comes first.
    fn linked(name: &str) -> (TempDir, PathBuf, PathBuf) {
        let (dir, _origin, work) = crate::gitfixture::repo(name);
        let tree = dir.join("linked");
        git(&work, &["worktree", "add", "-q", "-b", "linked", tree.to_str().unwrap()]);
        (dir, work, tree)
    }

    /// **git unregisters, then fails to delete, and exits non-zero.** A path
    /// deeper than `PATH_MAX` under the ignored `target/` is a deterministic
    /// way to make it do that.
    #[test]
    fn a_worktree_git_unregistered_but_left_on_disk_is_deleted_whole() {
        let (_dir, work, tree) = linked("wt-remove-leftovers");
        // Two chains of twenty, each short enough to make, one renamed into
        // the other's end: no path any call here names exceeds `PATH_MAX`.
        let chain = |at: &Path| {
            let mut end = at.to_path_buf();
            for _ in 0..20 {
                end.push("a-directory-name-thirty-bytes-");
            }
            fs::create_dir_all(&end).unwrap();
            end
        };
        let target = tree.join("target");
        let lower = chain(&tree.join("lower"));
        fs::write(lower.join("f"), "cache\n").unwrap();
        let upper = chain(&target);
        fs::rename(tree.join("lower"), upper.join("lower")).unwrap();
        let depth = upper.as_os_str().len() + lower.strip_prefix(&tree).unwrap().as_os_str().len();
        assert!(depth > 1024, "the fixture must exceed PATH_MAX to make git fail: {depth}");

        remove(&work, tree.to_str().unwrap());

        assert!(!tree.exists(), "{} is still on disk", tree.display());
        assert!(!registered(&work, &tree), "{} is still a worktree", tree.display());
    }

    /// git's own refusal stands: untracked work keeps the worktree registered,
    /// and nothing of it is deleted.
    #[test]
    fn a_worktree_holding_untracked_work_is_refused_and_left_whole() {
        let (_dir, work, tree) = linked("wt-remove-dirty");
        fs::write(tree.join("unsaved.rs"), "the only copy\n").unwrap();

        let refused = std::panic::catch_unwind(|| remove(&work, tree.to_str().unwrap()));

        assert!(refused.is_err(), "a worktree with untracked work was removed");
        assert!(registered(&work, &tree), "the refusal unregistered it");
        assert_eq!(fs::read_to_string(tree.join("unsaved.rs")).unwrap(), "the only copy\n");
    }
}
