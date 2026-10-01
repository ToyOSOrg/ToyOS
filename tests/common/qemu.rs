use std::collections::BTreeSet;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

use super::compile;
use toyos_build::arch::{Accel, Arch};
use toyos_build::tether::Tether;
use toyos_tmpdir::TempDir;

/// The architecture every machine this suite builds and boots is: the suite's
/// q35 shapes, i8042 and VT-d are x86-64's, and the aarch64 bring-up boots
/// through its own launcher ([`boot_bringup`]).
pub const SUITE_ARCH: Arch = Arch::X86_64;

/// When true, serial output is printed to stderr as it arrives.
pub static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Distinguishes every file one QEMU boot owns from every other boot's within
/// one test process — the UART log, the QMP socket, the screendump, and the
/// bootable image itself.
static BOOT_SEQ: AtomicU32 = AtomicU32::new(0);

/// The NVMe backing files live guests are holding open.
///
/// A lane reuses one image across its boots on purpose ([`super::lane`]), so
/// "one image, one guest" is an invariant this harness already believed and
/// nothing checked. QEMU checks it — it takes an exclusive `write` lock and the
/// second process exits 1 — but it checks it *after* the first one is unusable,
/// on stderr, in a sentence about locks that says nothing about which two boots
/// overlapped. This is the same claim, made before anything spawns and in the
/// harness's own words.
static NVME_HELD: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// One live guest's hold on the NVMe image it was given.
///
/// Taken before the QEMU process is spawned and released when the
/// [`QemuInstance`] is dropped — including when `wait_for_ready` panics on its
/// way out, which builds no instance to drop and so must not leave a hold
/// behind either.
pub struct NvmeClaim {
    path: PathBuf,
    /// A profile declaring no NVMe controller is handed no image: the path is
    /// `no-nvme`, it never reaches QEMU's argv, and every lane's is the same
    /// name. There is nothing to hold and nothing to conflict with.
    held: bool,
}

impl NvmeClaim {
    /// Hold `path` for a guest that is about to be launched with it.
    ///
    /// The refusal is returned rather than raised because it is what
    /// `nvme_image_is_held_by_one_guest` stages: [`QemuInstance::boot_with_options`]
    /// panics on it, since a lane whose image is already open cannot boot and
    /// there is nothing else to do about that.
    pub fn take(path: &Path) -> Result<Self, String> {
        // Decided under the lock and raised after it: a panic with the guard
        // held poisons the mutex, and one refusal would then become a refusal
        // on every later boot in the process — the shape this whole entry is
        // about.
        let refusal = {
            let mut held = NVME_HELD.lock().unwrap_or_else(|e| e.into_inner());
            match nvme_conflict(&held, path) {
                Some(why) => Some(why),
                None => {
                    held.insert(path.to_path_buf());
                    None
                }
            }
        };
        match refusal {
            Some(why) => Err(why),
            None => Ok(Self { path: path.to_path_buf(), held: true }),
        }
    }

    /// The image a profile with no controller names and never uses.
    pub fn unattached(path: &Path) -> Self {
        Self { path: path.to_path_buf(), held: false }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for NvmeClaim {
    fn drop(&mut self) {
        if self.held {
            NVME_HELD.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.path);
        }
    }
}

/// Why a boot may not open `want`, given what live guests are already holding.
///
/// Pure, and every input a parameter, so both directions can be staged without
/// a guest.
pub fn nvme_conflict(held: &std::collections::BTreeSet<PathBuf>, want: &Path) -> Option<String> {
    held.contains(want).then(|| {
        format!(
            "a live guest is still holding {}. QEMU takes an exclusive write lock on the image \
             it is given, so the second process exits 1 before it says anything and the boot \
             that waited on it panics — which is how one lost guest reported 129 tests red on \
             2026-08-17. A guest that replaces another must be built from that one's \
             `QemuInstance::shutdown`, which takes it by value; `qemu = boot()` evaluates its \
             right-hand side first and launches the replacement while the old guest is up.",
            want.display()
        )
    })
}

/// Guests this run has started, how many of them were not the shipping kernel,
/// and every distinct kernel build it asked cargo for.
///
/// A registration is not a boot — several tests boot two machines and one boots
/// four — so the count that decides whether a scheduling or build change worked
/// cannot be read off the test lists. It was static analysis until now, which
/// only ever gave a lower bound.
///
/// **The third is the one this run is judged on.** A kernel build is ~6.9 s of
/// wall clock and ~29.6 s of CPU after any edit to `kernel/`, and
/// until 2026-08-10 a full run made 45 of them. The set is what a run reports
/// and what [`declared_kernel_builds`] refuses an addition to.
///
/// **A boot that stages its own image builds nothing and counts nothing here.**
/// It used to build the image it then threw away — so the run reported a kernel
/// build no guest booted and counted the boot as one that was not the shipping
/// kernel, on a guest that was. What a boot with a staged image contributes is
/// what that image was built with, and the build that made it counted itself
/// (`qemu::build_boot_image`) at the point cargo was actually asked.
static BOOTS: AtomicU32 = AtomicU32::new(0);
static FEATURE_BOOTS: AtomicU32 = AtomicU32::new(0);
static KERNELS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// `(boots, boots that were not the shipping kernel, the kernels built)`.
pub fn boot_census() -> (u32, u32, Vec<String>) {
    (
        BOOTS.load(Ordering::Relaxed),
        FEATURE_BOOTS.load(Ordering::Relaxed),
        KERNELS.lock().expect("the kernel census").iter().cloned().collect(),
    )
}

/// The kernel builds an ordinary suite run is allowed to make, and the whole
/// list.
///
/// `""` is what an image ships. [`toyos_build::build::TEST_KERNEL`] is every
/// actuator compiled in, armed by boot parameter. An entry here is a decision
/// to pay a kernel build per suite run forever. Interactive debug mode is
/// separate: it builds [`toyos_build::build::DEBUG_KERNEL_BUILD`] and returns
/// before the suite.
pub const DECLARED_KERNEL_BUILDS: [&str; 2] =
    toyos_build::build::TEST_SUITE_KERNEL_BUILDS;

/// How many guests the phase now running may have up at once.
///
/// The harness's own wall-clock margins are margins on the *host*, and they were
/// all derived when one guest had it to itself. Four guests is a different
/// machine, so such a margin has to be stated against the regime it runs in
/// rather than widened outright — which is what this multiplies. A serial phase
/// sets it back to 1 and gets the number it always had.
static WIDTH: AtomicU32 = AtomicU32::new(1);

pub fn set_width(width: u32) {
    assert!(width >= 1, "a phase runs at least one guest");
    WIDTH.store(width, Ordering::SeqCst);
}

/// A liveness ceiling, stated for one guest and paid out for the phase's.
///
/// Every timeout a test hands [`QemuInstance::run_test`] and its relatives is a
/// guard against a wedge, never a verdict: the assertion is what the guest
/// *said*, and a test whose pass depended on a deadline expiring would be
/// asserting on the host's clock. So the number in the source stays the number
/// its author reasoned about — one guest, this host — and the phase multiplies
/// it, exactly as `wait_for_ready` has multiplied the boot timeout since the
/// parallel phase landed.
///
/// The cost of getting this wrong in the generous direction is that a wedge
/// takes longer to report. The cost in the other direction is a red run that
/// says a guest hung when it was only sharing a machine, which is the failure
/// mode that put the whole shared block in the serial tail.
///
/// This corrects for width and for how fast the host is, both host-wide facts.
/// It does not correct for a guest being wider than the host — an `smp:8` guest
/// on a four-core runner is oversubscribed and a mostly-serial boot never
/// showed it — which is [`budget_smp`]'s job and [`QemuInstance::budget`]'s
/// default. Callers that hold a guest want that one, so its ceiling reflects
/// the vCPUs it actually asked for.
pub fn budget(one_guest: Duration) -> Duration {
    let (num, den) = host_scale();
    one_guest * WIDTH.load(Ordering::SeqCst) * num / den
}

/// The fastest boot-to-ready this run has seen, in milliseconds.
///
/// A boot is the one piece of guest work every test does and no test asserts on
/// — `wait_for_ready`'s own comment names the two exceptions, and both read the
/// guest's stamps rather than this clock — so it is a measurement of the host
/// that costs nothing to take. The *fastest* rather than the mean because a boot
/// taken with three other guests up measures the phase; the minimum over a run
/// is the closest this can get to the machine with nothing else on it.
static FASTEST_BOOT_MS: AtomicU32 = AtomicU32::new(u32::MAX);

/// The same measurement on the host every ceiling in this tree was written for.
///
/// Dev host, M4 Pro, cross-arch TCG, measured 2026-08-08: the fastest of ten
/// boots at `--jobs 1` was 1433 ms against a 1433–2063 ms spread, and the
/// fastest of a whole 291-test suite at width 12 was this. The smaller of the
/// two is the one to hold, because the factor only widens and a reference set
/// too high is a correction that does not happen.
const REFERENCE_BOOT_MS: u32 = 1320;

fn record_boot(took: Duration) {
    let ms = took.as_millis().min(u32::MAX as u128) as u32;
    FASTEST_BOOT_MS.fetch_min(ms, Ordering::SeqCst);
}

/// How much slower than the host these ceilings were written on this one is, as
/// a fraction so that a 1.4× host is not rounded to 1.
///
/// [`budget`] corrects a ceiling for how many guests share the machine. It never
/// corrected for how fast the machine *is*, and that is the other half of the
/// same mistake: a number reasoned about on an M4 Pro is not a liveness ceiling
/// on a four-core Azure vCPU, it is a verdict about which of the two is running
/// the test. 307 bare timeouts were counted in one CI run and every one of
/// them was that.
///
/// **Only ever upward.** On a faster host the number in the source stands,
/// because it is the number its author reasoned about, and a ceiling that shrank
/// would start reporting wedges that are not there. The ceiling of 8 is because
/// one anomalous boot must not be able to disable every liveness guard in the
/// suite at once.
fn host_scale() -> (u32, u32) {
    let fastest = FASTEST_BOOT_MS.load(Ordering::SeqCst);
    // Before the first boot there is no measurement, and the sentinel must not
    // read as the slowest host imaginable.
    if fastest == u32::MAX || fastest <= REFERENCE_BOOT_MS {
        return (1, 1);
    }
    (fastest.min(REFERENCE_BOOT_MS * 8), REFERENCE_BOOT_MS)
}

/// The fastest boot seen, the reference, and the scale it produced — for a run's
/// own report, because the number in the source is no longer the number that was
/// enforced.
pub fn host_speed() -> (Option<u32>, u32, u32, u32) {
    let (num, den) = host_scale();
    let fastest = FASTEST_BOOT_MS.load(Ordering::SeqCst);
    ((fastest != u32::MAX).then_some(fastest), REFERENCE_BOOT_MS, num, den)
}

/// The host cores this process may run on, read once.
///
/// [`std::thread::available_parallelism`] is the Rust-native reading of what
/// `cargo run -- --ci`'s instrument line prints as `N core(s)`: it needs no host binary and
/// respects any affinity the runner imposed. The CI `guest` shard is a
/// four-core AMD EPYC; the dev host has fourteen, and that gap is the whole of
/// why the oversubscription factor below widens a ceiling on the runner and is
/// a no-op locally.
///
/// `TOYOS_HOST_CORES` overrides the reading, and only ever downward in effect —
/// it exists so a large host can reproduce a small one's oversubscription for a
/// measurement, and so a run can pin the number a verdict was read against. It
/// can only *widen* a liveness ceiling, never shorten one, so a stale value
/// costs a slower wedge report and never a false pass.
pub fn host_cores() -> u32 {
    static CORES: AtomicU32 = AtomicU32::new(0);
    let cached = CORES.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let detected = std::env::var("TOYOS_HOST_CORES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
        });
    CORES.store(detected, Ordering::Relaxed);
    detected
}

/// How much an `smp`-vCPU guest is oversubscribed on `cores` host cores, as a
/// fraction.
///
/// The derivation, and nothing tuned: `smp` vCPU threads time-sharing `cores`
/// cores each get `cores/smp` of a core, so a vCPU-bound stretch of guest work
/// takes `smp/cores` as long as the same work with a core per vCPU. When
/// `smp <= cores` there is no oversubscription and the factor is exactly 1 —
/// the guest is not competing with itself. So the factor is `vcpus/cores`, and
/// on the four-core runner an eight-vCPU guest is `8/4 = 2`.
pub(crate) fn oversub_ratio(smp: u32, cores: u32) -> (u32, u32) {
    if smp > cores {
        (smp, cores)
    } else {
        (1, 1)
    }
}

