//! `cargo run -- --ci <job>`: every CI job's logic, so a workflow is a
//! checkout, a cache and one line, and this host runs the same job to the same
//! verdict.
//!
//! A host job runs every step and reds if any failed; a guest job stops at the
//! first failure among the instrument, the toolchain and the suite, because
//! what follows a wrong instrument or a missing toolchain measures nothing —
//! but its last step, the private `$TMPDIR` it shares [`host`]'s rule for,
//! always runs, because a leak past a failing suite is still a leak. Each
//! step's verdict goes to
//! `$GITHUB_STEP_SUMMARY` where a runner provides one.
//!
//! **The instrument is declared once.** `.github/qemu-version` is the QEMU
//! every guest is measured with — the version has been measured to decide
//! verdicts. A guest job
//! reds on a disagreement, and on a `/dev/kvm` that is present and does not
//! open; `cargo run` only notes one, because a build must not stop for brew.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Once;
use std::time::{Duration, Instant};

use crate::arch::{Accel, Arch};
use crate::cicache;
use crate::sysroot::git_out;
use crate::userlandhost::{Host, Os, Program};
use crate::{flags, release, sdkversion};

const USAGE: &str = "cargo run -- --ci <job>, where <job> is one of:
  host              every host test: the build system, the harness's own checks,
                    the workspace's host members, the licences of what ships,
                    clippy, the model controls, userland and the SDK (ci.yml, nightly)
  seal              `host` from a cold tree, then that tree sealed as the host
                    cache's entry, which the step after it saves (nightly)
  toolchain         the cache entry of each store of this tree's toolchain
  bootstrap         build the stores of this tree's toolchain its job did not restore
  guest             the guest suite, on the sysroot its job restored
  release           put the sysroot main's nightly restored up as its toolchain release
  publish           put main's SDK crates on crates.io (publish.yml)";

#[derive(Debug, PartialEq, Eq)]
enum Job {
    Host,
    Seal,
    Toolchain,
    Bootstrap,
    Guest,
    Release,
    Publish,
}

fn parse(words: &[String]) -> Result<Job, String> {
    let job = match words.first().map(String::as_str) {
        Some("host") => Job::Host,
        Some("seal") => Job::Seal,
        Some("toolchain") => Job::Toolchain,
        Some("bootstrap") => Job::Bootstrap,
        Some("guest") => Job::Guest,
        Some("release") => Job::Release,
        Some("publish") => Job::Publish,
        Some(other) => return Err(format!("no CI job is called {other:?}")),
        None => return Err("which job?".to_string()),
    };
    if words.len() > 1 {
        return Err(format!("{:?} takes nothing after it: {:?}", words[0], &words[1..]));
    }
    Ok(job)
}

pub fn dispatch(root: &Path, args: &[String]) {
    let job = parse(flags::CARGO_RUN.rest(args, &flags::CI)).unwrap_or_else(|refusal| {
        eprintln!("Error: {refusal}\n{USAGE}");
        std::process::exit(2);
    });
    forget_own_package();
    let steps = match &job {
        Job::Host => host(root),
        Job::Seal => seal(root),
        Job::Toolchain => vec![step("the stores of this tree's toolchain", || release::toolchain(root))],
        Job::Bootstrap => vec![step("this tree's toolchain", || release::bootstrap(root))],
        Job::Guest => guest(root, &suite_args(&["--jobs", "1"])),
        Job::Release => vec![step("main's toolchain release", || release::release(root))],
        Job::Publish => vec![step("the SDK crates on crates.io", || publish(root))],
    };
    let failed: Vec<&Step> = steps.iter().filter(|s| s.verdict.is_err()).collect();
    summary(&steps.iter().map(Step::line).collect::<Vec<_>>().join("\n"));
    if failed.is_empty() {
        eprintln!("[ci] {job:?}: {} step(s), all green", steps.len());
    } else {
        eprintln!("[ci] {job:?}: {} of {} step(s) red:", failed.len(), steps.len());
        for s in failed {
            eprintln!("  {}", s.line());
        }
        std::process::exit(1);
    }
}

/// Take what `cargo run` told the driver about its own package out of what
/// every step inherits, before any thread.
///
/// Cargo judges a build script's `rerun-if-env-changed` by its own
/// environment, and ring's names these: left in, a step's cargo finds ring
/// stale against the build the job's cargo made without them, and builds it,
/// its dependents and this crate a second time.
fn forget_own_package() {
    for (name, _) in std::env::vars_os() {
        let own = name.to_str().is_some_and(|n| n.starts_with("CARGO_PKG_") || n.starts_with("CARGO_MANIFEST_"));
        if own {
            std::env::remove_var(name);
        }
    }
}

/// One step's verdict: a sentence on green, the refusal on red.
struct Step {
    label: String,
    verdict: Result<String, String>,
}

impl Step {
    fn line(&self) -> String {
        match &self.verdict {
            Ok(said) => format!("- green: {} ({said})", self.label),
            Err(why) => format!("- RED: {}: {why}", self.label),
        }
    }
}

fn step(label: &str, f: impl FnOnce() -> Result<String, String>) -> Step {
    eprintln!("\n=== [ci] {label}");
    let verdict = f();
    let (Ok(said) | Err(said)) = &verdict;
    eprintln!("[ci] {label}: {said}");
    Step { label: label.to_string(), verdict }
}

fn on_runner() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
}

/// Append to the runner's job summary; nowhere off a runner.
fn summary(text: &str) {
    let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
        let _ = writeln!(file, "{text}");
    }
}

/// `cargo <args>` in `dir`, as [`cargo_logged`] runs it, judged by its exit.
fn cargo(dir: &Path, args: &[&str]) -> Result<String, String> {
    let (status, _) = cargo_logged(dir, args)?;
    let line = format!("cargo {}", args.join(" "));
    if status.success() {
        Ok(line)
    } else {
        Err(format!("{line} exited {status}"))
    }
}

/// How long a step's cargo may say nothing: a build prints a line as each
/// crate starts, libtest one as each test ends, and the guest harness ends a
/// silent guest long before this. A hang ceiling and no measure of a step.
/// Shortened under `cfg(test)` to what a loaded host starts a built binary
/// in, so the gate on it costs seconds.
#[cfg(not(test))]
const QUIET: Duration = Duration::from_secs(15 * 60);
#[cfg(test)]
const QUIET: Duration = Duration::from_secs(10);

/// How long the processes of a signalled group may take to let go of its output.
const GONE: Duration = Duration::from_secs(10);

/// The signals that end the driver from outside.
const INTERRUPTS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The process group [`heard`] is running, for [`interrupted`]; 0 between
/// two, and [`STARTING`] while one is being spawned.
static GROUP: AtomicI32 = AtomicI32::new(0);

/// [`GROUP`] while a command is being spawned: its group has no name yet.
const STARTING: i32 = -1;

/// The last interrupt [`interrupted`] took, for [`started`]; 0 for none.
static PENDING: AtomicI32 = AtomicI32::new(0);

