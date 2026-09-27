//! Serialising the build system's stateful phases across the builds running
//! against this repository.
//!
//! Cargo's own build lock cannot do this job. `src/build.rs`'s `invalidate_stale`
//! runs the `clean` that `cargo clean`s a crate's `target/`, and cargo's lock
//! lives inside it at `target/<profile>/.cargo-lock` — the clean deletes the
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
//! - **exclusive** — "I am replacing it": the rust bootstrap, this worktree's
//!   std build, the `cargo clean`s. One at a time, and never while a build
//!   holds the shared mode.
//!
//! And two [`Scope`]s: a crate target directory is shared by the builds in one
//! worktree, while the primary's `rust/build` — the compiler every sysroot is
//! cloned from and compiled by — is shared by every worktree at once. A build
//! holds its worktree's lock shared for its whole length and the global one not
//! at all: it compiles against its own content-addressed sysroot
//! (`src/sysroot.rs`), which nothing rewrites. Only a sysroot being *made*
//! reads the compiler, and it holds [`compiler_shared`] while it does.
//!
//! A sysroot's own lock ([`keyed_building`], [`keyed_using`]) is per key,
//! so two worktrees with different ABIs never meet in it, and two with the same
//! one build it once. A compiler a worktree's fork checkout names apart from the
//! primary's (`src/compiler.rs`) is locked the same way under its own key, and
//! neither it nor a sysroot built from it takes the global lock.
//!
//! [`integration`] is neither: one file of its own, exclusive-only, and held
//! while this host's `main` moves rather than while anything builds.
//!
//! [`guest_slot`] and [`build_slot`] are not modes of anything — they are
//! counts. The host's cores are spent by intra-suite width and by inter-worktree
//! suites alike, and nothing was handing them out, so a second suite on the
//! machine timed the first one's boots out. A guest slot is weighed in vCPUs,
//! out of one unit per host core ([`host_cores`]); a worker that is *compiling*
//! holds one and is not a guest, which is what [`build_slot`] adds.
//!
//! **A budget is one process's view, not the machine's.** A process scans the
//! units `0..budget` of its own budget: one run with `--host-slots N` below the
//! cores cannot see units past `N` held, and one above them takes units nobody
//! else counts, so the host is only as bounded as the smallest and the largest
//! budget running on it agree. And a harness built before the units were
//! weighed takes one file per guest in the same `slots/` without the
//! [`QUEUE`], so until every worktree has merged it, its guests are undercounted
//! and it overtakes a queued wide slot.
//!
//! [`full_tier`] is exclusive-only too: one full QEMU tier on the machine at a
//! time, whoever starts it. [`run`] is the same per worktree: one harness run in
//! a worktree at a time, of any kind, from its first compile to its exit.
//!
//! **Neither count reaches work that does not go through `src/build.rs`** — a
//! `toyos-sched-sim measure`, a `cargo build` typed by hand in a fork clone,
//! `./x.py` run directly in `rust/`. Those spend the same cores and are counted
//! by nothing, so a phase can still be starved with every slot honestly free;
//! what separates that from an ordinary slow one is that no `[host-slots]` or
//! `[host-builds] waiting …` line was printed.
//!
//! **The order between them is a constraint, not a preference:** tier lock →
//! run lock → host slot (guest or build) → a compiler key's lock → a sysroot
//! key's lock → the worktree build lock → the global one → artifact. The tier
//! and run locks are taken before anything is built and never while a slot is
//! held, the tier first so that a tier queued behind another worktree's keeps
//! nothing of its own worktree waiting; a build slot is taken before
//! any build lock and never while one is held; a key's lock is taken with the
//! worktree lock put down ([`Held::without_shared`]), because the key's builder
//! takes the worktree lock exclusively.
//!
//! Holder death: `flock` is released by the kernel when the open file
//! description closes, so a builder that is SIGKILLed mid-phase — routine here
//! — strands no file, which a lock file with a pid in it could not promise.
//! Established on this host (Darwin 25.5.0) rather than assumed, and
//! `killed_holder_releases_the_lock` keeps it that way. It does strand cores: a
//! SIGKILLed harness's guests run on with their units free
//! (`issues/build/a-sigkilled-harness-leaves-its-qemu-children-running.md`).

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

const LOCK_SH: i32 = 1;
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// How often a wait that is lasting repeats itself.
///
/// Shortened under `cfg(test)` so the gate on the repetition costs a second
/// instead of a minute; every process in those gates is this same binary, so
/// both sides of a wait agree on it.
#[cfg(not(test))]
const HEARTBEAT: Duration = Duration::from_secs(30);
#[cfg(test)]
const HEARTBEAT: Duration = Duration::from_millis(300);

const LOCK_DIR: &str = ".build-locks";
/// Inside the git common directory: the one place every worktree of this
/// repository names identically, and one the build system never cleans.
const GLOBAL_LOCK_DIR: &str = "toyos-build-locks";

/// The one directory every worktree of this repository names identically.
fn git_lock_dir(root: &Path) -> PathBuf {
    crate::git_common_dir(root).join(GLOBAL_LOCK_DIR)
}

/// Which shared state a phase replaces, and so which lock has to serialise it.
///
/// Stated at every call site rather than inferred, because the two are not
/// interchangeable in either direction: a toolchain phase taken in the worktree
/// scope serialises nothing across worktrees, and a target-directory clean
/// taken in the global scope stalls builds it has no business stalling.
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// State every worktree shares: the primary's `rust/` build tree — the
    /// compiler every sysroot is made with — and the machine-global rustup link.
    Global,
    /// State this worktree alone owns — its crate target directories. Two
    /// worktrees cleaning their own have nothing to say to each other.
    Worktree,
}

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
/// no clean of its crate targets lands inside it. The global lock is not held:
/// what a build reads of the shared tree is its own sysroot, which nothing
/// rewrites once it is made.
pub struct Held {
    worktree_dir: PathBuf,
    global_dir: PathBuf,
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
        global_dir: git_lock_dir(root),
        what: what.to_string(),
        guard: None,
    };
    held.guard = Some(held.take_shared());
    held
}

