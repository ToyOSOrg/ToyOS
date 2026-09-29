//! `cargo run -- --sync`: this machine's `main` onto `origin/main`.
//!
//! Nothing here rewrites history and nothing pushes `main`.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn dispatch_sync(root: &Path) {
    match sync(root) {
        Ok(line) => println!("[sync] {line}"),
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(1);
        }
    }
}

/// `git fetch origin`, then this machine's `main` onto `origin/main`.
///
/// **`origin/main` is the truth and the local one is a cache.** Without this
/// the primary's tree — which owns `rust/`, the sysroot and the witness every
/// worktree compares against — silently falls behind whatever GitHub merged.
fn sync(root: &Path) -> Result<String, String> {
    let primary = crate::primary_checkout(root);

    git(root, &["fetch", "--quiet", "origin", "main"])
        .map_err(|e| format!("{e}\n[sync] `git fetch origin main` failed, so nothing below could \
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

    let ahead = git(&primary, &["rev-list", "--count", "origin/main..main"])?;
    if ahead.trim() != "0" {
        return Err(stranded(&primary));
    }
    let behind = git(&primary, &["rev-list", "--count", "main..origin/main"])?;
    if behind.trim() == "0" {
        let at = git(&primary, &["rev-parse", "--short", "main"])?;
        return Ok(format!(
            "fetched origin; this host's main is current at {at}{}",
            reclaimable(root)
        ));
    }
    let said = match fast_forward(&primary)? {
        Some((before, after, commits)) => format!("{before} -> {after} ({commits} commit(s))"),
        None => format!("already at {}", git(&primary, &["rev-parse", "--short", "main"])?),
    };
    Ok(format!("fetched origin; this host's main {said}{}", reclaimable(root)))
}

/// The move this call made, or `None` when a concurrent `--sync` had already
/// made it. A concurrent one holds git's index or ref lock, and git refuses
/// this one.
fn fast_forward(primary: &Path) -> Result<Option<(String, String, String)>, String> {
    let out = git(primary, &["merge", "--ff-only", "origin/main"])?;
    let Some(range) = out.lines().find_map(|l| l.strip_prefix("Updating ")) else {
        return Ok(None);
    };
    let (before, after) = range.split_once("..").unwrap_or_else(|| panic!("git said {range:?}"));
    let commits = git(primary, &["rev-list", "--count", range])?;
    Ok(Some((before.to_string(), after.to_string(), commits)))
}

/// What this host could give back, said where it becomes true.
///
/// A worktree whose branch has landed has no reason to hold its build caches,
/// and `--sync` runs at exactly the moment that becomes true of one.
fn reclaimable(root: &Path) -> String {
    crate::worktree::reclaim_line(&crate::worktree::survey(root, false))
        .map_or_else(String::new, |line| format!("\n[sync] {line}"))
}

/// This host's `main` has commits GitHub does not, so it is not a cache of
/// `origin/main` any more and nothing can fast-forward it.
fn stranded(primary: &Path) -> String {
    let extra = git(primary, &["log", "--oneline", "origin/main..main"]).unwrap_or_else(|e| e);
    let holders = git(primary, &["branch", "--contains", "main", "--list", "wt/*"])
        .unwrap_or_else(|e| e);
    format!(
        "[sync] this host's main carries commits origin/main has not got, so it cannot be \
         fast-forwarded and it is no longer a copy of what GitHub has:\n{extra}\n\
         [sync] branches that already contain all of them:\n{}\n\
         [sync] If one of those holds every commit above, nothing is lost — open a pull request \
         for it and put this host's main back with \
         `git -C {} reset --hard origin/main`. If none does, do not reset anything: work out \
         where those commits live first.",
        if holders.trim().is_empty() { "[sync]     none".to_string() } else { holders },
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
mod tests {
    use super::*;
    use crate::gitfixture::{commit, configure, repo, sh};
    use std::fs;

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
        assert_eq!(
            git(&wt, &["rev-parse", "main"]).unwrap(),
            git(&wt, &["rev-parse", "origin/main"]).unwrap()
        );
    }

    /// A primary that is this checkout skips the dirty question, so its dirt
    /// reaches the fast-forward: git's refusal is reported, not stranded commits.
    #[test]
    fn a_dirty_primary_that_is_this_checkout_gets_gits_refusal() {
        let (dir, origin, wt) = repo("sync-dirty-self");
        sh(&wt, &["switch", "-q", "main"]);
        land_elsewhere(&dir, &origin);
        fs::write(wt.join("h"), "untracked\n").unwrap();

        let refusal = sync(&wt).expect_err("an untracked file in the way must refuse");
        assert!(refusal.contains("untracked working tree files would be overwritten"), "{refusal}");
        assert!(!refusal.contains("carries commits origin/main has not got"), "{refusal}");
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

    /// Commits on `main` with nothing new on GitHub are still stranded.
    #[test]
    fn a_main_ahead_and_not_behind_is_refused_as_stranded() {
        let (_dir, _origin, wt) = repo("sync-ahead-only");
        sh(&wt, &["switch", "-q", "main"]);
        commit(&wt, "g", "local\n", "committed on main");

        let refusal = sync(&wt).expect_err("a main ahead of origin/main is not current");
        assert!(refusal.contains("carries commits origin/main has not got"), "{refusal}");
        assert!(refusal.contains("committed on main"), "{refusal}");
    }

    /// A concurrent `--sync` already made the move: this run says where `main`
    /// is and claims no move.
    #[test]
    fn a_fast_forward_a_concurrent_run_made_is_not_reported_as_this_ones() {
        let (dir, origin, wt) = repo("sync-already");
        sh(&wt, &["switch", "-q", "main"]);
        land_elsewhere(&dir, &origin);
        git(&wt, &["fetch", "--quiet", "origin", "main"]).unwrap();

        let (before, after, commits) =
            fast_forward(&wt).unwrap().expect("the first fast-forward moves main");
        assert_ne!(before, after);
        assert_eq!(commits, "1");
        assert_eq!(fast_forward(&wt).unwrap(), None, "main was already there");
    }

    /// A primary that is not this checkout and has uncommitted work is left alone.
    #[test]
    fn a_dirty_primary_that_is_another_checkout_is_left_where_it_is() {
        let (dir, origin, primary) = repo("sync-dirty-elsewhere");
        sh(&primary, &["switch", "-q", "main"]);
        let asking = dir.join("asking");
        sh(&primary, &["worktree", "add", "-q", "-b", "task", asking.to_str().unwrap()]);
        land_elsewhere(&dir, &origin);
        fs::write(primary.join("f"), "dirty\n").unwrap();
        let before = git(&primary, &["rev-parse", "main"]).unwrap();

        let said = sync(&asking).expect("a dirty primary is reported, never refused");
        assert!(said.contains("has uncommitted work in it"), "{said}");
        assert_eq!(git(&primary, &["rev-parse", "main"]).unwrap(), before);
    }
}