/// The driver's own interrupt, handed to the group it started and then taken:
/// a group of its own is out of a terminal's reach, and a step left running
/// has lost the driver that bounded it. One that arrives while a command is
/// being spawned is kept for [`started`].
extern "C" fn interrupted(signal: libc::c_int) {
    // Written before `GROUP` is read, as `heard` reads this after every write
    // of `GROUP`: whatever thread this runs on, one of the two sees the other.
    PENDING.store(signal, Ordering::SeqCst);
    let group = GROUP.load(Ordering::SeqCst);
    if group == STARTING {
        return;
    }
    // SAFETY: three async-signal-safe calls; the signal raised at its default ends the driver.
    unsafe {
        if group != 0 {
            libc::killpg(group, signal);
        }
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Say which group the driver runs now, 0 for none, and take the interrupt
/// the handler took before it could read that.
fn started(group: i32) {
    GROUP.store(group, Ordering::SeqCst);
    let signal = PENDING.load(Ordering::SeqCst);
    if signal != 0 {
        interrupted(signal);
    }
}

/// What a command says, passed through as it arrives and kept: in bytes, so
/// a line still unfinished is said too. libtest on one thread names a test
/// before it runs it and ends the line after.
struct Said {
    chunks: Receiver<Vec<u8>>,
    log: Vec<u8>,
}

impl Said {
    /// `Some(true)` for more of it within `within`, `Some(false)` at its end,
    /// which is every process that held it gone, and `None` for neither.
    fn more(&mut self, within: Duration) -> Option<bool> {
        match self.chunks.recv_timeout(within) {
            Ok(chunk) => {
                let _ = std::io::stderr().write_all(&chunk);
                self.log.extend(chunk);
                Some(true)
            }
            Err(RecvTimeoutError::Disconnected) => Some(false),
            Err(RecvTimeoutError::Timeout) => None,
        }
    }

    /// Whether it ended within [`GONE`].
    fn ended(&mut self) -> bool {
        let deadline = Instant::now() + GONE;
        loop {
            match self.more(deadline.saturating_duration_since(Instant::now())) {
                Some(true) => {}
                Some(false) => return true,
                None => return false,
            }
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.log).into_owned()
    }

    /// The last line with anything on it, finished or not, to the length a
    /// refusal can carry: a command may say megabytes without ending a line.
    fn last(&self) -> String {
        let text = self.text();
        let last = text.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("nothing");
        last.trim().chars().take(200).collect()
    }
}

/// `cargo <args>` in `dir`, as [`heard`] runs it.
fn cargo_logged(dir: &Path, args: &[&str]) -> Result<(ExitStatus, String), String> {
    let mut cargo = Command::new("cargo");
    cargo.args(args).current_dir(dir);
    heard(cargo).map_err(|why| format!("cargo {}: {why}", args.join(" ")))
}

/// Run `cmd`, its output passed through and also kept, both streams in the
/// order they were written: a verdict read off the log needs the whole of it.
///
/// **A command that says nothing for [`QUIET`] is hung, and is ended with all
/// it spawned**: it leads a process group of its own, because the process that
/// hangs is a test binary cargo started, and killing cargo alone would leave it
/// running. The refusal carries the last line said, which under libtest names
/// the test: `test <name> has been running for over 60 seconds`, or on one
/// thread the unfinished `test <name> ... `.
///
/// **The group is sent `SIGQUIT`, and `SIGKILL` if its output has not ended
/// [`GONE`] later**: a host that reports a process a signal ended with a core
/// then holds where the silent one was. The command starts with `SIGQUIT` at
/// its default whatever the driver inherited, and std starts every child with
/// no signal blocked, so the first signal does not depend on who started the
/// driver.
///
/// **An interrupt of the driver ends the group** ([`interrupted`]), unless the
/// driver was started ignoring that signal.
fn heard(mut cmd: Command) -> Result<(ExitStatus, String), String> {
    static HANDED_ON: Once = Once::new();
    HANDED_ON.call_once(|| {
        for signal in INTERRUPTS {
            // SAFETY: `interrupted` makes async-signal-safe calls only.
            unsafe {
                if libc::signal(signal, interrupted as *const () as libc::sighandler_t) == libc::SIG_IGN {
                    libc::signal(signal, libc::SIG_IGN);
                }
            }
        }
    });
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
    cmd.stdout(writer.try_clone().map_err(|e| format!("pipe: {e}"))?).stderr(writer).process_group(0);
    // SAFETY: one async-signal-safe call on the child's own state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::signal(libc::SIGQUIT, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    GROUP.store(STARTING, Ordering::SeqCst);
    // An interrupt taken between two steps, its handler not yet at its raise:
    // nothing is spawned for it to miss.
    if PENDING.load(Ordering::SeqCst) != 0 {
        started(0);
    }
    let spawned = cmd.spawn();
    let group = spawned.as_ref().map_or(0, |child| child.id() as i32);
    started(group);
    let mut child = spawned.map_err(|e| format!("spawn: {e}"))?;
    // The writers it holds: the output ends when the last process holding one does.
    drop(cmd);
    let (tx, chunks) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) if tx.send(chunk[..n].to_vec()).is_ok() => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Ok(_) | Err(_) => return,
            }
        }
    });
    let mut said = Said { chunks, log: Vec::new() };
    let hung = loop {
        match said.more(QUIET) {
            Some(true) => {}
            Some(false) => break None,
            None => break Some(said.last()),
        }
    };
    let ended = hung.map(|last| {
        // SAFETY: the group the child leads, which no other process can name until it is reaped.
        unsafe { libc::killpg(group, libc::SIGQUIT) };
        let mut by = "SIGQUIT";
        let mut gone = said.ended();
        if !gone {
            by = "SIGKILL";
            // SAFETY: as above. The child by its own name too: a group whose
            // members are gone but for an unreaped leader may answer for nobody.
            unsafe { libc::killpg(group, libc::SIGKILL) };
            let _ = child.kill();
            gone = said.ended();
        }
        let left = if gone { "" } else { "; a process outside that group still held its output after it" };
        format!("said nothing for {QUIET:?} and was ended with its process group by {by}{left}; the last it said: {last}")
    });
    // Before the child is reaped and its pid is anybody's: a handler that
    // read the group before this is seen here, and the driver ends unreaped.
    started(0);
    let status = child.wait().map_err(|e| format!("wait: {e}"))?;
    match ended {
        Some(refusal) => Err(refusal),
        None => Ok((status, said.text())),
    }
}

// --- The host jobs -------------------------------------------------------------

/// What a model of the kernel's concurrency is shown able to catch: a feature
/// that takes away the one edge the model's property rests on, and the verdict
/// lines the model must then print.
pub(crate) struct Control {
    /// The model's package: a host member of the workspace, or [`KERNEL`].
    pub(crate) krate: &'static str,
    pub(crate) feature: &'static str,
    /// The test target the verdicts' tests are in; `None` is the library's own.
    test: Option<&'static str>,
    /// `false` is a case that catches its own panic and asserts on it: its
    /// teeth are a green run.
    must_red: bool,
    /// Every one must be in the output. An exit code alone would read a compile
    /// error as the model having teeth.
    verdicts: &'static [Verdict],
}

