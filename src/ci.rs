//! `cargo run -- --ci <job>`: every CI job's logic, so a workflow is a
//! checkout, a cache and one line, and this host runs the same job to the same
//! verdict.
//!
//! `.github/workflows/` is three files. `ci.yml` runs on a pull request and in
//! the merge queue and boots no guest: [`Job::Host`] runs as `host`. Every
//! test that boots no guest is in [`Job::Host`], so a merge is gated on all of
//! them. `nightly.yml` runs everything that boots a guest, `host` again to
//! write the cache the merge queue restores, and portability. `publish.yml`
//! puts a landing's crates on crates.io.
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
//! verdicts (`desktop_typing_damage` and `usb_storage_shapes` are red on 8.2.2
//! and green on 11.0.3, same image, same commit, same accelerator). A guest job
//! reds on a disagreement, and on a `/dev/kvm` that is present and does not
//! open; `cargo run` only notes one, because a build must not stop for brew.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::{flags, release, sdkversion, sync};

pub const GUEST_ARCH: Arch = Arch::X86_64;

const USAGE: &str = "cargo run -- --ci <job>, where <job> is one of:
  host              every host test: the build system, the harness's own checks,
                    the host workspace, the licences of what ships, clippy, the
                    model controls, userland and the SDK (ci.yml, nightly)
  toolchain         publish this tree's toolchain if nobody has (nightly)
  guest <i>/<n>     one shard of the guest suite (nightly)
  tcg               one test on an emulated CPU (nightly)
  publish           put main's SDK crates on crates.io (publish.yml)";

#[derive(Debug, PartialEq, Eq)]
enum Job {
    Host,
    Toolchain,
    Guest(String),
    Tcg,
    Publish,
}

fn parse(words: &[String]) -> Result<Job, String> {
    let shard = |spec: Option<&String>| -> Result<String, String> {
        let spec = spec.ok_or("that job takes a shard, <index>/<count>")?;
        crate::testargs::parse_shard(&["--shard".to_string(), spec.clone()])?;
        Ok(spec.clone())
    };
    let job = match words.first().map(String::as_str) {
        Some("host") => Job::Host,
        Some("toolchain") => Job::Toolchain,
        Some("guest") => Job::Guest(shard(words.get(1))?),
        Some("tcg") => Job::Tcg,
        Some("publish") => Job::Publish,
        Some(other) => return Err(format!("no CI job is called {other:?}")),
        None => return Err("which job?".to_string()),
    };
    let takes = usize::from(matches!(job, Job::Guest(_))) + 1;
    if words.len() > takes {
        return Err(format!("{:?} takes nothing after it: {:?}", words[0], &words[takes..]));
    }
    Ok(job)
}