/// [`oversub_ratio`] for this host's core count.
///
/// [`host_scale`] corrects a ceiling for how slow this host's *boot* is, and a
/// boot is mostly one CPU — the BSP does the bring-up while the APs idle — so
/// the boot-derived factor is honest for a single-threaded workload and blind
/// to the one thing a wide-SMP guest pays that a boot never does. This is the
/// other half of the same correction, keyed on `vcpus/cores`: 307 bare
/// timeouts in one CI run and green alone were the boot-scaled ceiling
/// undercounting a starved-but-progressing eight-on-four guest.
fn oversubscription(smp: u32) -> (u32, u32) {
    oversub_ratio(smp, host_cores())
}

/// [`budget`] widened by a guest's own vCPU oversubscription.
///
/// The guest-agnostic [`budget`] scales by phase width and boot-derived host
/// speed; this multiplies in `smp/cores` on top, so a wide-SMP guest that a
/// mostly-serial boot said little about is given the extra room the derivation
/// above says it needs. `smp <= cores` leaves it exactly [`budget`], which is
/// every guest on the dev host.
pub fn budget_smp(one_guest: Duration, smp: u32) -> Duration {
    let (onum, oden) = oversubscription(smp);
    budget(one_guest) * onum / oden
}

/// A liveness guard that watches the guest instead of the host's clock.
///
/// [`budget`] corrects a ceiling for how many guests share the machine, which
/// is the part of "how fast is the host today" the harness knows. It does not
/// know the rest, and a retry loop bounded by elapsed time has that ceiling for
/// a *verdict* the moment the rest moves: a guest that is merely late reports
/// exactly what a wedged one reports.
///
/// The two are distinguishable and the console is what distinguishes them: a
/// guest still printing is a guest still working. So the ceiling here is time in
/// which **nothing arrived**, and a guest that keeps talking is given as long as
/// it needs. That is the whole idea — no number in this type is a statement
/// about the host.
///
/// `total` is the second half, and it is a wedge guard rather than a verdict
/// too. A guest can be stuck and chatty: the compositor prints an interval line
/// every two seconds whatever else has stopped, so silence alone cannot end a
/// desktop loop and a suite that never ends is worse than one that reds.
///
/// The caller owns the capture, so progress is "did it grow" and costs nothing.
pub struct Liveness {
    quiet_for: Duration,
    last_growth: Instant,
    seen: usize,
    give_up: Instant,
}

impl Liveness {
    /// `quiet_for` of silence ends the wait, and so does `total` however loud
    /// the guest is.
    pub fn new(quiet_for: Duration, total: Duration) -> Self {
        let now = Instant::now();
        Self { quiet_for, last_growth: now, seen: 0, give_up: now + total }
    }

    /// Whether the guest may still be working, given everything it has said.
    pub fn working(&mut self, capture: &str) -> bool {
        if capture.len() != self.seen {
            self.seen = capture.len();
            self.last_growth = Instant::now();
        }
        let now = Instant::now();
        now < self.give_up && now.duration_since(self.last_growth) < self.quiet_for
    }

    /// What ended the wait, for a caller putting it in a failure message.
    pub fn why(&self) -> &'static str {
        if Instant::now() >= self.give_up {
            "it never stopped talking and never got there"
        } else {
            "it went quiet"
        }
    }
}

/// How the reason line begins when what expired was a **guard** and not an
/// assertion.
///
/// A test that ran out of time has not found the guest doing the wrong thing;
/// it has found nothing at all, and the two readings send an agent to opposite
/// places. `screen_pager_keys` reporting `0 page moves over 30 keystrokes`
/// after 0.3 s was bisected as a kernel regression twice in one day by two
/// agents, and the fact it was hiding is that the whole run had collapsed
/// before the guest could answer once.
///
/// Still red. A guest that stopped answering may have stopped for a reason this
/// tree owns, and a status that is not a failure is a status nobody reads. What
/// this buys is that the summary says which of the two kinds of red it is.
///
/// It lives here rather than beside the classifier because [`Liveness`] is what
/// produces the evidence for it, and [`QemuInstance::run_test_paced`] is a
/// second producer: a test's own ceiling is a guard of exactly this kind.
pub const STALLED: &str = "STALLED:";

/// The backstop's red: a guest still talking past [`GUEST_WEDGED`] that never
/// finished. The ceiling too, and counted with [`STALLED`]'s.
pub const TIMED_OUT: &str = "timed out after";

/// How long a guest may say nothing before a wait on it is a stall.
///
/// Every config these waits run on has something on a periodic interval — the
/// compositor's frame batch and soundd's stats window are both about 2 s — so
/// silence here is a machine that has stopped rather than a machine that is
/// thinking. It is not a verdict about any of them: no assertion in this suite
/// is satisfied by the guest merely talking.
///
/// The other side of that coin: on a shared boot the kernel itself is one of
/// the periodic speakers, on a 10 s cadence, so a wait whose predicate never
/// comes true is never ended by this bound — the guest keeps talking, and the
/// wait runs the whole of [`GUEST_WEDGED`].
pub const GUEST_QUIET: Duration = Duration::from_secs(15);

/// The other end of the same guard: a guest can be stuck and chatty.
///
/// The compositor prints its interval line whatever else has stopped, so
/// silence alone cannot end a desktop wait, and a suite that never ends is
/// worse than one that reds.
pub const GUEST_WEDGED: Duration = Duration::from_secs(300);

pub fn guest_liveness() -> Liveness {
    Liveness::new(GUEST_QUIET, GUEST_WEDGED)
}

/// A kernel line without its `[kernel <t> cpu<N>] ` stamp.
fn without_stamp(line: &str) -> &str {
    if !is_kernel_line(line) {
        return line;
    }
    line.split_once("] ").map_or(line, |(_, rest)| rest)
}

/// The sentence a wait gives when what stopped the guest is on the console.
pub(crate) fn kernel_died_here(line: &str) -> String {
    format!(
        "kernel panic: {} — the guest went quiet because every CPU is halted, not because it \
         was still working. The panic is the finding and the guard never got to be one.",
        without_stamp(line.trim())
    )
}

/// The heading a verdict puts the guest's own account under.
///
/// One spelling, so an issue file and a CI log both quote the same words when
/// they quote a report.
pub const DIED_SAYING: &str = "--- what the kernel said as it died ---";

/// The heading a verdict puts a never-announced test's window under.
pub const NEVER_ANNOUNCED: &str =
    "--- the guest never announced this test; the window it was given ---";

/// How many of that window's lines the verdict carries.
const WINDOW_LINES: usize = 40;

/// Why a wait ended badly, carrying the guest's own account of it.
///
/// **A newtype, because what this closes is an omission and an omission cannot
/// be gated by review.** Fifty-two sites in this suite format
/// [`TestResult::error`] and thirty-six of them printed no capture beside it
/// (counted on the tree, 2026-08-18), and on
/// 2026-08-18 that is what a `DOUBLE FAULT on CPU 1` cost — the wait named the
/// death in one sentence, the kernel's report sat in `TestResult::serial`, and
/// the arm printed `stdout`
/// (`issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md`). Fixing
/// the arms would have fixed the arms. What is fixed here is that the sentence
/// cannot be built without the capture: [`Self::new`] is the only constructor
/// there is and the capture is one of its two arguments, so a wait that reports
/// a kernel death and no report is not expressible.
///
/// It carries nothing when the capture carries no kernel death, and that half
/// matters as much: an ordinary ceiling on a live guest, or a guest binary
/// reporting its own error, must not start pasting a boot's serial log into
/// somebody's terminal.
#[derive(Clone, Debug)]
pub struct WaitVerdict(String);

impl WaitVerdict {
    /// The sentence a wait reached, and the capture it reached it on.
    ///
    /// `capture` is the window in the order the guest wrote it, because the
    /// first kernel death in it is the one this verdict is about. An empty slice is a claim that there
    /// was no capture at all, and it is a visible one rather than an omission.
    pub fn new(sentence: String, capture: &[&str]) -> Self {
        let Some(report) = capture.iter().find_map(|c| super::serial::death_report(c)) else {
            return Self(sentence);
        };
        Self(format!("{sentence}\n{DIED_SAYING}\n{report}"))
    }

    /// The same, for a test that may never have announced itself.
    ///
    /// **A test whose `===TEST_START` never arrived has an empty `serial` by
    /// construction**, so `before` is the only record the boot left.
    /// [`Self::new`]'s silence on a capture nothing died in holds everywhere
    /// else.
    pub fn for_test(sentence: String, before: &str, serial: &str, started: bool) -> Self {
        let verdict = Self::new(sentence, &[before, serial]);
        if started || verdict.0.contains(DIED_SAYING) || before.trim().is_empty() {
            return verdict;
        }
        let lines: Vec<&str> = before.lines().collect();
        let kept = lines.len().min(WINDOW_LINES);
        let head = if lines.len() > kept {
            format!("(the last {kept} of the {} lines in it)\n", lines.len())
        } else {
            String::new()
        };
        let tail = lines[lines.len() - kept..].join("\n");
        Self(format!("{}\n{NEVER_ANNOUNCED}\n{head}{tail}", verdict.0))
    }
}

impl std::fmt::Display for WaitVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a test's ceiling caught — the panic, the stall, or the slow test.
///
/// `dying` is the line on which the kernel said it was dying, if it ever did,
/// and `quiet` is how long the guest has said nothing. **The first arm is the
/// whole point.** A Rust `panic!` in the kernel prints `PANIC:` and then
/// `halt_all_cpus` stops every CPU, so the guest goes silent and the ceiling
/// expires on a machine that has been dead since the panic.
///
/// **The wall clock is not the wedge; silence is.** A test's `ceiling` is the
/// budgeted wall clock (`budget_smp`-scaled, so it already carries #256's
/// `vcpus/cores` oversubscription widening), and until this it ended the wait
/// the instant it passed — so a merely-slow guest reported exactly what a wedged
/// one did. `launcher_refusals` was killed at `192s "still talking 1s ago"` on a
/// loaded `smp:2` runner its `vcpus/cores` factor clamps to 1, a guest making
/// steady progress called wedged by a clock.
///
/// **`elapsed > ceiling` stays a necessary condition, and that is what keeps
/// this safe.** Silence alone is not a wedge on this suite's boots: a healthy
/// but idle guest on a config with no live periodic speaker — no compositor, an
/// idle soundd, and the kernel's own ~10 s line halting with the idle loop — was
/// measured quiet for as long as 102 s, so a guard that fired on 15 s of silence
/// by itself would red a working machine. A guest's own budget is what says how
/// long its silence is allowed; only past *that* does quiet mean stopped.
pub fn ceiling_verdict(
    dying: Option<&str>,
    elapsed: Duration,
    ceiling: Duration,
    quiet: Duration,
    lines: usize,
) -> Option<String> {
    if let Some(line) = dying {
        if quiet >= GUEST_QUIET {
            return Some(kernel_died_here(line));
        }
    }
    // The per-test ceiling, now a silence guard rather than a wall-clock one: it
    // ends the wait only when the guest has run past its budget *and* fallen
    // silent for [`GUEST_QUIET`]. A guest still talking past its budget is slow,
    // not wedged, and is given until the backstop.
    if elapsed > ceiling && quiet >= GUEST_QUIET {
        return Some(format!(
            "{STALLED} {}s of guard expired, and the guest had said nothing for the last \
             {quiet:.0?} of it — the ceiling caught a machine that had stopped, which is not an \
             answer to what this test asked",
            ceiling.as_secs()
        ));
    }
    // The absolute backstop, for a guest that is stuck *and* chatty and so never
    // trips the silence guard — a suite that never ends is worse than one that
    // reds. Never below the per-test ceiling, so a long test whose own budget
    // already exceeds it is not cut short; never below [`GUEST_WEDGED`], the
    // vetted stuck-and-chatty number a talking guest is judged by everywhere
    // else. Not itself oversubscription-scaled — `ceiling` already carries that.
    let backstop = ceiling.max(GUEST_WEDGED);
    if elapsed > backstop {
        return Some(format!(
            "{TIMED_OUT} {}s, with the guest still talking {quiet:.0?} ago ({lines} \
             console line(s) while it ran) — it was working and did not finish",
            backstop.as_secs()
        ));
    }
    None
}