/// A line a control's run must print, and the test that prints it: the run is
/// the tests its verdicts name and no other, so nothing it runs goes unread.
#[derive(Clone, Copy)]
enum Verdict {
    /// The harness's own `<test> ... FAILED`.
    Fails(&'static str),
    /// The harness's own `<test> ... ok`.
    Passes(&'static str),
    /// A message the test prints itself.
    Says { test: &'static str, message: &'static str },
}

impl Verdict {
    fn test(self) -> &'static str {
        match self {
            Fails(test) | Passes(test) | Says { test, .. } => test,
        }
    }

    fn line(self) -> String {
        match self {
            Fails(test) => format!("{test} ... FAILED"),
            Passes(test) => format!("{test} ... ok"),
            Says { message, .. } => message.to_string(),
        }
    }
}

use Verdict::{Fails, Passes, Says};

/// The kernel package, whose library's tests are its models.
pub(crate) const KERNEL: &str = "kernel";
/// The SDK's package, whose tests link no syscall on a host.
pub(crate) const SDK: &str = "toyos";
const KERNEL_LOOM: &str = "kernel-loom";
const KERNEL_SIM: &str = "kernel-sim";
const BLOCKRING: &str = "toyos-blockring";
const TRANSPORT: &str = "toyos-transport";

const fn red(
    krate: &'static str,
    feature: &'static str,
    test: Option<&'static str>,
    verdicts: &'static [Verdict],
) -> Control {
    Control { krate, feature, test, must_red: true, verdicts }
}

/// Every negative control a model crate declares; `src/build.rs`'s
/// `every_model_control_is_run` holds this against the manifests.
pub(crate) const CONTROLS: &[Control] = &[
    red(KERNEL_LOOM, "wake-fence-off", Some("log_wake"), &[
        Fails("a_commit_and_an_arm_cannot_both_miss"),
    ]),
    red(KERNEL_LOOM, "lock-acquire-off", Some("ticket_lock"), &[
        Fails("try_lock_observes_the_previous_owners_writes"),
    ]),
    red(KERNEL_LOOM, "owed-fence-off", Some("owed_lock"), &[
        Fails("a_turned_away_cpu_holds_the_lock_or_is_answered"),
    ]),
    red(KERNEL_LOOM, "seqlock-writer-fence-off", Some("panic_console_publish"), &[
        Fails("a_snapshot_is_one_publication_whole"),
    ]),
    red(KERNEL_LOOM, "serial-try-lock-then-some", Some("serial_lock"), &[
        Fails("a_lost_try_lock_leaves_the_lock_held"),
        Fails("two_writers_never_overlap"),
    ]),
    red(KERNEL_LOOM, "reap-raise-relaxed", Some("reap_gate"), &[
        Fails("a_claim_sees_the_enrolled_work"),
    ]),
    red(KERNEL_LOOM, "shootdown-serve-relaxed", Some("tlb_shootdown"), &[
        Fails("an_acknowledged_flush_postdates_the_page_table_write"),
        Fails("one_serve_answers_two_concurrent_shootdowns"),
    ]),
    red(KERNEL_LOOM, "shootdown-served-relaxed", Some("counters_round"), &[
        Fails("a_cpu_read_as_answered_is_read_with_its_answer"),
    ]),
    red(KERNEL_LOOM, "roster-commit-relaxed", Some("smp_bringup"), &[
        Fails("a_committed_count_never_outruns_its_slot"),
    ]),
    red(KERNEL_LOOM, "smp-ready-split", Some("smp_bringup"), &[
        Fails("a_released_machine_is_answering"),
    ]),
    red(KERNEL_LOOM, "log-commit-release-off", Some("log_record"), &[
        Fails("a_committed_record_is_whole_or_absent"),
        Fails("a_key_and_the_record_it_names_come_from_one_generation"),
    ]),
    red(KERNEL_LOOM, "shard-publish-relaxed", Some("log_publish"), &[
        Fails("a_reader_that_finds_a_shard_finds_it_built"),
    ]),
    red(KERNEL_LOOM, "log-ring-publish-relaxed", Some("log_ring"), &[
        Fails("a_published_record_is_whole_and_read_once"),
        Fails("a_slot_is_reused_only_after_its_record_was_read"),
        Fails("a_lane_publishes_whole_and_reuses_only_after_a_read"),
    ]),
    red(KERNEL_LOOM, "log-ring-tail-relaxed", Some("log_ring"), &[
        Fails("a_published_record_is_whole_and_read_once"),
        Fails("a_slot_is_reused_only_after_its_record_was_read"),
        Fails("a_lane_publishes_whole_and_reuses_only_after_a_read"),
    ]),
    red(KERNEL_LOOM, "log-ring-loads-swapped", Some("log_ring"), &[
        Fails("a_published_record_is_whole_and_read_once"),
    ]),
    red(KERNEL_LOOM, "post-is-an-answer", Some("inbox_answer"), &[
        Fails("a_post_with_nothing_to_read_answers_nothing"),
        Fails("a_post_that_lands_after_its_bytes_were_read_answers_nothing"),
        Fails("a_poll_armed_again_does_not_end_the_look"),
    ]),
    red(KERNEL_LOOM, "poll-fire-load-store", Some("poll_once"), &[
        Fails("a_post_and_a_recheck_answer_a_poll_once"),
        Fails("a_withdrawal_and_a_post_never_both_take_a_poll"),
    ]),
    red(KERNEL_LOOM, "sleeplock-acquire-off", Some("sleep_lock"), &[
        Fails("a_parking_contender_observes_the_holders_writes"),
        Fails("two_holders_never_overlap"),
    ]),
    red(KERNEL_LOOM, "device-irq-lossy", Some("device_irq"), &[
        Fails("every_message_is_counted_once"),
    ]),
    red(KERNEL_LOOM, "dump-report-relaxed", Some("dump_request"), &[
        Fails("a_request_filed_during_a_report_is_reported"),
    ]),
    Control {
        krate: KERNEL_LOOM,
        feature: "no-preempt-guard",
        test: Some("loom_mailbox"),
        must_red: false,
        verdicts: &[Passes("preempted_producer_strands_suffix")],
    },
    red(KERNEL_LOOM, "doorbell-kick-relaxed", Some("loom_sleep"), &[Says {
        test: "a_halted_cpu_with_queued_work_was_kicked",
        message: "halted with 2 of 2 messages queued and no IPI in flight",
    }]),
    red(KERNEL_LOOM, "push-fence-relaxed", Some("loom_push"), &[Says {
        test: "a_cpu_that_halts_without_seeing_the_surplus_was_pushed",
        message: "published and no push behind it",
    }]),
    // The watch's lost wake, staged: the waiter parks over a post it was flagged
    // with.
    red(KERNEL_LOOM, "commit-ignores-notify", Some("loom_watch"), &[
        Says {
            test: "a_post_racing_a_registration_leaves_nobody_parked",
            message: "parked with the condition true and no wake owed: the post was lost",
        },
        Says {
            test: "a_fire_racing_a_submitters_park_is_never_lost",
            message: "parked over a fired poll and no wake owed: a fire was lost",
        },
    ]),
    // The notify's flagged arm answering off a load: a second post reads the
    // word from before the waiter consumed the first flag.
    red(KERNEL_LOOM, "notify-flag-load-only", Some("loom_watch"), &[Says {
        test: "a_second_post_is_not_lost_to_a_flag_the_waiter_consumed",
        message: "parked with both conditions true and no wake owed: a post answered off a load",
    }]),
    // The stop's store-buffering pair with the gate's fences gone.
    red(KERNEL_LOOM, "gate-fence-off", Some("loom_watch"), &[Says {
        test: "a_transition_racing_an_opening_gate_is_never_missed",
        message: "the stop parked over a thread that had parked, and nothing posted it",
    }]),
    // `kernel-loom`'s control, over the kernel's `Once` as the watch models'
    // ring entry.
    red(KERNEL_LOOM, "poll-fire-load-store", Some("loom_watch"), &[
        Fails("a_poll_registered_racing_a_post_completes_exactly_once"),
        Fails("a_poll_registered_racing_a_post_in_place_completes_exactly_once"),
        Fails("a_poll_on_two_watches_racing_both_posts_completes_exactly_once"),
    ]),
    // The ring models' lost-completion half: the producer posts before it
    // stores the readiness its registrant rechecks.
    red(KERNEL_LOOM, "fault-posted-before-it-is-set", Some("loom_watch"), &[
        Says {
            test: "a_poll_registered_racing_a_post_completes_exactly_once",
            message: "a poll over a ready object was completed by neither",
        },
        Fails("a_poll_registered_racing_a_post_completes_exactly_once"),
        Fails("a_poll_registered_racing_a_post_in_place_completes_exactly_once"),
    ]),
    // Reproduces an open defect
    // (`issues/steal-probe-node-dies-with-its-victim.md`) rather than
    // proving a lie is caught, and goes with its fix.
    Control {
        krate: KERNEL_LOOM,
        feature: "victim-retires-mid-probe",
        test: Some("loom_mailbox"),
        must_red: false,
        verdicts: &[Says {
            test: "a_probe_outstanding_when_its_victim_retires_is_never_reclaimed",
            message: "caught the verdict: the victim retired with a probe still linked in its \
                      queue",
        }],
    },
    // The isolation test reds, and the SDM's own judge of the allocator sees
    // the cross-space read it lets through.
    red(KERNEL, "counting-allocator", None, &[
        Fails("pcid::tests::two_live_address_spaces_never_share_a_pcid"),
        Passes("pcid::oracle::tests::the_counting_allocator_produces_a_cross_space_read"),
    ]),
    red(KERNEL, "mutate-spawn-skips-the-insert-recheck", None, &[
        Fails("proclife::interleave::tests::a_published_exit_leaves_no_unretired_thread"),
        Fails("proclife::interleave::tests::a_kill_racing_a_spawn_leaves_no_unretired_thread"),
    ]),
    red(KERNEL, "mutate-claim-teardown-always-wins", None, &[
        Fails("proclife::interleave::tests::an_exit_and_a_kill_never_both_tear_a_process_down"),
    ]),
    red(KERNEL, "mutate-kill-waits-for-its-victims", None, &[
        Fails("proclife::interleave::tests::two_processes_killing_each_other_both_end"),
        Fails("proclife::interleave::tests::a_kill_chain_of_three_ends"),
    ]),
    red(KERNEL, "mutate-first-out-tears-down", None, &[
        Fails("proclife::interleave::tests::an_exit_and_a_kill_never_both_tear_a_process_down"),
    ]),
    red(KERNEL, "mutate-join-collects-in-a-teardown", None, &[
        Fails("proclife::interleave::tests::a_join_racing_the_kill_that_takes_its_target"),
    ]),
    red(KERNEL, "mutate-last-out-leaves-before-its-teardown", None, &[
        Fails("proclife::interleave::tests::the_last_one_out_is_in_its_process_until_its_teardown_is_done"),
        Fails("proclife::teardown::tests::only_the_thread_that_empties_a_claimed_process_tears_it_down"),
    ]),
    red(KERNEL, "mutate-place-skips-the-insert-recheck", None, &[
        Fails("proclife::interleave::tests::a_spawn_racing_its_places_kill_leaves_nothing_under_it_and_publishes_it"),
    ]),
    red(KERNEL, "mutate-refused-spawn-keeps-the-count", None, &[
        Fails("proclife::interleave::tests::a_spawn_racing_its_places_kill_leaves_nothing_under_it_and_publishes_it"),
    ]),
    red(KERNEL, "mutate-landed-child-retires-nothing", None, &[
        Fails("proclife::interleave::tests::a_spawn_racing_its_places_kill_leaves_nothing_under_it_and_publishes_it"),
        Fails("proclife::tree::tests::a_child_landed_under_a_place_claimed_since_its_admission_is_claimed_with_it"),
    ]),
    red(KERNEL, "mutate-publish-before-the-children", None, &[
        Fails("proclife::interleave::tests::an_exit_publishes_after_every_end_below_it"),
    ]),
    red(KERNEL, "mutate-walk-in-one-hold", None, &[
        Fails("proclife::interleave::tests::a_spawn_under_an_unrelated_process_lands_between_two_claims_of_one_walk"),
    ]),
    red(KERNEL, "mutate-spawner-handle-after-the-landing", None, &[
        Fails("proclife::interleave::tests::a_spawn_racing_its_places_kill_leaves_nothing_under_it_and_publishes_it"),
        Fails("proclife::interleave::tests::a_spawn_racing_the_kill_of_its_own_spawner"),
    ]),
    red(KERNEL, "mutate-spawner-handle-before-the-childs-own", None, &[
        Fails("proclife::interleave::tests::a_sibling_closing_a_spawns_handle_before_the_spawn_returns"),
    ]),
    red(KERNEL_SIM, "placement-ignores-staleness", Some("policy"), &[
        Fails("a_stopped_cpu_stops_taking_work"),
    ]),
    // The block protocol's three: a completion lost to a session's end, a
    // completion given twice after a reset, and a loss nothing is written
    // again after.
    red(BLOCKRING, "mutate-session-end-forgets", None, &[
        Fails("model::tests::every_request_is_answered_exactly_once"),
    ]),
    red(BLOCKRING, "mutate-abort-keeps-inflight", None, &[
        Fails("model::tests::every_request_is_answered_exactly_once"),
    ]),
    red(BLOCKRING, "mutate-no-reissue-after-loss", None, &[
        Fails("model::tests::what_a_flush_calls_durable_is_on_the_medium"),
    ]),
    red(TRANSPORT, "publish-relaxed", Some("loom"), &[Fails("a_published_entry_is_read_whole")]),
    red(TRANSPORT, "no-clamp", Some("loom"), &[Fails(
        "a_hostile_producer_yields_entries_or_a_violation",
    )]),
    red(TRANSPORT, "end-keeps-inflight", None, &[Fails(
        "inflight::tests::an_end_answers_every_tag_once_and_a_late_completion_nothing",
    )]),
];

/// Whether a control's run showed its teeth.
fn judge_control(control: &Control, exited_green: bool, log: &str) -> Result<String, String> {
    if log.lines().any(|l| l == "running 0 tests") {
        return Err("the run selected no test: no name its verdicts give is a test of its target, \
                    so `CONTROLS` drifted from the model"
            .into());
    }
    if control.must_red && exited_green {
        return Err(format!("passed with `{}`: the model has no teeth", control.feature));
    }
    if !control.must_red && !exited_green {
        return Err(format!(
            "`{}` failed, so the case's own catch did not hold or something else broke",
            control.feature
        ));
    }
    let missing: Vec<String> =
        control.verdicts.iter().map(|v| v.line()).filter(|v| !log.contains(v.as_str())).collect();
    if !missing.is_empty() {
        return Err(format!(
            "no verdict {missing:?}: this proved nothing, and whatever stopped the model is \
             what to fix"
        ));
    }
    Ok(format!("{} verdict(s) reached", control.verdicts.len()))
}

fn run_control(root: &Path, control: &Control) -> Result<String, String> {
    let mut args = vec!["test", "-p", control.krate, "--features", control.feature];
    match control.test {
        Some(test) => args.extend(["--test", test]),
        None => args.push("--lib"),
    }
    args.extend(["--", "--exact"]);
    args.extend(control.verdicts.iter().map(|v| v.test()));
    if !control.must_red {
        args.push("--nocapture");
    }
    let (status, log) = cargo_logged(root, &args)?;
    judge_control(control, status.success(), &log)
}

/// Every test that runs on the host and boots no guest. The build system's own
/// tests, every host member of the workspace ([`crate::hostws`]), the kernel's
/// library, clippy with warnings denied, the concurrency models' negative
/// controls, every userland crate with a host test ([`crate::userlandhost`],
/// which also reds on a userland test none of them runs), every app the images
/// ship for each host ([`apps_for`]), and the SDK.
///
/// **Every step runs against a `$TMPDIR` of this job's own, and the last step
/// reds on anything left in it** but the lock `toyos_tmpdir` keeps there: a test
/// that writes scratch past a `toyos_tmpdir::TempDir`, or holds one past its
/// end, is a test that fills the host's disk one run at a time.
///
/// Clippy needs none of the ToyOS toolchain — the kernel and the bootloader
/// lint against every architecture's bare targets ([`crate::clippy::BARE_TARGETS`]),
/// which any rustup installs, and userland carries no clippy shape
/// (`src/clippy.rs`).
///
/// In a job that carries the cache ([`cicache::carried`]) the restored entry is
/// read before any step. A developer's tree keeps the dates its edits gave it.
fn host(root: &Path) -> Vec<Step> {
    let carried = cicache::carried(root, &std::env::current_exe().expect("the driver's own path"));
    let tmp = toyos_tmpdir::TempDir::new("ci-host");
    let short = Path::new(toyos_tmpdir::SHORT_BASE);
    let before = toyos_tmpdir::gone_roots(short);
    // Before any thread: nothing in this process reads the environment
    // concurrently with the write, and every child inherits it.
    std::env::set_var("TMPDIR", tmp.path());
    let host_triple = crate::toolchain::host_triple();
    let mut steps = Vec::new();
    if carried {
        carry();
        steps.push(step("the cache entry, read by content", || cicache::read(root)));
        // Every step after an unreadable entry would be judged against it.
        if steps[0].verdict.is_err() {
            return steps;
        }
    }
    steps.extend([
        step("the build system", || cargo(root, &["test", "--lib"])),
        step("the harness's own checks", || cargo(root, &["test", "--test", "toyos-checks"])),
        step("the workspace's host members", || {
            let guests = crate::hostws::guest_packages(root);
            let mut args = vec!["test", "--workspace", "--exclude", "toyos-build"];
            args.extend(guests.iter().flat_map(|guest| ["--exclude", guest.as_str()]));
            cargo(root, &args)
        }),
        step("the kernel's library", || {
            cargo(root, &["test", "-p", KERNEL, "--lib", "--features", "sched-check"])
        }),
        step("the licences of what ships", || crate::licence::judge(root)),
    ]);
    steps.push(step("clippy and the bare targets", || {
        for args in [
            vec!["component", "add", "clippy"],
            [&["target", "add"][..], &crate::clippy::BARE_TARGETS].concat(),
        ] {
            let status = Command::new("rustup").args(&args).status().map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("rustup {} exited {status}", args.join(" ")));
            }
        }
        Ok("installed".into())
    }));
    steps.push(step("clippy, warnings denied", || {
        let (failed, _) = crate::clippy::run(root)?;
        if failed.is_empty() {
            Ok("clean".into())
        } else {
            Err(failed.join("; "))
        }
    }));
    steps.push(step("kernel-loom without loom", || {
        cargo(root, &[
            "test",
            "-p",
            KERNEL_LOOM,
            "--no-default-features",
            "--test",
            "log_zeroed_init",
            "--test",
            "log_body_words",
            "--test",
            "log_cursor",
        ])
    }));
    for control in CONTROLS {
        steps.push(step(&format!("control `{}`", control.feature), || run_control(root, control)));
    }
    match crate::userlandhost::survey(&root.join("userland")) {
        Ok(survey) => {
            for name in survey.gated {
                let manifest = format!("userland/{name}/Cargo.toml");
                steps.push(step(&format!("userland/{name}"), || {
                    cargo(root, &["test", "--manifest-path", &manifest])
                }));
            }
        }
        Err(why) => steps.push(Step { label: "the userland host crates".into(), verdict: Err(why) }),
    }
    match crate::userlandhost::programs(root) {
        Ok(programs) => {
            for os in Os::ALL {
                steps.push(step(&format!("the apps for {}", os.name()), || {
                    apps_for(root, &programs, os, &host_triple)
                }));
            }
        }
        Err(why) => steps.push(Step { label: "the apps".into(), verdict: Err(why) }),
    }
    steps.push(step("the toyos SDK", || cargo(root, &["test", "-p", SDK])));
    steps.push(step("nothing left in $TMPDIR or /tmp", || left_behind(&tmp, short, &before)));
    steps
}

