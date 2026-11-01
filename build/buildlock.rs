//! Serialising the build system's stateful phases across the builds running
//! on this host.
//!
//! Cargo's own build lock cannot do this job. `build/build.rs`'s `invalidate_stale`
//! runs the `clean` that removes what a moved sysroot left stale of a `target/`,
//! and cargo's lock lives inside it at `target/<profile>/.cargo-lock` — the clean deletes the
//! file the other process's lock is on. So these files live outside every
//! directory the build system removes: a lock on an inode that can be unlinked
//! and recreated under a waiter is not a lock.
//!
//! Two modes, because two plain `cargo build`s of different packages are
//! cargo's business and serialising those would destroy the parallelism the
//! builds depend on:
//!
//! - **shared** — "I am building against the state as it stands". Any number
//!   at once.
//! - **exclusive** — "I am replacing it": a compiler's, an LLVM's or a std's
//!   build in this worktree's fork checkout, the cleans of stale targets, the
//!   making or moving of that checkout. One at a time, and never while a build
//!   holds the shared mode.
//!
//! A build holds its worktree's lock shared for its whole length. What it
//! shares with every other checkout on the host is the store
//! (`build/keystore.rs`), whose products nothing rewrites once they are placed,
//! each locked under its own key ([`keyed_made`], [`keyed_idle`]) in the store
//! itself: two worktrees with different ABIs never meet in a sysroot's lock,
//! and two with the same one build it once. A key's lock file is never
//! removed, and its modification time is when the key was last used or made.
//!
//! A compiler key's lock → a sysroot key's lock → a freestanding key's lock → the
//! worktree build lock → an LLVM key's lock → a ToyOS-hosted clang key's lock
//! → artifact. A compiler's, a sysroot's or a freestanding key lock is taken
//! with the worktree lock put down ([`Held::without_shared`]), because the
//! key's builder takes the worktree lock exclusively; an LLVM key's is taken
//! inside the worktree lock covering the fork build directory its builder
//! writes; a ToyOS-hosted clang key's inside the worktree lock and the use of
//! the sysroot it builds against, writing only the store.
//!
//! Holder death: `flock` is released by the kernel when the open file
//! description closes, so a builder that is SIGKILLed mid-phase — routine here
//! — strands nothing, which a lock file with a pid in it could not promise.
//! Established on this host (Darwin 25.5.0) rather than assumed, and
//! `killed_holder_releases_the_lock` keeps it that way.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::keystore::Key;

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Shared,
    Exclusive,
}

/// How often a wait that is lasting repeats itself.
///
/// Shortened under `cfg(test)` so the gate on the repetition costs a second
/// instead of a minute; every process in those gates is this same binary, so
/// both sides of a wait agree on it.
#[cfg(not(test))]
const HEARTBEAT: Duration = Duration::from_secs(30);
#[cfg(test)]
const HEARTBEAT: Duration = Duration::from_millis(300);

/// How long any one acquisition waits before it fails, naming the holder: past
/// the 350 minutes `.github/workflows/toolchain.yml` allows the whole
/// toolchain, the longest phase any of these locks covers.
const CEILING: Duration = Duration::from_secs(6 * 60 * 60);

/// The longest a waiter sleeps between two tries ([`poll`]).
const MAX_PAUSE: Duration = Duration::from_millis(50);

const LOCK_DIR: &str = ".build-locks";

/// A held lock. Releasing it is closing the file.
#[must_use]
pub struct Guard {
    file: fs::File,
    /// Exclusive holders record who they are, and clear it on the way out so a
    /// waiter never names a process that has already finished.
    records_holder: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.records_holder {
            write_note(&mut self.file, "");
        }
    }
}

/// The worktree's build lock, held in shared mode for the length of one build so
/// no clean of its crate targets lands inside it.
pub struct Held {
    worktree_dir: PathBuf,
    what: String,
    /// `None` only while [`Held::without_shared`] has it put down, which is the
    /// whole reason this is an `Option`.
    guard: Option<Guard>,
}

/// Take the build lock in shared mode for `what`, and hold it until the
/// returned value is dropped — which must be after the last artifact the build
/// reads back, not merely after the last thing it writes: a clean landing
/// between a `cargo build` and the read of what it built is the same defect.
pub fn shared(root: &Path, what: &str) -> Held {
    let mut held = Held {
        worktree_dir: root.join(LOCK_DIR),
        what: what.to_string(),
        guard: None,
    };
    held.guard = Some(held.take_shared());
    held
}

