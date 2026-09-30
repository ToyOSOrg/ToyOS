//! A lock is `flock(2)` on an open directory. The kernel releases it
//! when the holder's descriptor closes, so a killed holder strands nothing, and
//! the descriptor is close-on-exec, so no child a holder spawns keeps it held.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How often a lasting wait says again what it waits for: an agent that sees
/// silence kills the wait and retries.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// A held lock; dropping it releases it.
pub struct Lock {
    file: File,
}

impl Lock {
    /// `dir` shared: any number of holders at once, and no exclusive one.
    pub fn shared(dir: &Path, what: &str) -> Self {
        Self::there(dir, libc::LOCK_SH, what).unwrap_or_else(|| panic!("lock {}: it is not there", dir.display()))
    }

    /// `dir` exclusively.
    pub fn exclusive(dir: &Path, what: &str) -> Self {
        Self::there(dir, libc::LOCK_EX, what).unwrap_or_else(|| panic!("lock {}: it is not there", dir.display()))
    }

    /// `dir` shared, or `None` if there is no `dir`.
    pub(crate) fn shared_if_there(dir: &Path, what: &str) -> Option<Self> {
        Self::there(dir, libc::LOCK_SH, what)
    }

    /// `dir` exclusively if nobody holds it, without waiting; `None` if somebody
    /// does or there is no `dir`.
    pub(crate) fn try_exclusive(dir: &Path) -> Option<Self> {
        let file = open(dir)?;
        attempt(&file, libc::LOCK_EX | libc::LOCK_NB).then_some(Self { file })
    }

    /// Run `f` holding this lock exclusively, and hold it shared again after.
    /// The conversion is not atomic, so `f` decides afresh what to do.
    pub fn exclusively<R>(&mut self, what: &str, f: impl FnOnce() -> R) -> R {
        take(&self.file, libc::LOCK_EX, what);
        let out = f();
        take(&self.file, libc::LOCK_SH, what);
        out
    }

    /// The directory this holds, open.
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    fn there(dir: &Path, op: i32, what: &str) -> Option<Self> {
        let file = open(dir)?;
        take(&file, op, what);
        Some(Self { file })
    }
}

fn open(dir: &Path) -> Option<File> {
    match File::open(dir) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => panic!("open {}: {e}", dir.display()),
    }
}

/// Take `op` on `file`, saying so when it has to wait and every [`HEARTBEAT`]
/// while it does.
fn take(file: &File, op: i32, what: &str) {
    if attempt(file, op | libc::LOCK_NB) {
        return;
    }
    eprintln!("[lock] waiting: {what}");
    let (tx, rx) = channel::<()>();
    let heartbeat = {
        let what = what.to_string();
        std::thread::spawn(move || {
            let began = Instant::now();
            while rx.recv_timeout(HEARTBEAT) == Err(RecvTimeoutError::Timeout) {
                eprintln!("[lock] still waiting, {:.0?} so far: {what}", began.elapsed());
            }
        })
    };
    assert!(attempt(file, op), "a blocking flock returned without the lock");
    drop(tx);
    heartbeat.join().expect("the lock heartbeat panicked");
}