/// Collect console output until `done` reads true of the whole capture, or the
/// guest stops making progress.
///
/// The shape [`QemuInstance::drain_serial`] cannot have: its caller passes a
/// number of seconds, and a number of seconds is a claim about the host. Here
/// the wait ends when the guest goes quiet or wedges, so a guest with a twelfth
/// of the machine costs the run wall clock and never a verdict — and when it
/// does end early the message says so in the words the classifier reads
/// ([`STALLED`]).
///
/// `doing` is what the guest was asked to do, in the caller's own words. The
/// caller keeps its assertion; what this owns is the difference between "it did
/// the wrong thing" and "it never got there".
pub fn await_guest(
    qemu: &mut QemuInstance,
    log: &mut String,
    doing: &str,
    done: impl Fn(&str) -> bool,
) -> Result<(), String> {
    // Where this wait's own evidence starts.
    let from = log.len();
    let mut live = guest_liveness();
    while !done(log) && live.working(log) {
        let more = qemu.drain_serial(Duration::from_millis(200));
        log.push_str(&more);
    }
    if done(log) {
        return Ok(());
    }
    // **The third wait, asking the one question the other two ask.** A guest
    // that halted every CPU went quiet for a reason it wrote down first, and
    // `it went quiet` is that reason thrown away — which is the shape #156's
    // whole signature is stated in (`a total freeze of the guest`, judged by a
    // periodic line that stopped arriving), so what this says decides how the
    // next occurrence is read.
    let since = &log[from..];
    if let Some(line) = super::serial::kernel_death(since) {
        // Through [`WaitVerdict`] for the reason that type exists: this caller
        // owns the capture and usually prints it, and `usually` is what the
        // arms that do not have in common with the one that lost a double
        // fault's report.
        return Err(WaitVerdict::new(
            format!("{} It was waiting for {doing}", kernel_died_here(line)),
            &[since],
        )
        .to_string());
    }
    Err(format!("{STALLED} waiting for {doing} — {}", live.why()))
}

/// [`await_guest`] for the common case: one marker anywhere in the capture.
pub fn await_marker(
    qemu: &mut QemuInstance,
    log: &mut String,
    marker: &str,
    doing: &str,
) -> Result<(), String> {
    await_marker_new(qemu, log, marker, 0, doing)
}

/// [`await_marker`] over what arrives after `from`.
///
/// For a marker a test asks for more than once: a whole-capture scan answers
/// the second ask with the first ask's line and carries on against a guest that
/// has not done the thing yet.
pub fn await_marker_new(
    qemu: &mut QemuInstance,
    log: &mut String,
    marker: &str,
    from: usize,
    doing: &str,
) -> Result<(), String> {
    await_guest(qemu, log, doing, |log| log[from.min(log.len())..].contains(marker))
}

/// `wfi`, and the branch back to the instruction before it: the loop every
/// copy of the kernel's `arch::cpu::halt` compiles to on AArch64.
const WFI: u32 = 0xd503_207f;
const BACK_TO_WFI: u32 = 0x17ff_ffff;

/// Per vCPU in `info registers -a`, whether it is halted with interrupts off.
///
/// x86-64: `HLT=1` with `IF` clear, the stop's `cli; hlt`. An idle CPU halts
/// with `IF` set, and a running one is not halted, so neither is this.
///
/// AArch64: QEMU prints no halt, and an idle CPU waits in `wfi` with `I` set
/// too (`arch::hw::halt`), so the wait is told apart by where it is: at EL1
/// with `PSTATE.I` set, a `wfi` just behind the PC (QEMU's halted PC is the
/// instruction after it) and [`BACK_TO_WFI`] at it, where an idle CPU's next
/// instruction unmasks instead.
pub fn stopped_cpus(monitor: &mut QmpMonitor, arch: Arch) -> Vec<bool> {
    let registers = monitor.human("info registers -a");
    registers
        .split("CPU#")
        .skip(1)
        .map(|cpu| {
            let field = |name: &str| -> Option<u64> {
                let digits: String = cpu.split(name).nth(1)?.chars().take_while(char::is_ascii_hexdigit).collect();
                u64::from_str_radix(&digits, 16).ok()
            };
            match arch {
                Arch::X86_64 => field("HLT=") == Some(1) && field("RFL=").is_some_and(|f| f & 1 << 9 == 0),
                Arch::Aarch64 => {
                    let (Some(pc), Some(pstate)) = (field(" PC="), field("PSTATE=")) else {
                        return false;
                    };
                    let (el, masked) = (pstate >> 2 & 3, pstate & 1 << 7 != 0);
                    el == 1 && masked && pc.checked_sub(4).is_some_and(|at| words(monitor, at) == [WFI, BACK_TO_WFI])
                }
            }
        })
        .collect()
}

/// The two 32-bit words at guest virtual address `at`, as the monitor's CPU
/// translates it; none where it cannot.
fn words(monitor: &mut QmpMonitor, at: u64) -> Vec<u32> {
    let dump = monitor.human(&format!("x/2wx {at:#x}"));
    let Some((_, words)) = dump.split_once(':') else { return Vec::new() };
    words.split_whitespace().filter_map(|word| u32::from_str_radix(word.strip_prefix("0x")?, 16).ok()).collect()
}

/// The hardware shape QEMU presents to the guest.
///
/// Not a display setting: each variant is a whole machine. `Headless` is the
/// historical test config -- no VGA and no GPU device at all, so firmware
/// publishes no GOP and `kernel_args.gop_framebuffer` is zero. `Gop` swaps in
/// `-vga std` so firmware publishes a linear framebuffer, which is the path a
/// laptop takes and the only one in which the on-screen panic console renders
/// anything. `Metal` goes the whole way to the target laptop's shape.
#[derive(Clone, Copy, PartialEq)]
pub enum Profile {
    Headless,
    Gop,
    /// M1 metal-sim: GOP, NVMe, xHCI with the boot stick on it, i8042 from
    /// q35, and nothing else -- no virtio device and no USB HID. This is the
    /// machine shape that gets flashed, so it is the one the input tests run
    /// on. The 16550 stays: every defect metal-sim has found came from the
    /// device shape, and with a console the guest can be driven over the
    /// ===TEST_START=== protocol like any other. [`BootOptions::mute`] takes
    /// it away for the one test that certifies the T14's literal shape.
    Metal,
    /// QEMU `virt` on AArch64 (GICv3, AAVMF): a GOP from `ramfb`, the boot
    /// stick on an xHCI, the PL011, and nothing else — no virtio, NIC, NVMe or
    /// IOMMU. The machine the AArch64 port reaches its console on, and the only
    /// profile that is not a q35.
    Virt,
    /// [`Profile::Virt`] with EL2 (`virtualization=on`), emulated on `-cpu max`:
    /// firmware then hands the loader the CPU at EL2, and the kernel's entry
    /// has to drop from it. HVF gives a guest EL1 only.
    VirtEl2,
    /// [`Profile::Virt`] emulated on `-cpu max` whatever the host: firmware
    /// hands the loader the CPU at EL1, as HVF does, and QEMU's FADT names
    /// PSCI's conduit `HVC`; unlike HVF's, the CPU has RNDR for the kernel's
    /// hash seed.
    VirtTcg,
}

impl Profile {
    /// The architecture this machine is.
    pub fn arch(self) -> Arch {
        match self {
            Self::Virt | Self::VirtEl2 | Self::VirtTcg => Arch::Aarch64,
            Self::Headless
            | Self::Gop
            | Self::Metal => Arch::X86_64,
        }
    }

    /// How this host provides the machine.
    pub fn accel(self) -> Accel {
        match self {
            Self::VirtEl2 | Self::VirtTcg => Accel::Tcg,
            _ => self.arch().accel(),
        }
    }
}

/// The vIOMMU a profile puts on the machine.
///
/// A whole machine dimension rather than a flag: the unit is what decodes
/// every DMA and every interrupt message on the bus. Two fields, because two
/// are what a guest can tell apart — `aw_bits` moves `CAP.SAGAW`, `intremap`
/// moves `ECAP.IR` and the DMAR's `INTR_REMAP` flag — and a harness that
/// stages one value of each cannot distinguish a kernel that reads those
/// registers from one that prints what it expected to find.
///
/// `caching-mode` is deliberately not a field. It is on everywhere: it is the
/// stricter configuration, it is the only one QEMU can stage, and the kernel
/// refuses to branch on it — so a profile that
/// turned it off would be staging a machine no code here distinguishes.
#[derive(Clone, Copy, PartialEq)]
pub struct Iommu {
    /// `aw-bits`. QEMU 11.0.2 takes 39 or 48 and nothing else.
    pub aw_bits: u8,
    /// Interrupt remapping. Off is a platform declaring it cannot remap.
    pub intremap: bool,
    /// `ECAP.EIM`. QEMU's default is `auto`, which resolves to off without an
    /// in-kernel irqchip and so on every guest this host boots.
    pub eim: bool,
}

/// What every profile but the four that vary it declares: the widest address
/// width QEMU offers and interrupt remapping on.
pub const IOMMU_DEFAULT: Iommu = Iommu { aw_bits: 48, intremap: true, eim: false };

/// The controller every profile but [`Profile::MetalUsb`] gets. `nec-usb-xhci`
/// registers `MAX(p2, p3)` attachable USB ports over `p2 + p3` port registers —
/// the two ranges are two speed-specific views of the same ports, not two sets
/// of them — so the default `p2=4,p3=4` takes **four** devices, two short of the
/// crowded set rather than one.
const XHCI_DEFAULT: &str = "nec-usb-xhci,id=xhci";

/// Whether a machine has the virtio console and sound block. Which NIC it has
/// is [`Nic`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Virtio {
    Absent,
    Present,
}

impl Virtio {
    fn present(self) -> bool {
        self != Self::Absent
    }

    fn sound(self) -> bool {
        self == Self::Present
    }
}

/// The network card a machine has, and whether it can raise an interrupt.
///
/// A dimension of its own and not a field of [`Virtio`], because the machine
/// this project targets has an Intel NIC and no virtio device at all.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nic {
    Absent,
    Virtio,
}

/// Everything a profile decides about the machine, in one table. A new
/// variant answers every question here or does not compile — which `self !=
/// Profile::Metal` did the opposite of: it handed anything that was not
/// literally Metal the whole virtio block, a USB keyboard and a console.
struct Shape {
    /// `-vga` mode. "none" leaves firmware with no GOP to publish.
    vga: &'static str,
    /// The resolution this display's EDID advertises, which decides the panel:
    /// firmware sets that mode and the bootloader inherits it. `None` is
    /// QEMU's own default EDID, [`DEFAULT_PANEL`]. Declared because the panel's
    /// *size* is a shape dimension exactly as a disk's is, and the tests that
    /// read pixels were all blind to the remainder until one profile had one.
    panel: Option<(u32, u32)>,
    /// virtio-sound and the console on virtio-serial.
    virtio: Virtio,
    nic: Nic,
    /// The `-device` argument for each xHCI controller, port and slot counts
    /// included. A list because a machine can have more than one and the T14
    /// does — its keyboard is on the second.
    xhci: &'static [&'static str],
    /// Every USB device besides the boot stick, each naming its own bus.
    /// Absence is what makes an i8042 test measure anything: QEMU activates
    /// one input handler per device class, so with a usb-kbd present every
    /// injected keystroke goes to it.
    usb: &'static [&'static str],
    /// The NVMe namespace's size. The backing file is sparse, so this is free
    /// to state honestly — and it has to be stated, because a kernel
    /// structure sized per device block is bounded by this number and by
    /// nothing else.
    nvme_bytes: u64,
    /// The unit that decodes this machine's DMA, or its absence. Stated per
    /// profile because absence is a shape and because the unit's own
    /// capabilities are what the kernel reads at boot.
    iommu: Option<Iommu>,
}

/// The boot stick's device id: the removal the owner's machine dies on is the
/// one device whose removal takes `/boot` and `/log` with it.
pub const BOOT_STICK_ID: &str = "bootstick";

/// The boot stick's serial number string. Stated rather than left to QEMU,
/// whose default is built from the port the device is on, so the same stick
/// plugged into another port would read as another unit — which is exactly
/// what a test moving it has to be able to say is not so.
pub const BOOT_STICK_SERIAL: &str = "TOYOS0BOOTSTICK1";

/// What every x86-64 profile gives the guest. Large enough for a filesystem,
/// small enough that a boot formats it quickly.
pub const NVME_SMALL: u64 = 128 * 1024 * 1024;