/// [`host`] from a cold tree, then that tree sealed as the host cache's entry.
fn seal(root: &Path) -> Vec<Step> {
    let mut cold = None;
    let mut steps = vec![step("a cold tree, for the entry", || {
        let (found, said) = cicache::cold(root)?;
        cold = Some(found);
        Ok(said)
    })];
    let Some(cold) = cold else { return steps };
    steps.extend(host(root));
    steps.push(step("the tree, sealed as the host cache's entry", || cicache::seal(root, &cold)));
    steps
}

/// What every step of a job that carries the cache inherits, set before any
/// thread as `host`'s `TMPDIR` is.
fn carry() {
    // The job's `CARGO_TARGET_DIR` names the driver's target, and no step
    // builds there.
    std::env::remove_var("CARGO_TARGET_DIR");
    // No incremental state: it is most of an entry's bytes, and after a read
    // by content it helps only a crate whose bytes changed.
    std::env::set_var("CARGO_INCREMENTAL", "0");
}

/// Every app the images ship, judged for `os` with the features its image
/// builds it with ([`crate::userlandhost`]).
///
/// One cargo per app: features unify across the packages of one invocation, and
/// an app that builds only beside another's features is what this gate is for.
fn apps_for(
    root: &Path,
    programs: &[Program],
    os: Os,
    host_triple: &str,
) -> Result<String, String> {
    let triple = os.triple();
    let status = Command::new("rustup")
        .args(["target", "add", triple])
        .status()
        .map_err(|e| format!("rustup: {e}"))?;
    if !status.success() {
        return Err(format!("rustup target add {triple} exited {status}"));
    }
    let how = judged_as(os, host_triple);
    let (attempted, declared) = attempted(programs, os);
    let mut red = Vec::new();
    for program in &attempted {
        let manifest = format!("{}/Cargo.toml", program.dir);
        let args = [&how[..], &["--manifest-path", manifest.as_str()]].concat();
        if let Err(exit) = cargo(root, &args) {
            red.push(format!(
                "{} fails for {} and declares neither `fails` there nor `exempt`: {exit}",
                program.dir,
                os.name()
            ));
        }
    }
    if !red.is_empty() {
        return Err(red.join("; "));
    }
    let said = format!("{} app(s) pass `cargo {}`", attempted.len(), how.join(" "));
    if declared.is_empty() {
        Ok(said)
    } else {
        Ok(format!("{said}; {} not attempted, as their manifests declare", declared.join(", ")))
    }
}