impl Held {
    /// Ask `decide`, and if it reports work, do that work under this worktree's
    /// lock held exclusively.
    ///
    /// `decide` runs first under the shared lock this value holds, so a phase
    /// with nothing to do costs no serialisation at all. When it does report
    /// work the shared lock is dropped, the exclusive one taken, and `decide`
    /// asked **again**: whatever it saw a moment ago may have been done by the
    /// process that held the lock in between, and only this second answer is
    /// acted on. Serialising the action alone would still double-clean.
    pub fn act_if<W>(
        &mut self,
        phase: &str,
        decide: impl Fn() -> Option<W>,
        act: impl FnOnce(W),
    ) {
        if decide().is_none() {
            return;
        }
        let dir = self.worktree_dir.clone();
        self.without_shared(|| {
            let _exclusive = acquire(&dir, Mode::Exclusive, phase, BUILD);
            if let Some(work) = decide() {
                act(work);
            }
        });
    }

    /// Run `f` with this worktree's shared lock put down and take it back after:
    /// for a lock that orders before it, as a sysroot key's does.
    pub fn without_shared<R>(&mut self, f: impl FnOnce() -> R) -> R {
        self.guard = None;
        let out = f();
        self.guard = Some(self.take_shared());
        out
    }

    fn take_shared(&self) -> Guard {
        acquire(&self.worktree_dir, Mode::Shared, &self.what, BUILD)
    }
}

/// This worktree's build lock, exclusively: what a sysroot build holds while it
/// writes the worktree's fork build directory, with the key's lock already held.
pub fn worktree_exclusive(root: &Path, what: &str) -> Guard {
    acquire(&root.join(LOCK_DIR), Mode::Exclusive, what, BUILD)
}

/// Exclusive lock over the shared cargo artifact paths.
///
/// Cargo keys an artifact path on (crate, target, profile) and nothing else, so
/// every config writes and reads one path; this is held across each build→stage
/// pair so the staged copy is of what this build produced. Separate from the
/// build lock proper because every builder needs it and builders hold the build
/// lock in *shared* mode by design.
pub fn artifact(root: &Path) -> Guard {
    exclusive(&root.join(LOCK_DIR).join("artifact"), "artifact lock", "artifact staging")
}

/// A content-addressed product of the host, locked per key: a sysroot, the
/// freestanding targets' libraries it carries (`build/sysroot.rs`), a compiler
/// (`build/compiler.rs`), the LLVM a compiler links (`build/llvm.rs`), or the
/// clang and LLD built to run on ToyOS (`build/hostedclang.rs`).
#[derive(Clone, Copy)]
pub enum Keyed {
    Sysroot,
    Freestanding,
    Compiler,
    Llvm,
    HostedClang,
}

impl Keyed {
    fn dir(self) -> &'static str {
        match self {
            Keyed::Sysroot => "sysroots",
            Keyed::Freestanding => "freestanding",
            Keyed::Compiler => "compilers",
            Keyed::Llvm => "llvm",
            Keyed::HostedClang => "hosted-clang",
        }
    }

    /// Where `store` keeps every product of this kind, one directory per key.
    pub fn store(self, store: &Path) -> PathBuf {
        store.join(self.dir())
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Keyed::Sysroot => "sysroot",
            Keyed::Freestanding => "freestanding libraries",
            Keyed::Compiler => "compiler",
            Keyed::Llvm => "LLVM",
            Keyed::HostedClang => "ToyOS-hosted clang",
        }
    }
}

/// Make what `key` names, if nobody is making or using it: exclusive, and
/// waited for by every other process that wants the same key, which then finds
/// it made.
fn keyed_building(store: &Path, kind: Keyed, key: &Key) -> Option<Guard> {
    let file = open_lock_file(&keyed_lock_path(store, kind, key));
    if !try_lock(&file, Mode::Exclusive) {
        return None;
    }
    let mut guard = Guard { file, records_holder: true };
    write_note(&mut guard.file, &note_text(&format!("building {} {key}", kind.name())));
    Some(guard)
}

/// Use what `key` names: shared, so any number of builds use it at once, a
/// builder of it is waited for, and a sweep cannot remove it; and dated now,
/// which a sweep reads as its last use.
pub(crate) fn keyed_using(store: &Path, kind: Keyed, key: &Key) -> Guard {
    let (lock, what) = (format!("{} lock", kind.name()), format!("using {} {key}", kind.name()));
    let path = keyed_lock_path(store, kind, key);
    poll(&path, &lock, &what, UNNOTED, Instant::now() + CEILING, || keyed_try_using(store, kind, key))
}

/// [`keyed_using`], if nobody is making it now.
fn keyed_try_using(store: &Path, kind: Keyed, key: &Key) -> Option<Guard> {
    let path = keyed_lock_path(store, kind, key);
    let file = open_lock_file(&path);
    if !try_lock(&file, Mode::Shared) {
        return None;
    }
    file.set_modified(SystemTime::now()).unwrap_or_else(|e| panic!("build lock: date {}: {e}", path.display()));
    Some(Guard { file, records_holder: false })
}

