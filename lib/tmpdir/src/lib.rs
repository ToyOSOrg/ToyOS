//! A scratch directory that is gone when its holder is, on every way out.
//!
//! [`TempDir::new`] makes a fresh, empty directory under `$TMPDIR`, and its
//! `Drop` removes it: on a return and on a panic's unwind alike. Every test and
//! the QEMU harness take their scratch here, and
//! `cargo run -- --ci host` runs every host test against a `$TMPDIR` of its own
//! and reds on anything left in it.
//!
//! [`TempDir::short`] is the same directory under [`SHORT_BASE`] instead, for
//! a Unix socket: `sockaddr_un.sun_path` is 104 bytes on Darwin, and `$TMPDIR`'s
//! depth is the host's to choose. Everything below holds of each base apart.
//!
//! **A base may be shared by every user on the host; nothing in it is.** A
//! process works only in its user's [`private`] directory of the base,
//! `<base>/toyos-<euid>/`, mode 0700 and owned by that user, and refuses one
//! that is not. In a sticky base such as `/tmp` no other user can replace it,
//! so no other user's process reads, locks, sweeps or removes anything in it.
//!
//! **A process that dies without unwinding is reclaimed by the next one.**
//! Every directory a process holds lives under its root,
//! `<private>/toyos-tmp-<pid>-<n>/`, beside an [`OWNER`] file the process holds
//! an exclusive lock on for as long as the root exists. The kernel lets go of
//! that lock when the process dies, by any signal, `SIGKILL` included. The
//! first directory a process makes under a base sweeps it: a root whose owner
//! can be locked belongs to a process that is gone, and is removed.
//!
//! **A live process's root is never touched.** Making a root, removing one, and
//! the sweep each hold [`GLOBAL`] exclusively, so no sweep sees a root between
//! its `mkdir` and its owner's lock, or halfway through its removal. Liveness
//! is the lock and never the pid, so a reused pid cannot make a dead root look
//! live or a live one look dead; the pid only keeps two roots' names apart.
//! Every process of one user sharing a base, every worktree included, shares
//! the lock file in its private directory. A hold on it past
//! `GLOBAL_PATIENCE` panics naming the pid that holds it, rather than hanging
//! every worktree behind a stopped process.
//!
//! **A directory the sweep cannot remove does not cost any process but this
//! one.** It is moved back out of this process's root under a name no sweep
//! reads as a root, and reported; the next process still makes its first
//! directory.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::ops::Deref;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What every root's name starts with; the rest is `<pid>-<n>`.
pub const ROOT_PREFIX: &str = "toyos-tmp-";

/// The lock every root's making and removal and every sweep holds, in its
/// [`private`] directory. Never removed: a lock file deleted while another
/// process waits on it would let two holders in at once.
pub const GLOBAL: &str = "toyos-tmp.lock";

/// The file in a root its process holds locked for the root's whole life.
pub const OWNER: &str = "owner";

/// The base [`TempDir::short`] makes its roots under.
pub const SHORT_BASE: &str = "/tmp";

/// What a [`private`] directory's name starts with; the rest is its user's id.
/// [`ROOT_PREFIX`] is not a prefix of it, so no sweep reads it as a root.
pub const PRIVATE_PREFIX: &str = "toyos-";

/// A directory of its own under its base, removed with everything in it when
/// this is dropped.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
    base: Base,
}

impl TempDir {
    /// A fresh, empty directory under `$TMPDIR` whose name starts with `label`,
    /// which is one path component. Its path is resolved: `/private/var/…` on
    /// macOS, never the `/var/…` symlink, so git and a canonicalized comparison
    /// agree with it.
    pub fn new(label: &str) -> TempDir {
        Self::under(Base::TmpDir, label)
    }

    /// [`TempDir::new`] under [`SHORT_BASE`], whatever `$TMPDIR` is.
    pub fn short(label: &str) -> TempDir {
        Self::under(Base::Short, label)
    }