/// `build` where the gate runs on `os`'s own triple, and `check --target`
/// elsewhere. The host's own build names no triple, so it lands in the target
/// the userland test steps fill and compiles only what they did not.
fn judged_as(os: Os, host_triple: &str) -> Vec<&'static str> {
    if os.triple() == host_triple {
        vec!["build"]
    } else {
        vec!["check", "--target", os.triple()]
    }
}

/// The apps judged for `os`, and those whose manifests declare they fail there;
/// an exempt program is in neither.
fn attempted(programs: &[Program], os: Os) -> (Vec<&Program>, Vec<&str>) {
    let (mut attempted, mut declared) = (Vec::new(), Vec::new());
    for program in programs {
        let Host::App(fails) = &program.host else { continue };
        if fails.contains(&os) {
            declared.push(program.dir.as_str());
        } else {
            attempted.push(program);
        }
    }
    (attempted, declared)
}

/// What `tmp` holds but the lock `toyos_tmpdir` keeps in it, and every root
/// under `short` whose process is gone that `before` does not name.
///
/// Refuses if `tmp` holds no [`toyos_tmpdir::GLOBAL`] at all: every step above
/// makes at least one `toyos_tmpdir::TempDir`, which always writes that lock
/// file first, so its absence means this `$TMPDIR` never saw the steps at
/// all — the guard reading an empty directory it was never given, rather than
/// one every test actually cleaned.
fn left_behind(tmp: &Path, short: &Path, before: &[PathBuf]) -> Result<String, String> {
    let mut left: Vec<String> = std::fs::read_dir(tmp)
        .map_err(|e| format!("read {}: {e}", tmp.display()))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read {}: {e}", tmp.display()))?;
    if !left.iter().any(|name| name == toyos_tmpdir::GLOBAL) {
        return Err(format!(
            "{} holds no {}: every step above makes a `toyos_tmpdir::TempDir`, so its absence \
             means this $TMPDIR was never the one the steps actually wrote to",
            tmp.display(),
            toyos_tmpdir::GLOBAL
        ));
    }
    left.retain(|name| name != toyos_tmpdir::GLOBAL);
    left.sort();
    let mut dead: Vec<String> = toyos_tmpdir::gone_roots(short)
        .into_iter()
        .filter(|root| !before.contains(root))
        .map(|root| root.display().to_string())
        .collect();
    dead.sort();
    let mut said = Vec::new();
    if !left.is_empty() {
        said.push(format!(
            "left in {} by the steps above, each written past a `toyos_tmpdir::TempDir` or held \
             past its test: {}",
            tmp.display(),
            left.join(", ")
        ));
    }
    if !dead.is_empty() {
        said.push(format!("left by a process that died during the steps above: {}", dead.join(", ")));
    }
    if said.is_empty() {
        return Ok("every test took its scratch with it".into());
    }
    Err(said.join("; "))
}

// --- The guest jobs ------------------------------------------------------------

fn suite_args(args: &[&str]) -> Vec<String> {
    let mut all = vec!["test", "--test", "toyos-build", "--"];
    all.extend(args);
    all.into_iter().map(String::from).collect()
}

/// A guest job's own `$TMPDIR`, same rule as [`host`]: nothing the suite
/// writes past a `toyos_tmpdir::TempDir` — the harness's own `Run`, its lanes,
/// every boot image — survives past the last step, which reds on it.
fn guest(root: &Path, suite: &[String]) -> Vec<Step> {
    let tmp = toyos_tmpdir::TempDir::new("ci-guest");
    let short = Path::new(toyos_tmpdir::SHORT_BASE);
    let before = toyos_tmpdir::gone_roots(short);
    // Before any thread, same as `host`: every child this process spawns below
    // inherits this, and nothing here reads the environment concurrently with
    // the write.
    std::env::set_var("TMPDIR", tmp.path());
    let mut steps: Vec<Step> = Arch::ALL
        .iter()
        .map(|&arch| step(&format!("the {} instrument", arch.name()), || instrument(root, arch)))
        .collect();
    if steps.iter().all(|s| s.verdict.is_ok()) {
        steps.push(step("the toolchain", || release::install(root)));
    }
    if steps.iter().all(|s| s.verdict.is_ok()) {
        steps.push(step("the suite", || {
            let args: Vec<&str> = suite.iter().map(String::as_str).collect();
            let (status, log) = cargo_logged(root, &args)?;
            let said = verdicts(&log);
            if status.success() {
                Ok(said)
            } else {
                Err(said)
            }
        }));
    }
    // Unconditional: whatever stopped earlier, this $TMPDIR is still this
    // process's own to judge, and a leak past a failing suite is still a leak.
    steps.push(step("nothing left in $TMPDIR or /tmp", || left_behind(&tmp, short, &before)));
    steps
}

