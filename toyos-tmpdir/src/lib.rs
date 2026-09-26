//! A scratch directory that is gone when its holder is, on every way out.
//!
//! [`TempDir::new`] makes a fresh, empty directory under `$TMPDIR`, and its
//! `Drop` removes it: on a return and on a panic's unwind alike. Every test and
//! the QEMU harness take their scratch here, and
//! `cargo run -- --ci host` runs every host test against a `$TMPDIR` of its own
//! and reds on anything left in it.
//!
//! **A process that dies without unwinding is reclaimed by the next one.**
//! Every directory a process holds lives under its root,
//! `$TMPDIR/toyos-tmp-<pid>-<n>/`, beside an [`OWNER`] file the process holds an
//! exclusive lock on for as long as the root exists. The kernel lets go of that
//! lock when the process dies, by any signal, `SIGKILL` included. The first
//! directory a process makes sweeps `$TMPDIR`: a root whose owner can be locked
//! belongs to a process that is gone, and is removed.
//!
//! **A live process's root is never touched.** Making a root, removing one, and
//! the sweep each hold [`GLOBAL`] exclusively, so no sweep sees a root between
//! its `mkdir` and its owner's lock, or halfway through its removal. Liveness
//! is the lock and never the pid, so a reused pid cannot make a dead root look
//! live or a live one look dead; the pid only keeps two roots' names apart.
//! Every process that shares a `$TMPDIR` — every worktree on the host — shares
//! the lock file, because it is in that `$TMPDIR`. A hold on it past
//! `GLOBAL_PATIENCE` panics naming the pid that holds it, rather than hanging
//! every worktree on the host behind a stopped process.
//!
//! **A directory the sweep cannot remove does not cost any process but this
//! one.** It is moved back out of this process's root under a name no sweep
//! reads as a root, and reported; the next process on the host still makes its
//! first directory.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What every root's name starts with; the rest is `<pid>-<n>`.
pub const ROOT_PREFIX: &str = "toyos-tmp-";

/// The lock every root's making and removal and every sweep holds, in
/// `$TMPDIR`. Never removed: a lock file deleted while another process waits
/// on it would let two holders in at once.
pub const GLOBAL: &str = "toyos-tmp.lock";

/// The file in a root its process holds locked for the root's whole life.
pub const OWNER: &str = "owner";

/// A directory of its own under `$TMPDIR`, removed with everything in it when
/// this is dropped.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A fresh, empty directory whose name starts with `label`, which is one
    /// path component. Its path is resolved: `/private/var/…` on macOS, never
    /// the `/var/…` symlink, so git and a canonicalized comparison agree with it.
    pub fn new(label: &str) -> TempDir {
        assert!(
            !label.is_empty() && !label.contains(['/', '\\']) && label != "." && label != "..",
            "a scratch label is one path component, not {label:?}"
        );
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        if state.root.is_none() {
            let tmp = std::env::temp_dir();
            let tmp = fs::canonicalize(&tmp)
                .unwrap_or_else(|e| panic!("resolve $TMPDIR {}: {e}", tmp.display()));
            let root = Root::make(&tmp, &mut state.roots);
            state.root = Some(root);
        }
        let root = state.root.as_mut().expect("made above");
        let path = root.dir.join(format!("{label}-{}", root.made));
        root.made += 1;
        fs::create_dir(&path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        root.holders += 1;
        let tmp = root.tmp.clone();
        // Swept with this directory already counted, so the root the gone
        // processes' scratch is moved into cannot be removed under the reaping.
        let reap = if state.swept { Vec::new() } else { state.sweep() };
        drop(state);
        for dir in reap {
            if let Err(e) = fs::remove_dir_all(&dir) {
                stuck(&tmp, &dir, e);
            }
        }
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let removed = fs::remove_dir_all(&self.path);
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        let root = state.root.as_mut().expect("a TempDir outlived its root");
        root.holders -= 1;
        let last = if root.holders == 0 { state.root.take() } else { None };
        drop(state);
        if let Err(e) = removed {
            fail(format!("remove {}: {e}", self.path.display()));
        }
        if let Some(root) = last {
            root.remove();
        }
    }
}

/// A panic, unless this thread is already unwinding: a second panic aborts and
/// loses the first one's message, which is the one that explains this.
fn fail(what: String) {
    if std::thread::panicking() {
        eprintln!("toyos-tmpdir: {what}");
    } else {
        panic!("{what}");
    }
}

/// A gone process's directory the sweep moved into this process's own root but
/// could not then remove: reported once, and moved back out to `$TMPDIR` under
/// a name no sweep reads as a root — `ROOT_PREFIX` is not a prefix of it — so
/// this is the only process it ever costs, and no later process on the host
/// inherits it and panics in turn. `tmp` is the shared directory the reap
/// happened under, not `dir`'s own parent, so this never guesses at the layout
/// a future reap chooses.
fn stuck(tmp: &Path, dir: &Path, e: std::io::Error) {
    let name = dir.file_name().expect("a reaped directory has a name");
    let out = tmp.join(format!("stuck-{}", name.to_string_lossy()));
    match fs::rename(dir, &out) {
        Ok(()) => eprintln!(
            "toyos-tmpdir: could not remove a gone process's scratch {}: {e}; left for a human at {}",
            dir.display(),
            out.display()
        ),
        Err(e2) => eprintln!(
            "toyos-tmpdir: could not remove {} ({e}) or move it out of the way ({e2}); left in \
             place, which will keep this process's own root from removing itself too",
            dir.display()
        ),
    }
}