    fn under(base: Base, label: &str) -> TempDir {
        assert!(
            !label.is_empty() && !label.contains(['/', '\\']) && label != "." && label != "..",
            "a scratch label is one path component, not {label:?}"
        );
        let mut state = base.state().lock().unwrap_or_else(PoisonError::into_inner);
        if state.root.is_none() {
            let tmp = base.dir();
            let tmp = fs::canonicalize(&tmp)
                .unwrap_or_else(|e| panic!("resolve {}: {e}", tmp.display()));
            let tmp = private(&tmp);
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
        reclaim(&tmp, reap, |dir| fs::remove_dir_all(dir));
        TempDir { path, base }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move every root in `base`'s [`private`] directory that process `pid`
    /// made and is gone from into this directory, so it goes when this does:
    /// for a caller that `SIGKILL`ed `pid` after this process's one sweep of
    /// `base`. A live root is left alone, whatever its name.
    pub fn adopt(&self, base: &Path, pid: u32) {
        let prefix = format!("{ROOT_PREFIX}{pid}-");
        let base = private(base);
        let _global = global(&base);
        reap_into(&base, &self.path, |root| {
            root.file_name().expect("a root has a name").to_string_lossy().starts_with(&prefix)
        });
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
        let mut state = self.base.state().lock().unwrap_or_else(PoisonError::into_inner);
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

/// Remove every directory the sweep moved into this process's root, under the
/// private directory `tmp`, with `remove`, so a test can make one fail; one
/// that is not removed is [`stuck`].
fn reclaim(tmp: &Path, reap: Vec<PathBuf>, remove: impl Fn(&Path) -> std::io::Result<()>) {
    for dir in reap {
        if let Err(e) = remove(&dir) {
            stuck(tmp, &dir, e);
        }
    }
}

/// A gone process's directory the sweep moved into this process's own root but
/// could not then remove: reported once, and moved back out to its private
/// directory under a name no sweep reads as a root — `ROOT_PREFIX` is not a
/// prefix of it — so this is the only process it ever costs, and no later
/// process inherits it and panics in turn. `tmp` is the private directory the
/// reap happened under, not `dir`'s own parent, so this never guesses at the
/// layout a future reap chooses.
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

/// A directory roots are made in.
#[derive(Clone, Copy, Debug)]
enum Base {
    TmpDir,
    Short,
}

impl Base {
    /// This process's hold on it.
    fn state(self) -> &'static Mutex<State> {
        static TMPDIR: Mutex<State> = Mutex::new(State { root: None, roots: 0, swept: false });
        static SHORT: Mutex<State> = Mutex::new(State { root: None, roots: 0, swept: false });
        match self {
            Base::TmpDir => &TMPDIR,
            Base::Short => &SHORT,
        }
    }

    /// The directory, before it is resolved.
    fn dir(self) -> PathBuf {
        match self {
            Base::TmpDir => std::env::temp_dir(),
            Base::Short => PathBuf::from(SHORT_BASE),
        }
    }
}

struct State {
    root: Option<Root>,
    /// Roots this process has made, so one made after the last was removed
    /// does not take its name.
    roots: u64,
    /// Whether this process has swept its base: once is what reclaims every
    /// root a process that died left.
    swept: bool,
}

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
                // removing its root, the sweep's; or this process's root of the
                // other base, where `$TMPDIR` is `/tmp`. Neither is in the way.
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
    /// Move every root in its private directory whose process is gone into
    /// this process's own root, and return where each went: the caller removes them
    /// holding nothing, and a caller that dies meanwhile leaves them in a root
    /// the next sweep takes.
    fn sweep(&mut self) -> Vec<PathBuf> {
        self.swept = true;
        let root = self.root.as_ref().expect("a sweep runs from a live root");
        let _global = global(&root.tmp);
        reap_into(&root.tmp, &root.dir, |path| path != root.dir)
    }
}

/// Every root in `base`'s [`private`] directory whose process is gone: what a
/// process of this user that died there left, until the next sweep takes it.
pub fn gone_roots(base: &Path) -> Vec<PathBuf> {
    let base = private(base);
    let _global = global(&base);
    gone_under(&base)
}

/// Remove every root in `base`'s [`private`] directory whose process is gone
/// and that `take` passes, and answer how many: for a caller that ended those
/// processes itself, rather than leave them to the next process's sweep. They
/// go as a sweep's do, through a root of this process's there.
pub fn reclaim_gone(base: &Path, take: impl Fn(&Path) -> bool) -> usize {
    let tmp = private(base);
    let root = Root::make(&tmp, &mut 0);
    let reap = {
        let _global = global(&tmp);
        reap_into(&tmp, &root.dir, |path| path != root.dir && take(path))
    };
    let reclaimed = reap.len();
    reclaim(&tmp, reap, |dir| fs::remove_dir_all(dir));
    root.remove();
    reclaimed
}

/// This user's directory of `base`, which any user may share: made if absent,
/// and refused with a panic unless it is a directory, not a symlink, that this
/// user owns with mode 0700, since one another user made could be read or
/// emptied by them.
pub fn private(base: &Path) -> PathBuf {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    private_for(base, unsafe { libc::geteuid() })
}

/// [`private`] for the user `uid`, so a test can be a user other than the one
/// that owns what it made.
fn private_for(base: &Path, uid: u32) -> PathBuf {
    let dir = base.join(format!("{PRIVATE_PREFIX}{uid}"));
    match DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => panic!("create {}: {e}", dir.display()),
    }
    let meta =
        fs::symlink_metadata(&dir).unwrap_or_else(|e| panic!("inspect {}: {e}", dir.display()));
    let refused = if !meta.file_type().is_dir() {
        Some("is not a directory".to_string())
    } else if meta.uid() != uid {
        Some(format!("is owned by uid {}, not by this process's user, uid {uid}", meta.uid()))
    } else if meta.mode() & 0o777 != 0o700 {
        Some(format!("has mode {:o}, not 700", meta.mode() & 0o777))
    } else {
        None
    };
    if let Some(why) = refused {
        panic!(
            "{} {why}: it holds this user's scratch, so it must be this user's alone; \
             have its owner or root remove it",
            dir.display()
        );
    }
    dir
}

/// Every gone root under `base` that `take` passes, renamed into `dest` as
/// `reap-<its name>`; the caller holds [`GLOBAL`].
fn reap_into(base: &Path, dest: &Path, take: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut reap = Vec::new();
    for path in gone_under(base) {
        if !take(&path) {
            continue;
        }
        let name = path.file_name().expect("a root has a name").to_string_lossy();
        let to = dest.join(format!("reap-{name}"));
        fs::rename(&path, &to)
            .unwrap_or_else(|e| panic!("move {} to {}: {e}", path.display(), to.display()));
        reap.push(to);
    }
    reap
}

/// [`gone_roots`], with [`GLOBAL`] held by the caller.
fn gone_under(base: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(base).unwrap_or_else(|e| panic!("read {}: {e}", base.display()));
    let mut roots = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", base.display()));
        let is_root = entry.file_name().to_str().is_some_and(|n| n.starts_with(ROOT_PREFIX));
        if is_root && gone(&entry.path()) {
            roots.push(entry.path());
        }
    }
    roots
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
                 base is stuck behind it — find and end that process, or wait for it to move on",
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