/// The suite's own count line and every line naming a verdict worth reading
/// without the log: a failure.
fn verdicts(log: &str) -> String {
    let lines = || log.lines().map(crate::printer::unstamped);
    let total = lines()
        .rfind(|l| l.contains("test result:") && l.contains(" total ("))
        .unwrap_or("no suite result line");
    let named: Vec<&str> = lines()
        .filter(|l| {
            l.starts_with("FAIL ")
                || (l.starts_with(' ')
                    && ["STALL ", "INVL "].iter().any(|v| l.trim_start().starts_with(v)))
        })
        .collect();
    if named.is_empty() {
        total.to_string()
    } else {
        format!("{total}\n```\n{}\n```", named.join("\n"))
    }
}

/// The QEMU on `PATH` that boots `arch` against `.github/qemu-version`, the
/// firmware it declares, and whether `/dev/kvm` opens where it is present and
/// `arch` is the host's — the three things a guest verdict must be read against.
fn instrument(root: &Path, arch: Arch) -> Result<String, String> {
    let want = declared_qemu_version(root).ok_or(".github/qemu-version declares no version")?;
    let have = qemu_version(arch)?;
    let firmware = crate::firmware::of(arch)?;
    let node = Path::new("/dev/kvm").exists();
    let native = Arch::HOST == Some(arch);
    let accel = match (native, arch.accel(), node) {
        (false, _, _) => "another architecture's machine: emulated",
        (true, Accel::Kvm, _) => "/dev/kvm opens",
        (true, Accel::Hvf, _) => "Hypervisor.framework",
        (true, Accel::Tcg, true) => "/dev/kvm is present and does not open",
        (true, Accel::Tcg, false) => "no /dev/kvm: emulated",
    };
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("model name"))
                .map(|l| l.trim_start_matches([' ', '\t', ':']).to_string())
        })
        .unwrap_or_else(|| "an unnamed CPU".to_string());
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    let line = format!(
        "QEMU {have}, firmware {}, {accel}, {cpu}, {cores} core(s)",
        firmware.code.display()
    );
    if have != want {
        return Err(format!(
            "{line}: this runs QEMU {have} and .github/qemu-version declares {want}. The \
             container image's digest is what pins it, so moving it is a commit that says the \
             instrument moved"
        ));
    }
    if native && node && !arch.accel().is_hardware() {
        return Err(format!("{line}: every boot would fall back to emulation in silence"));
    }
    Ok(line)
}

/// The QEMU every guest in CI runs. Comment lines and blanks are stripped, so
/// the file can explain itself.
pub fn declared_qemu_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".github/qemu-version")).ok()?;
    let version: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("")
        .split_whitespace()
        .collect();
    (!version.is_empty()).then_some(version)
}

/// The version the QEMU on `PATH` that boots `arch` says it is.
pub fn qemu_version(arch: Arch) -> Result<String, String> {
    let out = Command::new(arch.qemu())
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", arch.qemu()))?;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    parse_qemu_version(&said).ok_or_else(|| format!("QEMU said {said:?}"))
}

