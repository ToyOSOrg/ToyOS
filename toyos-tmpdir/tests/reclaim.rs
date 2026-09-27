//! Every way out of a process, against a `$TMPDIR` of the test's own: the
//! processes whose scratch is judged are this binary run again as a holder,
//! which makes a directory, says where, and holds it until it is told to go.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use toyos_tmpdir::{TempDir, GLOBAL, OWNER, ROOT_PREFIX, SHORT_BASE};

const HOLD: &str = "TOYOS_TMPDIR_TEST_HOLD";
const HOLDING: &str = "holding ";

/// How long a holder has to say where it is, or to be gone once told to go. A
/// liveness bound on a process that does nothing but make one directory.
const PATIENCE: Duration = Duration::from_secs(60);

/// One test at a time spawns holders: a pipe made on macOS is marked
/// close-on-exec only after it exists, so a holder forked meanwhile by a sibling
/// test would keep this one's pipes open and its end would never be read.
static SPAWNING: Mutex<()> = Mutex::new(());

fn spawning() -> MutexGuard<'static, ()> {
    SPAWNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`HOLD`]'s value for a holder of a [`TempDir::new`], and of a [`TempDir::short`].
const IN_TMPDIR: &str = "tmpdir";
const SHORT: &str = "short";

/// The holder: this test, run as a child with [`HOLD`] set; a no-op otherwise.
#[test]
fn holder() {
    let dir = match std::env::var(HOLD) {
        Err(std::env::VarError::NotPresent) => return,
        Ok(hold) if hold == IN_TMPDIR => TempDir::new("held"),
        Ok(hold) if hold == SHORT => TempDir::short("held"),
        hold => panic!("{HOLD}={hold:?} is neither {IN_TMPDIR:?} nor {SHORT:?}"),
    };
    std::fs::write(dir.join("image.img"), b"a guest's disk").unwrap();
    println!("{HOLDING}{}", dir.display());
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

struct Holder {
    child: Child,
    stdin: ChildStdin,
    /// Its stdout's lines; disconnected when it has exited.
    lines: Receiver<String>,
    /// Its directory, in its root.
    dir: PathBuf,
}

impl Holder {
    /// A holder under `tmp`, returned once its directory exists, and so once its
    /// sweep of `tmp` is done.
    fn start(tmp: &Path) -> Holder {
        Self::spawn(IN_TMPDIR, tmp)
    }

    /// A holder of a [`TempDir::short`], run with `$TMPDIR` at `tmp`, returned
    /// once its sweep of [`SHORT_BASE`] is done.
    fn short(tmp: &Path) -> Holder {
        Self::spawn(SHORT, tmp)
    }

    fn spawn(hold: &str, tmp: &Path) -> Holder {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "holder", "--nocapture", "--test-threads", "1"])
            .env(HOLD, hold)
            .env("TMPDIR", tmp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run the holder");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    return;
                }
            }
        });
        let dir = loop {
            let line = lines.recv_timeout(PATIENCE).unwrap_or_else(|e| {
                panic!("the holder did not say where its directory is within {PATIENCE:?}: {e}")
            });
            // libtest puts `test holder ... ` before it on the same line.
            if let Some((_, dir)) = line.split_once(HOLDING) {
                break PathBuf::from(dir);
            }
        };
        assert!(dir.join("image.img").exists(), "{} holds nothing", dir.display());
        Holder { child, stdin, lines, dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.parent().unwrap().to_path_buf()
    }

    /// Let it return, and wait for it: its stdout's end is its exit.
    fn finish(mut self) {
        self.stdin.write_all(b"go\n").unwrap();
        loop {
            match self.lines.recv_timeout(PATIENCE) {
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the holder was still there {PATIENCE:?} after it was told to go"),
            }
        }
        let status = self.child.wait().unwrap();
        assert!(status.success(), "the holder exited {status}");
    }

    /// `SIGKILL`: nothing in it runs again.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

