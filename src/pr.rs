//! `cargo run -- --pr` and `cargo run -- --sync` — the landing protocol.
//!
//! Nothing here rewrites history and nothing pushes `main`.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn dispatch_pr(root: &Path) {
    report(push(root));
}

pub fn dispatch_sync(root: &Path) {
    report(sync(root).map(|line| format!("[sync] {line}")));
}

fn report(outcome: Result<String, String>) {
    match outcome {
        Ok(text) => println!("{text}"),
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(1);
        }
    }
}

/// The refusals the merge queue cannot make, then the push, then the `gh` line.
fn push(root: &Path) -> Result<String, String> {
    let branch = preflight(root)?;
    // Asked before the push: a second later the answer is the wrong one for ever.
    let first_push = git(root, &["ls-remote", "--heads", "origin", &branch])?.trim().is_empty();
    git(root, &["push", "-u", "origin", &branch])?;
    Ok(if first_push {
        format!(
            "[pr] pushed {branch}; CI runs on a pull request and nothing else, so open its draft \
             now:\n\
             [pr]   gh pr create --draft --base main --head {branch} --title \"{branch}: in \
             progress\" --body \"opened early; CI on every push\""
        )
    } else {
        format!(
            "[pr] pushed {branch}; when it is finished (never `--fill`: the title and body become \
             the merge commit's):\n\
             [pr]   gh pr edit {branch} --title \"<what landed>\" --body-file <file> && gh pr \
             ready {branch}"
        )
    })
}

/// The refusals that do not need the network.
fn preflight(root: &Path) -> Result<String, String> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "main" {
        return Err("[pr] this worktree is on main, so there is nothing to open a pull request \
                    for. `cargo run -- --worktree add <path>` makes one to work in."
            .to_string());
    }
    let dirty = git(root, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        return Err(format!(
            "[pr] this worktree has uncommitted work, and CI would gate a tree main is not going \
             to get:\n{dirty}\n\
             [pr] commit it — on your own branch that is free — then re-run \
             `cargo run -- --pr`."
        ));
    }
    Ok(branch)
}