struct State {
    root: Option<Root>,
    /// Roots this process has made, so one made after the last was removed
    /// does not take its name.
    roots: u64,
    /// Whether this process has swept `$TMPDIR`: once is what reclaims every
    /// root a process that died left.
    swept: bool,
}

static STATE: Mutex<State> = Mutex::new(State { root: None, roots: 0, swept: false });

struct Root {
    tmp: PathBuf,
    dir: PathBuf,
    /// Held locked until the root is gone.
    owner: File,
    /// Live [`TempDir`]s under it.
    holders: usize,
    /// Directories made under it, for their names.
    made: u64,
}

impl Root {
    /// A root of this process's under `tmp`, numbered from `roots` on.
    fn make(tmp: &Path, roots: &mut u64) -> Root {
        let _global = global(tmp);
        let dir = loop {
            let dir = tmp.join(format!("{ROOT_PREFIX}{}-{roots}", std::process::id()));
            *roots += 1;
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                // A process that had this pid before this one and died without
                // removing its root: the sweep's, not in the way.
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => panic!("create {}: {e}", dir.display()),
            }
        };
        let owner = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(OWNER))
            .unwrap_or_else(|e| panic!("create {}/{OWNER}: {e}", dir.display()));
        owner.try_lock().unwrap_or_else(|e| panic!("lock {}/{OWNER}: {e}", dir.display()));
        Root { tmp: tmp.to_path_buf(), dir, owner, holders: 0, made: 0 }
    }

    fn remove(self) {
        let global = global(&self.tmp);
        let owner = self.dir.join(OWNER);
        if let Err(e) = fs::remove_file(&owner) {
            fail(format!("remove {}: {e}", owner.display()));
        }
        // Not `remove_dir_all`: a root holds its owner and its live directories
        // and nothing else, so anything here was written past a `TempDir`.
        if let Err(e) = fs::remove_dir(&self.dir) {
            fail(format!(
                "remove {}, which should hold nothing now that its last TempDir is gone: {e}",
                self.dir.display()
            ));
        }
        drop(global);
        drop(self.owner);
    }
}

impl State {
    /// Move every root under `$TMPDIR` whose process is gone into this
    /// process's own root, and return where each went: the caller removes them
    /// holding nothing, and a caller that dies meanwhile leaves them in a root
    /// the next sweep takes.
    fn sweep(&mut self) -> Vec<PathBuf> {
        self.swept = true;
        let root = self.root.as_ref().expect("a sweep runs from a live root");
        let _global = global(&root.tmp);
        let entries =
            fs::read_dir(&root.tmp).unwrap_or_else(|e| panic!("read {}: {e}", root.tmp.display()));
        let mut reap = Vec::new();
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", root.tmp.display()));
            let path = entry.path();
            let is_root = entry.file_name().to_str().is_some_and(|n| n.starts_with(ROOT_PREFIX));
            if !is_root || path == root.dir || !gone(&path) {
                continue;
            }
            let to = root.dir.join(format!("reap-{}", entry.file_name().to_string_lossy()));
            fs::rename(&path, &to)
                .unwrap_or_else(|e| panic!("move {} to {}: {e}", path.display(), to.display()));
            reap.push(to);
        }
        reap
    }
}

/// Whether the root at `dir` belongs to a process that is gone. Asked with
/// [`GLOBAL`] held, so a root without an owner is one whose making or removal
/// was cut short, never one in progress.
fn gone(dir: &Path) -> bool {
    let owner = dir.join(OWNER);
    let file = match File::open(&owner) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return true,
        Err(e) => panic!("open {}: {e}", owner.display()),
    };
    match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => panic!("lock {}: {e}", owner.display()),
    }
}

/// How long [`global`] waits before it gives up on a stuck holder: far past a
/// directory listing and some renames, so nothing legitimate ever meets it.
const GLOBAL_PATIENCE: Duration = Duration::from_secs(15);

/// [`GLOBAL`] under `tmp`, held until the file is dropped. A blocking lock:
/// every holder keeps it for a directory listing and some renames at most.
fn global(tmp: &Path) -> File {
    global_within(tmp, GLOBAL_PATIENCE)
}

/// [`global`], bounded by `patience` rather than the production constant, so a
/// test can make a holder that never lets go look stuck without waiting for it.
fn global_within(tmp: &Path, patience: Duration) -> File {
    let path = tmp.join(GLOBAL);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => panic!("lock {}: {e}", path.display()),
        }
        if start.elapsed() > patience {
            let holder = fs::read_to_string(&path).unwrap_or_default();
            let holder = holder.trim();
            let holder =
                if holder.is_empty() { "an unknown process".to_string() } else { format!("pid {holder}") };
            panic!(
                "{} has been held over {patience:?} by {holder}: every worktree sharing this \
                 $TMPDIR is stuck behind it — find and end that process, or wait for it to move on",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Named while held, so a waiter that times out on the next line can say who
    // it is waiting for instead of just how long it has waited.
    file.set_len(0).unwrap_or_else(|e| panic!("truncate {}: {e}", path.display()));
    write!(file, "{}", std::process::id()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lock held past its patience fails loudly instead of hanging, and
    /// names the pid that is holding it.
    #[test]
    fn a_held_global_lock_times_out_naming_its_holder() {
        let tmp = TempDir::new("global-timeout");
        let path = tmp.join(GLOBAL);
        let mut holder = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        holder.lock().unwrap_or_else(|e| panic!("lock {}: {e}", path.display()));
        write!(holder, "999999999").unwrap();

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            global_within(&tmp, Duration::from_millis(50))
        }))
        .expect_err("a lock held past its patience must not be granted");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(message.contains("999999999"), "the holder's pid is not named: {message}");
        assert!(message.contains("held over"), "{message}");

        drop(holder);
    }
}