fn entries(tmp: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// **The whole contract, over a `$TMPDIR` and over `/tmp`**: a killed process's
/// scratch is taken by the next process's first directory, a live process's is
/// left alone however many processes sweep past it, and a process that returns
/// leaves nothing but the lock.
#[test]
fn a_killed_process_is_reclaimed_and_a_live_one_is_never_touched() {
    let _spawning = spawning();
    let tmp = TempDir::new("reclaim");
    reclaimed(|| Holder::start(&tmp));
    assert_eq!(entries(&tmp), [GLOBAL], "a returned process left something behind");
    reclaimed(|| Holder::short(&tmp));
}

fn reclaimed(start: impl Fn() -> Holder) {
    let killed = start();
    let killed_root = killed.root();
    killed.kill();
    assert!(killed_root.exists(), "the premise: SIGKILL leaves the root");

    let live = start();
    assert!(!killed_root.exists(), "a killed process's root outlived the next process's sweep");
    let past = start();
    assert!(live.dir.join("image.img").exists(), "a sweep took a live process's directory");

    let (live_root, past_root) = (live.root(), past.root());
    live.finish();
    past.finish();
    assert!(!live_root.exists() && !past_root.exists(), "a process that returned left its root");
}

/// A short directory is under `/tmp` however deep `$TMPDIR` is, so a socket's
/// path in it fits Darwin's `sun_path`, the shorter of the hosts'.
#[test]
fn a_socket_in_a_short_directory_fits_whatever_tmpdir_is() {
    const DARWIN_SUN_PATH: usize = 104;
    let _spawning = spawning();
    let tmp = TempDir::new("deep");
    let deep = tmp.join("d".repeat(DARWIN_SUN_PATH));
    std::fs::create_dir(&deep).unwrap();
    let holder = Holder::short(&deep);
    let socket = holder.dir.join("tap-out.sock");
    holder.finish();
    let base = std::fs::canonicalize(SHORT_BASE).unwrap();
    assert!(socket.starts_with(&base), "{} is not under {}", socket.display(), base.display());
    assert!(socket.as_os_str().len() < DARWIN_SUN_PATH, "{} outgrows sun_path", socket.display());
}

/// A root whose making or removal was cut short, with no owner file, is a gone
/// process's too; nothing that is not a root is the sweep's.
#[test]
fn a_root_without_an_owner_goes_and_nothing_else_is_touched() {
    let _spawning = spawning();
    let tmp = TempDir::new("ownerless");
    let cut = tmp.join(format!("{ROOT_PREFIX}1-0"));
    std::fs::create_dir(&cut).unwrap();
    std::fs::write(cut.join("half"), b"").unwrap();
    let other = tmp.join("toyos-tests-1");
    std::fs::create_dir(&other).unwrap();

    Holder::start(&tmp).finish();
    assert!(!cut.exists(), "an ownerless root survived a sweep");
    assert!(other.exists(), "a sweep took a directory that is not a root");
    assert!(!cut.join(OWNER).exists());
}

/// Drop is the removal, on a return and on an unwind.
#[test]
fn a_directory_goes_on_a_return_and_on_a_panic() {
    let returned = {
        let dir = TempDir::new("returned");
        std::fs::write(dir.join("f"), b"x").unwrap();
        dir.to_path_buf()
    };
    assert!(!returned.exists(), "{} outlived its TempDir", returned.display());

    let mut unwound = None;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = TempDir::new("unwound");
        unwound = Some(dir.to_path_buf());
        panic!("a test failing with its scratch held");
    }));
    assert!(caught.is_err());
    let unwound = unwound.unwrap();
    assert!(!unwound.exists(), "{} outlived an unwind", unwound.display());
}