impl Profile {
    fn shape(self) -> Shape {
        match self {
            Self::VirtEl2 | Self::VirtTcg => Self::Virt.shape(),
            Self::Virt => Shape {
                vga: "std",
                panel: None,
                virtio: Virtio::Absent,
                nic: Nic::Absent,
                xhci: &[XHCI_DEFAULT],
                usb: &[],
                nvme_bytes: 0,
                iommu: None,
            },
            Self::Headless => Shape {
                vga: "none",
                panel: None,
                virtio: Virtio::Present,
                nic: Nic::Virtio,
                xhci: &[XHCI_DEFAULT],
                usb: &["usb-kbd,bus=xhci.0"],
                nvme_bytes: NVME_SMALL,
                iommu: Some(IOMMU_DEFAULT),
            },
            Self::Gop => Shape {
                vga: "std",
                panel: None,
                virtio: Virtio::Present,
                nic: Nic::Virtio,
                xhci: &[XHCI_DEFAULT],
                usb: &["usb-kbd,bus=xhci.0"],
                nvme_bytes: NVME_SMALL,
                iommu: Some(IOMMU_DEFAULT),
            },
            Self::Metal => Shape {
                vga: "std",
                // The T14's panel, advertised the way the laptop's is: 240x67
                // cells with 8 pixels left over at the bottom, which is the
                // geometry the machine actually has and the one no default
                // expresses.
                panel: Some((1920, 1080)),
                virtio: Virtio::Absent,
                nic: Nic::Absent,
                xhci: &[XHCI_DEFAULT],
                usb: &[],
                nvme_bytes: NVME_SMALL,
                iommu: Some(IOMMU_DEFAULT),
            },
        }
    }
}

pub struct BootOptions {
    pub gdb_stub: bool,
    pub debug_wait: bool,
    pub smp: u32,
    pub profile: Profile,
    /// Open a per-instance QMP socket, which `screendump` needs. Per-instance
    /// because screen tests boot their own QEMU and several may exist at once.
    pub qmp: bool,
    /// Which of [`DECLARED_KERNEL_BUILDS`] this boot wants, and empty for the
    /// kernel an image ships. Only a test whose subject *is* a build sets it;
    /// everything else names an actuator in [`BootOptions::kernel_params`]
    /// instead.
    pub kernel_features: &'static [&'static str],
    /// The actuators this boot arms, by the names `kernel/src/actuator.rs`
    /// declares. Non-empty selects the test kernel, which carries all of them.
    pub kernel_params: &'static [&'static str],
    /// Take the 16550 away, leaving the framebuffer as the guest's only
    /// channel out. Only [`Profile::Metal`] may set it -- the others carry
    /// their console on it or on virtio-serial. A muted guest has no marker
    /// to wait for and no `run_test` to drive, so it is observed with
    /// [`QemuInstance::screendump_while`] and nothing else.
    pub mute: bool,
    /// The console line that means the boot reached the state under test.
    /// Anything other than [`DEFAULT_READY`] also declares that a panic is the
    /// expected outcome rather than a boot failure -- the early-panic screen
    /// test never reaches userland at all. Ignored when [`BootOptions::mute`]
    /// is set, which leaves no console for a marker to arrive on.
    pub ready_marker: &'static str,
    /// Files put on ROOT beside the image's own, each named by its
    /// ROOT-relative path — `share/pkg/x` is `/system/share/pkg/x` in the
    /// guest. A fixture the guest reads and no program in the image produces;
    /// the image is memoized on their names and bytes, so two boots staging
    /// different fixtures do not share one.
    pub extra_root_files: Vec<(String, Vec<u8>)>,
}

impl BootOptions {
    /// The whole parameter line this boot's image is built with: the names in
    /// [`BootOptions::kernel_params`].
    ///
    /// One function, called by the build and by the staged-image check, so a
    /// parameter that reaches the image and not the check — or the other way
    /// round — is not expressible.
    pub fn params(&self) -> Vec<String> {
        self.kernel_params.iter().map(|p| (*p).to_string()).collect()
    }
}

/// The in-guest test runner's startup marker.
///
/// It is that runner's own first line and nothing else's. Init spawns its
/// programs without waiting, so this marker orders nothing about any other
/// program's startup — a test asking about a daemon's line waits on the guest
/// for that line ([`await_guest`]), never on a span of host wall clock after
/// this one.
pub const DEFAULT_READY: &str = "===READY===";

impl Default for BootOptions {
    fn default() -> Self {
        Self {
            gdb_stub: false,
            debug_wait: false,
            smp: 2,
            profile: Profile::Headless,
            qmp: false,
            kernel_features: &[],
            kernel_params: &[],
            mute: false,
            ready_marker: DEFAULT_READY,
            extra_root_files: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    /// Why the run did not finish, when it did not.
    ///
    /// A [`WaitVerdict`] and not a `String`, so that the sentence and the
    /// kernel's own account of its death cannot come apart — see that type.
    /// Every arm that formats this gets the report for free, and there are
    /// fifty-two of them that were never going to be edited one at a time.
    pub error: Option<WaitVerdict>,
}

/// Every byte the guest's console has produced, the unfinished last line
/// included — **a view, not a queue: reading it takes nothing from anyone.**
///
/// The line channel is a `Receiver`, so a wait on it consumes: a helper that
/// drained lines looking for its own evidence would take the marker its caller's
/// assertion is waiting for. That is the whole reason this exists, and it is why
/// `shell_type_line` in `tests/toyos.rs` reads the guest's echo of a typed line
/// from here.
///
/// It also carries what the line channel structurally cannot. A surface owner
/// mirrors the shell's bytes to its own stdout and std buffers that by line, so
/// a prompt — `"{cwd}> "`, no newline — reaches a host reading bytes and no host
/// reading lines.
#[derive(Clone)]
pub struct ConsoleStream(Arc<Mutex<Vec<u8>>>);

impl ConsoleStream {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    /// Everything the guest has said since byte `at`.
    ///
    /// Lossy, and it has to be: `at` is a byte offset a caller took between two
    /// writes and the tail is whatever has arrived since, so both ends can fall
    /// inside a multi-byte character that is not finished yet.
    pub fn since(&self, at: usize) -> String {
        let buf = self.0.lock().expect("the console stream lock is never held across a panic");
        String::from_utf8_lossy(&buf[at.min(buf.len())..]).into_owned()
    }
}

pub struct QemuInstance {
    child: Child,
    /// What ends QEMU when this process dies without dropping this.
    _tether: Tether,
    stdin: BufWriter<Box<dyn Write + Send>>,
    rx: Receiver<String>,
    console: ConsoleStream,
    _reader_thread: thread::JoinHandle<String>,
    /// Held for the claim: one live guest per NVMe image.
    _nvme: NvmeClaim,
    sockets: Sockets,
    screendump: PathBuf,
    /// The image this boot built for itself.
    boot_image: PathBuf,
    /// The variable store this boot copied for itself.
    vars: PathBuf,
    boot_log: String,
    /// This guest's vCPU count, kept so its liveness ceilings can be widened by
    /// its own oversubscription on a host with fewer cores than vCPUs — see
    /// [`oversubscription`] and [`QemuInstance::budget`]. Boot-derived
    /// [`host_scale`] cannot see this: a boot is a mostly-serial workload and a
    /// wide-SMP guest pays lock-holder preemption a boot never does.
    smp: u32,
    /// The test binaries this boot put on ROOT, by the name `run` takes.
    carried: BTreeSet<String>,
}

/// Which of [`DECLARED_KERNEL_BUILDS`] this boot wants.
///
/// **A parameter never decides a build.** Every actuator lives in the one test
/// kernel, so asking for one selects that kernel and nothing more; the third
/// build is asked for by name and by one test.
fn kernel_of(options: &BootOptions) -> Vec<&'static str> {
    if options.kernel_params.is_empty() {
        return options.kernel_features.to_vec();
    }
    assert!(
        options.kernel_features.is_empty(),
        "a boot asking to arm {:?} also asks for the kernel build {:?}; an actuator is a \
         parameter and the test kernel carries all of them",
        options.kernel_params,
        options.kernel_features,
    );
    toyos_build::build::TEST_KERNEL.to_vec()
}

// Eight, because an image is its architecture as much as its files and its kernel.
#[allow(clippy::too_many_arguments)]
fn build_boot_image_with(
    arch: Arch,
    test_crate: &Path,
    c_tests: &[(String, Vec<u8>)],
    rust_tests: &[(String, Vec<u8>)],
    staged: &[(String, Vec<u8>)],
    kernel_features: &[&str],
    kernel_params: &[&str],
    debug_wait: bool,
) -> Vec<u8> {
    // **The two fields have the same type, so swapping them compiles.** It
    // happened once, in this file's own conversion: the shared boot handed
    // `["boot-actuators", "test-actuators"]` to `kernel_params` and every
    // `SYS_DEBUG` test died with the kernel refusing `boot-actuators` as a
    // parameter it does not declare. The kernel's refusal is what found it and
    // it is the right refusal, but a name is a name on this side of the wire
    // too, and the guest need not be started to know which kind it is.
    static ACTUATORS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let actuators =
        ACTUATORS.get_or_init(|| toyos_build::build::declared_actuators(&compile::repo_root()));
    // A parameter is an actuator's name or one of the kernel's own; both travel
    // in the same field and a shipping kernel takes only the second.
    static PARAMS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let params =
        PARAMS.get_or_init(|| toyos_build::build::declared_params(&compile::repo_root()));
    for name in kernel_params {
        assert!(
            actuators.iter().chain(params).any(|a| a == name)
                || toyos_build::build::is_valued_param(name),
            "{name:?} is a `kernel_params` and the kernel declares no such actuator or parameter"
        );
    }
    for name in kernel_features {
        assert!(
            !actuators.iter().any(|a| a == name),
            "{name:?} is an actuator and was passed as a `kernel_features`; it is a boot \
             parameter, so it belongs in `kernel_params`"
        );
    }

    let joined = kernel_features.join(",");
    assert!(
        toyos_build::build::harness_kernel_build_is_declared(&joined, debug_wait),
        "this boot asks for the kernel build {joined:?}, which is not one of the {} an ordinary \
         suite run may make: {DECLARED_KERNEL_BUILDS:?}; interactive debug mode may instead \
         make {:?}",
        DECLARED_KERNEL_BUILDS.len(),
        toyos_build::build::DEBUG_KERNEL_BUILD,
    );
    KERNELS.lock().expect("the kernel census").insert(joined);
    // The suite's programs are built for one architecture, and a ROOT of
    // another carries none of them.
    assert!(
        arch == SUITE_ARCH || (c_tests.is_empty() && rust_tests.is_empty()),
        "a {} image was handed programs built for {}",
        arch.name(),
        SUITE_ARCH.name()
    );
    let mut extra_files: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, data) in c_tests {
        extra_files.push((format!("bin/test_c_{name}"), data.clone()));
    }
    for (name, data) in rust_tests {
        if name.ends_with(".so") {
            extra_files.push((format!("lib/{name}"), data.clone()));
        } else {
            extra_files.push((format!("bin/test_rs_{name}"), data.clone()));
        }
    }
    extra_files.extend(staged.iter().cloned());

    let config_path = test_crate.join("system.toml");
    assert!(
        config_path.exists(),
        "Test crate missing system.toml: {}",
        config_path.display()
    );

    let quiet = !VERBOSE.load(Ordering::Relaxed);
    let plan = toyos_build::build::Plan::new(arch, &config_path, kernel_features, kernel_params);
    toyos_build::build::build_test_image(&compile::repo_root(), &plan, quiet, &extra_files)
}

/// Build all binaries in a test crate.
pub fn build_toyos_bins(crate_path: &Path) -> Vec<(String, Vec<u8>)> {
    let repo = compile::repo_root();
    let quiet = !VERBOSE.load(Ordering::Relaxed);
    toyos_build::build::build_toyos_bins(&repo, SUITE_ARCH, crate_path, quiet)
}

/// One binary of a test crate, built for `arch`: for a guest the crate's other
/// binaries do not all build for.
pub fn build_toyos_bin(arch: Arch, crate_path: &Path, name: &str) -> Vec<u8> {
    let quiet = !VERBOSE.load(Ordering::Relaxed);
    toyos_build::build::build_toyos_bin(&compile::repo_root(), arch, crate_path, name, quiet)
}

/// A kernel record's line, whichever writer spelled its head: `klogd`'s console
/// `[kernel …]` and a `/log` file's `[<date> <time> …]` alike. Nothing else
/// writes either — a program's line reaches both only through `logd`, under the
/// program's own head.
pub fn is_kernel_line(line: &str) -> bool {
    line.starts_with(toyos_build::kernelconsole::HEAD) || toyos_logstream::record_ms(line).is_some()
}

/// A console line's text as its program wrote it: a program's line without
/// the head `logd` gives it (`toyos_logstream::program_line`), and any other
/// line as it is.
pub fn user_text(line: &str) -> &str {
    toyos_logstream::program_line(line).map_or(line, |said| said.text)
}