/// What `key` names, held in use and whole: made by `make` under
/// [`keyed_building`] while `defect`, which says why it is not whole, says it is
/// not. A `make` that leaves it not whole is refused by that defect rather than
/// run again.
///
/// The making is tried, never waited for: a waiter queued to make it would
/// wait out every use of what another waiter made meanwhile.
pub fn keyed_made(
    store: &Path,
    kind: Keyed,
    key: &Key,
    defect: impl Fn() -> Option<String>,
    make: impl FnMut(),
) -> Guard {
    keyed_made_by(store, kind, key, defect, make, Instant::now() + CEILING)
}

/// [`keyed_made`], failing at `deadline`.
fn keyed_made_by(
    store: &Path,
    kind: Keyed,
    key: &Key,
    defect: impl Fn() -> Option<String>,
    mut make: impl FnMut(),
    deadline: Instant,
) -> Guard {
    let (lock, what) = (format!("{} lock", kind.name()), format!("using {} {key}", kind.name()));
    poll(&keyed_lock_path(store, kind, key), &lock, &what, UNNOTED, deadline, || {
        if let Some(using) = keyed_try_using(store, kind, key) {
            if defect().is_none() {
                return Some(using);
            }
        }
        let building = keyed_building(store, kind, key)?;
        if defect().is_some() {
            make();
            if let Some(defect) = defect() {
                panic!("{} {key} was made, and is not whole: {defect}", kind.name());
            }
        }
        drop(building);
        keyed_try_using(store, kind, key).filter(|_| defect().is_none())
    })
}

/// What `key` names, exclusively and only if nobody is making or using it: what
/// a sweep holds while it removes one.
pub fn keyed_idle(store: &Path, kind: Keyed, key: &Key) -> Option<Guard> {
    let file = open_lock_file(&keyed_lock_path(store, kind, key));
    try_lock(&file, Mode::Exclusive).then_some(Guard { file, records_holder: false })
}

/// When `kind`'s `key` in `store` was last used or made. A key nothing has
/// locked yet is dated by the lock this makes for it, which is now.
pub(crate) fn keyed_used(store: &Path, kind: Keyed, key: &Key) -> SystemTime {
    let path = keyed_lock_path(store, kind, key);
    open_lock_file(&path).metadata().and_then(|meta| meta.modified()).unwrap_or_else(|e| panic!("build lock: stat {}: {e}", path.display()))
}

/// The lock of `kind`'s `key`: in the store, where every checkout on the host
/// names it alike, and in none of its products.
pub(crate) fn keyed_lock_path(store: &Path, kind: Keyed, key: &Key) -> PathBuf {
    store.join("locks").join(kind.dir()).join(key)
}

/// One lock file, taken exclusively and held until the guard drops.
///
/// For the locks with no shared mode: every holder writes the note, so a waiter
/// can always be told who it is waiting for.
fn exclusive(path: &Path, lock: &str, what: &str) -> Guard {
    let file = open_lock_file(path);
    take(&file, Mode::Exclusive, path, lock, what, UNNOTED, Instant::now() + CEILING);
    let mut guard = Guard { file, records_holder: true };
    write_note(&mut guard.file, &note_text(what));
    guard
}

/// One lock with a shared and an exclusive mode, in the two words a waiting
/// agent needs.
///
/// The second field exists because the shared mode cannot leave a note: one
/// `state` file carries one, and shared holders come several at a time. So what
/// to say about them is a property of the lock rather than something
/// [`describe_holder`] could work out.
#[derive(Clone, Copy)]
struct Lock {
    name: &'static str,
    shared_holders: &'static str,
    queued_ahead: &'static str,
}

const BUILD: Lock = Lock {
    name: "build lock",
    shared_holders: "held by other builds in this tree",
    queued_ahead: "an exclusive phase is queued ahead of it",
};

/// Acquire one mode of a two-file lock.
///
/// Two files, not one. `flock` has no writer preference — measured on this
/// host, four shared churners kept an exclusive waiter out for the whole 5.5 s
/// they ran — and the exclusive phases are exactly the long, silent ones an
/// agent kills and retries. So an exclusive acquirer holds `intent` while it
/// waits for `state`, which makes later shared acquirers wait behind it
/// instead of overtaking it. `intent` is always taken before `state` and
/// dropped as soon as `state` is held, so nothing ever waits on `intent` while
/// holding `state`.
fn acquire(dir: &Path, mode: Mode, what: &str, lock: Lock) -> Guard {
    let intent_path = dir.join("intent");
    let state_path = dir.join("state");
    let intent = open_lock_file(&intent_path);
    let state = open_lock_file(&state_path);
    let label = format!("{}, {what}", if mode == Mode::Exclusive { "exclusive" } else { "shared" });
    let deadline = Instant::now() + CEILING;
    take(&intent, mode, &intent_path, lock.name, &label, lock.queued_ahead, deadline);
    take(&state, mode, &state_path, lock.name, &label, lock.shared_holders, deadline);
    drop(intent);

    let records_holder = mode == Mode::Exclusive;
    let mut guard = Guard { file: state, records_holder };
    if records_holder {
        write_note(&mut guard.file, &note_text(what));
    }
    guard
}

