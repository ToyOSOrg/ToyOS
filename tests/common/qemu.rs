use std::collections::BTreeSet;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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
/// to pay a kernel build per suite run forever.
/// [`toyos_build::build::MASK_WINDOWS_KERNEL`] made it: its hooks sit on every
/// entry and masking primitive, where a parameter would be a branch on the path
/// they measure. Interactive debug mode is separate: it builds
/// [`toyos_build::build::DEBUG_KERNEL_BUILD`] and returns before the suite.
pub const DECLARED_KERNEL_BUILDS: [&str; 3] =
    toyos_build::build::TEST_SUITE_KERNEL_BUILDS;

/// The fastest boot-to-ready this run has seen, in milliseconds.
///
/// A boot is the one piece of guest work every test does and no test asserts on
/// — `wait_for_ready`'s own comment names the two exceptions, and both read the
/// guest's stamps rather than this clock — so it is a measurement of the host
/// that costs nothing to take. The *fastest* rather than the mean because a boot
/// taken with three other guests up measures the others; the minimum over a run
/// is the closest this can get to the machine with nothing else on it.
static FASTEST_BOOT_MS: AtomicU32 = AtomicU32::new(u32::MAX);

/// The fastest boot of the host the ceilings were measured on: at or above the
/// fastest boot of every run a ceiling in this tree was measured in, so that
/// host pays them at 1×.
const REFERENCE_BOOT_MS: u32 = 1424;

fn record_boot(took: Duration) {
    let ms = took.as_millis().min(u32::MAX as u128) as u32;
    FASTEST_BOOT_MS.fetch_min(ms, Ordering::SeqCst);
}

/// How much slower than the host these ceilings were measured on this one is, as
/// a fraction so that a 1.4× host is not rounded to 1.
///
/// **Only ever upward**, since a ceiling that shrank would report wedges that
/// are not there; and at most 8×, so one anomalous boot cannot disable every
/// guard in the suite at once.
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

/// A ceiling, paid out for the host and the guest it bounds.
///
/// **Every ceiling in this suite is at most three times the slowest the test it
/// bounds was measured to take**, in whole-suite runs on the host [`REFERENCE_BOOT_MS`]
/// describes, at the suite's default width — so a test's time already carries
/// the guests it shares that host with. A wait is bounded by that multiple of
/// its whole test, the one number measured. The source states that number, and
/// this pays it out on a slower host ([`host_scale`]) and for a guest wider than
/// this one ([`oversubscription`]), and for nothing else.
///
/// A ceiling is a guard against a wedge and never a verdict: the assertion is
/// what the guest *said*.
pub fn budget_smp(ceiling: Duration, smp: u32) -> Duration {
    let (num, den) = host_scale();
    let (onum, oden) = oversubscription(smp);
    ceiling * num / den * onum / oden
}

/// A liveness guard that watches the guest instead of the host's clock.
///
/// A guest still printing is a guest still working. So the ceiling here is time in
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
/// places.
///
/// Still red. A guest that stopped answering may have stopped for a reason this
/// tree owns, and a status that is not a failure is a status nobody reads. What
/// this buys is that the summary says which of the two kinds of red it is.
///
/// It lives here rather than beside the classifier because [`Liveness`] is what
/// produces the evidence for it, and [`QemuInstance::run_test_paced`] is a
/// second producer: a test's own ceiling is a guard of exactly this kind.
pub const STALLED: &str = "STALLED:";

/// The backstop's red: a guest still talking that never finished. The ceiling
/// too, and counted with [`STALLED`]'s.
pub const TIMED_OUT: &str = "timed out after";

/// How long a guest may say nothing before a wait on it is a stall.
///
/// Every config these waits run on has something on a periodic interval — the
/// compositor's frame batch and soundserver's stats window are both about 2 s — so
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
/// worse than one that reds. [`budget_smp`]'s rule over the tests that wait
/// through [`await_guest`].
pub const GUEST_WEDGED: Duration = Duration::from_secs(141);

/// A kernel line without its head.
fn without_stamp(line: &str) -> &str {
    toyos_logstream::parse(line).filter(|p| p.source == toyos_logstream::Source::Kernel).map_or(line, |p| p.text)
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

/// How many of a window's lines a verdict carries.
const WINDOW_LINES: usize = 40;

/// The last [`WINDOW_LINES`] lines of `said`, under the count of what was cut.
fn window(said: &str) -> String {
    let lines: Vec<&str> = said.lines().collect();
    let kept = lines.len().min(WINDOW_LINES);
    let head = if lines.len() > kept {
        format!("(the last {kept} of the {} lines in it)\n", lines.len())
    } else {
        String::new()
    };
    format!("{head}{}", lines[lines.len() - kept..].join("\n"))
}

/// Why a wait ended badly, carrying the guest's own account of it.
///
/// **A newtype, because what this closes is an omission and an omission cannot
/// be gated by review.** Fifty-two sites in this suite format
/// [`TestResult::error`] and thirty-six of them printed no capture beside it
/// (counted on the tree, 2026-08-18), and on
/// 2026-08-18 that is what a `DOUBLE FAULT on CPU 1` cost — the wait named the
/// death in one sentence, the kernel's report sat in `TestResult::serial`, and
/// the arm printed `stdout`
/// (`issues/a-double-fault-on-cpu-1-under-a-wide-suite.md`). Fixing
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
        Self(format!("{}\n{NEVER_ANNOUNCED}\n{}", verdict.0, window(before)))
    }
}

