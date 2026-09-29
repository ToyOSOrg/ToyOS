//! Git repositories for tests: a bare origin, a clone of it, and commits in either.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use toyos_tmpdir::TempDir;

pub(crate) fn repo(name: &str) -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new(&format!("repo-{name}"));
    let origin = dir.join("origin.git");
    let work = dir.join("work");
    let seed = dir.join("seed");
    fs::create_dir(&seed).unwrap();
    sh(&seed, &["init", "-q", "-b", "main"]);
    configure(&seed);
    fs::write(seed.join("f"), "base\n").unwrap();
    fs::write(seed.join(".gitignore"), "target/\n").unwrap();
    sh(&seed, &["add", "f", ".gitignore"]);
    sh(&seed, &["commit", "-qm", "base"]);
    sh(&seed, &["clone", "-q", "--bare", ".", origin.to_str().unwrap()]);
    // A push runs auto maintenance in the receiving repository.
    sh(&origin, &["config", "maintenance.auto", "false"]);

    sh(&dir, &["clone", "-q", origin.to_str().unwrap(), work.to_str().unwrap()]);
    configure(&work);
    sh(&work, &["switch", "-q", "-c", "wt"]);
    (dir, origin, work)
}

/// An identity, and no signing: the host's global config signs every commit,
/// and a test that waited on gpg would be a test that hangs. No auto
/// maintenance: git runs it detached, so a repack started by the last
/// command still writes into the repository while its `TempDir` is removed.
/// **Every git repository a test anywhere in this crate creates sets
/// `maintenance.auto` false** — call this on one made by `init` or `clone`;
/// a fixture that passes `-c` on every invocation instead of persisting
/// config, because it runs against a repository it does not itself `init`
/// or `clone` (a submodule's own store), adds [`NO_AUTO_MAINTENANCE`] to
/// that same list instead.
pub(crate) fn configure(dir: &Path) {
    sh(dir, &["config", "user.email", "t@t"]);
    sh(dir, &["config", "user.name", "t"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
    sh(dir, &["config", "maintenance.auto", "false"]);
}

/// The `-c` form of [`configure`]'s `maintenance.auto false`, for a
/// fixture whose `git` helper already passes `-c` on every invocation.
pub(crate) const NO_AUTO_MAINTENANCE: [&str; 2] = ["-c", "maintenance.auto=false"];

pub(crate) fn sh(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git")
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

pub(crate) fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
    if let Some(parent) = Path::new(file).parent() {
        fs::create_dir_all(dir.join(parent)).unwrap();
    }
    fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-qm", msg]);
}