/// `git fetch origin`, then this machine's `main` onto `origin/main`.
///
/// **`origin/main` is the truth and the local one is a cache.** Without this
/// the primary's tree — which owns `rust/`, the sysroot and the witness every
/// worktree compares against — silently falls behind whatever GitHub merged.
///
/// It is housekeeping and not a gate, so a primary that is dirty or on another
/// branch is *reported*, not refused.
fn sync(root: &Path) -> Result<String, String> {
    let primary = crate::primary_checkout(root);

    git(root, &["fetch", "--quiet", "origin", "main"])
        .map_err(|e| format!("{e}\n[pr] `git fetch origin main` failed, so nothing below could \
                              be judged against what GitHub has."))?;

    // The fast-forward runs on whatever branch the primary has out, so the
    // question is about that checkout and not about which checkout is asking.
    let on = git(&primary, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if on != "main" {
        return Ok(format!(
            "fetched origin; {} is on {on}, so this host's main was left where it is",
            primary.display()
        ));
    }

    if canonical(root) != canonical(&primary) {
        let dirty = git(&primary, &["status", "--porcelain"])?;
        if !dirty.is_empty() {
            return Ok(format!(
                "fetched origin; {} has uncommitted work in it, so this host's main was left \
                 where it is",
                primary.display()
            ));
        }
    }

    let before = git(&primary, &["rev-parse", "--short", "main"])?;
    let behind = git(&primary, &["rev-list", "--count", "main..origin/main"])?;
    if behind.trim() == "0" {
        return Ok(format!(
            "fetched origin; this host's main is current at {before}{}",
            reclaimable(root)
        ));
    }
    let ahead = git(&primary, &["rev-list", "--count", "origin/main..main"])?;
    if ahead.trim() != "0" {
        return Err(stranded(&primary));
    }
    // A concurrent `--sync` holds git's index or ref lock, and git refuses this.
    git(&primary, &["merge", "--ff-only", "origin/main"])?;
    let after = git(&primary, &["rev-parse", "--short", "main"])?;
    Ok(format!(
        "fetched origin; this host's main {before} -> {after} ({} commit(s)){}",
        behind.trim(),
        reclaimable(root),
    ))
}

/// What this host could give back, said where it becomes true.
///
/// A worktree whose branch has landed has no reason to hold its build caches,
/// and `--sync` runs at exactly the moment that becomes true of one.
fn reclaimable(root: &Path) -> String {
    crate::worktree::reclaim_line(&crate::worktree::survey(root, false))
        .map_or_else(String::new, |line| format!("\n[pr] {line}"))
}

/// This host's `main` has commits GitHub does not, so it is not a cache of
/// `origin/main` any more and nothing can fast-forward it.
fn stranded(primary: &Path) -> String {
    let extra = git(primary, &["log", "--oneline", "origin/main..main"]).unwrap_or_else(|e| e);
    let holders = git(primary, &["branch", "--contains", "main", "--list", "wt/*"])
        .unwrap_or_else(|e| e);
    format!(
        "[pr] this host's main carries commits origin/main has not got, so it cannot be \
         fast-forwarded and it is no longer a copy of what GitHub has:\n{extra}\n\
         [pr] branches that already contain all of them:\n{}\n\
         [pr] If one of those holds every commit above, nothing is lost — open a pull request \
         for it and put this host's main back with \
         `git -C {} reset --hard origin/main`. If none does, do not reset anything: work out \
         where those commits live first.",
        if holders.trim().is_empty() { "[pr]     none".to_string() } else { holders },
        primary.display(),
    )
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalise {}: {e}", path.display()))
}

/// `Err` carries what git printed, both streams, because a refusal that hides
/// git's own message makes the agent run the command again by hand to see it.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run git in {}: {e}", dir.display()));
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    Err(format!("git {} (in {})\n{stdout}\n{stderr}", args.join(" "), dir.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use toyos_tmpdir::TempDir;

    /// A bare "origin" with a `main`, and a clone of it on a branch — the only
    /// shape `--pr` runs in. Every repository is [`configure`]d. `sdkversion`'s
    /// tests stage in it too. All of it is in the directory that comes first,
    /// which is the caller's to hold.
    pub(crate) fn repo(name: &str) -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new(&format!("pr-{name}"));
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

    /// Someone else lands, so `origin/main` is ahead of every clone.
    fn land_elsewhere(dir: &Path, origin: &Path) {
        let theirs = dir.join("theirs");
        sh(dir, &["clone", "-q", origin.to_str().unwrap(), theirs.to_str().unwrap()]);
        configure(&theirs);
        commit(&theirs, "h", "theirs\n", "meanwhile");
        sh(&theirs, &["push", "-q", "origin", "main"]);
    }

    /// A fixture clone is its own primary, the shape the branch question used
    /// to be skipped for: the fast-forward ran on whatever branch was out, git
    /// refused it, and `sync` reported lost commits about an ancestor.
    #[test]
    fn a_primary_on_a_branch_is_left_where_it_is() {
        let (dir, origin, wt) = repo("sync-on-a-branch");
        commit(&wt, "g", "mine\n", "work");
        land_elsewhere(&dir, &origin);

        let said = sync(&wt).expect("a primary on a branch is reported, never refused");
        assert!(said.contains("is on wt, so this host's main was left where it is"), "{said}");
        assert!(!said.contains("carries commits origin/main has not got"), "{said}");
        // Left where it is, and main still strictly behind.
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), "wt");
        assert_eq!(git(&wt, &["rev-list", "--count", "main..origin/main"]).unwrap(), "1");
    }

    /// `--sync` takes no lock of its own: a second fast-forward racing the
    /// first is git's to refuse, and the refusal is git's words, never the
    /// stranded-commits story about a `main` that is merely behind.
    #[test]
    fn a_fast_forward_git_refuses_is_reported_as_git_said_it() {
        let (dir, origin, wt) = repo("sync-racing");
        sh(&wt, &["switch", "-q", "main"]);
        land_elsewhere(&dir, &origin);
        let before = git(&wt, &["rev-parse", "main"]).unwrap();

        let lock = wt.join(".git/index.lock");
        fs::write(&lock, "").unwrap();
        let refusal = sync(&wt).expect_err("a held index lock must refuse the fast-forward");
        fs::remove_file(&lock).unwrap();
        assert!(refusal.contains("index.lock"), "{refusal}");
        assert!(!refusal.contains("carries commits origin/main has not got"), "{refusal}");
        assert_eq!(git(&wt, &["rev-parse", "main"]).unwrap(), before, "main moved under a lock");

        let said = sync(&wt).expect("with the lock gone the fast-forward runs");
        assert!(said.contains("(1 commit(s))"), "{said}");
        assert_eq!(git(&wt, &["rev-parse", "main"]).unwrap(), git(&wt, &["rev-parse", "origin/main"]).unwrap());
    }

    /// `main` ahead of `origin/main` is refused with what is stranded, before
    /// any fast-forward is tried.
    #[test]
    fn a_main_github_has_not_got_is_refused_as_stranded() {
        let (dir, origin, wt) = repo("sync-stranded");
        sh(&wt, &["switch", "-q", "main"]);
        commit(&wt, "g", "local\n", "committed on main");
        land_elsewhere(&dir, &origin);

        let refusal = sync(&wt).expect_err("a main with commits of its own cannot fast-forward");
        assert!(refusal.contains("carries commits origin/main has not got"), "{refusal}");
        assert!(refusal.contains("committed on main"), "{refusal}");
    }

    /// Each refusal pushes nothing: the remote never learns of the branch.
    #[test]
    fn a_dirty_worktree_and_main_itself_are_refused_by_name() {
        let (_dir, _origin, wt) = repo("dirty");
        commit(&wt, "g", "mine\n", "work");
        fs::write(wt.join("g"), "not committed\n").unwrap();
        assert!(push(&wt).expect_err("uncommitted work must refuse").contains("uncommitted"));
        assert!(git(&wt, &["ls-remote", "--heads", "origin", "wt"]).unwrap().is_empty());

        sh(&wt, &["checkout", "-q", "--", "g"]);
        sh(&wt, &["switch", "-q", "main"]);
        assert!(push(&wt).expect_err("main is not a branch to land").contains("on main"));
    }

    /// **The draft has to be the answer on the push that creates the branch**,
    /// because that is the only moment an agent is reading for what to do next
    /// and CI runs on a pull request and on nothing else.
    #[test]
    fn the_first_push_is_told_to_open_a_draft_and_later_ones_are_not() {
        let (_dir, _origin, wt) = repo("first-push");
        commit(&wt, "g", "mine\n", "work");

        let first = push(&wt).expect("the first --pr should push and print");
        assert!(first.contains("gh pr create --draft"), "{first}");
        assert!(!first.contains("--fill"), "{first}");
        let head = git(&wt, &["rev-parse", "HEAD"]).unwrap();
        let pushed = git(&wt, &["ls-remote", "--heads", "origin", "wt"]).unwrap();
        assert!(pushed.starts_with(&head), "{pushed}");

        commit(&wt, "g2", "more\n", "more work");
        let later = push(&wt).expect("a later --pr should push and print");
        assert!(later.contains("gh pr ready"), "{later}");
        assert!(!later.contains("gh pr create"), "{later}");
        let head = git(&wt, &["rev-parse", "HEAD"]).unwrap();
        let pushed = git(&wt, &["ls-remote", "--heads", "origin", "wt"]).unwrap();
        assert!(pushed.starts_with(&head), "{pushed}");
    }
}