impl std::fmt::Display for WaitVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How often a test's read loop asks [`ceiling_verdict`] about a guest that
/// has said nothing since it last asked.
pub const VERDICT_POLL: Duration = Duration::from_millis(100);

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
/// one did.
///
/// **`elapsed > ceiling` stays a necessary condition, and that is what keeps
/// this safe.** Silence alone is not a wedge on this suite's boots: a healthy
/// but idle guest on a config with no live periodic speaker — no compositor, an
/// idle soundserver, and the kernel's own ~10 s line halting with the idle loop — was
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
    // For a guest that is stuck *and* chatty and so never trips the silence
    // guard; never before a guest silent since `ceiling` has been silent for
    // [`GUEST_QUIET`].
    let backstop = (ceiling * 2).max(ceiling + GUEST_QUIET);
    if elapsed > backstop {
        if let Some(line) = dying {
            return Some(kernel_died_here(line));
        }
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
/// The capture runs past the line that made `done` true to the end of that
/// line's drain, which can fall inside anything the guest says in more than
/// one line.
///
/// The shape [`QemuInstance::drain_serial`] cannot have: its caller passes a
/// number of seconds, and a number of seconds is a claim about the host. Here
/// the wait ends when the guest goes quiet or wedges, so a guest with a twelfth
/// of the machine costs the run wall clock and never a verdict — and when it
/// does end early the message says so in the words the classifier reads
/// ([`STALLED`]), above the [`window`] of what the guest said during the wait.
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
    let mut live = Liveness::new(GUEST_QUIET, qemu.budget(GUEST_WEDGED));
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
    // Under the sentence, which stays the line a summary quotes: a wait that
    // ended on nothing has no other account of what the guest did instead.
    Err(format!(
        "{STALLED} waiting for {doing} — {}\n--- what the guest said while it was waited on ---\n{}",
        live.why(),
        window(since)
    ))
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
/// publishes no GOP and `kernel_args.gop_framebuffer` is zero. `Metal` is the
/// target laptop's shape, with `-vga std` so firmware publishes a linear
/// framebuffer, the only path in which the on-screen panic console renders
/// anything.
#[derive(Clone, Copy, PartialEq)]
pub enum Profile {
    Headless,
    /// [`Profile::Headless`] with no unit at all: the negative control for
    /// whether a virtio function is behind one. QEMU offers
    /// `VIRTIO_F_ACCESS_PLATFORM` only for a function created with
    /// `iommu_platform=on`, and the harness sets that only where a unit exists,
    /// so the guest's own negotiation comes out the other way here.
    HeadlessNoIommu,
    /// [`Profile::Headless`] with QEMU's `e1000e` for its NIC, the 82574L
    /// `toyos-i219` drives beside the T14's I219: the one guest in which
    /// netstack runs its Intel driver, a ring of fifteen frames and a wake
    /// for transmit room.
    HeadlessE1000e,
    /// [`Profile::Headless`] with no USB controller: the machine whose NVMe
    /// disk is the only storage it has, and the only device its firmware can
    /// boot.
    HeadlessNoUsb,
    /// [`Profile::Headless`] with a virtio-gpu behind the unit: a virtio
    /// function the kernel still drives itself beside the console, which is
    /// what `virtio-no-access-platform` withholds the bit from.
    HeadlessVirtioGpu,
    /// [`Profile::Headless`] with a second controller, QEMU's `qemu-xhci`
    /// on MSI and with no MSI-X, carrying a stick and a keyboard: the
    /// controller `xhci-leave=1b36:000d` leaves to `usbd`, armed on MSI as
    /// the T14's two are, because a claim never maps the BAR that holds an
    /// MSI-X table and QEMU puts this one's in its registers' BAR.
    HeadlessUsbSpare,
    /// M1 metal-sim: GOP, NVMe, xHCI with the boot stick on it, i8042 from
    /// q35, and nothing else -- no virtio device and no USB HID. This is the
    /// machine shape that gets flashed, so it is the one the input tests run
    /// on. The 16550 stays: every defect metal-sim has found came from the
    /// device shape, and with a console the guest can be driven over the
    /// ===TEST_START=== protocol like any other. [`BootOptions::mute`] takes
    /// it away for the one test that certifies the T14's literal shape.
    Metal,
    /// QEMU `virt` on AArch64 (GICv3, AAVMF): a GOP from `ramfb`, the boot
    /// stick on an xHCI, the PL011, a virtio-rng for firmware's
    /// `EFI_RNG_PROTOCOL`, and nothing else — no NIC, NVMe or IOMMU. The
    /// machine the AArch64 port runs on, under HVF on an Apple host and
    /// emulated at EL1 elsewhere, and the only profile that is not a q35.
    Virt,
    /// [`Profile::Virt`] with no virtio-rng, on a CPU with no RNDR: the host's
    /// under HVF, and `cortex-a72` where it is emulated. Firmware then has no
    /// `EFI_RNG_PROTOCOL`, so the kernel's generator has nothing to be keyed
    /// from.
    VirtNoRng,
    /// [`Profile::Virt`] with EL2 (`virtualization=on`), emulated on `-cpu max`:
    /// firmware then hands the loader the CPU at EL2, and the kernel's entry
    /// has to drop from it. HVF gives a guest EL1 only.
    VirtEl2,
    /// [`Profile::VirtEl2`] on `-cpu cortex-a72`, which has no FEAT_VHE: any
    /// firmware hands the loader EL2 with `HCR_EL2.E2H` clear, where an `_el1`
    /// register name is EL1's own, so the loader's EL2 arm alone turns EL2's
    /// MMU off.
    VirtEl2NoVhe,
    /// [`Profile::Virt`] emulated on `-cpu max` whatever the host: firmware
    /// hands the loader the CPU at EL1 and QEMU's FADT names PSCI's conduit
    /// `HVC`, as under HVF. `virt_el1_smp`'s machine while a boot's last word
    /// can miss the console under HVF.
    VirtTcg,
    /// [`Profile::Virt`] with its SMMUv3 and two of QEMU's `iommu-testdev`, a
    /// function that writes where it is told to through the unit.
    VirtSmmu,
}

impl Profile {
    /// The architecture this machine is.
    pub fn arch(self) -> Arch {
        match self {
            Self::Virt | Self::VirtNoRng | Self::VirtEl2 | Self::VirtEl2NoVhe | Self::VirtTcg | Self::VirtSmmu => {
                Arch::Aarch64
            }
            Self::Headless
            | Self::HeadlessNoIommu
            | Self::HeadlessE1000e
            | Self::HeadlessNoUsb
            | Self::HeadlessVirtioGpu
            | Self::HeadlessUsbSpare
            | Self::Metal => Arch::X86_64,
        }
    }

    /// How this host provides the machine.
    pub fn accel(self) -> Accel {
        match self {
            Self::VirtEl2 | Self::VirtEl2NoVhe | Self::VirtTcg => Accel::Tcg,
            _ => self.arch().accel(),
        }
    }

    /// The CPU this machine has.
    fn cpu(self) -> &'static str {
        match self {
            Self::VirtEl2NoVhe => "cortex-a72",
            Self::VirtNoRng if !self.accel().is_hardware() => "cortex-a72",
            _ => self.arch().cpu(self.accel()),
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

/// [`Profile::HeadlessUsbSpare`]'s second controller, and the stick on it,
/// whose bytes are zeros and whose writes go nowhere: what is on it is no
/// question this machine asks. `msi=on` stated, because with MSI-X off and
/// MSI left `auto` this QEMU's `qemu-xhci` offers neither, and the kernel
/// refuses the claim for it.
const XHCI_SPARE: &str = "qemu-xhci,id=spare,msix=off,msi=on";
const SPARE_STICK: &str = "driver=null-co,node-name=spare-stick,size=67108864,read-zeroes=on";

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
    /// QEMU's model of the 82574L.
    E1000e,
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
    /// The `-blockdev` behind each device in [`Shape::usb`] that names a
    /// `drive=`, and nothing else: one no device names is refused.
    blockdevs: &'static [&'static str],
    storage: Storage,
    /// The unit that decodes this machine's DMA, or its absence. Stated per
    /// profile because absence is a shape and because the unit's own
    /// capabilities are what the kernel reads at boot.
    iommu: Option<Iommu>,
    /// `virt`'s SMMUv3, which is a machine property rather than a device.
    smmu: Smmu,
    /// A virtio-rng, which is firmware's alone: edk2's driver puts
    /// `EFI_RNG_PROTOCOL` behind it for the loader's seed, and the kernel
    /// drives no such device. `virt` has it because an HVF guest's CPU has no
    /// RNDR for firmware or the kernel to draw from; a q35's firmware answers
    /// the protocol from RDRAND without one.
    rng: bool,
    /// A virtio-gpu, which the kernel drives.
    virtio_gpu: bool,
}

/// Whether `virt` has its SMMUv3. With it come two of QEMU's `iommu-testdev`,
/// whose writes are the only DMA a guest of this suite can aim at an address
/// of its choosing: a unit no function writes through is a unit no test reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Smmu {
    Absent,
    WithTestdev,
}