/// `QEMU emulator version 11.0.3 (Debian 1:11.0.3+ds-1)` → `11.0.3`.
fn parse_qemu_version(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("QEMU emulator version ")?;
    let version = rest.split_whitespace().next()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// The line `cargo run` prints when this host's QEMU is not the version
/// `.github/qemu-version` declares, and nothing at all when it is.
pub fn qemu_version_note(root: &Path, arch: Arch) -> Option<String> {
    let want = declared_qemu_version(root)?;
    let have = qemu_version(arch).ok()?;
    (have != want).then(|| {
        format!(
            "Note: this host runs QEMU {have} and .github/qemu-version declares {want} — \
             CI's guests are on {want}, and the QEMU version \
             has been measured to decide test outcomes. Nothing here is broken; a comparison \
             across the two is."
        )
    })
}

// --- The publisher -------------------------------------------------------------

/// Each SDK crate, under the version [`sdkversion::plan`] assigns it, in
/// dependency order, waiting for each to be readable before the next resolves
/// it. Only a push to `main` publishes, on a runner whose checkout the
/// rewritten manifests are thrown away with.
fn publish(root: &Path) -> Result<String, String> {
    if !on_runner() || std::env::var("GITHUB_REF").ok().as_deref() != Some("refs/heads/main") {
        return Err("only a push to main publishes".into());
    }
    if std::env::var("CARGO_REGISTRY_TOKEN").map_or(true, |t| t.is_empty()) {
        return Err(
            "CARGO_REGISTRY_TOKEN is not set; publish.yml takes it from crates.io trusted \
             publishing, which each published crate must name this workflow for"
                .into()
        );
    }
    let tip = git_out(root, &["ls-remote", "origin", "refs/heads/main"]);
    at_tip(&tip, git_out(root, &["rev-parse", "HEAD"]).trim())?;
    let plan = sdkversion::plan(root)?;
    sdkversion::write_published_manifests(root, &plan)?;
    let mut said = Vec::new();
    for release in &plan {
        let (name, version) = (release.krate.name, &release.version);
        if !release.publish {
            said.push(format!("{name} {version} was there"));
            continue;
        }
        let manifest = format!("{}/Cargo.toml", release.krate.dir);
        cargo(root, &["publish", "--allow-dirty", "--manifest-path", &manifest])?;
        let mut seen = false;
        for _ in 0..60 {
            if sdkversion::assign(&sdkversion::index(name)?, &release.key)? == (version.clone(), false) {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        if !seen {
            return Err(format!("{name} {version} was published and the index did not show it"));
        }
        said.push(format!("{name} {version} published"));
    }
    Ok(said.join(", "))
}

/// Whether `HEAD` is `main`'s tip as `git ls-remote` printed it: a re-run of an
/// older push would put older code up under a newer minor.
fn at_tip(ls_remote: &str, head: &str) -> Result<(), String> {
    match ls_remote.split_whitespace().next() {
        Some(tip) if tip == head => Ok(()),
        tip => Err(format!(
            "HEAD {head} is not main's tip {tip:?}; a red publish is run again by \
             workflow_dispatch at the tip"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic control on `host`'s `std::env::set_var("TMPDIR", ...)`:
    /// delete that line and every child writes to the real `$TMPDIR` instead of
    /// this job's private one, which never gains the lock a real step always
    /// makes — this refuses instead of reading an empty directory as clean.
    #[test]
    fn left_behind_refuses_a_tmpdir_that_never_saw_the_lock() {
        let tmp = toyos_tmpdir::TempDir::new("left-behind-blind");
        let short = toyos_tmpdir::TempDir::new("left-behind-blind-short");
        let refusal =
            left_behind(&tmp, &short, &[]).expect_err("an untouched $TMPDIR is a red, not a pass");
        assert!(refusal.contains(toyos_tmpdir::GLOBAL), "{refusal}");
    }

    /// The lock is the one thing a `$TMPDIR` keeps; anything else is named.
    #[test]
    fn the_host_job_names_what_its_tests_left_behind() {
        let tmp = toyos_tmpdir::TempDir::new("left-behind");
        let short = toyos_tmpdir::TempDir::new("left-behind-short");
        std::fs::write(tmp.join(toyos_tmpdir::GLOBAL), b"").unwrap();
        assert!(left_behind(&tmp, &short, &[]).is_ok());
        std::fs::create_dir(tmp.join("stale-1-current")).unwrap();
        let refusal = left_behind(&tmp, &short, &[]).expect_err("a directory left behind is a red");
        assert!(refusal.contains("stale-1-current"), "{refusal}");
    }

    /// A short root whose process died while the steps ran is named; one already
    /// dead before them is not the steps'.
    #[test]
    fn the_job_names_a_short_root_a_step_left_when_it_died() {
        let tmp = toyos_tmpdir::TempDir::new("left-behind-died");
        let short = toyos_tmpdir::TempDir::new("left-behind-died-short");
        std::fs::write(tmp.join(toyos_tmpdir::GLOBAL), b"").unwrap();
        let earlier = format!("{}1-0", toyos_tmpdir::ROOT_PREFIX);
        std::fs::create_dir(short.join(&earlier)).unwrap();
        let before = toyos_tmpdir::gone_roots(&short);
        assert!(left_behind(&tmp, &short, &before).is_ok());
        let died = format!("{}2-0", toyos_tmpdir::ROOT_PREFIX);
        std::fs::create_dir(short.join(&died)).unwrap();
        let refusal = left_behind(&tmp, &short, &before).expect_err("a dead step's root is a red");
        assert!(refusal.contains(&died) && !refusal.contains(&earlier), "{refusal}");
    }

    /// The crate [`a_carried_jobs_step`] builds; unset, it is not a test.
    const FIXTURE: &str = "TOYOS_CI_TEST_FIXTURE";

    /// What a job that carries the cache hands its driver reaches no step: each
    /// builds in its own workspace's target, with no incremental state.
    /// The driver is a process of its own, because `carry`
    /// writes the environment, which no other thread may read meanwhile.
    #[test]
    fn a_step_of_a_job_that_carries_the_cache_builds_in_its_own_target() {
        let fixture = toyos_tmpdir::TempDir::new("ci-carried");
        std::fs::create_dir(fixture.join("src")).unwrap();
        let manifest = "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
        std::fs::write(fixture.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(fixture.join("src/lib.rs"), "pub fn one() -> u8 {\n    1\n}\n").unwrap();
        let out = crate::buildlock::tests::rerun("ci::tests::a_carried_jobs_step")
            .env(FIXTURE, fixture.path())
            .env("CARGO_TARGET_DIR", cicache::DRIVER)
            .env_remove("CARGO_INCREMENTAL")
            .output()
            .expect("run the driver");
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    #[ignore = "the driver of the test above; never runs on its own"]
    fn a_carried_jobs_step() {
        let fixture = PathBuf::from(std::env::var_os(FIXTURE).unwrap_or_else(|| panic!("run without {FIXTURE}")));
        carry();
        let cargo = |args: &[&str]| {
            let out = Command::new("cargo").args(args).current_dir(&fixture).output().expect("run cargo");
            assert!(out.status.success(), "cargo {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            out
        };
        let metadata = cargo(&["metadata", "--format-version", "1", "--no-deps", "--offline"]);
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
        let ours = std::fs::canonicalize(&fixture).unwrap().join("target");
        assert_eq!(metadata["target_directory"].as_str().map(Path::new), Some(ours.as_path()));
        let build = String::from_utf8(cargo(&["build", "-v", "--offline"]).stderr).unwrap();
        let rustc = build.lines().find(|l| l.contains("--crate-name one")).unwrap_or_else(|| panic!("{build}"));
        assert!(!rustc.contains("-C incremental"), "{rustc}");
    }

    /// A build script naming what `cargo run` hands the driver is fresh for a
    /// step's cargo once the job's cargo, which ran without them, has built
    /// it. The driver is a process of its own, as above.
    #[test]
    fn a_steps_cargo_finds_fresh_what_the_jobs_cargo_built() {
        const NAMED: [&str; 2] = ["CARGO_PKG_NAME", "CARGO_MANIFEST_DIR"];
        let fixture = toyos_tmpdir::TempDir::new("ci-own-package");
        std::fs::create_dir(fixture.join("src")).unwrap();
        let manifest = "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
        std::fs::write(fixture.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(fixture.join("src/lib.rs"), "").unwrap();
        let script = NAMED.map(|name| format!("    println!(\"cargo:rerun-if-env-changed={name}\");\n")).concat();
        std::fs::write(fixture.join("build.rs"), format!("fn main() {{\n{script}}}\n")).unwrap();
        let mut jobs = Command::new("cargo");
        jobs.args(["build", "--offline"]).current_dir(&fixture);
        for name in NAMED {
            jobs.env_remove(name);
        }
        let out = jobs.output().expect("run cargo");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let out = crate::buildlock::tests::rerun("ci::tests::a_steps_cargo")
            .env(FIXTURE, fixture.path())
            .output()
            .expect("run the driver");
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    #[ignore = "the driver of the test above; never runs on its own"]
    fn a_steps_cargo() {
        let fixture = PathBuf::from(std::env::var_os(FIXTURE).unwrap_or_else(|| panic!("run without {FIXTURE}")));
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some(), "cargo names no package to this process");
        forget_own_package();
        let out = Command::new("cargo")
            .args(["build", "-v", "--offline"])
            .current_dir(&fixture)
            .output()
            .expect("run cargo");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("Fresh one v0.1.0"), "{said}");
    }

    /// What [`a_hung_step`] says last.
    const LAST: &str = "the hung step's last line";

    /// A step that goes quiet is a red naming the last line it said, and what
    /// it spawned is ended with it: the step here is this binary, which starts
    /// a second and then says nothing, as cargo does with a test that hangs.
    /// The driver is a process of its own, because it reads the end of a pipe,
    /// which on macOS a process another thread spawns meanwhile can hold open.
    #[test]
    fn a_step_that_goes_quiet_is_ended_with_what_it_spawned_and_names_its_last_line() {
        let (_read_by_nobody, hung_on) = std::io::pipe().unwrap();
        let out = crate::buildlock::tests::rerun("ci::tests::a_hung_steps_driver")
            .env(FIXTURE, "hung")
            .stdin(hung_on)
            .output()
            .expect("run the driver");
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("test result: ok. 1 passed"), "{said}");
    }

    /// Its stdin is the step's, and what the step spawns'.
    #[test]
    #[ignore = "the driver of the tests around it; never runs on its own"]
    fn a_hung_steps_driver() {
        let hung = crate::buildlock::tests::rerun("ci::tests::a_hung_step");
        let refusal = heard(hung).expect_err("a step that hangs is a red");
        assert!(refusal.ends_with(LAST) && refusal.contains("by SIGQUIT") && !refusal.contains("outside"), "{refusal}");
    }

    /// An interrupt of the driver ends the group its step runs in. The judge
    /// is a process of its own, for the reason above.
    #[test]
    fn an_interrupt_of_the_driver_ends_the_group_its_step_runs_in() {
        let out = crate::buildlock::tests::rerun("ci::tests::an_interrupted_drivers_judge")
            .env(FIXTURE, "hung")
            .output()
            .expect("run the judge");
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    #[ignore = "the judge of the test above; never runs on its own"]
    fn an_interrupted_drivers_judge() {
        use std::io::BufRead;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::ExitStatusExt;
        let (mut hung_on, stdin) = std::io::pipe().unwrap();
        let mut driver = crate::buildlock::tests::rerun("ci::tests::a_hung_steps_driver");
        driver.stdin(stdin).stderr(std::process::Stdio::piped());
        let mut child = driver.spawn().expect("spawn the driver");
        drop(driver);
        // The driver passes on what its step says, and ends a step that never
        // says it: this read ends either way.
        let mut said = std::io::BufReader::new(child.stderr.take().expect("a piped stderr")).lines();
        assert!(said.by_ref().map_while(Result::ok).any(|line| line.contains(LAST)), "the step never said its last line");
        // `SIGTERM`: a shell may have started all of this ignoring `SIGINT`.
        // SAFETY: a signal to a child this test has not reaped.
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        let status = child.wait().expect("reap the driver");
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status}");
        // The step and what it spawned write into this pipe for as long as
        // they live: its end is both of them gone.
        let deadline = Instant::now() + GONE;
        let mut chunk = [0; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the step's group outlived its driver by {GONE:?}");
            let mut ready = libc::pollfd { fd: hung_on.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one descriptor, which this test owns.
            assert!(unsafe { libc::poll(&mut ready, 1, left.as_millis() as libc::c_int) } >= 0);
            if ready.revents != 0 && hung_on.read(&mut chunk).expect("read the pipe") == 0 {
                break;
            }
        }
    }

    /// The fixtures' own end at `SIGQUIT`, whose default leaves a host's
    /// report of the process behind at every run of these tests.
    extern "C" fn quit(_: libc::c_int) {
        // SAFETY: async-signal-safe.
        unsafe { libc::_exit(0) }
    }

    /// Write into stdin, the end of a pipe the driver's parent holds the other
    /// end of: blocked once it is full, and over when nobody reads it.
    fn hang() {
        assert!(std::env::var_os(FIXTURE).is_some(), "run without {FIXTURE}");
        let full = [0u8; 4096];
        // SAFETY: a write of a buffer that outlives the call.
        while unsafe { libc::write(0, full.as_ptr().cast(), full.len()) } > 0 {}
    }

    #[test]
    #[ignore = "the step of the driver above; never runs on its own"]
    fn a_hung_step() {
        // SAFETY: `quit` makes one async-signal-safe call.
        assert!(unsafe { libc::signal(libc::SIGQUIT, quit as *const () as libc::sighandler_t) } != libc::SIG_ERR);
        // libtest's own lines go nowhere, so `LAST` is the last line said; its
        // stderr is the step's, which it holds open.
        let mut spawned = crate::buildlock::tests::rerun("ci::tests::a_hung_steps_child")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn what the step spawns");
        eprintln!("{LAST}");
        hang();
        // Reached only where nothing ended the step: its child's pipe ended with its own.
        spawned.wait().expect("reap what the step spawned");
    }

    #[test]
    #[ignore = "what the step above spawns; never runs on its own"]
    fn a_hung_steps_child() {
        // SAFETY: as in the step.
        assert!(unsafe { libc::signal(libc::SIGQUIT, quit as *const () as libc::sighandler_t) } != libc::SIG_ERR);
        hang();
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    /// **An app is judged for every host its `fails` does not name**, built
    /// where the gate runs on that host and checked elsewhere; an exempt
    /// program is judged for none.
    #[test]
    fn an_app_is_judged_for_every_host_its_fails_does_not_name() {
        let program = |dir: &str, host| Program { dir: dir.into(), host };
        let programs = [
            program("calc", Host::App(Vec::new())),
            program("doom", Host::App(vec![Os::Windows])),
            program("supervisor", Host::Exempt),
        ];
        let judged = |os| {
            let (attempted, declared) = attempted(&programs, os);
            (attempted.iter().map(|p| p.dir.as_str()).collect::<Vec<_>>(), declared)
        };
        assert_eq!(judged(Os::Linux), (vec!["calc", "doom"], vec![]));
        assert_eq!(judged(Os::Macos), (vec!["calc", "doom"], vec![]));
        assert_eq!(judged(Os::Windows), (vec!["calc"], vec!["doom"]));
        assert_eq!(Os::ALL.map(|os| judged_as(os, Os::Linux.triple())), [
            vec!["build"],
            vec!["check", "--target", Os::Macos.triple()],
            vec!["check", "--target", Os::Windows.triple()],
        ]);
    }

    #[test]
    fn a_job_is_named_and_takes_nothing_after_it() {
        assert_eq!(parse(&words("host")), Ok(Job::Host));
        assert_eq!(parse(&words("seal")), Ok(Job::Seal));
        assert_eq!(parse(&words("guest")), Ok(Job::Guest));
        assert_eq!(parse(&words("release")), Ok(Job::Release));
        assert!(parse(&words("guest 3/12")).is_err());
        assert!(parse(&words("tcg")).is_err());
        assert!(parse(&words("host extra")).is_err());
        assert!(parse(&words("smoke")).is_err());
        assert!(parse(&[]).is_err());
    }

    /// Teeth for the controls' judge, on libtest's text as a control's run
    /// prints it: a green negative control, a verdict line absent or saying the
    /// opposite, a filter that selected nothing, and a self-catching case that
    /// failed are all red.
    #[test]
    fn a_control_is_judged_by_its_verdict_and_not_its_exit_alone() {
        let control =
            |feature: &str| CONTROLS.iter().find(|c| c.feature == feature).expect(feature);

        let two = control("serial-try-lock-then-some");
        let both = "running 2 tests\n\
                    test a_lost_try_lock_leaves_the_lock_held ... FAILED\n\
                    test two_writers_never_overlap ... FAILED\n";
        assert!(judge_control(two, false, both).is_ok());
        assert!(judge_control(two, true, both).unwrap_err().contains("no teeth"));
        let one = "running 2 tests\n\
                   test a_lost_try_lock_leaves_the_lock_held ... FAILED\n\
                   test two_writers_never_overlap ... ok\n";
        assert!(judge_control(two, false, one).unwrap_err().contains("proved nothing"));
        let none = "running 0 tests\n\n\
                    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; \
                    finished in 0.00s\n";
        assert!(judge_control(two, true, none).unwrap_err().contains("selected no test"));

        let says = control("doorbell-kick-relaxed");
        let panicked = "running 1 test\n\n\
                        thread 'a_halted_cpu_with_queued_work_was_kicked' (77012) panicked at \
                        kernel/loom/tests/loom_sleep.rs:102:13:\n\
                        halted with 2 of 2 messages queued and no IPI in flight — a sleep-through\n";
        assert!(judge_control(says, false, panicked).is_ok());

        let catches = control("no-preempt-guard");
        let caught = "running 1 test\ntest preempted_producer_strands_suffix ... ok\n";
        assert!(judge_control(catches, true, caught).is_ok());
        assert!(judge_control(catches, false, caught).is_err());
        assert!(judge_control(catches, true, "").is_err());

        for c in CONTROLS {
            let judged = judge_control(c, !c.must_red, "error[E0425]: cannot find value");
            assert!(
                matches!(&judged, Err(why) if why.contains("proved nothing")),
                "{}: {judged:?}",
                c.feature
            );
        }
    }

    #[test]
    fn the_summary_keeps_the_count_and_the_verdicts() {
        let log = "test result: ok. 3 passed\n\
                   12:00:00 FAIL rs::lan_talk: no exit code\nnoise\n\
                   12:00:07   STALL rs::lan_talk  (7s)\n\
                   12:00:09 test result: FAILED. 40 passed; 1 failed, 41 total (300 s)\n";
        let said = verdicts(log);
        assert!(said.starts_with("test result: FAILED. 40 passed; 1 failed, 41 total"), "{said}");
        assert!(said.contains("\nFAIL rs::lan_talk: no exit code\n  STALL rs::lan_talk  (7s)\n"), "{said}");
        assert!(!said.contains("noise"), "{said}");
        assert_eq!(verdicts(""), "no suite result line");
    }

    /// Only `main`'s tip publishes: a re-run of an older push is refused.
    #[test]
    fn only_mains_tip_publishes() {
        let tip = "0123456789abcdef0123456789abcdef01234567";
        assert!(at_tip(&format!("{tip}\trefs/heads/main"), tip).is_ok());
        let refusal = at_tip(&format!("{tip}\trefs/heads/main"), &tip.replace('0', "f")).unwrap_err();
        assert!(refusal.contains("workflow_dispatch"), "{refusal}");
        assert!(at_tip("", tip).is_err());
    }

    #[test]
    fn the_required_check_is_a_job_on_every_pull_request() {
        let text = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml"))
            .expect("ci.yml is readable");
        assert!(text.contains("\n  pull_request:\n") && text.contains("\n  merge_group:"));
        assert!(text.contains("\n  host:"), "ci.yml runs no job `host`");
    }

    /// Every workflow's `pull_request:` trigger names `main` alone, and none
    /// asks for a runner this project does not have — a self-hosted label
    /// queues until it times out, silently.
    #[test]
    fn workflows_run_against_main_on_hosted_runners() {
        let dir = repo_root().join(".github/workflows");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect(".github/workflows is readable").flatten() {
            let text = std::fs::read_to_string(entry.path()).expect("a readable workflow");
            let name = entry.file_name().to_string_lossy().into_owned();
            seen += 1;
            if let Some((_, after)) = text.split_once("\n  pull_request:\n") {
                let branches = after.lines().next().unwrap_or("").trim();
                assert_eq!(branches, "branches: [main]", "{name}");
            }
            for runner in text.lines().filter_map(|l| l.trim_start().strip_prefix("runs-on:")) {
                assert!(
                    !runner.contains("self-hosted") && !runner.contains("toyos"),
                    "{name}: runs-on:{runner}"
                );
            }
        }
        assert_eq!(seen, 5, "ci.yml, nightly.yml and publish.yml, and guest.yml and toolchain.yml");
    }

    #[test]
    fn the_declared_version_is_a_version() {
        let declared =
            declared_qemu_version(&repo_root()).expect(".github/qemu-version declares a version");
        assert!(
            declared.split('.').count() >= 2
                && declared.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "{declared:?} is not a QEMU version"
        );
    }

    #[test]
    fn the_version_parser_takes_what_qemu_prints_and_refuses_the_rest() {
        assert_eq!(
            parse_qemu_version("QEMU emulator version 11.0.3 (Debian 1:11.0.3+ds-1)\n").as_deref(),
            Some("11.0.3")
        );
        assert_eq!(parse_qemu_version("QEMU emulator version 11.1.0\n").as_deref(), Some("11.1.0"));
        assert_eq!(parse_qemu_version("qemu-system-x86_64: no such option\n"), None);
        assert_eq!(parse_qemu_version(""), None);
    }
}
