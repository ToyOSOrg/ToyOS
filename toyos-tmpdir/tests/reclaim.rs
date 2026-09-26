//! Every way out of a process, against a `$TMPDIR` of the test's own: the
//! processes whose scratch is judged are this binary run again as a holder,
//! which makes a directory, says where, and holds it until it is told to go.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use toyos_tmpdir::{TempDir, GLOBAL, OWNER, ROOT_PREFIX};

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

/// The holder: this test, run as a child with [`HOLD`] set; a no-op otherwise.
#[test]
fn holder() {
    if std::env::var_os(HOLD).is_none() {
        return;
    }
    let dir = TempDir::new("held");
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
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "holder", "--nocapture", "--test-threads", "1"])
            .env(HOLD, "1")
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

/// **The whole contract over one `$TMPDIR`**: a killed process's scratch is
/// taken by the next process's first directory, a live process's is left alone
/// however many processes sweep past it, and a process that returns leaves
/// nothing but the lock.
#[test]
fn a_killed_process_is_reclaimed_and_a_live_one_is_never_touched() {
    let _spawning = spawning();
    let tmp = TempDir::new("reclaim");

    let killed = Holder::start(&tmp);
    let killed_root = killed.root();
    killed.kill();
    assert!(killed_root.exists(), "the premise: SIGKILL leaves the root");

    let live = Holder::start(&tmp);
    assert!(!killed_root.exists(), "a killed process's root outlived the next process's sweep");
    let past = Holder::start(&tmp);
    assert!(live.dir.join("image.img").exists(), "a sweep took a live process's directory");

    let (live_root, past_root) = (live.root(), past.root());
    live.finish();
    past.finish();
    assert!(!live_root.exists() && !past_root.exists(), "a process that returned left its root");
    assert_eq!(entries(&tmp), [GLOBAL], "a returned process left something behind");
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