/// Where a machine's image and its DATA are. A size is stated because a
/// structure sized per device block is bounded by it and by nothing else; the
/// backing file is sparse either way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Storage {
    /// The image on a USB stick on the first xHCI, which the kernel drives,
    /// and DATA on an NVMe disk of its own of this many bytes; zero is a
    /// machine with no NVMe controller.
    Stick { nvme_bytes: u64 },
    /// One NVMe disk and no stick: DATA, of this many bytes, and the image
    /// installed after it (`toyos_build::image::install`). The firmware boots
    /// it, and after the loader every partition of it is `diskserver`'s.
    Disk { data_bytes: u64 },
}

impl Storage {
    /// The id of the `-drive` the image's file is.
    fn image_drive(self) -> &'static str {
        match self {
            Self::Stick { .. } => "stick",
            Self::Disk { .. } => "disk",
        }
    }
}

/// The boot stick's device id: the removal the owner's machine dies on is the
/// one device whose removal takes `/boot` and `/log` with it.
pub const BOOT_STICK_ID: &str = "bootstick";

/// The boot stick's serial number string. Stated rather than left to QEMU,
/// whose default is built from the port the device is on, so the same stick
/// plugged into another port would read as another unit — which is exactly
/// what a test moving it has to be able to say is not so.
pub const BOOT_STICK_SERIAL: &str = "TOYOS0BOOTSTICK1";

