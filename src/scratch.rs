//! The QEMU harness's scratch: one directory per run, `$TMPDIR/toyos-tests-<pid>`,
//! holding every lane's images, boot logs, screendumps and sockets.
//!
//! **A run removes its own when it ends**, unless it ended red: then the
//! directory is kept, marked with [`KEPT`], and its path is the last thing the
//! run prints, because its boot logs are the evidence the red is read from.
//! A kept directory lives at most [`KEEP_FAILED`]; a run killed before it could
//! answer either way leaves one unmarked, and both are [`sweep`]'s, which every
//! run calls before it boots anything.
//!
//! A directory whose pid is still alive is never touched: it is either a run
//! in flight or a reused pid, and the second costs one directory until the pid
//! dies.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The name every run directory starts with; the rest is the run's pid.
pub const PREFIX: &str = "toyos-tests-";

/// The marker a red run leaves in its directory. Its mtime is when the run
/// ended, which is what [`KEEP_FAILED`] is counted from.
pub const KEPT: &str = "KEPT";

/// How long a red run's evidence outlives it.
pub const KEEP_FAILED: Duration = Duration::from_secs(24 * 60 * 60);

/// The directory the run with `pid` writes under `tmp`.
pub fn run_dir(tmp: &Path, pid: u32) -> PathBuf {
    tmp.join(format!("{PREFIX}{pid}"))
}

/// What becomes of one run directory found under `$TMPDIR`.
#[derive(Debug, PartialEq)]
enum Fate {
    /// Its run is alive, or it is a red run's evidence still inside its day.
    Stays,
    Goes,
}

fn fate(alive: bool, kept_at: Option<SystemTime>, now: SystemTime) -> Fate {
    if alive {
        return Fate::Stays;
    }
    match kept_at {
        // A clock that moved backwards makes the age zero: kept, never deleted early.
        Some(at) if now.duration_since(at).unwrap_or(Duration::ZERO) < KEEP_FAILED => Fate::Stays,
        _ => Fate::Goes,
    }
}

/// Remove every run directory under `tmp` whose run is gone and whose evidence,
/// if it kept any, is older than [`KEEP_FAILED`]. Returns what it removed.
pub fn sweep(tmp: &Path, alive: impl Fn(u32) -> bool, now: SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(tmp) else { return Vec::new() };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix(PREFIX)) else { continue };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        let dir = entry.path();
        let kept_at = fs::metadata(dir.join(KEPT)).and_then(|m| m.modified()).ok();
        if fate(alive(pid), kept_at, now) == Fate::Goes {
            fs::remove_dir_all(&dir)
                .unwrap_or_else(|e| panic!("remove the old run directory {}: {e}", dir.display()));
            removed.push(dir);
        }
    }
    removed
}

/// End the run that owns `dir`: remove it when `red` is false, and otherwise mark
/// it kept and return it, for the run to print.
pub fn finish(dir: &Path, red: bool) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    if red {
        fs::write(dir.join(KEPT), "a red run's scratch, kept for its logs\n")
            .unwrap_or_else(|e| panic!("mark {} kept: {e}", dir.display()));
        return Some(dir.to_path_buf());
    }
    fs::remove_dir_all(dir).unwrap_or_else(|e| panic!("remove {}: {e}", dir.display()));
    None
}

/// Whether a process with `pid` exists.
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 runs the existence and permission checks and delivers
    // nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("toyos-scratch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(tmp: &Path, pid: u32) -> PathBuf {
        let dir = run_dir(tmp, pid).join("lane-0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("uart-0.log"), "boot\n").unwrap();
        run_dir(tmp, pid)
    }

    /// **The whole policy over one `$TMPDIR`**: a live run is left alone, a
    /// killed one is swept, a red run's evidence stays its day and goes after
    /// it, and nothing that is not a run directory is touched.
    #[test]
    fn a_sweep_removes_the_dead_and_the_expired_and_nothing_else() {
        let tmp = tmp("sweep");
        let live = run(&tmp, 10);
        let killed = run(&tmp, 11);
        let red_today = run(&tmp, 12);
        let red_old = run(&tmp, 13);
        assert!(finish(&red_today, true).is_some());
        assert!(finish(&red_old, true).is_some());
        let other = tmp.join("toyos-tests-notapid");
        fs::create_dir_all(&other).unwrap();
        let unrelated = tmp.join("toyos-pr-x");
        fs::create_dir_all(&unrelated).unwrap();

        let now = SystemTime::now();
        let removed = sweep(&tmp, |pid| pid == 10, now);
        assert_eq!(removed, [killed], "only the killed run goes today");
        assert!(live.exists() && red_today.exists() && red_old.exists());

        let tomorrow = now + KEEP_FAILED + Duration::from_secs(1);
        let mut removed = sweep(&tmp, |pid| pid == 10, tomorrow);
        removed.sort();
        assert_eq!(removed, [red_today, red_old], "a red run's evidence lasts a day");
        assert!(live.exists(), "a live run's directory was removed");
        assert!(other.exists() && unrelated.exists(), "a sweep reached past its own names");
    }

    /// A green run leaves nothing; a red one leaves its logs and says where.
    #[test]
    fn a_run_removes_its_own_unless_it_ended_red() {
        let tmp = tmp("finish");
        let green = run(&tmp, 20);
        assert_eq!(finish(&green, false), None);
        assert!(!green.exists(), "a green run left its scratch behind");

        let red = run(&tmp, 21);
        assert_eq!(finish(&red, true).as_deref(), Some(red.as_path()));
        assert!(red.join("lane-0/uart-0.log").exists(), "a red run lost its evidence");
        assert!(red.join(KEPT).exists());

        // A run that booted nothing has nothing to remove or keep.
        assert_eq!(finish(&run_dir(&tmp, 22), true), None);
    }

    #[test]
    fn this_process_is_alive_and_an_impossible_pid_is_not() {
        assert!(alive(std::process::id()));
        assert!(!alive(u32::MAX));
    }
}