pub fn dispatch(root: &Path, args: &[String]) {
    let job = parse(flags::CARGO_RUN.rest(args, &flags::CI)).unwrap_or_else(|refusal| {
        eprintln!("Error: {refusal}\n{USAGE}");
        std::process::exit(2);
    });
    let steps = match &job {
        Job::Host => host(root),
        Job::Toolchain => vec![step("the toolchain release", || release::ensure_published(root))],
        Job::Guest(shard) => guest(root, &suite_args(&["--shard", shard, "--jobs", "1"])),
        Job::Tcg => guest(root, &suite_args(&["--jobs", "1", "process_stats"])),
        Job::Publish => vec![step("the SDK crates on crates.io", || publish(root))],
    };
    let failed: Vec<&Step> = steps.iter().filter(|s| s.verdict.is_err()).collect();
    summary(&steps.iter().map(Step::line).collect::<Vec<_>>().join("\n"));
    if failed.is_empty() {
        println!("[ci] {job:?}: {} step(s), all green", steps.len());
    } else {
        eprintln!("[ci] {job:?}: {} of {} step(s) red:", failed.len(), steps.len());
        for s in failed {
            eprintln!("  {}", s.line());
        }
        std::process::exit(1);
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
    println!("\n=== [ci] {label}");
    let verdict = f();
    match &verdict {
        Ok(said) => println!("[ci] {label}: {said}"),
        Err(why) => eprintln!("[ci] {label}: {why}"),
    }
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

/// `cargo <args>` in `dir`, its output passed straight through.
fn cargo(dir: &Path, args: &[&str]) -> Result<String, String> {
    let status = Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    let line = format!("cargo {}", args.join(" "));
    if status.success() {
        Ok(line)
    } else {
        Err(format!("{line} exited {status}"))
    }
}

/// `cargo <args>` in `dir`, its output passed through and also kept, both
/// streams in the order they were written: a verdict read off the log needs the
/// whole of it.
fn cargo_logged(dir: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let (reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
    let mut child = Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .stdout(writer.try_clone().map_err(|e| format!("pipe: {e}"))?)
        .stderr(writer)
        .spawn()
        .map_err(|e| format!("cargo: {e}"))?;
    let mut log = String::new();
    let mut out = std::io::stdout();
    for line in BufReader::new(reader).split(b'\n') {
        let line = line.map_err(|e| format!("reading cargo: {e}"))?;
        let line = String::from_utf8_lossy(&line);
        let _ = writeln!(out, "{line}");
        log.push_str(&line);
        log.push('\n');
    }
    let status = child.wait().map_err(|e| format!("cargo: {e}"))?;
    Ok((status.success(), log))
}

// --- The host jobs -------------------------------------------------------------

/// What a model of the kernel's concurrency is shown able to catch: a feature
/// that takes away the one edge the model's property rests on, and the verdict
/// lines the model must then print.
pub(crate) struct Control {
    /// The model's package in the host workspace.
    pub(crate) krate: &'static str,
    pub(crate) feature: &'static str,
    test: Option<&'static str>,
    /// `false` is a case that catches its own panic and asserts on it: its
    /// teeth are a green run.
    must_red: bool,
    /// Every one must be in the output. An exit code alone would read a compile
    /// error as the model having teeth.
    verdicts: &'static [&'static str],
}

const KERNEL_LOOM: &str = "kernel-loom";
const SCHED_LOOM: &str = "toyos-sched-loom";
const SCHED_SIM: &str = "toyos-sched-sim";
const PROCLIFE: &str = "toyos-proclife";
const BLOCKRING: &str = "toyos-blockring";
const TRANSPORT: &str = "toyos-transport";

const fn red(
    krate: &'static str,
    feature: &'static str,
    test: Option<&'static str>,
    verdicts: &'static [&'static str],
) -> Control {
    Control { krate, feature, test, must_red: true, verdicts }
}

/// Every negative control a model crate declares; `src/build.rs`'s
/// `every_model_control_is_run` holds this against the manifests.
pub(crate) const CONTROLS: &[Control] = &[
    red(KERNEL_LOOM, "wake-fence-off", Some("log_wake"), &[
        "a_commit_and_an_arm_cannot_both_miss ... FAILED",
    ]),
    red(KERNEL_LOOM, "lock-acquire-off", Some("ticket_lock"), &[
        "try_lock_observes_the_previous_owners_writes ... FAILED",
    ]),
    red(KERNEL_LOOM, "seqlock-writer-fence-off", Some("panic_console_publish"), &[
        "a_snapshot_is_one_publication_whole ... FAILED",
    ]),
    red(KERNEL_LOOM, "serial-try-lock-then-some", Some("serial_lock"), &[
        "a_lost_try_lock_leaves_the_lock_held ... FAILED",
        "two_writers_never_overlap ... FAILED",
    ]),
    red(KERNEL_LOOM, "reap-raise-relaxed", Some("reap_gate"), &[
        "a_claim_sees_the_enrolled_work ... FAILED",
    ]),
    red(KERNEL_LOOM, "shootdown-serve-relaxed", Some("tlb_shootdown"), &[
        "an_acknowledged_flush_postdates_the_page_table_write ... FAILED",
        "one_serve_answers_two_concurrent_shootdowns ... FAILED",
    ]),
    red(KERNEL_LOOM, "roster-commit-relaxed", Some("smp_bringup"), &[
        "a_committed_count_never_outruns_its_slot ... FAILED",
    ]),
    red(KERNEL_LOOM, "smp-ready-split", Some("smp_bringup"), &[
        "a_released_machine_is_answering ... FAILED",
    ]),
    red(KERNEL_LOOM, "log-commit-release-off", Some("log_record"), &[
        "a_committed_record_is_whole_or_absent ... FAILED",
        "a_key_and_the_record_it_names_come_from_one_generation ... FAILED",
    ]),
    red(KERNEL_LOOM, "shard-publish-relaxed", Some("log_publish"), &[
        "a_reader_that_finds_a_shard_finds_it_built ... FAILED",
    ]),
    red(KERNEL_LOOM, "log-ring-publish-relaxed", Some("log_ring"), &[
        "a_published_record_is_whole_and_read_once ... FAILED",
        "a_slot_is_reused_only_after_its_record_was_read ... FAILED",
        "a_lane_publishes_whole_and_reuses_only_after_a_read ... FAILED",
    ]),
    red(KERNEL_LOOM, "log-ring-tail-relaxed", Some("log_ring"), &[
        "a_published_record_is_whole_and_read_once ... FAILED",
        "a_slot_is_reused_only_after_its_record_was_read ... FAILED",
        "a_lane_publishes_whole_and_reuses_only_after_a_read ... FAILED",
    ]),
    red(KERNEL_LOOM, "log-ring-loads-swapped", Some("log_ring"), &[
        "a_published_record_is_whole_and_read_once ... FAILED",
    ]),
    red(KERNEL_LOOM, "poll-fire-load-store", Some("poll_once"), &[
        "a_post_and_a_recheck_answer_a_poll_once ... FAILED",
        "a_withdrawal_and_a_post_never_both_take_a_poll ... FAILED",
    ]),
    red(KERNEL_LOOM, "sleeplock-acquire-off", Some("sleep_lock"), &[
        "a_parking_contender_observes_the_holders_writes ... FAILED",
        "two_holders_never_overlap ... FAILED",
    ]),
    red(KERNEL_LOOM, "device-irq-lossy", Some("device_irq"), &[
        "every_message_is_counted_once ... FAILED",
        "one_message_is_one_wake ... FAILED",
    ]),
    red(KERNEL_LOOM, "dump-report-relaxed", Some("dump_request"), &[
        "a_request_filed_during_a_report_is_reported ... FAILED",
    ]),
    Control {
        krate: SCHED_LOOM,
        feature: "no-preempt-guard",
        test: Some("loom_mailbox"),
        must_red: false,
        verdicts: &["preempted_producer_strands_suffix ... ok"],
    },
    // A double panic aborts before the harness prints a `FAILED` line, so the
    // verdict is the first panic's own message.
    red(SCHED_LOOM, "doorbell-kick-relaxed", Some("loom_sleep"), &[
        "halted with 2 of 2 messages queued and no IPI in flight",
    ]),
    red(SCHED_LOOM, "push-fence-relaxed", Some("loom_push"), &["published and no push behind it"]),
    // The watch's lost wake, staged: the waiter parks over a post it was flagged
    // with. A double panic, so the verdict is the first one's message.
    red(SCHED_LOOM, "commit-ignores-notify", Some("loom_watch"), &[
        "parked with the condition true and no wake owed: the post was lost",
    ]),
    // The notify's flagged arm answering off a load: a second post reads the
    // word from before the waiter consumed the first flag.
    red(SCHED_LOOM, "notify-flag-load-only", Some("loom_watch"), &[
        "parked with both conditions true and no wake owed: a post answered off a load",
    ]),
    // The stop's store-buffering pair with the gate's fences gone.
    red(SCHED_LOOM, "gate-fence-off", Some("loom_watch"), &[
        "the stop parked over a thread that had parked, and nothing posted it",
    ]),
    // `kernel-loom`'s control, over the kernel's `Once` as the watch models'
    // ring entry.
    red(SCHED_LOOM, "poll-fire-load-store", Some("loom_watch"), &[
        "a_poll_registered_racing_a_post_completes_exactly_once ... FAILED",
        "a_poll_on_two_watches_racing_both_posts_completes_exactly_once ... FAILED",
    ]),
    // Reproduces an open defect
    // (`issues/kernel/steal-probe-node-dies-with-its-victim.md`) rather than
    // proving a lie is caught, and goes with its fix.
    Control {
        krate: SCHED_LOOM,
        feature: "victim-retires-mid-probe",
        test: Some("loom_mailbox"),
        must_red: false,
        verdicts: &[
            "caught the verdict: the victim retired with a probe still linked in its queue",
        ],
    },
    red(PROCLIFE, "mutate-spawn-skips-the-insert-recheck", None, &[
        "a_published_exit_leaves_no_unretired_thread ... FAILED",
        "a_kill_racing_a_spawn_leaves_no_unretired_thread ... FAILED",
    ]),
    red(PROCLIFE, "mutate-claim-teardown-always-wins", None, &[
        "an_exit_and_a_kill_never_both_tear_a_process_down ... FAILED",
    ]),
    red(PROCLIFE, "mutate-kill-waits-for-its-victims", None, &[
        "two_processes_killing_each_other_both_end ... FAILED",
        "a_kill_chain_of_three_ends ... FAILED",
    ]),
    red(PROCLIFE, "mutate-first-out-tears-down", None, &[
        "an_exit_and_a_kill_never_both_tear_a_process_down ... FAILED",
    ]),
    red(PROCLIFE, "mutate-join-collects-in-a-teardown", None, &[
        "a_join_racing_the_kill_that_takes_its_target ... FAILED",
    ]),
    red(PROCLIFE, "mutate-last-out-leaves-before-its-teardown", None, &[
        "the_last_one_out_is_in_its_process_until_its_teardown_is_done ... FAILED",
        "only_the_thread_that_empties_a_claimed_process_tears_it_down ... FAILED",
    ]),
    red(SCHED_SIM, "placement-ignores-staleness", Some("policy"), &[
        "a_stopped_cpu_stops_taking_work ... FAILED",
    ]),
    // The block protocol's three: a completion lost to a session's end, a
    // completion given twice after a reset, and a loss nothing is written
    // again after.
    red(BLOCKRING, "mutate-session-end-forgets", None, &[
        "every_request_is_answered_exactly_once ... FAILED",
    ]),
    red(BLOCKRING, "mutate-abort-keeps-inflight", None, &[
        "every_request_is_answered_exactly_once ... FAILED",
    ]),
    red(BLOCKRING, "mutate-no-reissue-after-loss", None, &[
        "what_a_flush_calls_durable_is_on_the_medium ... FAILED",
    ]),
    red(TRANSPORT, "publish-relaxed", Some("loom"), &["a_published_entry_is_read_whole ... FAILED"]),
    red(TRANSPORT, "no-clamp", Some("loom"), &["a_hostile_producer_yields_entries_or_a_violation ... FAILED"]),
    red(TRANSPORT, "end-keeps-inflight", None, &[
        "an_end_answers_every_tag_once_and_a_late_completion_nothing ... FAILED",
    ]),
];

/// Whether a control's run showed its teeth.
fn judge_control(control: &Control, exited_green: bool, log: &str) -> Result<String, String> {
    if control.must_red && exited_green {
        return Err(format!("passed with `{}`: the model has no teeth", control.feature));
    }
    if !control.must_red && !exited_green {
        return Err(format!(
            "`{}` failed, so the case's own catch did not hold or something else broke",
            control.feature
        ));
    }
    let missing: Vec<&str> =
        control.verdicts.iter().copied().filter(|v| !log.contains(v)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "no verdict {missing:?}: this proved nothing, and whatever stopped the model is \
             what to fix"
        ));
    }
    Ok(format!("{} verdict(s) reached", control.verdicts.len()))
}