/// What a waiter is told of a holder that left no note it can read.
const UNNOTED: &str = "held, but the holder left no readable note";

/// Take `mode` of `file`, the lock at `path`, by `deadline` ([`poll`]).
fn take(file: &fs::File, mode: Mode, path: &Path, lock: &str, label: &str, unnoted: &str, deadline: Instant) {
    poll(path, lock, label, unnoted, deadline, || try_lock(file, mode).then_some(()));
}

/// What `attempt` gets, tried until it gets it or `deadline` passes, which
/// fails naming the holder of `path`. A wait says what it waits for and who
/// has it, when that can be established ([`describe_holder`], else `unnoted`),
/// and says so again every [`HEARTBEAT`]: an agent staring at silence kills and
/// retries.
///
/// A poll, not a blocking `flock`: macOS lets blocked shared waiters in one at
/// a time (`issues/macos-lets-blocked-shared-flock-waiters-in-one-at-a-time.md`).
fn poll<T>(path: &Path, lock: &str, label: &str, unnoted: &str, deadline: Instant, mut attempt: impl FnMut() -> Option<T>) -> T {
    if let Some(got) = attempt() {
        return got;
    }
    let holder = || describe_holder(path).unwrap_or_else(|| unnoted.to_string());
    eprintln!("[build-lock] waiting for the {lock} ({label}) — {}", holder());
    let began = Instant::now();
    let mut said = began;
    let mut pause = Duration::from_millis(1);
    loop {
        let now = Instant::now();
        if now >= deadline {
            panic!("build lock: gave up waiting for the {lock} ({label}) after {:.0?} — {}", began.elapsed(), holder());
        }
        if now - said >= HEARTBEAT {
            eprintln!("[build-lock] still waiting for the {lock} ({label}), {:.0?} so far — {}", began.elapsed(), holder());
            said = now;
        }
        std::thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(MAX_PAUSE);
        if let Some(got) = attempt() {
            eprintln!("[build-lock] acquired the {lock} ({label}) after {:.1?}", began.elapsed());
            return got;
        }
    }
}

/// What the last exclusive holder of `path` recorded, if it is still running.
///
/// A killed holder leaves its note behind, so the pid is checked before it is
/// named: telling a waiting agent to go look at a dead pid is worse than
/// telling it nothing. `None` is that "nothing" — what to say instead is the
/// caller's, because a lock with a shared mode is usually held by holders who
/// never wrote a note at all, and a lock without one never is.
fn describe_holder(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    let mut parts = text.trim().splitn(3, ' ');
    let (pid, since, what) = (parts.next()?, parts.next()?, parts.next()?);
    let (pid, since) = (pid.parse::<i32>().ok()?, since.parse::<u64>().ok()?);
    if !alive(pid) {
        return None;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(since))
        .unwrap_or(0);
    Some(format!("held by pid {pid} ({what}), {secs}s so far"))
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 runs the existence and permission checks and delivers
    // nothing.
    if unsafe { kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().kind() == io::ErrorKind::PermissionDenied
}

fn note_text(what: &str) -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{} {since} {what}", std::process::id())
}

/// The note is advisory — it names a holder in a waiter's message and nothing
/// reads it to decide anything — so failing to write it must not fail a build.
fn write_note(file: &mut fs::File, text: &str) {
    let _ = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| file.write_all(text.as_bytes()))
        .and_then(|_| file.flush());
}

fn open_lock_file(path: &Path) -> fs::File {
    let dir = path.parent().expect("lock path has a parent");
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("build lock: create {}: {e}", dir.display()));
    // Never truncating: the file carries the holder note, and `File::create`
    // would wipe a live holder's.
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap_or_else(|e| panic!("build lock: open {}: {e}", path.display()))
}