/// File the userland half of one captured console line under `stdout`: a
/// program's text, and nothing of the kernel's. Every console line is one
/// writer's whole — `klogd` is the wire's one writer — so a line is one or
/// the other.
fn push_user_half(line: &str, stdout: &mut String) {
    if is_kernel_line(line) {
        return;
    }
    stdout.push_str(user_text(line));
    stdout.push('\n');
}

/// The in-guest runner's end-of-test marker, which opens its line's text.
const END_MARKER: &str = "===TEST_END ";

impl QemuInstance {
    pub fn boot_with_options(
        test_crate: &Path,
        c_tests: &[(String, Vec<u8>)],
        rust_tests: &[(String, Vec<u8>)],
        options: BootOptions,
    ) -> Self {
        let mut features: Vec<&str> = kernel_of(&options);
        if options.debug_wait {
            features.push(toyos_build::build::DEBUG_KERNEL_BUILD);
        }
        BOOTS.fetch_add(1, Ordering::Relaxed);
        if !features.is_empty() {
            FEATURE_BOOTS.fetch_add(1, Ordering::Relaxed);
        }

        let test_dir = super::lane::dir();
        let seq = BOOT_SEQ.fetch_add(1, Ordering::Relaxed);

        // Named for the boot rather than for the process. Two guests handed one
        // image file is not a slow test, it is a guest reading bytes another
        // boot is in the middle of writing — and the lane directory alone would
        // not settle it, since one test may hold two instances at once.
        let boot_image = test_dir.join(format!("boot-{seq}.img"));
        let params = options.params();
        let params: Vec<&str> = params.iter().map(String::as_str).collect();
        let image = build_boot_image_with(
            options.profile.arch(),
            test_crate,
            c_tests,
            rust_tests,
            &options.extra_root_files,
            &features,
            &params,
            options.debug_wait,
        );
        fs::write(&boot_image, image).expect("Failed to write test boot image");
        let carried = c_tests
            .iter()
            .map(|(name, _)| format!("test_c_{name}"))
            .chain(
                rust_tests
                    .iter()
                    .filter(|(name, _)| !name.ends_with(".so"))
                    .map(|(name, _)| format!("test_rs_{name}")),
            )
            .collect();

        // **Every boot gets a blank DATA volume**, so what one boot leaves under
        // `/home` — sshd's host identity, a package, a cache — is never the
        // premise of whatever test the lane runs next. The lane's one file is
        // remade rather than a file per boot.
        //
        // One live guest per image, claimed here rather than discovered from
        // QEMU's stderr after the second process has already exited — see
        // [`NvmeClaim`] — and claimed before the remaking, which truncates.
        let nvme_bytes = options.profile.shape().nvme_bytes;
        let nvme_image = if nvme_bytes == 0 {
            // A profile with no controller gets no backing file either; the
            // path is never passed to QEMU.
            test_dir.join("no-nvme")
        } else {
            test_dir.join(format!("test-nvme-{nvme_bytes}.img"))
        };
        let nvme = if nvme_bytes == 0 {
            NvmeClaim::unattached(&nvme_image)
        } else {
            let claim = NvmeClaim::take(&nvme_image).unwrap_or_else(|why| panic!("[qemu] {why}"));
            toyos_build::build::create_sparse(claim.path(), nvme_bytes);
            claim
        };

        let sockets = Sockets::new(&options);
        let screendump = test_dir.join(format!("screen-{seq}.ppm"));

        // Per-instance, not a fixed /tmp path: a screen test waits on this
        // file, so a shared one would let instances read each other's early
        // boot.
        let uart_log = test_dir.join(format!("uart-{seq}.log"));
        let _ = fs::remove_file(&uart_log);

        let vars = test_dir.join(format!("vars-{seq}.fd"));
        toyos_build::firmware::of(options.profile.arch())
            .and_then(|firmware| firmware.fresh_vars(&vars))
            .unwrap_or_else(|why| panic!("[qemu] {why}"));

        let qemu = qemu_command(&boot_image, nvme.path(), &uart_log, &sockets.dir, &vars, &options);
        spawn_and_wait_ready(
            qemu,
            &options,
            Files {
                seq,
                uart_log,
                nvme,
                sockets,
                screendump,
                boot_image,
                vars,
                carried,
            },
        )
    }

    /// A span of the guest's *physical* memory, as QEMU reads it.
    ///
    /// **The oracle for anything a guest leaves in DRAM for a later boot.** The
    /// guest cannot be asked — the claim is precisely about what survives it —
    /// and a screendump says nothing about bytes. `pmemsave` is the monitor
    /// command that answers, so what a test judges is memory QEMU dumped and not
    /// a report the guest wrote about itself.
    pub fn guest_memory(&mut self, phys: u64, bytes: usize) -> Result<Vec<u8>, String> {
        let socket = self.sockets.qmp.clone().expect("guest_memory needs BootOptions { qmp: true }");
        // Beside the screendump, which is this instance's own scratch path.
        let out = self.screendump.with_extension(format!("mem-{phys:#x}"));
        let _ = fs::remove_file(&out);
        // Quoted for the monitor, whose unquoted filename is read as an
        // expression and stops on the first letter of the path; the backslashes
        // are the JSON `human-monitor-command` carries it in.
        let command = format!("pmemsave {phys:#x} {bytes} \\\"{}\\\"", out.display());
        let said = QmpMonitor::open(&socket).human(&command);
        let read = fs::read(&out).map_err(|e| {
            format!("{command:?} wrote no file ({e}); the monitor said {said:?}")
        })?;
        if read.len() != bytes {
            return Err(format!(
                "pmemsave {phys:#x} wrote {} bytes and not {bytes}; the monitor said {said:?}",
                read.len()
            ));
        }
        Ok(read)
    }

    /// Capture the guest's scanout through QMP and return the decoded PPM.
    ///
    /// After a halt the guest is stopped, so the dump is stable. QEMU writes
    /// the file itself, so the only synchronization needed is the command's
    /// own reply.
    pub fn screendump(&mut self) -> super::screen::Ppm {
        let socket = self.sockets.qmp.clone().expect("screendump needs BootOptions { qmp: true }");
        let out = self.screendump.clone();
        let _ = fs::remove_file(&out);

        // A guest that triple-faults exits QEMU (`-no-reboot`), and the
        // socket then refuses every connect. Without this the retry loop
        // spends its full ten seconds and reports `qmp: cannot connect`,
        // which says nothing about what happened — the worst diagnostic the
        // harness produces, for the failure class the metal profile exists to
        // catch. `wait_for_ready` reports the same event properly, but a muted
        // guest never goes through it and no guest goes through it twice.
        let child = &mut self.child;
        let mut qmp = Qmp::connect_while(&socket, || {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("[qemu] QEMU died before the screendump (status: {status})");
            }
        });
        qmp.execute(&format!(
            "{{\"execute\":\"screendump\",\"arguments\":{{\"filename\":\"{}\"}}}}",
            out.display()
        ));

