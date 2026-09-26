//! A scratch directory that is gone when its holder is, on every way out.
//!
//! [`TempDir::new`] makes a fresh, empty directory under `$TMPDIR`, and its
//! `Drop` removes it: on a return and on a panic's unwind alike. Every test and
//! the QEMU harness take their scratch here and nowhere else, and
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
//! the lock file, because it is in that `$TMPDIR`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

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
        // Swept with this directory already counted, so the root the gone
        // processes' scratch is moved into cannot be removed under the reaping.
        let reap = if state.swept { Vec::new() } else { state.sweep() };
        drop(state);
        for dir in reap {
            fs::remove_dir_all(&dir)
                .unwrap_or_else(|e| panic!("remove a gone process's scratch {}: {e}", dir.display()));
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

/// [`GLOBAL`] under `tmp`, held until the file is dropped. A blocking lock:
/// every holder keeps it for a directory listing and some renames at most.
fn global(tmp: &Path) -> File {
    let path = tmp.join(GLOBAL);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    file.lock().unwrap_or_else(|e| panic!("lock {}: {e}", path.display()));
    file
}