impl Held {
    /// Ask `decide`, and if it reports work, do that work under `scope`'s
    /// exclusive lock.
    ///
    /// `decide` runs first under the shared lock this value holds, so a phase
    /// with nothing to do costs no serialisation at all. When it does report
    /// work the shared lock is dropped, the exclusive one taken, and `decide`
    /// asked **again**: whatever it saw a moment ago may have been done by the
    /// process that held the lock in between, and only this second answer is
    /// acted on. Serialising the action alone would still double-clean.
    ///
    /// The shared lock goes down whichever scope is escalated: holding it while
    /// queueing for the global one is a deadlock with a process holding the
    /// global one that wants this worktree's.
    pub fn act_if<W>(
        &mut self,
        scope: Scope,
        phase: &str,
        decide: impl Fn() -> Option<W>,
        act: impl FnOnce(W),
    ) {
        if decide().is_none() {
            return;
        }
        let dir = match scope {
            Scope::Global => self.global_dir.clone(),
            Scope::Worktree => self.worktree_dir.clone(),
        };
        self.without_shared(|| {
            let _exclusive = acquire(&dir, LOCK_EX, phase, BUILD);
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
        acquire(&self.worktree_dir, LOCK_SH, &self.what, BUILD)
    }
}

/// This worktree's build lock, exclusively: what a sysroot build holds while it
/// writes the worktree's fork build directory, with the key's lock already held.
pub fn worktree_exclusive(root: &Path, what: &str) -> Guard {
    acquire(&root.join(LOCK_DIR), LOCK_EX, what, BUILD)
}

/// The global lock in shared mode: "I am reading the primary's compiler". A
/// sysroot build holds it from its std compile to its clone of `stage2`, so a
/// toolchain rebuild does not land inside either.
pub fn compiler_shared(root: &Path, what: &str) -> Guard {
    acquire(&git_lock_dir(root), LOCK_SH, what, BUILD)
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

/// The integration lock: one process at a time moves this host's `main`.
///
/// It used to hold a whole landing — lock, merge, gate, fast-forward.
/// GitHub does the merging now, so what is left on
/// this side is `--sync` fast-forwarding the primary checkout onto
/// `origin/main`, and that is still a tree somebody may be building in.
///
/// Its own file and not `Scope::Global`'s `state`, because a sysroot build
/// holds `state` shared for its whole length and this must not wait for one.
///
/// No `intent` beside it either. Writer preference exists because a stream of
/// shared acquirers can starve an exclusive one out of `state`; nothing takes
/// this file in shared mode at all, so there is no stream to be starved by, and
/// an `intent` here would be a file only its own exclusive holders ever touched.
pub fn integration(root: &Path) -> Guard {
    exclusive(&integration_path(root), "integration lock", "moving main")
}

fn integration_path(root: &Path) -> PathBuf {
    git_lock_dir(root).join("integration")
}

/// The tier lock: one full QEMU tier on this host at a time, across every
/// worktree.
///
/// A full tier is every worker of a suite busy for minutes, and it is the run
/// the owner rations to one per finished branch. Two at once divide the guest
/// budget between them and each measures a machine half the size its margins
/// assume, so the second waits, naming the first, whoever started either. A
/// named-test run never takes it; `testargs::is_full_tier` says which a run is.
///
/// Its own file and exclusive-only, for [`integration`]'s reason: nothing takes
/// it shared, so there is no stream of acquirers to prefer a writer over.
pub fn full_tier(root: &Path, what: &str) -> Guard {
    exclusive(&tier_path(root), "tier lock", what)
}

fn tier_path(root: &Path) -> PathBuf {
    git_lock_dir(root).join("tier")
}

/// The run lock: one harness run in this worktree at a time, from before its
/// first compile to its exit.
///
/// Two runs in one worktree share its crate targets and the C corpus's libc
/// archive, which each rebuilds and the other reads, and a full tier's lock
/// cannot keep a named run out of either. Per worktree, so two worktrees' runs
/// meet only in the machine-wide locks after it.
pub fn run(root: &Path, what: &str) -> Guard {
    exclusive(&run_path(root), "run lock", what)
}

fn run_path(root: &Path) -> PathBuf {
    root.join(LOCK_DIR).join("run")
}

/// The host cores this process may run on, read once: the guest budget, and the
/// core count every liveness ceiling that widens for an oversubscribed guest
/// divides by. One declaration, so a run pinned to a small host's count is
/// pinned in both.
///
/// `TOYOS_HOST_CORES` overrides the reading, so a large host can reproduce a
/// small one for a measurement; a value that is not a positive count is refused
/// rather than ignored. With it below a guest's width, that guest's slot is
/// refused, and `--host-slots 0` is the run that measures it.
///
/// How many vCPUs may be up at once is one per core, and that is derived, not
/// tuned. Under TCG each vCPU is one host thread, busy while its
/// vCPU runs and asleep while it halts, so a guest occupies at most its `-smp`
/// cores; guests holding one unit per core never outnumber the cores between
/// them, and none waits for a core on another guest's account. QEMU's main-loop
/// thread is not counted: it serves the vCPUs' device accesses and otherwise
/// waits.
///
/// **Builds are not in this budget, and cannot be.** A worker takes its task's
/// slot before the task compiles its kernel variant under a [`build_slot`], so a
/// build weighed in the same units would be a holder waiting for a second slot —
/// the cycle [`guest_slot`]'s rule exists to rule out. They share the cores in
/// time instead: a task compiles before it boots, so its own build runs while
/// the units it holds have no guest in them, and what builds cost above this
/// budget is bounded by [`HOST_BUILDS`], one cargo each.
pub fn host_cores() -> usize {
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CORES.get_or_init(|| match std::env::var("TOYOS_HOST_CORES") {
        Ok(pinned) => parse_cores(&pinned).unwrap_or_else(|why| panic!("{why}")),
        Err(std::env::VarError::NotPresent) => std::thread::available_parallelism()
            .unwrap_or_else(|e| panic!("the host's core count, which the guest budget is: {e}"))
            .get(),
        Err(e) => panic!("TOYOS_HOST_CORES: {e}"),
    })
}

fn parse_cores(pinned: &str) -> Result<usize, String> {
    match pinned.parse::<usize>() {
        Ok(cores) if cores >= 1 => Ok(cores),
        _ => Err(format!("TOYOS_HOST_CORES={pinned:?} is not a count of cores")),
    }
}

/// One of the host's guest slots, `vcpus` units wide, held until it drops.
///
/// A counting semaphore over `budget` lock files, because there is nothing here
/// to count with: `flock`'s shared mode admits any number of holders and reports
/// no number at all. So a unit is a file, and a slot is `vcpus` files nobody
/// else holds — which inherits the property the rest of this module rests on,
/// that a slot a SIGKILLed holder had is free the moment the process dies, with
/// no reaper, no pid file and no staleness.
///
/// **A caller holds at most one slot and never waits for a second while holding
/// one.** That is what makes the semaphore deadlock-free rather than merely
/// deadlock-free-so-far, and it is a constraint on callers: a task that needs
/// two guests takes one slot as wide as both, because two half-served tasks are
/// a cycle.
///
/// `Err` is a slot wider than the whole budget: no release can ever admit it,
/// so it is refused by name rather than queued forever.
///
/// `budget` is a parameter so a run can be told to use fewer, and so the gates
/// below can fill a host of four.
pub fn guest_slot(root: &Path, budget: usize, vcpus: usize, what: &str) -> Result<Slot, String> {
    slot(&git_lock_dir(root).join(SLOT_DIR), budget, vcpus, what, GUESTS)
}

/// A held slot: the unit files it took, given back together when it drops.
#[must_use]
pub struct Slot {
    _units: Vec<Guard>,
}

/// Its own directory under the global one: the unit files, named by index, and
/// the [`QUEUE`] in front of them.
const SLOT_DIR: &str = "slots";

/// The file an acquirer holds while it gathers its units, one acquirer at a
/// time.
const QUEUE: &str = "queue";

/// How many compiles may run on this host at once, across every worktree.
///
/// [`guest_slot`] counts vCPUs, and a compile is not one. A suite worker holds
/// a guest slot from the moment it picks a task up, and the first thing the
/// task does is build its kernel variant.
///
/// Four rather than one, because a build is not saturating for its whole
/// length — the tail of any crate graph is a single rustc — and because a bound
/// that is too generous is recoverable where one that is too tight makes every
/// agent wait on every other. It is policy, not physics: the second question of
/// any bound is what the caller sees when it is hit, and here that is a
/// `[host-builds] waiting …` line naming the holders.
pub const HOST_BUILDS: usize = 4;

/// The budget [`build_slot`] hands out, which `--host-builds N` overrides and
/// `0` turns off.
///
/// A static rather than a parameter because the callers are three functions
/// deep inside `src/build.rs` that a suite reaches through its own boot
/// machinery, and threading a number through them would put the flag in every
/// signature between here and there. Set once, before anything is compiled.
static BUILD_BUDGET: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(HOST_BUILDS);

pub fn set_host_builds(budget: usize) {
    BUILD_BUDGET.store(budget, std::sync::atomic::Ordering::Relaxed);
}

/// One of the host's build slots, held until the guard drops.
///
/// `None` is the semaphore turned off, which is the only way to measure a run
/// against one that has it.
///
/// **Taken before any build lock and never while one is held**, so the order in
/// the module header holds at every acquirer. Its own directory, and so its own
/// count: a suite holding every guest unit must not be unable to compile, and a
/// machine full of builds must not be unable to boot.
pub fn build_slot(root: &Path, what: &str) -> Option<Slot> {
    let budget = BUILD_BUDGET.load(std::sync::atomic::Ordering::Relaxed);
    let here = root.file_name().map_or_else(
        || "this worktree".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    (budget > 0).then(|| {
        let what = format!("{here}: {what}");
        slot(&git_lock_dir(root).join(BUILD_SLOT_DIR), budget, 1, &what, BUILDS)
            .unwrap_or_else(|why| panic!("{why}"))
    })
}

const BUILD_SLOT_DIR: &str = "build-slots";

/// A content-addressed product of the host, locked per key: a sysroot, or a
/// compiler a worktree's fork checkout names (`src/compiler.rs`).
#[derive(Clone, Copy)]
pub enum Keyed {
    Sysroot,
    Compiler,
}

impl Keyed {
    fn dir(self) -> &'static str {
        match self {
            Keyed::Sysroot => "sysroots",
            Keyed::Compiler => "compilers",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Keyed::Sysroot => "sysroot",
            Keyed::Compiler => "compiler",
        }
    }
}

/// Make what `key` names: exclusive, and waited for by every other process that
/// wants the same key, which then finds it made.
pub fn keyed_building(root: &Path, kind: Keyed, key: &str) -> Guard {
    let lock = format!("{} lock", kind.name());
    exclusive(&keyed_lock_path(root, kind, key), &lock, &format!("building {} {key}", kind.name()))
}

/// Use what `key` names: shared, so any number of builds use it at once, a
/// builder of it is waited for, and a sweep cannot remove it.
pub fn keyed_using(root: &Path, kind: Keyed, key: &str) -> Guard {
    let path = keyed_lock_path(root, kind, key);
    let file = open_lock_file(&path);
    if !try_lock(&file, LOCK_SH) {
        let lock = format!("{} lock", kind.name());
        let what = format!("using {} {key}", kind.name());
        let holder = describe_holder(&path)
            .unwrap_or_else(|| "held, but the holder left no readable note".to_string());
        announce(&lock, &what, &holder);
        take_lock_announcing(&file, LOCK_SH, &path, &lock, &what);
    }
    Guard { file, records_holder: false }
}

/// What `key` names, exclusively and only if nobody is making or using it: what
/// a sweep holds while it removes one.
pub fn keyed_idle(root: &Path, kind: Keyed, key: &str) -> Option<Guard> {
    let file = open_lock_file(&keyed_lock_path(root, kind, key));
    try_lock(&file, LOCK_EX).then_some(Guard { file, records_holder: false })
}

fn keyed_lock_path(root: &Path, kind: Keyed, key: &str) -> PathBuf {
    git_lock_dir(root).join(kind.dir()).join(key)
}

fn slot_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("slot-{index}"))
}

/// What a counting semaphore counts, in the words its waiting message needs.
///
/// Two counts, two directories, two prefixes: an agent reading
/// `[host-builds] waiting …` is being told something different from
/// `[host-slots] waiting …`, and the first thing it needs to know is which.
#[derive(Clone, Copy)]
struct Slots {
    tag: &'static str,
    one: &'static str,
    /// What one of the files is, in the plural.
    units: &'static str,
}

const GUESTS: Slots = Slots { tag: "host-slots", one: "guest slot", units: "vCPUs" };
const BUILDS: Slots = Slots { tag: "host-builds", one: "build slot", units: "build slots" };

/// `want` of the `budget` files in `dir`, all or none.
///
/// **One acquirer gathers at a time, and keeps what it finds.** It holds
/// [`QUEUE`] while it gathers, so it is the only process holding part of a
/// slot, and no two waiters can each hold what the other needs. Without the
/// queue a wide slot would wait forever behind narrow ones that each take a unit
/// the moment one frees — `flock`'s missing writer preference again.
///
/// The gathering polls. `flock` cannot wait on "the next of these N files to be
/// released", and a round is `budget` non-blocking syscalls against tasks that
/// run for seconds.
fn slot(dir: &Path, budget: usize, want: usize, what: &str, kind: Slots) -> Result<Slot, String> {
    assert!(want >= 1, "a {} of no {} holds nothing", kind.one, kind.units);
    if want > budget {
        return Err(format!(
            "{what} needs {want} {units} and this host's whole budget is {budget} {units}: no \
             release can ever admit it, so it is refused rather than queued",
            units = kind.units
        ));
    }
    // Quiet unless it lasts: every contended dispatch of a wide suite queues
    // here for the microseconds another gatherer takes, and a line for each is
    // noise that buries the waits that matter.
    let _head = exclusive_quietly(
        &dir.join(QUEUE),
        &format!("{} queue", kind.tag),
        &format!("{what}, gathering {want} {}", kind.units),
    );
    let mut files: Vec<Option<fs::File>> =
        (0..budget).map(|i| Some(open_lock_file(&slot_path(dir, i)))).collect();

    // Where this process starts its scan, so the units a suite's workers take
    // are spread rather than all counted from 0.
    let start = std::process::id() as usize % budget;
    let began = Instant::now();
    let mut said: Option<Instant> = None;
    let mut held: Vec<Guard> = Vec::with_capacity(want);

    loop {
        for offset in 0..budget {
            if held.len() == want {
                break;
            }
            let index = (start + offset) % budget;
            if files[index].as_ref().is_some_and(|file| try_lock(file, LOCK_EX)) {
                let file = files[index].take().expect("checked just now");
                let mut guard = Guard { file, records_holder: true };
                write_note(&mut guard.file, &note_text(what));
                held.push(guard);
            }
        }
        if held.len() == want {
            if said.is_some() {
                eprintln!(
                    "[{}] {what} got its {want} {} after {:.1?}",
                    kind.tag,
                    kind.units,
                    began.elapsed()
                );
            }
            return Ok(Slot { _units: held });
        }
        // Once when the wait starts and every half minute it lasts: an agent
        // staring at silence kills and retries, and a wait that is working
        // looks exactly like a wedge until it says so.
        if said.is_none_or(|last| last.elapsed() >= HEARTBEAT) {
            announce_slots(dir, budget, want, held.len(), what, began.elapsed(), kind);
            said = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn announce_slots(
    dir: &Path,
    budget: usize,
    want: usize,
    have: usize,
    what: &str,
    waited: Duration,
    kind: Slots,
) {
    let me = std::process::id() as i32;
    // Per holder rather than per file: one suite is one pid holding several
    // slots, and a slot is several files.
    let mut runs: Vec<(i32, String, usize)> = Vec::new();
    for index in 0..budget {
        let Some((pid, holder, _)) = read_note(&slot_path(dir, index)) else { continue };
        if pid == me && holder == what {
            continue;
        }
        match runs.iter_mut().find(|(p, h, _)| *p == pid && *h == holder) {
            Some(run) => run.2 += 1,
            None => runs.push((pid, holder, 1)),
        }
    }
    let who = if runs.is_empty() {
        "the holders left no readable note".to_string()
    } else {
        let named: Vec<String> = runs
            .iter()
            .map(|(pid, holder, n)| format!("pid {pid} ({holder}) holds {n}"))
            .collect();
        named.join(", ")
    };
    eprintln!(
        "[{}] waiting for {want} of {budget} {units} ({what}), {have} gathered, {waited:.0?} so \
         far — {who}",
        kind.tag,
        units = kind.units
    );
}

/// One lock file, taken exclusively and held until the guard drops.
///
/// For the locks with no shared mode: every holder writes the note, so a waiter
/// can always be told who it is waiting for.
fn exclusive(path: &Path, lock: &str, what: &str) -> Guard {
    let file = open_lock_file(path);
    let start = Instant::now();
    if !try_lock(&file, LOCK_EX) {
        let holder = describe_holder(path)
            .unwrap_or_else(|| "held, but the holder left no readable note".to_string());
        announce(lock, what, &holder);
        take_lock_announcing(&file, LOCK_EX, path, lock, what);
        eprintln!("[build-lock] {what} acquired after {:.1?}", start.elapsed());
    }
    let mut guard = Guard { file, records_holder: true };
    write_note(&mut guard.file, &note_text(what));
    guard
}

/// [`exclusive`] for a lock held for moments: the wait says nothing until it
/// has lasted a [`HEARTBEAT`], and then says what [`exclusive`]'s would.
fn exclusive_quietly(path: &Path, lock: &str, what: &str) -> Guard {
    let file = open_lock_file(path);
    let start = Instant::now();
    if !try_lock(&file, LOCK_EX) {
        take_lock_announcing(&file, LOCK_EX, path, lock, what);
        if start.elapsed() >= HEARTBEAT {
            eprintln!("[build-lock] {what} acquired after {:.1?}", start.elapsed());
        }
    }
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
/// queues for `state`, which makes later shared acquirers line up behind it
/// instead of overtaking it. `intent` is always taken before `state` and
/// dropped as soon as `state` is held, so nothing ever waits on `intent` while
/// holding `state`.
fn acquire(dir: &Path, op: i32, what: &str, lock: Lock) -> Guard {
    let intent_path = dir.join("intent");
    let state_path = dir.join("state");
    let intent = open_lock_file(&intent_path);
    let state = open_lock_file(&state_path);
    let label = format!("{}, {what}", if op == LOCK_EX { "exclusive" } else { "shared" });

    let start = Instant::now();
    let mut waited = false;

    if !try_lock(&intent, op) {
        announce(lock.name, &label, lock.queued_ahead);
        waited = true;
        take_lock_announcing(&intent, op, &intent_path, lock.name, &label);
    }
    if !try_lock(&state, op) {
        if !waited {
            let holder = describe_holder(&state_path)
                .unwrap_or_else(|| lock.shared_holders.to_string());
            announce(lock.name, &label, &holder);
            waited = true;
        }
        take_lock_announcing(&state, op, &state_path, lock.name, &label);
    }
    drop(intent);

    if waited {
        eprintln!("[build-lock] acquired ({label}) after {:.1?}", start.elapsed());
    }

    let records_holder = op == LOCK_EX;
    let mut guard = Guard { file: state, records_holder };
    if records_holder {
        write_note(&mut guard.file, &note_text(what));
    }
    guard
}

/// An agent staring at silence kills and retries, which is the pathology this
/// module exists to remove — so a wait says what it is waiting for and, when
/// that can be established, who has it.
fn announce(lock: &str, label: &str, holder: &str) {
    eprintln!("[build-lock] waiting for the {lock} ({label}) — {holder}");
}

/// [`take_lock`], saying every 30 s that it is still waiting and who for.
///
/// One opening line is enough for a wait of seconds and not for one of tens of
/// minutes. On 2026-08-07 eight `--land` processes queued on the integration
/// lock at once; each printed its line and then went silent for as long as the
/// seven ahead of it took, which is indistinguishable from a wedge — and an
/// agent that cannot tell a queue from a wedge kills it and retries, which puts
/// its gate back at the end of the queue.
///
/// The kernel keeps the queue and a thread does the talking: nothing here polls
/// a lock, so `flock`'s own ordering is given up nowhere. The holder is re-read
/// each time, so the message follows the queue forward rather than naming the
/// process that was in front when the wait began.
fn take_lock_announcing(file: &fs::File, op: i32, path: &Path, lock: &str, label: &str) {
    use std::sync::mpsc::{channel, RecvTimeoutError};

    // A channel and not a polled flag: this runs on every *contended*
    // acquisition, the artifact lock among them, and a flag checked every few
    // milliseconds would put that granularity on the front of each one. The
    // sender dropping wakes the thread at once and it never sleeps past the
    // acquisition.
    let (tx, rx) = channel::<()>();
    let heartbeat = {
        let path = path.to_path_buf();
        let lock = lock.to_string();
        let label = label.to_string();
        std::thread::spawn(move || {
            let began = Instant::now();
            while rx.recv_timeout(HEARTBEAT) == Err(RecvTimeoutError::Timeout) {
                let holder = describe_holder(&path)
                    .unwrap_or_else(|| "the holder left no readable note".to_string());
                eprintln!(
                    "[build-lock] still waiting for the {lock} ({label}), {:.0?} so far — {holder}",
                    began.elapsed()
                );
            }
        })
    };
    take_lock(file, op, path);
    drop(tx);
    heartbeat.join().expect("the lock heartbeat panicked");
}

/// What the last exclusive holder of `path` recorded, if it is still running.
///
/// A killed holder leaves its note behind, so the pid is checked before it is
/// named: telling a waiting agent to go look at a dead pid is worse than
/// telling it nothing. `None` is that "nothing" — what to say instead is the
/// caller's, because a lock with a shared mode is usually held by holders who
/// never wrote a note at all, and a lock without one never is.
fn describe_holder(path: &Path) -> Option<String> {
    let (pid, what, secs) = read_note(path)?;
    Some(format!("held by pid {pid} ({what}), {secs}s so far"))
}

/// The note a live holder of `path` left: its pid, what it said it was doing,
/// and how long ago it said so.
fn read_note(path: &Path) -> Option<(i32, String, u64)> {
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
    Some((pid, what.to_string(), secs))
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

fn take_lock(file: &fs::File, op: i32, path: &Path) {
    loop {
        // SAFETY: `file` owns the fd for the duration of the call and of the
        // guard the caller builds from it.
        if unsafe { flock(file.as_raw_fd(), op) } == 0 {
            return;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        panic!("build lock: flock on {}: {err}", path.display());
    }
}

fn try_lock(file: &fs::File, op: i32) -> bool {
    loop {
        // SAFETY: as in `take_lock`.
        if unsafe { flock(file.as_raw_fd(), op | LOCK_NB) } == 0 {
            return true;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return false,
            _ => panic!("build lock: flock: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::Duration;
    use toyos_tmpdir::TempDir;

    // Two processes are the point. `flock` is per open file description, so a
    // single-process test would prove nothing about the thing that actually
    // races in this tree. The child is this same test binary, re-run with one
    // `#[ignore]`d test selected by name and its role in the environment, so an
    // ordinary `cargo test` never runs the child half on its own.
    const ROLE: &str = "TOYOS_BUILDLOCK_TEST_ROLE";
    const ROOT: &str = "TOYOS_BUILDLOCK_TEST_ROOT";

    /// A git repository, because the global scope is keyed on the common
    /// directory and a scratch tree that is not one would exercise a path the
    /// build system never takes.
    fn scratch(name: &str) -> TempDir {
        let dir = TempDir::new(&format!("buildlock-{name}"));
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .expect("git init")
            .success();
        assert!(ok, "git init in {}", dir.display());
        dir
    }

    fn worktree_lock_dir(root: &Path) -> PathBuf {
        root.join(LOCK_DIR)
    }

    /// A host of two, so filling it costs two processes rather than twelve.
    const TEST_SLOTS: usize = 2;

    /// A host of four vCPUs: the narrowest on which a two-wide guest can find
    /// one unit free and still have to wait.
    const TEST_VCPUS: usize = 4;

    fn take(dir: &Path, budget: usize, want: usize, what: &str, kind: Slots) -> Slot {
        slot(dir, budget, want, what, kind).unwrap_or_else(|why| panic!("{why}"))
    }

    /// `want` of this scratch host's [`TEST_VCPUS`].
    fn take_vcpus(root: &Path, want: usize, what: &str) -> Slot {
        guest_slot(root, TEST_VCPUS, want, what).unwrap_or_else(|why| panic!("{why}"))
    }

    /// How many of this scratch host's [`TEST_VCPUS`] are held right now.
    fn units_held(root: &Path) -> usize {
        (0..TEST_VCPUS)
            .filter(|i| !try_lock(&open_lock_file(&slot_path(&slot_dir(root), *i)), LOCK_EX))
            .count()
    }

    fn slot_dir(root: &Path) -> PathBuf {
        git_lock_dir(root).join(SLOT_DIR)
    }

    fn build_slot_dir(root: &Path) -> PathBuf {
        git_lock_dir(root).join(BUILD_SLOT_DIR)
    }

    /// How many of the host's slots are held right now, asked with a fresh fd
    /// per slot for the reason [`intent_is_taken`] gives.
    fn slots_held_in(dir: &Path) -> usize {
        (0..TEST_SLOTS)
            .filter(|i| !try_lock(&open_lock_file(&slot_path(dir, *i)), LOCK_EX))
            .count()
    }

    fn slots_held(root: &Path) -> usize {
        slots_held_in(&slot_dir(root))
    }

    fn competitor(root: &Path, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "buildlock::tests::child_role", "--include-ignored", "--nocapture"])
            .env(ROLE, role)
            .env(ROOT, root);
        command
    }

    fn child(root: &Path, role: &str) -> Child {
        competitor(root, role).spawn().expect("spawn the competing process")
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
        !try_lock(&open_lock_file(&worktree_lock_dir(root).join("intent")), LOCK_SH)
    }

    fn note(root: &Path, line: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("order.log"))
            .unwrap();
        // One `write`, so two holders noting at once cannot interleave a line.
        f.write_all(format!("{line}\n").as_bytes()).unwrap();
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
                held.act_if(
                    Scope::Worktree,
                    "child exclusive phase",
                    || (!root.join("release").exists()).then_some(()),
                    |()| {
                        touch(&root.join("held"));
                        appeared(&root.join("release"), Duration::from_secs(20));
                    },
                );
            }
            "hold-exclusive-forever" => {
                let mut held = shared(&root, "child");
                held.act_if(
                    Scope::Worktree,
                    "child exclusive phase",
                    || Some(()),
                    |()| {
                        touch(&root.join("held"));
                        until_orphaned();
                    },
                );
            }
            "hold-integration" => {
                let _landing = integration(&root);
                touch(&root.join("held"));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "hold-integration-forever" => {
                let _landing = integration(&root);
                touch(&root.join("held"));
                until_orphaned();
            }
            "want-exclusive" => {
                let mut held = shared(&root, "child");
                held.act_if(Scope::Worktree, "queued exclusive phase", || Some(()), |()| note(&root, "ex"));
            }
            "want-shared" => {
                let _held = shared(&root, "child");
                note(&root, "sh");
            }
            "hold-slot" => {
                let _slot = take(&slot_dir(&root), TEST_SLOTS, 1, "a child's task", GUESTS);
                touch(&root.join(format!("held-{}", std::process::id())));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "hold-slot-forever" => {
                let _slot = take(&slot_dir(&root), TEST_SLOTS, 1, "a child's task", GUESTS);
                touch(&root.join(format!("held-{}", std::process::id())));
                until_orphaned();
            }
            "hold-sysroot-build" => {
                let _building = keyed_building(&root, Keyed::Sysroot, "k1");
                touch(&root.join("held"));
                appeared(&root.join("release"), Duration::from_secs(20));
                note(&root, "built");
            }
            "want-integration" => {
                let _landing = integration(&root);
                note(&root, "landed");
            }
            "want-sysroot" => {
                let _using = keyed_using(&root, Keyed::Sysroot, "k1");
                note(&root, "used");
            }
            "want-slot" => {
                let _slot = take(&slot_dir(&root), TEST_SLOTS, 1, "the queued run", GUESTS);
                note(&root, "got a slot");
            }
            "hold-build" => {
                let _slot = take(&build_slot_dir(&root), TEST_SLOTS, 1, "a child's build", BUILDS);
                touch(&root.join(format!("held-{}", std::process::id())));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "hold-build-forever" => {
                let _slot = take(&build_slot_dir(&root), TEST_SLOTS, 1, "a child's build", BUILDS);
                touch(&root.join(format!("held-{}", std::process::id())));
                until_orphaned();
            }
            "want-build" => {
                let _slot = take(&build_slot_dir(&root), TEST_SLOTS, 1, "the queued build", BUILDS);
                note(&root, "got a build slot");
            }
            "hold-three" => {
                let _slot = take_vcpus(&root, 3, "a three-vCPU task");
                touch(&root.join(format!("held-{}", std::process::id())));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "hold-three-forever" => {
                let _slot = take_vcpus(&root, 3, "a three-vCPU task");
                touch(&root.join(format!("held-{}", std::process::id())));
                until_orphaned();
            }
            "want-two" => {
                let _slot = take_vcpus(&root, 2, "the two-vCPU task");
                note(&root, "wide");
            }
            "want-one" => {
                let _slot = take_vcpus(&root, 1, "the one-vCPU task");
                note(&root, "narrow");
            }
            "gather-two-forever" => {
                let _slot = take_vcpus(&root, 2, "a gatherer killed mid-gather");
                until_orphaned();
            }
            "want-five" => {
                let said = match guest_slot(&root, TEST_VCPUS, TEST_VCPUS + 1, "a five-vCPU guest") {
                    Ok(_) => "admitted".to_string(),
                    Err(why) => format!("refused: {why}"),
                };
                note(&root, &said);
            }
            "hold-run" => {
                let _run = run(&root, "the first run");
                touch(&root.join("held"));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "want-run" => {
                let _run = run(&root, "the second run");
                note(&root, "second run");
            }
            "hold-tier" => {
                let _tier = full_tier(&root, "the first full tier");
                touch(&root.join("held"));
                appeared(&root.join("release"), Duration::from_secs(20));
            }
            "want-tier" => {
                let _tier = full_tier(&root, "the second full tier");
                note(&root, "second tier");
            }
            "clean" | "clean-unlocked" => {
                touch(&root.join("cleaner-ready"));
                assert!(appeared(&root.join("builder-mid"), Duration::from_secs(20)));
                let target = root.join("crate/target");
                if role == "clean" {
                    let mut held = shared(&root, "child");
                    held.act_if(
                        Scope::Worktree,
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
        let mut kid = child(&root, "hold-exclusive");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let state = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&state, LOCK_SH), "a build got in while an exclusive phase ran");
        assert!(!try_lock(&state, LOCK_EX), "two exclusive phases at once");
        let holder = describe_holder(&worktree_lock_dir(&root).join("state"))
            .expect("no holder note");
        assert!(
            holder.starts_with(&format!("held by pid {} ", kid.id())),
            "the waiting side cannot name the holder: {holder}"
        );

        touch(&root.join("release"));
        assert!(kid.wait().unwrap().success());
        drop(state);
        let _mine = shared(&root, "parent");
    }

    #[test]
    fn killed_holder_releases_the_lock() {
        let root = scratch("killed");
        let mut kid = child(&root, "hold-exclusive-forever");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let state = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&state, LOCK_EX), "the lock was not actually held");

        kid.kill().unwrap();
        kid.wait().unwrap();

        assert!(try_lock(&state, LOCK_EX), "a SIGKILLed holder stranded the lock");
        // And the note it left behind names a pid that is gone, so nobody is
        // sent to wait on it.
        assert_eq!(describe_holder(&worktree_lock_dir(&root).join("state")), None);
    }

    /// Two processes moving this host's `main` at once is what the lock stops:
    /// the primary is a checkout somebody may be building in, and `--sync`
    /// fast-forwards its tree.
    #[test]
    fn two_landings_serialise() {
        let root = scratch("integration");
        let mut kid = child(&root, "hold-integration");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let mine = open_lock_file(&integration_path(&root));
        assert!(!try_lock(&mine, LOCK_EX), "two landings held the integration lock at once");
        let holder = describe_holder(&integration_path(&root)).expect("no holder note");
        assert!(
            holder.starts_with(&format!("held by pid {} (moving main)", kid.id())),
            "the queued landing cannot name the one ahead of it: {holder}"
        );

        touch(&root.join("release"));
        assert!(kid.wait().unwrap().success());
        drop(mine);
        let _mine = integration(&root);
    }

    /// An agent kills a landing that is taking too long at least as readily as
    /// it kills a build, and a stranded integration lock wedges every worktree
    /// at once.
    #[test]
    fn a_killed_landing_releases_the_integration_lock() {
        let root = scratch("integration-killed");
        let mut kid = child(&root, "hold-integration-forever");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let mine = open_lock_file(&integration_path(&root));
        assert!(!try_lock(&mine, LOCK_EX), "the lock was not actually held");

        kid.kill().unwrap();
        kid.wait().unwrap();

        assert!(try_lock(&mine, LOCK_EX), "a SIGKILLed landing stranded the integration lock");
        assert_eq!(describe_holder(&integration_path(&root)), None);
    }

    /// The property that forced a second file. A sysroot build takes the global
    /// `state` shared for its whole length, and `--sync` must not wait for one:
    /// a landing that queued behind it would be a hang rather than a message.
    #[test]
    fn a_landing_and_a_build_do_not_exclude_each_other() {
        let root = scratch("integration-vs-build");

        let building = compiler_shared(&root, "the gate's sysroot build");
        let landing = open_lock_file(&integration_path(&root));
        assert!(try_lock(&landing, LOCK_EX), "a build in flight kept a landing out");
        drop(landing);
        drop(building);

        let _landing = integration(&root);
        let state = open_lock_file(&git_common_lock_dir(&root).join("state"));
        assert!(try_lock(&state, LOCK_SH), "a landing kept its own gate's build out");
    }

    #[test]
    fn shared_admits_shared() {
        let root = scratch("shared");
        let _mine = shared(&root, "parent");
        let second = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(try_lock(&second, LOCK_SH), "two builds cannot run at once");
        drop(second);
        let third = open_lock_file(&worktree_lock_dir(&root).join("state"));
        assert!(!try_lock(&third, LOCK_EX), "a clean got in while a build was running");
    }

    /// Two worktrees of one repository must name one global lock file and two
    /// worktree ones.
    ///
    /// Getting either half backwards is silent — every build still runs. One
    /// global file per worktree means the phases that replace the shared sysroot
    /// stop excluding each other, which is the defect worktrees were introduced
    /// without; one worktree file for all of them means a clean of a target
    /// directory stalls builds that cannot see it.
    #[test]
    fn worktrees_share_the_global_lock_and_not_the_worktree_one() {
        let root = scratch("worktrees");
        fs::write(root.join("f"), b"x").unwrap();
        git(&root, &["add", "f"]);
        git(&root, &["commit", "-qm", "init"]);
        let linked = root.join("wt");
        git(&root, &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "wt"]);

        let mine = shared(&root, "primary");
        let theirs = shared(&linked, "linked");
        assert_eq!(
            mine.global_dir, theirs.global_dir,
            "two worktrees disagree about where the global lock lives"
        );
        assert_ne!(
            mine.worktree_dir, theirs.worktree_dir,
            "two worktrees share one target-directory lock"
        );
        drop(theirs);
        drop(mine);

        // Naming one path is not yet excluding on it: `flock` conflicts between
        // open file descriptions, so a second handle on the shared file is the
        // question a second process would ask.
        let held = acquire(&root.join(LOCK_DIR), LOCK_SH, "a build in the primary", BUILD);
        let global = open_lock_file(&git_common_lock_dir(&linked).join("state"));
        assert!(
            try_lock(&global, LOCK_EX),
            "the worktree lock excluded a global phase it knows nothing about"
        );
        drop(global);
        drop(held);

        // A build compiles against its own sysroot and reads no compiler, so a
        // toolchain rebuild does not wait for it; a sysroot being made reads the
        // compiler, so the rebuild waits for that.
        let building = shared(&linked, "a build in the worktree");
        let global = open_lock_file(&git_common_lock_dir(&root).join("state"));
        assert!(try_lock(&global, LOCK_EX), "a build kept the toolchain from being rebuilt");
        drop(global);
        drop(building);
        let making = compiler_shared(&linked, "a sysroot build in the worktree");
        let global = open_lock_file(&git_common_lock_dir(&root).join("state"));
        assert!(
            !try_lock(&global, LOCK_EX),
            "a toolchain rebuild could land inside a sysroot build in another worktree"
        );
        drop(making);
    }

    fn git_common_lock_dir(root: &Path) -> PathBuf {
        crate::git_common_dir(root).join(GLOBAL_LOCK_DIR)
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} in {}", dir.display());
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

    /// **One key is built once, and two keys never meet.** A second process
    /// wanting the key being built waits on the builder's lock — not on a
    /// timer — and gets it only once the build is done; a process making
    /// another key is not held at all; and a sweep cannot take a key that
    /// somebody is making or using.
    #[test]
    fn a_key_being_built_is_waited_for_and_another_key_is_not() {
        let root = scratch("sysroot-keys");
        let mut builder = child(&root, "hold-sysroot-build");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "the builder never started");

        assert!(keyed_idle(&root, Keyed::Sysroot, "k1").is_none(), "a sweep could remove a key being built");
        let other = keyed_building(&root, Keyed::Sysroot, "k2");
        drop(other);

        let mut user = child(&root, "want-sysroot");
        assert!(
            !appeared(&root.join("order.log"), Duration::from_millis(300)),
            "a build used a sysroot while it was still being made"
        );
        touch(&root.join("release"));
        assert!(builder.wait().unwrap().success());
        assert!(user.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "built\nused\n");

        let using = keyed_using(&root, Keyed::Sysroot, "k1");
        assert!(keyed_idle(&root, Keyed::Sysroot, "k1").is_none(), "a sweep could remove a key in use");
        drop(using);
        assert!(keyed_idle(&root, Keyed::Sysroot, "k1").is_some());
    }

    /// The whole point of a counting semaphore: the run past the budget waits.
    ///
    /// Two suites on one host was not slower, it was wrong — `screen_fatal_halt`
    /// red at 11 s against 3.3 s alone, and an hour spent chasing it as a
    /// regression.
    #[test]
    fn a_full_host_makes_the_next_run_wait() {
        let root = scratch("slots-full");
        let mut holders: Vec<Child> = (0..TEST_SLOTS).map(|_| child(&root, "hold-slot")).collect();
        for kid in &holders {
            assert!(
                appeared(&root.join(format!("held-{}", kid.id())), Duration::from_secs(20)),
                "a child never took its slot"
            );
        }
        assert_eq!(slots_held(&root), TEST_SLOTS, "the host is not full");

        let mut queued = child(&root, "want-slot");
        assert!(
            !appeared(&root.join("order.log"), Duration::from_millis(400)),
            "a {TEST_SLOTS}-slot host admitted a {}th guest", TEST_SLOTS + 1
        );

        touch(&root.join("release"));
        for kid in &mut holders {
            assert!(kid.wait().unwrap().success());
        }
        assert!(queued.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "got a slot\n");
    }

    /// An agent kills a suite that is taking too long, and the host is one host:
    /// a slot its guest never gave back would shrink the machine for everybody
    /// else until the next reboot, with nothing in the tree able to notice.
    #[test]
    fn a_killed_run_gives_its_slot_back() {
        let root = scratch("slots-killed");
        let mut holders: Vec<Child> =
            (0..TEST_SLOTS).map(|_| child(&root, "hold-slot-forever")).collect();
        for kid in &holders {
            assert!(
                appeared(&root.join(format!("held-{}", kid.id())), Duration::from_secs(20)),
                "a child never took its slot"
            );
        }
        assert_eq!(slots_held(&root), TEST_SLOTS, "the host is not full");

        holders[0].kill().unwrap();
        holders[0].wait().unwrap();
        assert_eq!(slots_held(&root), TEST_SLOTS - 1, "the dead run's slot is still held");

        let start = Instant::now();
        let mine = take(&slot_dir(&root), TEST_SLOTS, 1, "the parent", GUESTS);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a SIGKILLed run stranded its guest slot"
        );
        drop(mine);
        holders[1].kill().unwrap();
        holders[1].wait().unwrap();
    }

    /// The count `guest_slot` was not: twelve workers each holding a guest slot
    /// and each running `cargo build` is twelve concurrent compiles, which is
    /// load 49.9 on fourteen cores with one guest live.
    #[test]
    fn a_full_host_makes_the_next_build_wait() {
        let root = scratch("builds-full");
        let mut holders: Vec<Child> = (0..TEST_SLOTS).map(|_| child(&root, "hold-build")).collect();
        for kid in &holders {
            assert!(
                appeared(&root.join(format!("held-{}", kid.id())), Duration::from_secs(20)),
                "a child never took its build slot"
            );
        }
        assert_eq!(slots_held_in(&build_slot_dir(&root)), TEST_SLOTS, "the host is not full");

        let mut queued = child(&root, "want-build");
        assert!(
            !appeared(&root.join("order.log"), Duration::from_millis(400)),
            "a {TEST_SLOTS}-build host admitted a {}th compile", TEST_SLOTS + 1
        );

        touch(&root.join("release"));
        for kid in &mut holders {
            assert!(kid.wait().unwrap().success());
        }
        assert!(queued.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "got a build slot\n");
    }

    /// Same argument as the guest slots': a killed build that kept its slot
    /// would shrink the machine for every worktree until the next reboot.
    #[test]
    fn a_killed_build_gives_its_slot_back() {
        let root = scratch("builds-killed");
        let mut holders: Vec<Child> =
            (0..TEST_SLOTS).map(|_| child(&root, "hold-build-forever")).collect();
        for kid in &holders {
            assert!(
                appeared(&root.join(format!("held-{}", kid.id())), Duration::from_secs(20)),
                "a child never took its build slot"
            );
        }
        assert_eq!(slots_held_in(&build_slot_dir(&root)), TEST_SLOTS, "the host is not full");

        holders[0].kill().unwrap();
        holders[0].wait().unwrap();
        assert_eq!(
            slots_held_in(&build_slot_dir(&root)),
            TEST_SLOTS - 1,
            "the dead build's slot is still held"
        );
        holders[1].kill().unwrap();
        holders[1].wait().unwrap();
    }

    /// [`child`], with its stderr handed over line by line: what a waiting
    /// process says is the event that it is waiting.
    fn child_saying(root: &Path, role: &str) -> (Child, std::sync::mpsc::Receiver<String>) {
        let mut kid = competitor(root, role)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the competing process");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let stderr = kid.stderr.take().unwrap();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        (kid, rx)
    }

    /// Whether a child said `waiting` before it ran — `false` is that it wrote
    /// `order.log` first. Whichever happens, bounded, and red if neither does.
    fn waits_before_it_runs(root: &Path, said: &std::sync::mpsc::Receiver<String>, waiting: &str) -> bool {
        use std::sync::mpsc::RecvTimeoutError;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if root.join("order.log").exists() {
                return false;
            }
            match said.recv_timeout(Duration::from_millis(5)) {
                Ok(line) if line.contains(waiting) => return true,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(5)),
            }
            assert!(Instant::now() < deadline, "the child neither ran nor said {waiting:?}");
        }
    }

    /// **A guest takes as many units as it has vCPUs.** With three of four
    /// held, a two-vCPU guest waits though a unit is free — where a count of
    /// guests would have admitted it as the second of four.
    #[test]
    fn a_guest_wider_than_what_is_left_waits() {
        let root = scratch("vcpus-wide");
        let mut holder = child(&root, "hold-three");
        assert!(
            appeared(&root.join(format!("held-{}", holder.id())), Duration::from_secs(20)),
            "the holder never took its slot"
        );

        let (mut queued, said) = child_saying(&root, "want-two");
        assert!(
            waits_before_it_runs(&root, &said, "[host-slots] waiting for 2 of 4 vCPUs"),
            "a two-vCPU guest was admitted to a {TEST_VCPUS}-vCPU host with three held"
        );
        assert_eq!(
            units_held(&root),
            TEST_VCPUS,
            "the holder's three and the one the waiter gathered are not all held"
        );

        touch(&root.join("release"));
        assert!(holder.wait().unwrap().success());
        assert!(queued.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "wide\n");
    }

    /// **A guest wider than the whole host is refused by name, at once.** No
    /// release can ever admit it, so queueing it would be a wait with no end
    /// that says only that it is waiting.
    ///
    /// Asked in a child under a deadline, because the defect it catches is a
    /// wait that never ends, and a test that waits with it reports nothing.
    #[test]
    fn a_guest_wider_than_the_budget_is_refused() {
        let root = scratch("vcpus-refused");
        let mut asker = child(&root, "want-five");
        let deadline = Instant::now() + Duration::from_secs(20);
        while asker.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                asker.kill().unwrap();
                asker.wait().unwrap();
                panic!("a {}-vCPU guest was queued on a {TEST_VCPUS}-vCPU host, not refused", TEST_VCPUS + 1);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let said = fs::read_to_string(root.join("order.log")).unwrap();
        assert!(
            !said.starts_with("admitted"),
            "a {}-vCPU guest was admitted to a {TEST_VCPUS}-vCPU host",
            TEST_VCPUS + 1
        );
        assert!(
            said.contains("a five-vCPU guest") && said.contains("needs 5 vCPUs") && said.contains("budget is 4"),
            "the refusal does not name what it refused and why: {said}"
        );
        assert_eq!(units_held(&root), 0, "a refused guest kept units");
        drop(take_vcpus(&root, TEST_VCPUS, "a guest exactly as wide as the host"));
    }

    /// **A gatherer killed mid-gather strands neither the queue nor what it
    /// had gathered.** It is the one process holding part of a slot, so its
    /// death is the one that could leave the host short for good.
    #[test]
    fn a_gatherer_killed_mid_gather_gives_back_the_queue_and_its_units() {
        let root = scratch("vcpus-gatherer-killed");
        let mut holder = child(&root, "hold-three");
        assert!(
            appeared(&root.join(format!("held-{}", holder.id())), Duration::from_secs(20)),
            "the holder never took its slot"
        );
        let mut gatherer = child(&root, "gather-two-forever");
        let queue = slot_dir(&root).join(QUEUE);
        let deadline = Instant::now() + Duration::from_secs(20);
        while units_held(&root) < TEST_VCPUS {
            assert!(Instant::now() < deadline, "the gatherer never took the free unit");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !try_lock(&open_lock_file(&queue), LOCK_EX),
            "the gatherer holds a unit and not the queue"
        );

        gatherer.kill().unwrap();
        gatherer.wait().unwrap();
        assert!(try_lock(&open_lock_file(&queue), LOCK_EX), "a killed gatherer stranded the queue");
        assert_eq!(units_held(&root), 3, "a killed gatherer stranded the unit it had gathered");

        touch(&root.join("release"));
        assert!(holder.wait().unwrap().success());
    }

    /// **Two runs in one worktree never overlap**, and the second names the
    /// first while it waits; a run in another worktree does not wait at all.
    #[test]
    fn two_runs_in_one_worktree_serialise() {
        let root = scratch("runs");
        let mut first = child(&root, "hold-run");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "the first run never took the lock");

        let elsewhere = scratch("runs-elsewhere");
        assert!(
            try_lock(&open_lock_file(&run_path(&elsewhere)), LOCK_EX),
            "a run in one worktree kept a run in another out"
        );

        let (mut second, said) = child_saying(&root, "want-run");
        let waiting = format!(
            "waiting for the run lock (the second run) — held by pid {} (the first run)",
            first.id()
        );
        assert!(
            waits_before_it_runs(&root, &said, &waiting),
            "a second run started in a worktree while the first was running, or waited without \
             naming it"
        );

        touch(&root.join("release"));
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "second run\n");
    }

    /// A pinned core count is a positive count or it is refused: a pin that
    /// was quietly ignored would measure the host it was meant to replace.
    #[test]
    fn a_pinned_core_count_is_a_count_or_refused() {
        assert_eq!(parse_cores("4"), Ok(4));
        for bad in ["0", "", "four", "-1", "4 "] {
            let why = parse_cores(bad).expect_err(bad);
            assert!(why.contains("TOYOS_HOST_CORES"), "{why}");
        }
    }

    /// **A wide slot at the head of the queue is not overtaken.** With three
    /// of four held and a two-vCPU guest queued, a one-vCPU guest that comes
    /// after it waits behind it though a unit is free — or a stream of narrow
    /// guests would keep a wide one out for as long as the stream lasted.
    #[test]
    fn a_queued_wide_slot_is_not_overtaken() {
        let root = scratch("vcpus-order");
        let mut holder = child(&root, "hold-three");
        assert!(
            appeared(&root.join(format!("held-{}", holder.id())), Duration::from_secs(20)),
            "the holder never took its slot"
        );
        let (mut wide, wide_said) = child_saying(&root, "want-two");
        assert!(
            waits_before_it_runs(&root, &wide_said, "[host-slots] waiting for 2 of 4 vCPUs"),
            "a two-vCPU guest was admitted with three of four held"
        );

        let (mut narrow, narrow_said) = child_saying(&root, "want-one");
        assert!(
            waits_before_it_runs(&root, &narrow_said, "waiting for the host-slots queue"),
            "a guest was admitted past a wider one queued ahead of it"
        );

        touch(&root.join("release"));
        assert!(holder.wait().unwrap().success());
        assert!(wide.wait().unwrap().success());
        assert!(narrow.wait().unwrap().success());
        // In either order: once released, the two fit the host together.
        let mut ran: Vec<String> =
            fs::read_to_string(root.join("order.log")).unwrap().lines().map(String::from).collect();
        ran.sort();
        assert_eq!(ran, ["narrow", "wide"]);
    }

    /// Every unit of a killed wide slot comes back, not one of them.
    #[test]
    fn a_killed_wide_run_gives_every_unit_back() {
        let root = scratch("vcpus-killed");
        let mut holder = child(&root, "hold-three-forever");
        assert!(
            appeared(&root.join(format!("held-{}", holder.id())), Duration::from_secs(20)),
            "the holder never took its slot"
        );
        assert_eq!(units_held(&root), 3, "the holder does not hold three units");
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert_eq!(units_held(&root), 0, "a SIGKILLed run stranded some of its units");
    }

    /// **Two full tiers never overlap**, and the second names the first while
    /// it waits.
    #[test]
    fn two_full_tiers_serialise() {
        let root = scratch("tiers");
        let mut first = child(&root, "hold-tier");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "the first tier never took the lock");

        let (mut second, said) = child_saying(&root, "want-tier");
        let waiting = format!(
            "waiting for the tier lock (the second full tier) — held by pid {} (the first full tier)",
            first.id()
        );
        assert!(
            waits_before_it_runs(&root, &said, &waiting),
            "a second full tier started while the first was running, or waited without naming it"
        );

        touch(&root.join("release"));
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("order.log")).unwrap(), "second tier\n");
    }