        let bytes = fs::read(&out).expect("screendump: QEMU wrote no file");
        super::screen::Ppm::parse(&bytes)
    }

    /// Screendump until the decoded screen carries `needle`, or the timeout.
    ///
    /// Every fatal path needs this, for one of two reasons. The panic
    /// handler's own path paints after the drain that emits the report, so a
    /// marker on serial does not yet prove a paint. The halt_all_cpus paths
    /// are the other way round and once *did* need only a single dump — but a
    /// report too long for one screen now pages, so the screen a marker
    /// proves is only the first of several and any given dump may hold a
    /// different one.
    pub fn screendump_until(&mut self, needle: &str, timeout: Duration) -> super::screen::Ppm {
        self.screendump_while(timeout, Duration::from_millis(100), |dump| {
            dump.text().contains(needle)
        })
    }

    /// Screendump until `done`, or the timeout. A muted guest has no console,
    /// so this is the only way to observe that boot at all — and what it
    /// watches for is a pixel pattern, not text.
    ///
    /// Returns the last dump either way; a caller that timed out gets the
    /// screen as its diagnostic, which under metal-sim is where the kernel's
    /// boot checkpoints and panic report are.
    pub fn screendump_while(
        &mut self,
        timeout: Duration,
        interval: Duration,
        done: impl Fn(&super::screen::Ppm) -> bool,
    ) -> super::screen::Ppm {
        let deadline = Instant::now() + budget_smp(timeout, self.smp);
        loop {
            let dump = self.screendump();
            if done(&dump) || Instant::now() >= deadline {
                return dump;
            }
            thread::sleep(interval);
        }
    }

    /// [`Self::screendump_while`], but a guest still *painting* is still working.
    ///
    /// The screen-channel form of what [`ceiling_verdict`] does for
    /// [`Self::run_test_paced`] on serial: past the budgeted deadline the wait
    /// does not give up while the framebuffer keeps *changing*. A console
    /// rendering slowly under a loaded `smp:2` runner is making progress, which
    /// is the case whose paint "never arrived in the window" while the guest was
    /// alive — the budget-scaled deadline undercounts a later moment in the run
    /// exactly as the serial ceiling did. Only a screen *frozen* for
    /// [`GUEST_QUIET`] past the deadline ends the wait; `done` firing ends it at
    /// once, so a passing caller is untouched
    /// and a real bug (the paint that should not be there, and stays) still fires
    /// its assertion, a frozen-screen `GUEST_QUIET` later.
    ///
    /// **Only for a config whose screen freezes when idle** — no compositor;
    /// `/system/bin/console` repaints on I/O alone. A compositor's cursor blink and its
    /// once-a-second taskbar clock never let the screen freeze, so such a caller
    /// would wait the whole backstop when its `done` never comes and keeps the
    /// plain [`Self::screendump_while`] (which is also why the `screen_blocked_dump`
    /// retry loop, whose timeout is a deliberate re-send signal, must not use
    /// this).
    ///
    /// Reuses the one classifier so the two channels cannot drift: `dying` is the
    /// serial path's alone, and a halted kernel freezes the screen and is caught
    /// by the freeze here.
    pub fn screendump_while_rendering(
        &mut self,
        timeout: Duration,
        interval: Duration,
        done: impl Fn(&super::screen::Ppm) -> bool,
    ) -> super::screen::Ppm {
        let ceiling = budget_smp(timeout, self.smp);
        let start = Instant::now();
        let mut last_change = start;
        let mut prev: Option<Vec<[u8; 3]>> = None;
        loop {
            let dump = self.screendump();
            if done(&dump) {
                return dump;
            }
            let now = Instant::now();
            if prev.as_deref() != Some(dump.pixels.as_slice()) {
                last_change = now;
                prev = Some(dump.pixels.clone());
            }
            if ceiling_verdict(
                None,
                now.duration_since(start),
                ceiling,
                now.duration_since(last_change),
                0,
            )
            .is_some()
            {
                return dump;
            }
            thread::sleep(interval);
        }
    }

    /// Every console line the guest printed before the ready marker.
    ///
    /// The kernel's own boot lines sit in the log ring until the scheduler
    /// drains them, by which time the virtio-console is the backend — so the
    /// 16550 file holds only the bootloader, and this is the only place a
    /// host test can read what the kernel said while booting. Under
    /// [`Profile::Metal`] the 16550 is the console and carries everything;
    /// empty when [`BootOptions::mute`] takes it away.
    pub fn boot_log(&self) -> &str {
        &self.boot_log
    }

    /// The guest's console byte for byte, unfinished last line included — see
    /// [`ConsoleStream`].
    pub fn console_stream(&self) -> &ConsoleStream {
        &self.console
    }

    pub fn stdin_mut(&mut self) -> &mut BufWriter<Box<dyn Write + Send>> {
        &mut self.stdin
    }

    pub fn flush_stdin(&mut self) {
        self.stdin.flush().expect("Failed to flush QEMU stdin");
    }

    /// Keep collecting serial output for `dur` after a test has returned.
    /// **Not scaled by the width**, and it is the one duration in this file that
    /// is not. Callers use it to *pace* — "let the guest run for 400 ms and tell
    /// me what it said" — so multiplying it does not buy a slow guest more room,
    /// it buys the test a longer sleep. `metal_sim_pointer_churn` has
    /// twenty-four of these; scaled, they made it an 86 s job at width 8 and the
    /// critical path of the whole phase.
    pub fn drain_serial(&mut self, dur: Duration) -> String {
        self.drain_for(dur, |_| false)
    }

    /// Drain until `line` reads true of a line just seen, or until the guest
    /// goes quiet for the rest of `dur`.
    ///
    /// A guest that is *shut down* ends a plain [`Self::drain_serial`] the
    /// moment QEMU exits and the reader disconnects, so the ceiling there costs
    /// nothing. A guest the fatal path has halted does not exit — every CPU is
    /// stopped and the process stays up — so the drain pays the whole ceiling
    /// waiting for a machine that will never speak again. `double_fault_stack`
    /// spent twenty seconds of every run that way, which was 80% of it.
    ///
    /// Here the duration *is* a liveness ceiling — the marker is what ends
    /// it — so it scales.
    pub fn drain_until(&mut self, dur: Duration, line: impl Fn(&str) -> bool) -> String {
        self.drain_for(budget_smp(dur, self.smp), line)
    }

    fn drain_for(&mut self, dur: Duration, line: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + dur;
        let mut out = String::new();
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return out;
            };
            match self.rx.recv_timeout(remaining) {
                Ok(seen) => {
                    out.push_str(&seen);
                    out.push('\n');
                    if line(&seen) {
                        return out;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return out,
                Err(RecvTimeoutError::Disconnected) => return out,
            }
        }
    }

    /// The QMP socket this instance opened. Injection needs it, and it needs
    /// `BootOptions { qmp: true }`.
    pub fn qmp_socket(&self) -> &Path {
        self.sockets.qmp.as_deref().expect("qmp_socket needs BootOptions { qmp: true }")
    }

    pub fn run_test(&mut self, name: &str, timeout: Duration) -> TestResult {
        self.run_test_hooked(name, timeout, "", |_| {})
    }

    /// `run_test`, with `action` run once the guest prints `ready_line`.
    ///
    /// The hook is inside the read loop because that is the only place the
    /// two facts meet: the guest is holding the keyboard claim, and the host has
    /// not injected yet. A sleep would be a guess in both directions.
    pub fn run_test_hooked(
        &mut self,
        name: &str,
        timeout: Duration,
        ready_line: &str,
        action: impl FnOnce(&Path),
    ) -> TestResult {
        let mut action = Some(action);
        self.run_test_paced(name, timeout, |socket, line| {
            if ready_line.is_empty() || !line.contains(ready_line) {
                return;
            }
            if let Some(action) = action.take() {
                action(socket.expect("run_test_hooked needs BootOptions { qmp: true }"));
            }
        })
    }

    /// `run_test`, with `step` run on every console line the guest prints.
    ///
    /// [`Self::run_test_hooked`] injects a whole sequence in one call and holds
    /// the reader while it does, so the host runs at its own speed and what
    /// reaches the guest is whatever survived the queues in between — a packet
    /// the guest was never given reads exactly like one it lost. A step driven
    /// by the guest's own output can stay behind it, which is how an injection
    /// test costs a slow guest wall-clock instead of a verdict.
    pub fn run_test_paced(
        &mut self,
        name: &str,
        timeout: Duration,
        mut step: impl FnMut(Option<&Path>, &str),
    ) -> TestResult {
        writeln!(self.stdin, "run {name}").expect("Failed to write to QEMU stdin");
        self.stdin.flush().expect("Failed to flush QEMU stdin");

        // `run <name> [args...]`, and the markers carry only the binary name.
        let want = name.split_whitespace().next().unwrap_or(name);
        let harness = want.starts_with("test_rs_") || want.starts_with("test_c_");
        assert!(
            !harness || self.carried.contains(want),
            "[qemu] `run {want}` on a boot whose ROOT does not carry it: a boot carries the test \
             binaries its caller handed it, and this one carries {:?}",
            self.carried
        );

        let timeout = budget_smp(timeout, self.smp);
        let start = Instant::now();
        let mut stdout = String::new();
        let mut serial = String::new();
        // Every line seen before this test announced itself. Kept, never
        // dropped.
        let mut before = String::new();
        let mut in_test = false;
        // **Which of the two things the ceiling caught**: a guest that has said
        // nothing for [`GUEST_QUIET`] has stopped, and one still talking at the
        // ceiling has not.
        let mut last_line = Instant::now();
        let mut lines = 0usize;
        // **The line on which the kernel said it was dying, if it ever did.**
        // The first one only: a crash report's later lines carry the spelling
        // too, and the header is the one worth quoting. What it buys is in
        // [`ceiling_verdict`] — until it existed, a Rust `panic!` in the kernel
        // matched nothing here, the machine halted, and the whole guard expired
        // onto a verdict that said the guest had stopped answering.
        let mut dying: Option<String> = None;

        loop {
            if let Some(error) = ceiling_verdict(
                dying.as_deref(),
                start.elapsed(),
                timeout,
                last_line.elapsed(),
                lines,
            ) {
                // The window in the order the guest wrote it: `before` holds
                // every line up to `===TEST_START===` and `serial` everything
                // after, so a kernel that died before this test announced
                // itself has its report found in the first and one that died
                // during it in the second.
                let error = WaitVerdict::for_test(error, &before, &serial, in_test);
                return TestResult {
                    name: name.to_string(),
                    exit_code: None,
                    stdout,
                    error: Some(error),
                };
            }

            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    last_line = Instant::now();
                    lines += 1;
                    step(self.sockets.qmp.as_deref(), &line);
                    if dying.is_none()
                        && super::serial::died(&line) == Some(super::serial::Died::Kernel)
                    {
                        dying = Some(line.clone());
                    }
                    if line.contains(&format!("===TEST_START {want}===")) {
                        in_test = true;
                        // **The runner's marker reaches the console through
                        // `logd` and the kernel's records through `klogd`**, so
                        // the kernel's record of this test's spawn, and what
                        // followed it, may arrive before the marker that opens
                        // the window. Everything from that record on is this
                        // test's, and moves into the window.
                        let spawned = format!("/{want} pid=");
                        let from = before
                            .match_indices('\n')
                            .map(|(at, _)| at + 1)
                            .chain(std::iter::once(0))
                            .filter(|&start| {
                                before[start..].lines().next().is_some_and(|l| {
                                    l.contains("spawn: ") && l.contains(&spawned)
                                })
                            })
                            .max();
                        if let Some(at) = from {
                            let moved = before.split_off(at);
                            for early in moved.lines() {
                                serial.push_str(early);
                                serial.push('\n');
                                push_user_half(early, &mut stdout);
                            }
                        }
                    } else if let Some(rest) = user_text(&line).strip_prefix(END_MARKER) {
                        let rest = rest.split_once("===").map_or(rest, |(head, _)| head);
                        let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                        // **A marker naming another test is the previous one's**,
                        // still on the wire because that test timed out and this
                        // one's window opened over its output. Filed where any
                        // other line of that window goes rather than dropped: it
                        // is the one line that says the window is desynced, and
                        // taking it as this test's end is what turned one
                        // timed-out test into 110 red ones.
                        if parts[0] != want {
                            let window = if in_test { &mut serial } else { &mut before };
                            window.push_str(&line);
                            window.push('\n');
                            continue;
                        }
                        let (exit_code, error) = if parts.len() > 1 {
                            if let Some(code_str) = parts[1].strip_prefix("exit=") {
                                (code_str.parse::<i32>().ok(), None)
                            } else if let Some(err) = parts[1].strip_prefix("error=") {
                                (None, Some(err.to_string()))
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                        // The guest's runner said this one, so the capture is
                        // handed over for the same reason: a runner reporting
                        // an error on a machine whose kernel had already died
                        // is reporting the smaller of the two facts.
                        let error =
                            error.map(|e| WaitVerdict::for_test(e, &before, &serial, in_test));
                        return TestResult {
                            name: name.to_string(),
                            exit_code,
                            stdout,
                            error,
                        };
                    } else if !in_test {
                        // **The window between two tests, kept rather than
                        // dropped**: a daemon still finishing its startup writes
                        // into it, and a death report carries it.
                        before.push_str(&line);
                        before.push('\n');
                    } else if in_test {
                        serial.push_str(&line);
                        serial.push('\n');
                        push_user_half(&line, &mut stdout);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // QEMU going away is a sentence about the host process, and
                    // a guest whose kernel panicked on the way out wrote down
                    // the reason first.
                    let error = WaitVerdict::for_test(
                        String::from("QEMU disconnected"),
                        &before,
                        &serial,
                        in_test,
                    );
                    return TestResult {
                        name: name.to_string(),
                        exit_code: None,
                        stdout,
                        error: Some(error),
                    };
                }
            }
        }
    }
}

impl Drop for QemuInstance {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        // **Reaped, not merely signalled.** The `NvmeClaim` field is released
        // after this body returns, and what makes that release true rather than
        // hopeful is that the process whose descriptors hold QEMU's write lock
        // on the image is gone by the time it happens.
        let _ = self.child.wait();
        // **The 16550's log outlives the guest, because it is the one channel
        // that exists before the console does.** 1.4 KB on a
        // healthy `tests/testcases` boot, measured, against the hundreds of
        // megabytes of per-boot image beside it.
        //
        // Deleting it here is also why `ci.yml`'s "what a red run left" step had
        // never uploaded one byte: run `31252989653` reds four shards and
        // publishes twelve duration files and no scratch artifact at all, which
        // reads as "there was nothing to keep" rather than "it was deleted
        // before the step ran".
        let _ = fs::remove_file(&self.screendump);
        // A per-boot image is hundreds of megabytes and a full run makes ~76 of
        // them; the shared name used to make that one file.
        for own in [&self.boot_image, &self.vars] {
            let _ = fs::remove_file(own);
        }
        // `sockets` goes with the fields, after QEMU is reaped.
    }
}

/// A QMP session. Line-delimited JSON: greeting, `qmp_capabilities`, then
/// commands; the reply carrying `return` is the completion signal. A handful
/// of commands with fixed shapes does not justify a JSON dependency.
struct Qmp {
    stream: std::os::unix::net::UnixStream,
    pending: Vec<u8>,
}

impl Qmp {
    fn connect(socket: &Path) -> Self {
        Self::connect_while(socket, || {})
    }

    /// `on_retry` runs between connect attempts. It is where a caller holding
    /// the QEMU process turns "connection refused" into "QEMU is gone, and
    /// here is its exit status" — see [`QemuInstance::screendump`].
    fn connect_while(socket: &Path, mut on_retry: impl FnMut()) -> Self {
        use std::os::unix::net::UnixStream;
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(s) => break s,
                Err(e) => {
                    on_retry();
                    assert!(
                        Instant::now() < deadline,
                        "qmp: cannot connect to {}: {e}",
                        socket.display()
                    );
                    thread::sleep(Duration::from_millis(50));
                }
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut qmp = Self { stream, pending: Vec::new() };
        qmp.await_reply("\"QMP\"");
        qmp.execute("{\"execute\":\"qmp_capabilities\"}");
        qmp
    }

    fn await_reply(&mut self, want: &str) {
        use std::io::Read;
        let start = Instant::now();
        loop {
            if let Some(pos) =
                self.pending.windows(want.len()).position(|w| w == want.as_bytes())
            {
                self.pending.drain(..pos + want.len());
                return;
            }
            // A refused command never produces a `return`, so without this the
            // wait spends its whole timeout and reports `qmp: read failed` —
            // which says nothing about the command QEMU declined or why.
            if let Some(at) = self.pending.windows(7).position(|w| w == b"\"error\"") {
                panic!(
                    "qmp: refused while waiting for {want}: {}",
                    String::from_utf8_lossy(&self.pending[at..])
                );
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "qmp: no {want} in reply: {}",
                String::from_utf8_lossy(&self.pending)
            );
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).expect("qmp: read failed");
            assert!(n > 0, "qmp: socket closed waiting for {want}");
            self.pending.extend_from_slice(&buf[..n]);
        }
    }

    fn execute(&mut self, command: &str) {
        self.stream.write_all(command.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
        self.await_reply("\"return\"");
    }

    /// `execute`, keeping what the command answered with. Only the human
    /// monitor answers with anything; every other command here returns `{}`.
    fn execute_capturing(&mut self, command: &str) -> String {
        use std::io::Read;
        self.stream.write_all(command.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
        self.await_reply("\"return\"");
        let rest = loop {
            if let Some(at) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=at).collect();
                break String::from_utf8_lossy(&line).into_owned();
            }
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).expect("qmp: read failed");
            assert!(n > 0, "qmp: socket closed mid-reply");
            self.pending.extend_from_slice(&buf[..n]);
        };
        let Some(body) = rest.split_once('"').map(|(_, tail)| tail) else {
            return String::new();
        };
        let body = body.rsplit_once('"').map_or(body, |(head, _)| head);
        let mut out = String::with_capacity(body.len());
        let mut chars = body.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => {}
                Some(escaped) => out.push(escaped),
                None => break,
            }
        }
        out
    }
}

/// An open QMP connection to QEMU's human monitor, for the questions QMP has
/// no command of its own for.
pub struct QmpMonitor(Qmp);

impl QmpMonitor {
    pub fn open(socket: &Path) -> Self {
        Self(Qmp::connect(socket))
    }