    /// **A directory the sweep cannot remove costs no process but this one**:
    /// it leaves this process's root, which can then remove itself, for its
    /// base under a name no sweep reads as a root, and this process goes on.
    #[test]
    fn a_directory_the_sweep_cannot_remove_costs_no_process_but_this_one() {
        let tmp = TempDir::new("stuck");
        let name = format!("reap-{ROOT_PREFIX}999999-0");
        let reaped = tmp.join("root").join(&name);
        fs::create_dir_all(reaped.join("locked")).unwrap();
        fs::write(reaped.join(OWNER), b"").unwrap();

        reclaim(&tmp, vec![reaped.clone()], |_| Err(std::io::Error::other("refused")));
        assert!(!reaped.exists(), "{} stayed in the root it was reaped into", reaped.display());
        let stuck = tmp.join(format!("stuck-{name}"));
        assert!(stuck.join("locked").is_dir(), "{} was not moved aside to {}", reaped.display(), stuck.display());
    }

    /// The panic `f` raises, which it must.
    fn refusal(f: impl FnOnce() -> PathBuf) -> String {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .expect_err("a directory that is not this user's alone was accepted");
        panic.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    fn me() -> u32 {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// A user's private directory is made a directory of its own, mode 0700.
    #[test]
    fn a_private_directory_is_made_this_users_alone() {
        let base = TempDir::new("private-made");
        let dir = private(&base);
        assert_eq!(dir, base.join(format!("{PRIVATE_PREFIX}{}", me())));
        let meta = fs::symlink_metadata(&dir).unwrap();
        assert!(meta.file_type().is_dir());
        assert_eq!((meta.uid(), meta.mode() & 0o777), (me(), 0o700));
        assert_eq!(private(&base), dir, "a second use refused the first's directory");
    }

    /// A directory another user owns is theirs to read and empty: this user
    /// refuses it. Here the other user is the one this process is not, and the
    /// directory is this process's own, mode 0700, so ownership alone refuses it.
    #[test]
    fn a_private_directory_another_user_owns_is_refused() {
        let base = TempDir::new("private-theirs");
        let other = me().wrapping_add(1);
        let message = refusal(|| private_for(&base, other));
        assert!(message.contains(&format!("owned by uid {}", me())), "{message}");
    }

    /// One this user owns that others may enter is refused too.
    #[test]
    fn a_private_directory_with_another_mode_is_refused() {
        let base = TempDir::new("private-mode");
        let dir = base.join(format!("{PRIVATE_PREFIX}{}", me()));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let message = refusal(|| private(&base));
        assert!(message.contains("mode 755"), "{message}");
    }

    /// A symlink in its place is refused, even to a directory that would pass:
    /// another user could have planted it, or point it elsewhere.
    #[test]
    fn a_symlink_in_place_of_a_private_directory_is_refused() {
        let base = TempDir::new("private-link");
        let target = base.join("target");
        DirBuilder::new().mode(0o700).create(&target).unwrap();
        std::os::unix::fs::symlink(&target, base.join(format!("{PRIVATE_PREFIX}{}", me()))).unwrap();
        let message = refusal(|| private(&base));
        assert!(message.contains("not a directory"), "{message}");
    }
}