/// A deterministic control on `Root::make`'s `let _global = global(tmp);`:
/// deleting that line lets a holder's first directory appear while this test
/// still holds `GLOBAL`, which turns the first assertion here red.
#[test]
fn making_a_root_waits_for_global() {
    let _spawning = spawning();
    let tmp = TempDir::new("global-gate-make");
    let lock_path = tmp.join(GLOBAL);
    let held = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| panic!("open {}: {e}", lock_path.display()));
    held.lock().unwrap_or_else(|e| panic!("lock {}: {e}", lock_path.display()));

    let tmp_path = tmp.to_path_buf();
    let handle = std::thread::spawn(move || Holder::start(&tmp_path));
    std::thread::sleep(Duration::from_millis(500));
    let before: Vec<String> =
        entries(&tmp).into_iter().filter(|n| n.starts_with(ROOT_PREFIX)).collect();
    assert!(before.is_empty(), "a root was made while GLOBAL was held: {before:?}");
    assert!(!handle.is_finished(), "the holder made its directory without GLOBAL");

    drop(held);
    let holder = handle.join().expect("the holder thread panicked");
    holder.finish();
}

/// A deterministic control on `Root::remove`'s `let global = global(&self.tmp);`:
/// deleting that line lets a returning holder's root disappear while this test
/// still holds `GLOBAL`, which turns the first assertion here red.
#[test]
fn removing_a_root_waits_for_global() {
    let _spawning = spawning();
    let tmp = TempDir::new("global-gate-remove");
    let holder = Holder::start(&tmp);
    let root = holder.root();

    let lock_path = tmp.join(GLOBAL);
    let held = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| panic!("open {}: {e}", lock_path.display()));
    held.lock().unwrap_or_else(|e| panic!("lock {}: {e}", lock_path.display()));

    let handle = std::thread::spawn(move || holder.finish());
    std::thread::sleep(Duration::from_millis(500));
    assert!(root.exists(), "a root was removed while GLOBAL was held: {}", root.display());
    assert!(!handle.is_finished(), "the holder's exit finished without GLOBAL");

    drop(held);
    handle.join().expect("the holder's exit panicked");
    assert!(!root.exists(), "{} survived once GLOBAL was released", root.display());
}

/// A dead root the sweep cannot fully remove — its owner file sits in a
/// directory with no write permission, so unlinking it fails — is reported and
/// moved aside rather than panicking the process that met it, and the next
/// process still gets its own directory.
///
/// The dead root itself keeps ordinary permissions, so the sweep's own
/// cross-directory rename (which needs to rewrite the moved directory's `..`)
/// still succeeds; what blocks `remove_dir_all` is a directory *inside* it with
/// no write permission, so unlinking the file below it fails.
#[cfg(unix)]
#[test]
fn a_directory_the_sweep_cannot_remove_is_reported_and_the_next_process_still_works() {
    use std::os::unix::fs::PermissionsExt;

    let _spawning = spawning();
    let tmp = TempDir::new("stuck");
    let dead = tmp.join(format!("{ROOT_PREFIX}999999-0"));
    let locked = dead.join("locked");
    std::fs::create_dir(&dead).unwrap();
    std::fs::write(dead.join(OWNER), b"").unwrap();
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("f"), b"").unwrap();
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&locked, perms).unwrap();

    // The dead root's owner is unlocked, so the holder's first directory
    // sweeps it; it cannot remove what it cannot unlink, and must report and
    // move on rather than panic — the holder still says where its own
    // directory is and exits clean.
    Holder::start(&tmp).finish();

    let stuck: Vec<String> = entries(&tmp).into_iter().filter(|n| n.starts_with("stuck-")).collect();
    assert_eq!(stuck.len(), 1, "the unremovable root was not reported and moved aside: {:?}", entries(&tmp));
    assert!(!dead.exists(), "the dead root was left where the sweep found it");

    // Restore permissions so this test's own `tmp` can remove itself.
    let stuck_locked = tmp.join(&stuck[0]).join("locked");
    let mut perms = std::fs::metadata(&stuck_locked).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stuck_locked, perms).unwrap();
}