    /// Run `command` in the human monitor and return what it printed.
    pub fn human(&mut self, command: &str) -> String {
        self.0.execute_capturing(&format!(
            "{{\"execute\":\"human-monitor-command\",\"arguments\":\
             {{\"command-line\":\"{command}\"}}}}"
        ))
    }
}

/// An open QMP connection for injecting input.
///
/// One connection rather than one per event, because QEMU delivers each
/// `input-send-event` as its own input sync — so a thousand pointer packets
/// is a thousand commands, and a thousand reconnects on top of that is the
/// difference between a second and a minute.
pub struct QmpInput(Qmp);

impl QmpInput {
    pub fn open(socket: &Path) -> Self {
        Self(Qmp::connect(socket))
    }

    fn send(&mut self, body: &[String]) {
        if body.is_empty() {
            return;
        }
        self.0.execute(&format!(
            "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[{}]}}}}",
            body.join(",")
        ));
    }

    /// Every key transition in `events` as one batch, so a chord like Shift+B
    /// arrives as a chord rather than as a race.
    pub fn keys(&mut self, events: &[(&str, bool)]) {
        let body: Vec<String> = events
            .iter()
            .map(|(qcode, down)| {
                format!(
                    "{{\"type\":\"key\",\"data\":{{\"down\":{down},\"key\":{{\"type\":\"qcode\",\"data\":\"{qcode}\"}}}}}}"
                )
            })
            .collect();
        self.send(&body);
    }

    /// Type `text` as one batch of transitions, with no wait anywhere in it.
    ///
    /// **The caller owns the bound, and there is no version of this that does
    /// not need one.** QEMU's PS/2 keyboard queue holds `QEMU_PS2_QUEUE` set-1
    /// bytes and drops what does not fit silently, one byte at a time, so a
    /// batch wider than that queue is a hole in the middle of a word whatever
    /// the guest is doing. Use [`scancode_bytes`] to measure a batch, and send
    /// the next one only once the guest has shown it consumed this one —
    /// `console_type_line` and `shell_type_line` in `tests/toyos.rs` are the
    /// two patterns, one reading the panel and one reading [`ConsoleStream`].
    ///
    /// **There is no wall-clock form of this and there must not be one.** A gap
    /// between characters is the same bound bet on the guest being scheduled,
    /// and a guest whose vCPU the host has not run for a couple of hundred
    /// milliseconds drains none of them — at which point the queue starts
    /// dropping, silently and one byte at a time, and the guest receives the
    /// line with a hole in it. Both times `screen_console_panic` has ever gone
    /// red that is what happened, and neither side of the wire says a word
    /// about it.
    pub fn type_burst(&mut self, text: &str) {
        let mut events: Vec<(&str, bool)> = Vec::new();
        for ch in text.chars() {
            let (qcode, shift) = qcode(ch);
            if shift {
                events.extend([("shift", true), (qcode, true), (qcode, false), ("shift", false)]);
            } else {
                events.extend([(qcode, true), (qcode, false)]);
            }
        }
        self.keys(&events);
    }
}

/// What one character costs on the wire, in set-1 bytes.
///
/// Every qcode [`qcode`] maps is a one-byte make and its break, and none of
/// them is `0xE0`-prefixed; a shifted one carries the modifier's pair around
/// it. This exists because a caller that has to bound what it puts in flight
/// against QEMU's PS/2 queue cannot do it without knowing what a character
/// weighs — an unmapped character panics in `qcode` rather than being counted
/// as anything, which is the same refusal typing one would get.
pub fn scancode_bytes(ch: char) -> usize {
    if qcode(ch).1 { 4 } else { 2 }
}

/// The QEMU qcode for `ch`, and whether Shift is held to produce it.
///
/// A US layout, because that is what `kernel/src/keyboard.rs` boots with. Only
/// the characters a console test types: an unmapped one panics rather than
/// being dropped, since a command missing a character is a test asserting on
/// output nothing was ever asked to produce.
fn qcode(ch: char) -> (&'static str, bool) {
    const LOWER: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGIT: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match ch {
        'a'..='z' => (LOWER[ch as usize - 'a' as usize], false),
        'A'..='Z' => (LOWER[ch as usize - 'A' as usize], true),
        '0'..='9' => (DIGIT[ch as usize - '0' as usize], false),
        ' ' => ("spc", false),
        '\n' => ("ret", false),
        '-' => ("minus", false),
        '_' => ("minus", true),
        '.' => ("dot", false),
        '/' => ("slash", false),
        '&' => ("7", true),
        _ => panic!("no qcode for {ch:?}; add it rather than typing something else"),
    }
}

/// An open QMP connection for attaching and detaching devices while the guest
/// runs — QEMU's own `device_add`/`device_del`, which is what a person
/// plugging something in looks like from the host side.
///
/// Its own type rather than more methods on [`QmpInput`], and never open at the
/// same time as one: a `-qmp unix:…,server` socket serves one monitor, so a
/// caller that needs both alternates. A type called `QmpInput` with
/// `device_add` on it would also be describing the wrong thing.
pub struct QmpDevices(Qmp);

impl QmpDevices {
    pub fn open(socket: &Path) -> Self {
        Self(Qmp::connect(socket))
    }

    /// Attach `driver` on `bus` as `id`, with `extra` naming any further
    /// properties. Every value is a bare JSON string, which is what every
    /// property these tests set happens to be.
    pub fn add(&mut self, driver: &str, bus: &str, id: &str, extra: &[(&str, &str)]) {
        let mut args = format!("\"driver\":\"{driver}\",\"bus\":\"{bus}\",\"id\":\"{id}\"");
        for (key, value) in extra {
            args.push_str(&format!(",\"{key}\":\"{value}\""));
        }
        self.0.execute(&format!("{{\"execute\":\"device_add\",\"arguments\":{{{args}}}}}"));
    }

    pub fn del(&mut self, id: &str) {
        self.0
            .execute(&format!("{{\"execute\":\"device_del\",\"arguments\":{{\"id\":\"{id}\"}}}}"));
    }

    /// Give QEMU an image to back a device that is not on the machine yet, so
    /// a hot-plugged disk needs nothing in argv. A disk declared at boot is a
    /// disk the guest could have enumerated at boot.
    pub fn blockdev_add(&mut self, node: &str, image: &Path) {
        self.0.execute(&format!(
            "{{\"execute\":\"blockdev-add\",\"arguments\":{{\"node-name\":\"{node}\",\
             \"driver\":\"raw\",\"file\":{{\"driver\":\"file\",\"filename\":\"{}\"}}}}}}",
            image.display()
        ));
    }
}