/// The DATA every x86-64 profile gives the guest. Large enough for a
/// filesystem, small enough that a boot formats it quickly.
pub const NVME_SMALL: u64 = 128 * 1024 * 1024;

impl Profile {
    fn shape(self) -> Shape {
        match self {
            Self::VirtEl2 | Self::VirtEl2NoVhe | Self::VirtTcg => Self::Virt.shape(),
            Self::VirtNoRng => Shape { rng: false, ..Self::Virt.shape() },
            Self::VirtSmmu => Shape { smmu: Smmu::WithTestdev, ..Self::Virt.shape() },
            Self::Virt => Shape {
                vga: "std",
                panel: None,
                virtio: Virtio::Absent,
                nic: Nic::Absent,
                xhci: &[XHCI_DEFAULT],
                usb: &[],
                blockdevs: &[],
                storage: Storage::Stick { nvme_bytes: 0 },
                iommu: None,
                smmu: Smmu::Absent,
                rng: true,
                virtio_gpu: false,
            },
            Self::Headless => Shape {
                vga: "none",
                panel: None,
                virtio: Virtio::Present,
                nic: Nic::Virtio,
                xhci: &[XHCI_DEFAULT],
                usb: &["usb-kbd,bus=xhci.0"],
                blockdevs: &[],
                storage: Storage::Disk { data_bytes: NVME_SMALL },
                iommu: Some(IOMMU_DEFAULT),
                smmu: Smmu::Absent,
                rng: false,
                virtio_gpu: false,
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
                blockdevs: &[],
                storage: Storage::Stick { nvme_bytes: NVME_SMALL },
                iommu: Some(IOMMU_DEFAULT),
                smmu: Smmu::Absent,
                rng: false,
                virtio_gpu: false,
            },
            Self::HeadlessNoIommu => Shape { iommu: None, ..Self::Headless.shape() },
            Self::HeadlessE1000e => Shape { nic: Nic::E1000e, ..Self::Headless.shape() },
            Self::HeadlessNoUsb => Shape { xhci: &[], usb: &[], ..Self::Headless.shape() },
            Self::HeadlessVirtioGpu => Shape { virtio_gpu: true, ..Self::Headless.shape() },
            Self::HeadlessUsbSpare => Shape {
                xhci: &[XHCI_DEFAULT, XHCI_SPARE],
                usb: &[
                    "usb-kbd,bus=xhci.0",
                    "usb-storage,bus=spare.0,drive=spare-stick",
                    "usb-kbd,bus=spare.0",
                ],
                blockdevs: &[SPARE_STICK],
                ..Self::Headless.shape()
            },
        }
    }