fn run_control(root: &Path, control: &Control) -> Result<String, String> {
    let mut args = vec!["test", "-p", control.krate];
    args.extend(["--features", control.feature]);
    if let Some(test) = control.test {
        args.extend(["--test", test]);
    }
    if !control.must_red {
        args.extend(["--", "--nocapture"]);
    }
    let (green, log) = cargo_logged(root, &args)?;
    judge_control(control, green, &log)
}

/// The merge queue's whole gate, and the nightly's host lane: every test that
/// runs on the host and boots no guest. The build system's own tests, every
/// member of the host workspace, clippy with warnings denied, the concurrency
/// models' negative controls, every userland crate with a host test
/// ([`crate::userlandhost`], which also reds on a userland test none of them
/// runs), and the SDK.
///
/// **Every step runs against a `$TMPDIR` of this job's own, and the last step
/// reds on anything left in it** but the lock `toyos_tmpdir` keeps there: a test
/// that writes scratch past a `toyos_tmpdir::TempDir`, or holds one past its
/// end, is a test that fills the host's disk one run at a time.
///
/// Clippy needs none of the ToyOS toolchain the nightly alone builds — the
/// kernel and the bootloader lint against every architecture's bare targets
/// ([`crate::clippy::BARE_TARGETS`]), which any rustup installs, and userland carries no
/// clippy shape (`src/clippy.rs`). Userland and the SDK are tested against the
/// host triple for the same reason.
fn host(root: &Path) -> Vec<Step> {
    let tmp = toyos_tmpdir::TempDir::new("ci-host");
    let short = Path::new(toyos_tmpdir::SHORT_BASE);
    let before = toyos_tmpdir::gone_roots(short);
    // Before any thread: nothing in this process reads the environment
    // concurrently with the write, and every child inherits it.
    std::env::set_var("TMPDIR", tmp.path());
    let host_triple = crate::toolchain::host_triple();
    let mut steps = vec![
        step("the build system", || cargo(root, &["test", "--lib"])),
        step("the harness's own checks", || cargo(root, &["test", "--test", "toyos-checks"])),
        step("the host workspace", || {
            cargo(root, &["test", "--workspace", "--exclude", "toyos-build"])
        }),
        step("the licences of what ships", || crate::licence::judge(root)),
    ];
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
        let failed = crate::clippy::run(root);
        if failed.is_empty() {
            Ok("clean".into())
        } else {
            Err(failed.join("; "))
        }
    }));
    // `log_zeroed_init` and `log_body_words` are gated `cfg(not(feature =
    // "loom"))`, so the default invocation runs nothing from either.
    steps.push(step("kernel-loom without loom", || {
        cargo(root, &[
            "test",
            "--manifest-path",
            "kernel-loom/Cargo.toml",
            "--no-default-features",
            "--test",
            "log_zeroed_init",
            "--test",
            "log_body_words",
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
                    cargo(root, &["test", "--manifest-path", &manifest, "--target", &host_triple])
                }));
            }
        }
        Err(why) => steps.push(Step { label: "the userland host crates".into(), verdict: Err(why) }),
    }
    // The SDK compiles against the ToyOS sysroot everywhere but here, and this
    // build links no syscall.
    steps.push(step("the toyos SDK", || {
        cargo(root, &["test", "--manifest-path", "toyos/Cargo.toml", "--target", &host_triple])
    }));
    steps.push(step("nothing left in $TMPDIR or /tmp", || left_behind(&tmp, short, &before)));
    steps
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
    let mut steps = vec![step("the instrument", || instrument(root, GUEST_ARCH))];
    if steps.iter().all(|s| s.verdict.is_ok()) {
        steps.push(step("the toolchain", || release::install(root)));
    }
    if steps.iter().all(|s| s.verdict.is_ok()) {
        steps.push(step("the suite", || {
            let args: Vec<&str> = suite.iter().map(String::as_str).collect();
            let (green, log) = cargo_logged(root, &args)?;
            let said = verdicts(&log);
            if green {
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
    let total = log
        .lines()
        .rfind(|l| l.contains("test result:") && l.contains(" total ("))
        .unwrap_or("no suite result line");
    let named: Vec<&str> = log
        .lines()
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

/// The QEMU on `PATH` against `.github/qemu-version`, the firmware it declares,
/// and whether `/dev/kvm` opens where it is present — the three things a guest
/// verdict must be read against.
fn instrument(root: &Path, arch: Arch) -> Result<String, String> {
    let want = declared_qemu_version(root).ok_or(".github/qemu-version declares no version")?;
    let have = qemu_version(arch)?;
    let firmware = crate::firmware::of(arch)?;
    let node = Path::new("/dev/kvm").exists();
    let accelerated = arch.accel().is_hardware();
    let accel = match (node, accelerated) {
        (true, true) => "/dev/kvm opens",
        (true, false) => "/dev/kvm is present and does not open",
        (false, _) => "no /dev/kvm: emulated",
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
    if node && !accelerated {
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
    let tip = sync::git(root, &["ls-remote", "origin", "refs/heads/main"])?;
    at_tip(&tip, &sync::git(root, &["rev-parse", "HEAD"])?)?;
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

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn a_job_is_named_and_a_shard_is_a_shard() {
        assert_eq!(parse(&words("host")), Ok(Job::Host));
        assert_eq!(parse(&words("guest 3/12")), Ok(Job::Guest("3/12".into())));
        assert!(parse(&words("guest")).is_err());
        assert!(parse(&words("guest 13/12")).is_err());
        assert!(parse(&words("host extra")).is_err());
        assert!(parse(&words("smoke")).is_err());
        assert!(parse(&[]).is_err());
    }

    /// Teeth for the controls' judge: a green negative control, a control that
    /// never reached its verdict, and a self-catching case that failed are all
    /// red.
    #[test]
    fn a_control_is_judged_by_its_verdict_and_not_its_exit_alone() {
        let must_red = &CONTROLS[0];
        let verdict = must_red.verdicts[0];
        assert!(judge_control(must_red, false, &format!("test {verdict}\n")).is_ok());
        assert!(judge_control(must_red, true, verdict).unwrap_err().contains("no teeth"));
        assert!(judge_control(must_red, false, "error[E0425]: cannot find value")
            .unwrap_err()
            .contains("proved nothing"));

        let catches = CONTROLS.iter().find(|c| !c.must_red).expect("a self-catching case");
        assert!(judge_control(catches, true, catches.verdicts[0]).is_ok());
        assert!(judge_control(catches, false, catches.verdicts[0]).is_err());
        assert!(judge_control(catches, true, "").is_err());
    }

    #[test]
    fn the_summary_keeps_the_count_and_the_verdicts() {
        let log = "test result: ok. 3 passed\n\
                   FAIL rs::lan_talk: no exit code\nnoise\n\
                   test result: FAILED. 40 passed; 1 failed, 41 total (300 s)\n";
        let said = verdicts(log);
        assert!(said.starts_with("test result: FAILED. 40 passed; 1 failed, 41 total"), "{said}");
        assert!(said.contains("FAIL rs::lan_talk"), "{said}");
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
        assert_eq!(seen, 3, "ci.yml, nightly.yml and publish.yml");
    }

    /// Exactly one job writes each cache, on the nightly, so what a pull request
    /// restores is one run's tree and never a race between two writers.
    #[test]
    fn each_cache_has_one_writer() {
        let dir = repo_root().join(".github/workflows");
        let mut writers = Vec::new();
        for entry in std::fs::read_dir(&dir).expect(".github/workflows is readable").flatten() {
            let text = std::fs::read_to_string(entry.path()).expect("a readable workflow");
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!text.contains("actions/cache@"), "{name}: the combined action saves too");
            let lines: Vec<&str> = text.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                if line.contains("actions/cache/save@") {
                    let key = lines[at..]
                        .iter()
                        .find_map(|l| l.trim_start().strip_prefix("key: "))
                        .expect("a save names its key");
                    writers.push((name.clone(), key.split('$').next().unwrap_or("").to_string()));
                }
            }
        }
        assert!(!writers.is_empty(), "no job writes a cache, so every restore is cold");
        writers.sort();
        let mut prefixes: Vec<&String> = writers.iter().map(|(_, p)| p).collect();
        prefixes.dedup();
        assert_eq!(prefixes.len(), writers.len(), "a cache with two writers: {writers:?}");
        assert!(writers.iter().all(|(f, _)| f == "nightly.yml"), "{writers:?}");
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