/// The argv `options` would launch QEMU with, built against placeholder
/// paths. A profile's claim about which devices exist is a claim about this
/// list and nothing else — no screendump can see a device that is present but
/// unused — so this is what a profile assertion has to read.
pub fn profile_argv(options: &BootOptions) -> Vec<String> {
    let p = Path::new("/nonexistent");
    qemu_command(p, p, p, p, p, options)
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn qemu_command(
    boot_image: &Path,
    nvme_image: &Path,
    uart_log: &Path,
    socket_dir: &Path,
    firmware_vars: &Path,
    options: &BootOptions,
) -> Command {
    let qmp_socket = qmp_socket(socket_dir, options);
    let shape = options.profile.shape();
    assert!(
        !options.mute || !shape.virtio.present(),
        "mute removes the only console a virtio profile has"
    );

    let arch = options.profile.arch();
    let [firmware_code, firmware_vars] = toyos_build::firmware::of(arch)
        .unwrap_or_else(|why| panic!("[qemu] {why}"))
        .drives(firmware_vars);

    let mut qemu = Command::new(arch.qemu());
    if let Some(boot) = arch.boot() {
        qemu.arg("-boot").arg(boot);
    }

    let accel = options.profile.accel();
    if accel.is_hardware() {
        qemu.arg("-accel").arg(accel.name());
    }

    // Without this QEMU runs its default-device pass whenever no network
    // option is given, which is exactly and only the Metal profile: measured
    // on QEMU 11.0.2, an e1000e at 00:02.0 with a slirp backend, an empty
    // ide-cd on the ich9-ahci, and an isa-parallel — none of them declared by
    // anything, none of them visible to an argv assertion, and the first of
    // them enough to make netd claim a NIC on the machine whose whole point is
    // that it has none. `-net none` and `-nic none` are gone in QEMU 11; this
    // is the option that does it, and it leaves i8042/ps2-kbd/ps2-mouse alone.
    qemu.arg("-nodefaults");

    // `kernel-irqchip=split` only when there is a unit: interrupt remapping
    // needs the userspace half of the irqchip, and a machine with no unit has
    // no reason to be built differently from the one it has always been.
    let mut machine = match arch {
        Arch::X86_64 => arch.machine().to_string(),
        Arch::Aarch64 => {
            // The unit a profile declares is VT-d, which `virt` has none of.
            assert!(shape.iommu.is_none(), "`virt` has no VT-d");
            match options.profile {
                Profile::VirtEl2 => format!("{},gic-version=3,virtualization=on", arch.machine()),
                _ => format!("{},gic-version=3", arch.machine()),
            }
        }
    };
    if shape.iommu.is_some() {
        machine.push_str(",kernel-irqchip=split");
    }

    // `virt` puts RAM at 1 GiB and AAVMF allocates from its top, so with 4 GiB
    // the loader's allocations land past the 4 GiB its boot map reaches and it
    // refuses the boot: issues/boot-media/the-boot-map-reaches-4-gib-and-firmware-decides-what-lands-in-it.md.
    let memory = match arch {
        Arch::X86_64 => "4G",
        Arch::Aarch64 => "2G",
    };
    qemu.arg("-machine")
        .arg(&machine)
        .arg("-cpu")
        .arg(arch.cpu(accel))
        .arg("-smp")
        .arg(options.smp.to_string())
        .arg("-m")
        .arg(memory)
        .arg("-drive")
        .arg(firmware_code)
        .arg("-drive")
        .arg(firmware_vars)
        .arg("-drive")
        .arg(format!("if=none,id=stick,format=raw,file={}", boot_image.display()));
    assert!(!shape.xhci.is_empty() || shape.usb.is_empty(), "a USB device needs a controller");

    // Ahead of every other `-device`: QEMU gives a PCI function the bypassing
    // address space unless the unit exists when the function is created, so a
    // unit emitted after the devices it is meant to decode is a unit that
    // decodes nothing — the vacuity trap, in its harness-side form.
    if let Some(unit) = shape.iommu {
        qemu.arg("-device").arg(format!(
            "intel-iommu,intremap={},caching-mode=on,aw-bits={},eim={}",
            if unit.intremap { "on" } else { "off" },
            unit.aw_bits,
            if unit.eim { "on" } else { "off" }
        ));
    }
    // A virtio function reaches memory through `vdev->dma_as`, the machine's own
    // address space with the unit bypassed, unless it is created with this — the
    // other half of the trap: a unit that decodes every function but these.
    let platform = if shape.iommu.is_some() { ",iommu_platform=on" } else { "" };

    for controller in shape.xhci {
        qemu.arg("-device").arg(*controller);
    }

    qemu.arg("-device").arg(format!(
        "usb-storage,bus=xhci.0,drive=stick,id={BOOT_STICK_ID},serial={BOOT_STICK_SERIAL},bootindex=0"
    ));
    match (arch, shape.vga) {
        (Arch::X86_64, vga) => {
            qemu.arg("-vga").arg(vga);
        }
        // `virt` has no VGA: a GOP there is firmware's over `ramfb`, a
        // framebuffer in guest memory that needs no driver after it.
        (Arch::Aarch64, "std") => {
            qemu.arg("-device").arg("ramfb");
        }
        (Arch::Aarch64, "none") => {}
        (Arch::Aarch64, other) => panic!("`virt` has no `-vga {other}`"),
    }
    qemu.arg("-display").arg("none").arg("-no-reboot");
    if let Some((w, h)) = shape.panel {
        assert_eq!(arch, Arch::X86_64, "a panel is declared through VGA's EDID, and `virt` has no VGA");
        // A panel on a machine with no VGA adapter is a declaration nothing
        // emits, which is the silently-inert field this suite refuses by name.
        assert_eq!(
            shape.vga, "std",
            "a {w}x{h} panel declared on a machine whose `-vga` is {:?}",
            shape.vga
        );
        qemu.arg("-global")
            .arg(format!("VGA.xres={w}"))
            .arg("-global")
            .arg(format!("VGA.yres={h}"));
    }

    // Controller and namespace as two devices rather than QEMU's implicit
    // one, so the logical block size is something a profile states instead of
    // something the default decides — and so that stating *zero* bytes gives
    // the guest no controller at all, rather than an empty one. A machine
    // with no NVMe is a shape, and the argv is the only place it is visible:
    // no console line and no screendump can see a device that is absent.
    //
    // Its MSI-X table in a BAR of its own, because blockd drives it and a claim
    // never maps the BAR holding the table. On a machine that boots off NVMe it
    // answers under Intel's ids, so the `pci:1b36:0010` row names the boot
    // controller alone and this one is nobody's, as the kernel's first-by-class
    // probe left it.
    if shape.nvme_bytes != 0 {
        qemu.arg("-drive")
            .arg(format!("if=none,id=nvme0,format=raw,file={}", nvme_image.display()))
            .arg("-device")
            .arg("nvme,serial=deadbeef,id=nvme0ctl,msix-exclusive-bar=on")
            .arg("-device")
            .arg("nvme-ns,drive=nvme0,bus=nvme0ctl,logical_block_size=512,physical_block_size=512");
    }
    for dev in shape.usb {
        qemu.arg("-device").arg(*dev);
    }

    // The NIC before the virtio block, so a profile that has one and not the
    // other still creates it after the unit and before everything else.
    // `iommu_platform` is virtio's own way of asking to be decoded.
    match shape.nic {
        Nic::Absent => {}
        Nic::Virtio => {
            qemu.arg("-netdev").arg("user,id=net0").arg("-device").arg(format!(
                "virtio-net-pci-non-transitional,netdev=net0{platform}"
            ));
        }
    }
    if shape.virtio.present() {
        if shape.virtio.sound() {
            // No guest test plays audio: the device is here as a DMA master and
            // a claim, so its audio goes nowhere.
            qemu.arg("-audiodev")
                .arg("none,id=audio0")
                .arg("-device")
                .arg(format!("virtio-sound-pci,audiodev=audio0,streams=1{platform}"));
        }
        qemu
            // virtio-console on stdio is the primary I/O channel; UART goes to
            // a temp file so early-boot logs and panic fallback still land
            // somewhere when the kernel switches backends.
            .arg("-serial")
            .arg(format!("file:{}", uart_log.display()))
            .arg("-chardev")
            .arg("stdio,id=cs0,signal=off")
            .arg("-device")
            .arg(format!(
                "virtio-serial-pci-non-transitional,id=virtio-serial0,max_ports=1{platform}"
            ))
            .arg("-device")
            .arg("virtconsole,chardev=cs0,id=console0");
    } else if options.mute {
        qemu.arg("-serial").arg("none");
    } else {
        // The 16550 *is* the console here: no virtio-serial exists, so the
        // kernel's log ring drains to it and the guest reads its commands
        // off it. signal=off matches the virtio console above, so a ^C in
        // the stream reaches the guest rather than killing QEMU.
        qemu.arg("-chardev")
            .arg("stdio,id=uart0,signal=off")
            .arg("-serial")
            .arg("chardev:uart0");
    }

    if options.gdb_stub {
        qemu.arg("-s");
    }
    if let Some(socket) = qmp_socket {
        qemu.arg("-qmp")
            .arg(format!("unix:{},server,nowait", socket.display()));
    }

    qemu
}

/// A boot's Unix sockets, in a directory of its own under `/tmp` rather than
/// the lane's: `sun_path` is 104 bytes on Darwin, and `$TMPDIR`'s depth is the
/// host's. The directory goes with this, after QEMU is reaped.
struct Sockets {
    dir: TempDir,
    qmp: Option<PathBuf>,
}

impl Sockets {
    fn new(options: &BootOptions) -> Sockets {
        let dir = TempDir::short("boot");
        let qmp = qmp_socket(&dir, options);
        Sockets { dir, qmp }
    }
}

/// The QMP socket `options` asks for, named in `dir`.
fn qmp_socket(dir: &Path, options: &BootOptions) -> Option<PathBuf> {
    options.qmp.then(|| dir.join("qmp.sock"))
}

/// Every file one boot owns, so that adding another does not lengthen a
/// parameter list eight paths long.
struct Files {
    seq: u32,
    uart_log: PathBuf,
    nvme: NvmeClaim,
    sockets: Sockets,
    screendump: PathBuf,
    boot_image: PathBuf,
    vars: PathBuf,
    carried: BTreeSet<String>,
}

fn spawn_and_wait_ready(mut qemu: Command, options: &BootOptions, files: Files) -> QemuInstance {
    let Files { seq, uart_log, nvme, sockets, screendump, boot_image, vars, carried } = files;

    // Inherited: `orphan` reads QEMU's exit as the end of its harness's stderr.
    qemu.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if VERBOSE.load(Ordering::Relaxed) {
        eprintln!("[qemu {seq}] Launching QEMU...");
    }
    let (mut child, tether) = toyos_build::tether::spawn(qemu).expect("Failed to launch QEMU");

    let stdin: Box<dyn Write + Send> = Box::new(child.stdin.take().unwrap());
    let stdin = BufWriter::new(stdin);
    let stdout: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());

    let (tx, rx) = mpsc::channel::<String>();
    let console = ConsoleStream::new();
    let reader_console = console.clone();
    // The virtio port starts at the kernel's first record; a 16550 on stdio has
    // no other file, so it is read whole.
    let mut kernel_console = options
        .profile
        .shape()
        .virtio
        .present()
        .then(toyos_build::kernelconsole::KernelConsole::default);
    let reader_thread = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut full_log = String::new();
        // Read bytes and split them, rather than `BufRead::lines`: every
        // consumer below still gets whole lines and nothing else, and
        // [`ConsoleStream`] gets the tail that is not a line yet, which is
        // where a prompt lives.
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = reader.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                // EOF, and whatever has no newline behind it is the last line —
                // which is what `lines` hands over here too.
                if !pending.is_empty() {
                    publish_line(pending, seq, &mut full_log, &tx);
                }
                return full_log;
            }
            let read = match &mut kernel_console {
                Some(console) => console.pass(&chunk[..read]),
                None => std::borrow::Cow::Borrowed(&chunk[..read]),
            };
            reader_console
                .0
                .lock()
                .expect("the console stream lock is never held across a panic")
                .extend_from_slice(&read);
            pending.extend_from_slice(&read);
            while let Some(at) = pending.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = pending.drain(..=at).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !publish_line(line, seq, &mut full_log, &tx) {
                    return full_log;
                }
            }
        }
    });

    // A muted guest has no console at all, so there is no marker to wait for:
    // the caller polls the framebuffer. Blocking here would only time out.
    let boot_log = if options.mute {
        String::new()
    } else {
        wait_for_ready(&mut child, &rx, options, &uart_log)
    };

    QemuInstance {
        child,
        _tether: tether,
        stdin,
        rx,
        _reader_thread: reader_thread,
        _nvme: nvme,
        sockets,
        screendump,
        boot_image,
        vars,
        boot_log,
        console,
        smp: options.smp,
        carried,
    }
}

/// One finished console line into everything that keeps one.
///
/// `false` means nothing is left to read for: the receiver has gone, or the
/// guest put a byte on the wire that is not UTF-8 — the second being the same
/// refusal `BufRead::lines` made here before, kept because a console that has
/// started emitting bytes no decoder agrees on is not a stream any assertion
/// below should be run against.
fn publish_line(
    raw: Vec<u8>,
    seq: u32,
    full_log: &mut String,
    tx: &mpsc::Sender<String>,
) -> bool {
    let Ok(line) = String::from_utf8(raw) else { return false };
    full_log.push_str(&line);
    full_log.push('\n');
    // Here rather than in a caller's capture, because no caller holds every
    // line: `boot_log` ends at the ready marker and a `TestResult` begins at
    // `===TEST_START===`. The census is cumulative, so what the suite's summary
    // wants is the last one of the boot, whichever of those windows it fell in.
    super::irqcensus::observe(seq, &line);
    if VERBOSE.load(Ordering::Relaxed) {
        // The boot's own number, because `--nocapture` on a wide run is several
        // guests talking into one terminal and an unattributed line is worse
        // than no line.
        eprintln!("[serial {seq}] {line}");
    }
    tx.send(line).is_ok()
}

/// Returns every line seen on the way to the marker — see [`QemuInstance::boot_log`].
fn wait_for_ready(
    child: &mut Child,
    rx: &Receiver<String>,
    options: &BootOptions,
    uart_log: &Path,
) -> String {
    let no_timeout = options.debug_wait;
    let ready = options.ready_marker;
    let panic_aborts = ready == DEFAULT_READY;
    // Ten seconds per guest this phase may have up, and never fewer than two
    // guests' worth — the tree runs 15-25 suites a day across several agents,
    // so one guest on a quiet host stopped being
    // the regime some time before this did. Measured on 2026-08-03 with other
    // agents building: two boots exceeded the flat ten seconds, one of them in a
    // phase running a single guest.
    //
    // A wedge costs that much longer to report and nothing else.
    //
    // Scaled by the host too, and the first boot of a run is the one that
    // cannot be: nothing has been measured yet, so it gets the flat number and
    // every boot after it gets the corrected one. Two boot timeouts in CI run
    // `31233476555` were this — `console: ready` and `compositor: ready`, on a
    // runner where the same boots take twice what they take here.
    //
    // And by this guest's own oversubscription: an `smp:8` guest brings up all
    // eight vCPUs during boot, so on the four-core runner even the boot is
    // `8/4` oversubscribed, which the boot-derived `host_scale` cannot fold in
    // because it *is* what boot measured. `oversubscription` says why in terms
    // of `vcpus/cores`; on a host with a core per vCPU it multiplies by one.
    let (num, den) = host_scale();
    let (onum, oden) = oversubscription(options.smp);
    let boot_timeout =
        Duration::from_secs(10) * WIDTH.load(Ordering::SeqCst).max(2) * num / den * onum / oden;
    let start = Instant::now();
    let mut seen = String::new();
    loop {
        if !no_timeout && start.elapsed() > boot_timeout {
            let _ = child.kill();
            // With what it did say. A timeout that discards the console is the
            // one failure in this harness that arrives with no evidence at all,
            // and "the guest printed nothing" and "the guest printed sixty
            // lines and then stopped" are different machines.
            panic!(
                "[qemu] Boot timed out waiting for {ready}; the console carried:\n{}",
                if seen.is_empty() { "nothing at all".to_string() } else { seen.clone() }
            );
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(line) if line.contains(ready) => {
                seen.push_str(&line);
                seen.push('\n');
                if VERBOSE.load(Ordering::Relaxed) {
                    eprintln!("[qemu] Reached {ready}");
                }
                break;
            }
            // **A death nothing left on this machine can come back from.** The
            // kernel's own, or a process the kernel killed — before the ready
            // marker the second is as fatal as the first, because whatever died
            // was `init` or one of its children and nothing else is going to
            // reach the marker.
            //
            // A process that ended *itself* is not on that list, and the
            // difference is not academic: `sshd` panicked across four recorded
            // boots that then came up perfectly, losing a race with `netd`'s
            // teardown on a machine with no NIC.
            // The words are the same words — `panicked at` — and who wrote the
            // line is the whole of what tells them apart. `super::serial::died`
            // is where that is decided, for this wait and for [`await_guest`]
            // and [`QemuInstance::run_test_paced`] alike, so the three cannot
            // drift into disagreeing about a spelling.
            Ok(ref line)
                if panic_aborts
                    && !no_timeout
                    && matches!(
                        super::serial::died(line),
                        Some(super::serial::Died::Kernel | super::serial::Died::Faulted)
                    ) =>
            {
                let mut crash_msg = line.clone();
                let drain_deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < drain_deadline {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(bt_line) => {
                            crash_msg.push('\n');
                            crash_msg.push_str(&bt_line);
                        }
                        Err(_) => break,
                    }
                }
                let _ = child.kill();
                panic!("[qemu] Init process crashed during boot:\n{crash_msg}");
            }
            Ok(line) => {
                seen.push_str(&line);
                seen.push('\n');
                continue;
            }
            // A guest that dies before virtio-console init never reaches
            // stdio at all; the UART file is the only channel it has.
            Err(RecvTimeoutError::Timeout) => {
                if !panic_aborts
                    && fs::read_to_string(uart_log).is_ok_and(|s| s.contains(ready))
                {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = child.wait();
                let uart = fs::read_to_string(uart_log).unwrap_or_default();
                panic!(
                    "[qemu] QEMU died before {ready} (status: {status:?})\nconsole:\n{}\nuart:\n{}",
                    if seen.is_empty() { "nothing at all" } else { &seen },
                    if uart.is_empty() { "nothing at all" } else { &uart },
                );
            }
        }
    }
    record_boot(start.elapsed());
    seen
}