    /// The unit this profile puts on the machine, or `None`.
    pub fn iommu(self) -> Option<Iommu> {
        self.shape().iommu
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
    /// Have QEMU record every PSCI call a vCPU makes into this file, with the
    /// calling CPU's affinity: the firmware side's own account of what the
    /// kernel asked of it.
    pub psci_trace: Option<PathBuf>,
    /// A second slot beside the one the image boots, with room for an
    /// update's ROOT of this many bytes: a machine that updates itself.
    /// `None` for every guest whose subject is not the update.
    pub second_slot: Option<u64>,
    /// Whole ACPI tables, header and checksum included, that QEMU lists
    /// beside its own (`-acpitable`): firmware AML a guest's own tables do
    /// not carry. q35's alone.
    pub acpi_tables: Vec<Vec<u8>>,
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
/// It is that runner's own first line and nothing else's. The supervisor spawns its
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
            psci_trace: None,
            second_slot: None,
            acpi_tables: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    /// The window as the guest wrote it: [`Self::stdout`]'s lines and the
    /// kernel's records of the same span, which `stdout` leaves out.
    pub serial: String,
    /// Why the run did not finish, when it did not.
    ///
    /// A [`WaitVerdict`] and not a `String`, so that the sentence and the
    /// kernel's own account of its death cannot come apart — see that type.
    /// Every arm that formats this gets the report for free, and there are
    /// fifty-two of them that were never going to be edited one at a time.
    pub error: Option<WaitVerdict>,
}

pub struct QemuInstance {
    child: Child,
    /// What ends QEMU when this process dies without dropping this.
    _tether: Tether,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
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
    /// What this machine was booted with, and the 16550's file: what a
    /// second wait for its ready marker reads.
    options: BootOptions,
    uart_log: PathBuf,
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
    second_slot: Option<u64>,
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
            actuators.iter().chain(params).any(|a| toyos_build::build::arms(a, name))
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
    let mut plan = toyos_build::build::Plan::new(arch, &config_path, kernel_features, kernel_params);
    plan.second = second_slot.map(|root_bytes| toyos_build::image::SecondSlot { root_bytes });
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
/// and a `/log` file's alike. Nothing else writes one — a program's line
/// reaches both only through `logkeeper`, under the program's own head.
pub fn is_kernel_line(line: &str) -> bool {
    toyos_logstream::is_kernel_line(line)
}

/// A console line's text as its program wrote it: a program's line without
/// the head `logkeeper` gives it (`toyos_logstream::program_line`), and any other
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
            options.second_slot,
        );
        let storage = options.profile.shape().storage;
        match storage {
            Storage::Stick { .. } => fs::write(&boot_image, image).expect("Failed to write test boot image"),
            Storage::Disk { data_bytes } => toyos_build::image::install(&image, &boot_image, data_bytes),
        }
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
        // `/home` — sshserver's host identity, a package, a cache — is never the
        // premise of whatever test the lane runs next. A disk that carries the
        // image is its boot's own file, made just above; a DATA disk beside a
        // stick is the lane's one file, remade rather than a file per boot.
        //
        // One live guest per image, claimed here rather than discovered from
        // QEMU's stderr after the second process has already exited — see
        // [`NvmeClaim`] — and claimed before the remaking, which truncates.
        let nvme_bytes = match storage {
            Storage::Stick { nvme_bytes } => nvme_bytes,
            Storage::Disk { .. } => 0,
        };
        let nvme_image = if nvme_bytes == 0 {
            // A profile with no NVMe disk beside its image gets no backing
            // file either; the path is never passed to QEMU.
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
        for (i, table) in options.acpi_tables.iter().enumerate() {
            fs::write(acpi_table(&sockets.dir, i), table).expect("[qemu] write an added ACPI table");
        }
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
            options,
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

    /// From here a reset this guest asks for resets the machine, its memory
    /// and its disks kept, where `-no-reboot` would have ended QEMU with it.
    pub fn reset_on_reboot(&mut self) {
        let socket = self.sockets.qmp.clone().expect("reset_on_reboot needs BootOptions { qmp: true }");
        Qmp::connect(&socket).execute("{\"execute\":\"set-action\",\"arguments\":{\"reboot\":\"reset\"}}");
    }

    /// Wait for this machine to reach its ready marker again, on the boot
    /// after a reset: [`Self::boot_log`] is that boot's from here, opening
    /// with what the boot before it said last.
    pub fn await_boot(&mut self) {
        self.boot_log = wait_for_ready(&mut self.child, &self.rx, &self.options, &self.uart_log);
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

    /// Reset the machine with its memory kept, and hold it at the next reset
    /// the guest itself asks for: the boot that follows keeps its screen,
    /// stopped, where a `-no-reboot` QEMU would have exited with it. The
    /// machine is stopped while QEMU's reset actions change, so the guest has
    /// no reset of its own to land between them.
    pub fn reset_and_hold_the_next(&mut self, budget: Duration) -> Result<(), String> {
        let socket = self.sockets.qmp.clone().expect("reset_and_hold_the_next needs BootOptions { qmp: true }");
        let mut qmp = Qmp::connect(&socket);
        qmp.execute("{\"execute\":\"stop\"}");
        qmp.execute("{\"execute\":\"set-action\",\"arguments\":{\"reboot\":\"reset\"}}");
        qmp.execute("{\"execute\":\"system_reset\"}");
        qmp.execute("{\"execute\":\"set-action\",\"arguments\":{\"reboot\":\"shutdown\",\"shutdown\":\"pause\"}}");
        qmp.execute("{\"execute\":\"cont\"}");
        qmp.stream.set_read_timeout(Some(self.budget(budget))).expect("qmp: the hold's budget");
        match QmpShutdown(qmp).reason().as_deref() {
            Some("guest-reset") => Ok(()),
            Some(other) => Err(format!("the boot after the reset stopped by {other:?}, not by a reset of its own")),
            None => Err(format!("the boot after the reset never asked for a reset within {budget:?}")),
        }
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
    /// The panic handler's own path paints after the drain that emits the
    /// report, so a marker on serial does not yet prove a paint.
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

    pub fn stdin_mut(&mut self) -> &mut BufWriter<ChildStdin> {
        &mut self.stdin
    }

    pub fn flush_stdin(&mut self) {
        self.stdin.flush().expect("Failed to flush QEMU stdin");
    }

    /// Wait for QEMU to exit within `by`: its console closing is the event, and
    /// the process is reaped after it. Answers what the guest said on the way.
    /// A file QEMU finishes only at its exit is whole once this answers, and is
    /// still there until this instance is dropped.
    pub fn await_exit(&mut self, by: Duration) -> Result<String, String> {
        let deadline = Instant::now() + by;
        let mut said = String::new();
        loop {
            let left = deadline.checked_duration_since(Instant::now()).unwrap_or_default();
            match self.rx.recv_timeout(left) {
                Ok(line) => {
                    said.push_str(&line);
                    said.push('\n');
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("QEMU had not exited {} s after it was asked to\n{said}", by.as_secs()))
                }
            }
        }
        let status = self.child.wait().map_err(|e| format!("QEMU could not be waited for: {e}"))?;
        if !status.success() {
            return Err(format!("QEMU exited {status}\n{said}"));
        }
        Ok(said)
    }

    /// [`budget_smp`] for a host-side wait on this guest's own vCPU count.
    pub fn budget(&self, ceiling: Duration) -> Duration {
        budget_smp(ceiling, self.smp)
    }

    /// Keep collecting serial output for `dur` after a test has returned.
    /// **Not a ceiling, and not paid out as one**: callers use it to *pace* —
    /// "let the guest run for 400 ms and tell me what it said" — so scaling it
    /// would buy a test a longer sleep and a slow guest nothing.
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
    /// waiting for a machine that will never speak again.
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

    /// The disk this guest booted from, which it writes: read back while the
    /// guest runs, since the instance's end deletes it.
    pub fn boot_image(&self) -> &Path {
        &self.boot_image
    }

    pub fn run_test(&mut self, name: &str, timeout: Duration) -> TestResult {
        self.run_test_paced(name, timeout, |_, _| {})
    }

    /// `run_test`, with `step` run on every console line the guest prints.
    ///
    /// A step driven by the guest's own output can stay behind it, which is
    /// how an injection test costs a slow guest wall-clock instead of a
    /// verdict.
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
                    serial,
                    error: Some(error),
                };
            }