fn try_lock(file: &fs::File, mode: Mode) -> bool {
    let tried = match mode {
        Mode::Shared => file.try_lock_shared(),
        Mode::Exclusive => file.try_lock(),
    };
    match tried {
        Ok(()) => true,
        Err(fs::TryLockError::WouldBlock) => false,
        Err(fs::TryLockError::Error(e)) => panic!("build lock: flock: {e}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::process::{Child, Command};
    use std::time::Duration;
    use toyos_tmpdir::TempDir;

    // A lock is per open file description, so threads that each open the file
    // contend as processes do. A child process is for what a thread cannot
    // show: a holder that dies, and a lock whose release no copy of this
    // process's descriptors delays (see [`Elsewhere`]). The child is this same test binary, re-run with one
    // `#[ignore]`d test selected by name and its role in the environment, so an
    // ordinary `cargo test` never runs the child half on its own.
    const ROLE: &str = "TOYOS_BUILDLOCK_TEST_ROLE";
    const ROOT: &str = "TOYOS_BUILDLOCK_TEST_ROOT";
    const MARKS: &str = "TOYOS_BUILDLOCK_TEST_MARKS";
    const KEY: &str = "TOYOS_BUILDLOCK_TEST_KEY";

    /// A lock held by a process of its own, which [`Elsewhere::release`] waits
    /// to exit.
    ///
    /// A test never asserts free a lock this process has held: another test
    /// thread's spawn copies every descriptor open at that moment into its child
    /// until the child's exec, and the copy holds the lock past the drop.
    pub(crate) struct Elsewhere {
        child: Child,
        marks: TempDir,
        released: bool,
    }

    impl Elsewhere {
        /// Run the `#[ignore]`d test `role` names, with `env`, and return once
        /// it has called [`hold_until_released`].
        pub(crate) fn hold(role: &str, env: &[(&str, &OsStr)]) -> Self {
            let marks = TempDir::new("buildlock-elsewhere");
            let mut child = rerun(role)
                .envs(env.iter().copied())
                .env(MARKS, &marks)
                .spawn()
                .expect("spawn the holder");
            let deadline = Instant::now() + Duration::from_secs(20);
            while !marks.join("held").exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("{role} exited before it took its lock: {status}");
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    panic!("{role} never took its lock in 20 s, killed: {}", child.wait().unwrap());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Elsewhere { child, marks, released: false }
        }

        pub(crate) fn id(&self) -> u32 {
            self.child.id()
        }

        /// Let go, and return once the holder has exited.
        pub(crate) fn release(mut self) {
            self.released = true;
            touch(&self.marks.join("release"));
            assert!(self.child.wait().unwrap().success(), "the holder failed");
        }
    }

    impl Drop for Elsewhere {
        /// An assertion between `hold` and `release` skips `release`; without
        /// this, the holder it leaked keeps running and its lock held past
        /// the test that dropped it.
        fn drop(&mut self) {
            if self.released {
                return;
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The holder's half of [`Elsewhere`].
    pub(crate) fn hold_until_released() {
        let marks = PathBuf::from(std::env::var(MARKS).expect("a holder runs under Elsewhere::hold"));
        touch(&marks.join("held"));
        assert!(appeared(&marks.join("release"), Duration::from_secs(20)), "the holder was never released");
    }

    /// Sysroot `key` of `root`, held in use by a process of its own.
    pub(crate) fn sysroot_used_elsewhere(root: &Path, key: &Key) -> Elsewhere {
        let env = [(ROLE, OsStr::new("use-sysroot")), (ROOT, root.as_os_str()), (KEY, OsStr::new(key.as_str()))];
        Elsewhere::hold("buildlock::tests::child_role", &env)
    }

    /// `root`'s worktree lock held shared by a process of its own.
    pub(crate) fn shared_elsewhere(root: &Path) -> Elsewhere {
        held_elsewhere(root, "hold-shared")
    }

    /// Whether `root`'s worktree lock would keep a shared acquirer out now.
    pub(crate) fn keeps_out_shared(root: &Path) -> bool {
        !try_lock(&open_lock_file(&worktree_lock_dir(root).join("state")), Mode::Shared)
    }

    /// What `role` of [`child_role`] takes in `root`, held by a process of its own.
    fn held_elsewhere(root: &Path, role: &str) -> Elsewhere {
        Elsewhere::hold("buildlock::tests::child_role", &[(ROLE, OsStr::new(role)), (ROOT, root.as_os_str())])
    }

    fn scratch(name: &str) -> TempDir {
        TempDir::new(&format!("buildlock-{name}"))
    }

    fn worktree_lock_dir(root: &Path) -> PathBuf {
        root.join(LOCK_DIR)
    }

    /// This test binary, to run the one `#[ignore]`d test `test` names.
    pub(crate) fn rerun(test: &str) -> Command {
        let mut rerun = Command::new(std::env::current_exe().unwrap());
        rerun.args(["--exact", test, "--include-ignored", "--nocapture"]);
        rerun
    }

    fn child(root: &Path, role: &str) -> Child {
        rerun("buildlock::tests::child_role")
            .env(ROLE, role)
            .env(ROOT, root)
            .spawn()
            .expect("spawn the competing process")
    }

    fn appeared(path: &Path, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        path.exists()
    }

    fn touch(path: &Path) {
        fs::write(path, b"").unwrap();
    }

    /// Hold whatever this role took until the test that spawned it is gone.
    ///
    /// A flat `sleep(600)` is what these roles used to do, and it is wrong in
    /// exactly the case they exist for: when the *parent* assertion fails, the
    /// test never reaches its `kill`, and two children go on holding the
    /// harness's stdout for ten minutes — so the negative control an agent runs
    /// on purpose wedges the run it was checking. Costs one `getppid` every
    /// 100 ms and needs no reaper.
    fn until_orphaned() {
        unsafe extern "C" {
            fn getppid() -> i32;
        }
        let deadline = Instant::now() + Duration::from_secs(600);
        // SAFETY: `getppid` takes nothing and cannot fail.
        while Instant::now() < deadline && unsafe { getppid() } > 1 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A fresh fd per probe: a successful `try_lock` *holds* what it took, and
    /// polling on one fd would itself be the thing keeping the writer out.
    fn intent_is_taken(root: &Path) -> bool {
        !try_lock(&open_lock_file(&worktree_lock_dir(root).join("intent")), Mode::Shared)
    }

    fn note(root: &Path, line: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("order.log"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    #[test]
    #[ignore = "the competing process for the tests below; never runs on its own"]
    fn child_role() {
        let role = std::env::var(ROLE)
            .unwrap_or_else(|_| panic!("child_role ran without {ROLE}; it is not a test"));
        let root = PathBuf::from(std::env::var(ROOT).unwrap());
        match role.as_str() {
            "hold-exclusive" => {
                let mut held = shared(&root, "child");
                held.act_if("child exclusive phase", || Some(()), |()| hold_until_released());
            }
            "hold-exclusive-forever" => {
                let mut held = shared(&root, "child");
                held.act_if(
                    "child exclusive phase",
                    || Some(()),
                    |()| {
                        touch(&root.join("held"));
                        until_orphaned();
                    },
                );
            }
            "hold-artifact-forever" => {
                let _landing = artifact(&root);
                touch(&root.join("held"));
                until_orphaned();
            }
            "want-exclusive" => {
                let mut held = shared(&root, "child");
                held.act_if("queued exclusive phase", || Some(()), |()| note(&root, "ex"));
            }
            "hold-shared" => {
                let _held = shared(&root, "child");
                hold_until_released();
            }
            "want-shared" => {
                let _held = shared(&root, "child");
                note(&root, "sh");
            }
            "hold-sysroot-build" => {
                let _building = keyed_building(&root, Keyed::Sysroot, &Key::of(b"k1")).expect("nobody else has k1");
                hold_until_released();
                note(&root, "built");
            }
            "want-artifact" => {
                let _landing = artifact(&root);
                note(&root, "landed");
            }
            "want-sysroot" => {
                let _using = keyed_using(&root, Keyed::Sysroot, &Key::of(b"k1"));
                note(&root, "used");
            }
            "use-sysroot" => {
                let _using = keyed_using(&root, Keyed::Sysroot, &Key::parse(&std::env::var(KEY).unwrap()).unwrap());
                hold_until_released();
            }
            "clean" | "clean-unlocked" => {
                touch(&root.join("cleaner-ready"));
                assert!(appeared(&root.join("builder-mid"), Duration::from_secs(20)));
                let target = root.join("crate/target");
                if role == "clean" {
                    let mut held = shared(&root, "child");
                    held.act_if(
                        "clean the crate target",
                        || target.exists().then_some(()),
                        |()| fs::remove_dir_all(&target).unwrap(),
                    );
                } else {
                    fs::remove_dir_all(&target).unwrap();
                }
                touch(&root.join("cleaner-done"));
            }
            other => panic!("unknown child role {other}"),
        }
    }

    #[test]
    fn exclusive_excludes_every_other_acquirer() {
        let root = scratch("exclusive");
        let kid = held_elsewhere(&root, "hold-exclusive");

        let state = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&state, Mode::Shared), "a build got in while an exclusive phase ran");
        assert!(!try_lock(&state, Mode::Exclusive), "two exclusive phases at once");
        let holder = describe_holder(&worktree_lock_dir(&root).join("state"))
            .expect("no holder note");
        assert!(
            holder.starts_with(&format!("held by pid {} ", kid.id())),
            "the waiting side cannot name the holder: {holder}"
        );

        kid.release();
        drop(state);
        let _mine = shared(&root, "parent");
    }

    #[test]
    fn killed_holder_releases_the_lock() {
        let root = scratch("killed");
        let mut kid = child(&root, "hold-exclusive-forever");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let state = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&state, Mode::Exclusive), "the lock was not actually held");

        kid.kill().unwrap();
        kid.wait().unwrap();

        assert!(try_lock(&state, Mode::Exclusive), "a SIGKILLed holder stranded the lock");
        // And the note it left behind names a pid that is gone, so nobody is
        // sent to wait on it.
        assert_eq!(describe_holder(&worktree_lock_dir(&root).join("state")), None);
    }

    #[test]
    fn shared_admits_shared() {
        let root = scratch("shared");
        let _mine = shared(&root, "parent");
        let second = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(try_lock(&second, Mode::Shared), "two builds cannot run at once");
        drop(second);
        let third = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&third, Mode::Exclusive), "a clean got in while a build was running");
    }

    /// `flock` alone would let a stream of builds starve the rebuild they are
    /// all waiting for; the `intent` file is what stops that, and this is the
    /// gate on it.
    #[test]
    fn a_queued_exclusive_phase_goes_first() {
        let root = scratch("preference");
        let mine = shared(&root, "parent");

        let mut writer = child(&root, "want-exclusive");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !intent_is_taken(&root) {
            assert!(Instant::now() < deadline, "the exclusive child never queued");
            std::thread::sleep(Duration::from_millis(5));
        }

        let mut reader = child(&root, "want-shared");
        assert!(
            !appeared(&root.join("order.log"), Duration::from_millis(300)),
            "a build overtook a queued exclusive phase"
        );

        drop(mine);
        assert!(writer.wait().unwrap().success());
        assert!(reader.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "ex\nsh\n");
    }

    /// [`Elsewhere::release`] returns only once the holder has exited and been
    /// reaped: a zombie still answers `kill(pid, 0)`.
    #[test]
    fn a_released_holder_is_gone() {
        let root = scratch("released");
        let user = sysroot_used_elsewhere(&root, &Key::of(b"k"));
        let pid = user.id() as i32;
        user.release();
        assert!(!alive(pid), "release returned before the holder was reaped");
    }

    /// **One key is built once, and two keys never meet.** A second process
    /// wanting the key being built waits for it, and gets it only once the
    /// build is done; a process making
    /// another key is not held at all; and a sweep cannot take a key that
    /// somebody is making or using.
    #[test]
    fn a_key_being_built_is_waited_for_and_another_key_is_not() {
        let root = scratch("sysroot-keys");
        let builder = held_elsewhere(&root, "hold-sysroot-build");

        assert!(keyed_idle(&root, Keyed::Sysroot, &Key::of(b"k1")).is_none(), "a sweep could remove a key being built");
        let other = keyed_building(&root, Keyed::Sysroot, &Key::of(b"k2")).expect("a process making another key held this one");
        drop(other);

        let mut user = child(&root, "want-sysroot");
        assert!(
            !appeared(&root.join("order.log"), Duration::from_millis(300)),
            "a build used a sysroot while it was still being made"
        );
        builder.release();
        assert!(user.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "built\nused\n");

        let user = sysroot_used_elsewhere(&root, &Key::of(b"k1"));
        assert!(keyed_idle(&root, Keyed::Sysroot, &Key::of(b"k1")).is_none(), "a sweep could remove a key in use");
        user.release();
        assert!(keyed_idle(&root, Keyed::Sysroot, &Key::of(b"k1")).is_some(), "a sweep could not remove a key nobody uses");
    }

    #[test]
    fn a_lasting_wait_keeps_saying_so() {
        let root = scratch("heartbeat");
        let mut holder = child(&root, "hold-artifact-forever");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let mut queued = rerun("buildlock::tests::child_role")
            .env(ROLE, "want-artifact")
            .env(ROOT, &root)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the queued landing");

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let stderr = queued.stderr.take().unwrap();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });

        let mut repeats = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while repeats.len() < 2 && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(line) if line.contains("still waiting for the artifact lock") => {
                    repeats.push(line);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        queued.kill().unwrap();
        queued.wait().unwrap();
        holder.kill().unwrap();
        holder.wait().unwrap();

        assert!(
            repeats.len() >= 2,
            "a queued landing said {} times that it was still waiting: {repeats:?}",
            repeats.len()
        );
        assert!(
            repeats[0].contains(&format!("pid {}", holder.id())),
            "the repeat does not name the holder: {}",
            repeats[0]
        );
    }

    /// **Builds waiting on a key's making all get in, whether the making
    /// succeeds or fails, and none waits on another's use.** Eight threads
    /// wait while one makes a sysroot; once it is made, by that one or, when
    /// that one fails, by a waiter, every one holds its use at once, as builds
    /// hold theirs for their whole length. The waiters are threads of one
    /// process, as a test run's builds are.
    #[test]
    fn waiters_on_a_build_all_get_in() {
        waiters_on_a_build_get_in(false);
        waiters_on_a_build_get_in(true);
    }

    const WAITERS: usize = 8;

    /// Whether all [`WAITERS`] waiters on a make that `fails` or not hold
    /// their use within 20 s, the maker's own use held beside them. Asserted
    /// here, before the store goes: waiters still waiting write in it.
    fn waiters_on_a_build_get_in(fails: bool) {
        use std::sync::mpsc::channel;
        let store = scratch("waited-build");
        let key = Key::of(b"a build waited on");
        let whole = Keyed::Sysroot.store(&store).join(&key);
        let defect = {
            let whole = whole.clone();
            move || (!whole.is_dir()).then(|| "not made".to_string())
        };

        let (started, has_started) = channel();
        let (finish, finished) = channel::<()>();
        let (used, uses) = channel();
        let first = {
            let (store, key, defect, whole) = (store.to_path_buf(), key.clone(), defect.clone(), whole.clone());
            std::thread::spawn(move || {
                let using = keyed_made(&store, Keyed::Sysroot, &key, defect, || {
                    started.send(()).unwrap();
                    finished.recv().unwrap();
                    assert!(!fails, "the build failed");
                    fs::create_dir_all(&whole).unwrap();
                });
                using
            })
        };
        has_started.recv_timeout(Duration::from_secs(20)).expect("the first build never began");

        for _ in 0..WAITERS {
            let (store, key, defect, whole, used) = (store.to_path_buf(), key.clone(), defect.clone(), whole.clone(), used.clone());
            std::thread::spawn(move || {
                let using = keyed_made(&store, Keyed::Sysroot, &key, defect, || fs::create_dir_all(&whole).unwrap());
                used.send(using).unwrap();
            });
        }
        // Long enough for every waiter to be waiting before the build ends;
        // one that is not yet only makes this test weaker.
        std::thread::sleep(Duration::from_millis(300));
        finish.send(()).unwrap();
        let first = first.join();
        assert_eq!(first.is_err(), fails, "the first build did not end as staged");

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut held = Vec::new();
        while held.len() < WAITERS {
            match uses.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(using) => held.push(using),
                Err(_) => break,
            }
        }
        let ended = if fails { "failed" } else { "succeeded" };
        assert_eq!(held.len(), WAITERS, "of {WAITERS} builds waiting on one that {ended}, only {} got their sysroot in 20 s", held.len());
        drop(first);
    }

    /// **A build that found the key unmade, while another made it, does not
    /// wait out that one's use**: the key is whole and in use by another
    /// process, and `defect` says it is not once, as it did for a build that
    /// checked just before it was placed.
    #[test]
    fn a_would_be_maker_does_not_wait_out_another_use() {
        let store = scratch("would-be-maker");
        let key = Key::of(b"made meanwhile");
        let user = sysroot_used_elsewhere(&store, &key);
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let store = store.to_path_buf();
            std::thread::spawn(move || {
                let checked = std::cell::Cell::new(false);
                let defect = || (!checked.replace(true)).then(|| "placed just after this check".to_string());
                let using = keyed_made(&store, Keyed::Sysroot, &key, defect, || panic!("made twice"));
                let _ = tx.send(using);
            });
        }
        // Within the holder's own 20 s, so the verdict is this test's.
        let using = rx.recv_timeout(Duration::from_secs(10)).expect("a build that found the key unmade waited out another's use of it for 10 s");
        user.release();
        drop(using);
    }

    /// **A wait that outlasts its deadline fails, naming the holder**: a build
    /// wanting a key that another holds in the making for longer.
    #[test]
    fn a_wait_past_its_deadline_names_the_holder() {
        let store = scratch("deadline");
        let key = Key::of(b"never made");
        let _building = keyed_building(&store, Keyed::Sysroot, &key).expect("nobody else has the key");
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let (store, key) = (store.to_path_buf(), key.clone());
            std::thread::spawn(move || {
                let waited = std::panic::catch_unwind(|| {
                    let defect = || Some("not made".to_string());
                    keyed_made_by(&store, Keyed::Sysroot, &key, defect, || panic!("made under another's making"), Instant::now() + Duration::from_millis(500))
                });
                let _ = tx.send(waited.map(drop).map_err(|e| e.downcast_ref::<String>().cloned().unwrap_or_default()));
            });
        }
        let refusal = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("a wait with a 500 ms deadline had not ended 20 s later")
            .expect_err("a wait on a key nobody let go of succeeded");
        let holder = format!("held by pid {} (building sysroot {key})", std::process::id());
        assert!(refusal.contains("gave up") && refusal.contains(&holder), "the refusal does not name the holder: {refusal}");
    }

    /// The defect this module exists for, staged so that it is not itself a
    /// race: a clean lands in the middle of a build in the same target
    /// directory, and the build's next write finds the directory gone. Run once
    /// without the lock to show the ENOENT, once with it to show the clean
    /// waiting its turn.
    #[test]
    fn a_clean_cannot_land_inside_a_build() {
        let unlocked = clean_racing_a_build(false);
        assert_eq!(
            unlocked.unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "unlocked, the clean was expected to pull the target dir out from under the build"
        );
        clean_racing_a_build(true)
            .expect("locked, the build's write must not land in a cleaned directory");
    }

    fn clean_racing_a_build(locked: bool) -> io::Result<()> {
        let root = scratch(if locked { "race-locked" } else { "race-unlocked" });
        let target = root.join("crate/target");
        fs::create_dir_all(&target).unwrap();

        let mut kid = child(&root, if locked { "clean" } else { "clean-unlocked" });
        assert!(appeared(&root.join("cleaner-ready"), Duration::from_secs(20)));
        let guard = locked.then(|| shared(&root, "parent build"));

        fs::write(target.join("a.o"), b"a").unwrap();
        touch(&root.join("builder-mid"));
        let cleaned = appeared(&root.join("cleaner-done"), Duration::from_millis(700));
        assert_eq!(cleaned, !locked, "the clean's turn came at the wrong time");

        let outcome = fs::write(target.join("b.o"), b"b");

        drop(guard);
        assert!(kid.wait().unwrap().success());
        // Delayed, never dropped: the clean still happens, after the build.
        assert!(root.join("cleaner-done").exists());
        assert!(!target.exists());
        outcome
    }
}