/// Whether `flock(op)` took the lock; `false` only for a non-blocking `op`
/// somebody else's lock refused.
fn attempt(file: &File, op: i32) -> bool {
    loop {
        // SAFETY: `file` owns the descriptor for the length of the call.
        if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            ErrorKind::Interrupted => continue,
            ErrorKind::WouldBlock => return false,
            _ => panic!("flock: {err}"),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::{Child, Command};
    use toyos_tmpdir::TempDir;

    const MARKS: &str = "TOYOS_DIRLOCK_TEST_MARKS";

    /// This test binary, to run the one `#[ignore]`d test `test` names.
    pub(crate) fn rerun(test: &str) -> Command {
        let mut rerun = Command::new(std::env::current_exe().unwrap());
        rerun.args(["--exact", test, "--include-ignored", "--nocapture"]);
        rerun
    }

    /// A process of its own running the `#[ignore]`d test `role` names, which
    /// has reached [`held_until_killed`].
    ///
    /// Another process because a lock is per open file description and a test
    /// thread's spawn copies every descriptor open at that moment until its exec:
    /// what a test asserts about a lock held elsewhere, it asserts of a process.
    pub(crate) struct Elsewhere {
        child: Child,
        _marks: TempDir,
    }

    impl Elsewhere {
        pub(crate) fn hold(role: &str, env: &[(&str, &OsStr)]) -> Self {
            let marks = TempDir::new("dirlock-elsewhere");
            let mut child = rerun(role).envs(env.iter().copied()).env(MARKS, &marks).spawn().expect("spawn the holder");
            let deadline = Instant::now() + Duration::from_secs(20);
            while !marks.join("held").exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("{role} exited before it held anything: {status}");
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    panic!("{role} held nothing in 20 s, killed: {}", child.wait().unwrap());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Self { child, _marks: marks }
        }

        /// SIGKILL it, and return once it is reaped.
        pub(crate) fn kill(mut self) {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }

    impl Drop for Elsewhere {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// `file`, a directory opened earlier, exclusively if nobody holds it: a
    /// lock granted after whatever named it may have moved on.
    pub(crate) fn try_exclusive_opened(file: File) -> Option<Lock> {
        attempt(&file, libc::LOCK_EX | libc::LOCK_NB).then_some(Lock { file })
    }

    /// The holder's half of [`Elsewhere`]: say it holds, and hold until it is
    /// killed, or fail loudly after a minute.
    pub(crate) fn held_until_killed() {
        let marks = PathBuf::from(std::env::var(MARKS).expect("a holder runs under Elsewhere::hold"));
        std::fs::write(marks.join("held"), b"").unwrap();
        std::thread::sleep(Duration::from_secs(60));
        panic!("the holder was never killed");
    }

    const DIR: &str = "TOYOS_DIRLOCK_TEST_DIR";

    #[test]
    #[ignore = "the holder for the tests below; never runs on its own"]
    fn hold_exclusive() {
        let dir = PathBuf::from(std::env::var(DIR).unwrap_or_else(|_| panic!("hold_exclusive ran without {DIR}; it is not a test")));
        let _held = Lock::exclusive(&dir, "the test holder");
        held_until_killed();
    }

    /// **An exclusive holder excludes, and a killed one strands nothing.**
    #[test]
    fn an_exclusive_lock_excludes_until_its_holder_dies() {
        let dir = TempDir::new("dirlock");
        let holder = Elsewhere::hold("dirlock::tests::hold_exclusive", &[(DIR, dir.as_os_str())]);
        assert!(Lock::try_exclusive(&dir).is_none(), "two exclusive holders at once");
        holder.kill();
        assert!(Lock::try_exclusive(&dir).is_some(), "a SIGKILLed holder stranded the lock");
    }

    /// **Shared admits shared and refuses exclusive**, and a conversion
    /// excludes the other shared holders while it lasts.
    #[test]
    fn shared_admits_shared_and_refuses_exclusive() {
        let dir = TempDir::new("dirlock-shared");
        let shared_now = || attempt(&open(&dir).unwrap(), libc::LOCK_SH | libc::LOCK_NB);
        let mut mine = Lock::shared(&dir, "one");
        let theirs = Lock::shared(&dir, "two");
        assert!(Lock::try_exclusive(&dir).is_none(), "an exclusive holder got in beside shared ones");
        drop(theirs);
        mine.exclusively("clean", || assert!(!shared_now(), "a shared holder got in beside an exclusive one"));
        assert!(shared_now(), "the conversion back left the lock exclusive");
        assert!(Lock::shared_if_there(&dir.join("absent"), "absent").is_none());
    }
}