            match self.rx.recv_timeout(VERDICT_POLL) {
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
                        // `logkeeper` and the kernel's records through `klogd`**, so
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
                            serial,
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
                        serial,
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

/// QEMU's own account of why a guest stopped, off the `SHUTDOWN` event. Held
/// open across the stop: the event is emitted once and QEMU exits behind it, so
/// a connection opened afterwards finds nothing.
pub struct QmpShutdown(Qmp);

impl QmpShutdown {
    /// `budget` bounds the wait and is set here, while the peer is still there
    /// to accept it: macOS refuses a `setsockopt` on a socket already closed.
    pub fn open(socket: &Path, budget: Duration) -> Self {
        let qmp = Qmp::connect(socket);
        qmp.stream.set_read_timeout(Some(budget)).expect("qmp: the shutdown-event budget");
        Self(qmp)
    }

    /// Press the guest's power button: QEMU raises the ACPI fixed event, the
    /// way the button on a machine does.
    pub fn power_button(&mut self) {
        self.0.execute("{\"execute\":\"system_powerdown\"}");
    }

    /// The `reason` the `SHUTDOWN` event names — `guest-reset`,
    /// `guest-shutdown`, `host-signal` — or `None` if the guest never stopped.
    pub fn reason(&mut self) -> Option<String> {
        let qmp = &mut self.0;
        loop {
            if let Some(reason) = shutdown_reason(&qmp.pending) {
                return Some(reason);
            }
            let mut buf = [0u8; 4096];
            match qmp.stream.read(&mut buf) {
                // Budget spent, or the socket ended: what it had is in `pending`.
                Ok(0) | Err(_) => return shutdown_reason(&qmp.pending),
                Ok(n) => qmp.pending.extend_from_slice(&buf[..n]),
            }
        }
    }
}

/// The `reason` of the `SHUTDOWN` event in what QMP has sent so far.
fn shutdown_reason(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().find(|l| l.contains("\"SHUTDOWN\""))?;
    let (_, after) = line.split_once("\"reason\"")?;
    let (_, value) = after.split_once('"')?;
    let (value, _) = value.split_once('"')?;
    Some(value.to_string())
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
    // them enough to make netstack claim a NIC on the machine whose whole point is
    // that it has none. `-net none` and `-nic none` are gone in QEMU 11; this
    // is the option that does it, and it leaves i8042/ps2-kbd/ps2-mouse alone.
    qemu.arg("-nodefaults");

    // `kernel-irqchip=split` only when there is a unit: interrupt remapping
    // needs the userspace half of the irqchip, and a machine with no unit has
    // no reason to be built differently from the one it has always been.
    let mut machine = match arch {
        Arch::X86_64 => {
            assert!(shape.smmu == Smmu::Absent, "a q35 has no SMMUv3");
            arch.machine().to_string()
        }
        Arch::Aarch64 => {
            // The unit a profile declares is VT-d, which `virt` has none of.
            assert!(shape.iommu.is_none(), "`virt` has no VT-d");
            let machine = match options.profile {
                Profile::VirtEl2 | Profile::VirtEl2NoVhe => {
                    format!("{},gic-version=3,virtualization=on", arch.machine())
                }
                _ => format!("{},gic-version=3", arch.machine()),
            };
            match shape.smmu {
                Smmu::Absent => machine,
                Smmu::WithTestdev => format!("{machine},iommu=smmuv3"),
            }
        }
    };
    if shape.iommu.is_some() {
        machine.push_str(",kernel-irqchip=split");
    }

    // `virt` puts RAM at 1 GiB and AAVMF allocates from its top, so with 4 GiB
    // the loader's allocations land past the 4 GiB its boot map reaches and it
    // refuses the boot: issues/the-boot-map-reaches-4-gib-and-firmware-decides-what-lands-in-it.md.
    let memory = match arch {
        Arch::X86_64 => "4G",
        Arch::Aarch64 => "2G",
    };
    qemu.arg("-machine")
        .arg(&machine)
        .arg("-cpu")
        .arg(options.profile.cpu())
        .arg("-smp")
        .arg(options.smp.to_string())
        .arg("-m")
        .arg(memory)
        .arg("-drive")
        .arg(firmware_code)
        .arg("-drive")
        .arg(firmware_vars)
        .arg("-drive")
        .arg(format!("if=none,id={},format=raw,file={}", shape.storage.image_drive(), boot_image.display()));
    assert!(!shape.xhci.is_empty() || shape.usb.is_empty(), "a USB device needs a controller");
    assert!(
        !shape.xhci.is_empty() || matches!(shape.storage, Storage::Disk { .. }),
        "a boot stick needs a controller"
    );

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

    if let Storage::Stick { .. } = shape.storage {
        qemu.arg("-device").arg(format!(
            "usb-storage,bus=xhci.0,drive=stick,id={BOOT_STICK_ID},serial={BOOT_STICK_SERIAL},bootindex=0"
        ));
    }
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
    // Its MSI-X table in a BAR of its own, because diskserver drives it and a claim
    // never maps the BAR holding the table. One controller and one namespace
    // whichever disk it is: diskserver reads namespace 1 of the one controller
    // its row names.
    let namespace = match shape.storage {
        Storage::Stick { nvme_bytes: 0 } => None,
        Storage::Stick { .. } => {
            qemu.arg("-drive").arg(format!("if=none,id=nvme0,format=raw,file={}", nvme_image.display()));
            Some("drive=nvme0")
        }
        Storage::Disk { .. } => Some("drive=disk,bootindex=0"),
    };
    if let Some(namespace) = namespace {
        qemu.arg("-device")
            .arg("nvme,serial=deadbeef,id=nvme0ctl,msix-exclusive-bar=on")
            .arg("-device")
            .arg(format!("nvme-ns,{namespace},bus=nvme0ctl,logical_block_size=512,physical_block_size=512"));
    }
    for blockdev in shape.blockdevs {
        let node = blockdev.split(',').find_map(|kv| kv.strip_prefix("node-name=")).expect("a blockdev names its node");
        assert!(
            shape.usb.iter().any(|dev| dev.split(',').any(|kv| kv == format!("drive={node}"))),
            "the blockdev {node:?} backs no USB device this machine has"
        );
        qemu.arg("-blockdev").arg(*blockdev);
    }
    for dev in shape.usb {
        qemu.arg("-device").arg(*dev);
    }
    if shape.rng {
        qemu.arg("-device").arg("virtio-rng-pci");
    }
    if shape.virtio_gpu {
        qemu.arg("-device").arg(format!("virtio-gpu-pci{platform}"));
    }
    if shape.smmu == Smmu::WithTestdev {
        // Two, the first enumerated below the second: the kernel's selftest
        // routes nothing for the first, whose entry the table holds.
        qemu.arg("-device").arg("iommu-testdev");
        qemu.arg("-device").arg("iommu-testdev");
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
        Nic::E1000e => {
            qemu.arg("-netdev").arg("user,id=net0").arg("-device").arg("e1000e,netdev=net0");
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
    assert!(options.acpi_tables.is_empty() || arch == Arch::X86_64, "an added ACPI table is q35's");
    for i in 0..options.acpi_tables.len() {
        qemu.arg("-acpitable").arg(format!("file={}", acpi_table(socket_dir, i).display()));
    }
    if let Some(socket) = qmp_socket {
        qemu.arg("-qmp")
            .arg(format!("unix:{},server,nowait", socket.display()));
    }
    if let Some(trace) = &options.psci_trace {
        assert!(
            arch == Arch::Aarch64 && accel == Accel::Tcg,
            "a PSCI trace is TCG's `arm_psci_call`, and this profile's PSCI is not QEMU's TCG"
        );
        qemu.arg("-trace").arg("arm_psci_call").arg("-D").arg(trace);
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

/// The `i`th of [`BootOptions::acpi_tables`], as a file in a boot's own `dir`.
fn acpi_table(dir: &Path, i: usize) -> PathBuf {
    dir.join(format!("acpi-{i}.aml"))
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

fn spawn_and_wait_ready(mut qemu: Command, options: BootOptions, files: Files) -> QemuInstance {
    let Files { seq, uart_log, nvme, sockets, screendump, boot_image, vars, carried } = files;

    // Inherited: `orphan` reads QEMU's exit as the end of its harness's stderr.
    qemu.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if VERBOSE.load(Ordering::Relaxed) {
        eprintln!("[qemu {seq}] Launching QEMU...");
    }
    let (mut child, tether) = toyos_build::tether::spawn(qemu).expect("Failed to launch QEMU");

    let stdin = BufWriter::new(child.stdin.take().unwrap());
    let stdout = child.stdout.take().unwrap();

    let (tx, rx) = mpsc::channel::<String>();
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
        // consumer below still gets whole lines and nothing else.
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
        wait_for_ready(&mut child, &rx, &options, &uart_log)
    };
    // The ready marker is test-runner's, which the supervisor starts after its
    // first line, so a boot that reached it has said its build, and the kernel
    // named the ROOT that carries it before that. Both are read off the image
    // this boot was handed, never off the checkout, which may have moved since.
    if options.ready_marker == DEFAULT_READY && !options.mute {
        let (root, bytes) = toyos_build::image::root_file_on(&boot_image, toyos_osrelease::PATH)
            .unwrap_or_else(|why| panic!("[qemu] {why}"));
        let release = toyos_osrelease::parse(&bytes)
            .unwrap_or_else(|why| panic!("[qemu] {} on {} is not the build's: {why:?}", toyos_osrelease::PATH, boot_image.display()));
        let named = format!("boot: root={root}");
        let said = format!(
            "{}{} {}, committed {} UTC, toolchain {}, {}",
            toyos_osrelease::SAID,
            release.commit.as_str(),
            release.tree,
            toyos_wallclock::Civil::from_unix_secs(release.committed),
            release.toolchain.as_str(),
            release.arch.machine()
        );
        for line in [&named, &said] {
            if !boot_log.lines().any(|l| l.contains(line.as_str())) {
                let _ = child.kill();
                panic!("[qemu] the boot never said `{line}`, of the image this run built\nconsole:\n{boot_log}");
            }
        }
    }

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
        smp: options.smp,
        carried,
        options,
        uart_log,
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
    if VERBOSE.load(Ordering::Relaxed) {
        // The boot's own number, because `--nocapture` on a wide run is several
        // guests talking into one terminal and an unattributed line is worse
        // than no line.
        eprintln!("[serial {seq}] {line}");
    }
    tx.send(line).is_ok()
}

/// A boot's ceiling: [`budget_smp`]'s rule over the tests that are one boot and
/// nothing after it.
pub const BOOT_CEILING: Duration = Duration::from_secs(63);

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
    let boot_timeout = budget_smp(BOOT_CEILING, options.smp);
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
            // was the supervisor or one of its children and nothing else is going to
            // reach the marker.
            //
            // A process that ended *itself* is not on that list, and the
            // difference is not academic: `sshserver` panicked across four recorded
            // boots that then came up perfectly, losing a race with `netstack`'s
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
                panic!("[qemu] The supervisor process crashed during boot:\n{crash_msg}");
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