    /// **Two counts, and neither may be the other.** One directory for both
    /// would make a suite that legitimately holds every guest slot unable to
    /// compile the next kernel variant it needs — which is a deadlock, since
    /// the slot it is waiting for is one it holds itself.
    #[test]
    fn guests_and_builds_are_counted_separately() {
        let root = scratch("slots-vs-builds");
        let mut holders: Vec<Child> = (0..TEST_SLOTS).map(|_| child(&root, "hold-slot")).collect();
        for kid in &holders {
            assert!(
                appeared(&root.join(format!("held-{}", kid.id())), Duration::from_secs(20)),
                "a child never took its slot"
            );
        }
        assert_eq!(slots_held(&root), TEST_SLOTS, "the host is not full of guests");

        let start = Instant::now();
        let mine = take(&build_slot_dir(&root), TEST_SLOTS, 1, "a build on a full host", BUILDS);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a host full of guests could not compile anything"
        );
        drop(mine);

        touch(&root.join("release"));
        for kid in &mut holders {
            assert!(kid.wait().unwrap().success());
        }
    }

    /// A wait of minutes that says one line and then goes silent is
    /// indistinguishable from a wedge, and an agent kills a wedge. Eight
    /// landings queued on this lock on 2026-08-07; the ones behind saw nothing
    /// after their opening line for as long as the queue took.
    #[test]
    fn a_lasting_wait_keeps_saying_so() {
        let root = scratch("heartbeat");
        let mut holder = child(&root, "hold-integration-forever");
        assert!(appeared(&root.join("held"), Duration::from_secs(20)), "child never acquired");

        let mut queued = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "buildlock::tests::child_role", "--include-ignored", "--nocapture"])
            .env(ROLE, "want-integration")
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
                Ok(line) if line.contains("still waiting for the integration lock") => {
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
