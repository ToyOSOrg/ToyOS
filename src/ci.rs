//! `cargo run -- --ci <job>`: every CI job's logic, so a workflow is a
//! checkout, a cache and one line, and this host runs the same job to the same
//! verdict.
//!
//! `.github/workflows/` is three files. `ci.yml` runs on a pull request and in
//! the merge queue and boots no guest, split so each half reads only the event
//! it needs: [`Job::AbiSplit`] runs as its own job on the pull request, where
//! the branch's own commits are; [`Job::Host`] and then [`Job::GateStage`] run
//! as `host`, only in the merge queue, where every branch has already been
//! judged as a pull request. A required check a workflow skips on the other
//! event still reports, and a skip counts as passing — that is how each half
//! enters the queue it does not itself run in. `nightly.yml` runs everything
//! that boots a guest, the rest of the host checks (`host-full`), and
//! portability. `publish.yml` puts a landing's crates on crates.io.
//!
//! A host job runs every step and reds if any failed; a guest job stops at the
//! first failure, because what follows a wrong instrument or a missing
//! toolchain measures nothing. Each step's verdict goes to
//! `$GITHUB_STEP_SUMMARY` where a runner provides one.
//!
//! **The instrument is declared once.** `.github/qemu-version` is the QEMU
//! every guest is measured with — the version has been measured to decide
//! verdicts (`desktop_typing_damage` and `usb_storage_shapes` are red on 8.2.2
//! and green on 11.0.3, same image, same commit, same accelerator). A guest job
//! reds on a disagreement, and on a `/dev/kvm` that is present and does not
//! open; `cargo run` only notes one, because a build must not stop for brew.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;

use crate::{flags, pr, release, sdkversion};

/// The checks `main`'s ruleset must require, as `gate-stage` reads them back:
/// a minimum, never an equality, so a name GitHub requires and this does not
/// is reported rather than refused.
pub(crate) const REQUIRED_CHECKS: &[&str] = &["host", "abi-split"];

/// The one issue a red nightly files or comments on, found by title.
const NIGHTLY_RED: &str = "nightly is red";

const USAGE: &str = "cargo run -- --ci <job>, where <job> is one of:
  host              cargo test --lib, every host-workspace suite and clippy (ci.yml)
  abi-split         the published crates' versions (ci.yml; the name is the required check's)
  gate-stage        what protects main, read back from GitHub (ci.yml)
  host-full         host, plus the model controls, userland and the SDK (nightly)
  toolchain         publish this tree's toolchain if nobody has (nightly)
  guest <i>/<n>     one shard of the whole guest suite, nightly tier included (nightly)
  tcg               one test on an emulated CPU (nightly)
  audio <i>/<n>     one shard of gate A (nightly)
  nightly-red       file or update the nightly-red issue from $NEEDS (nightly)
  publish           put main's SDK crates on crates.io (publish.yml)";

#[derive(Debug, PartialEq, Eq)]
enum Job {
    Host,
    AbiSplit,
    GateStage,
    HostFull,
    Toolchain,
    Guest(String),
    Tcg,
    Audio(String),
    NightlyRed,
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
        Some("abi-split") => Job::AbiSplit,
        Some("gate-stage") => Job::GateStage,
        Some("host-full") => Job::HostFull,
        Some("toolchain") => Job::Toolchain,
        Some("guest") => Job::Guest(shard(words.get(1))?),
        Some("tcg") => Job::Tcg,
        Some("audio") => Job::Audio(shard(words.get(1))?),
        Some("nightly-red") => Job::NightlyRed,
        Some("publish") => Job::Publish,
        Some(other) => return Err(format!("no CI job is called {other:?}")),
        None => return Err("which job?".to_string()),
    };
    let takes = usize::from(matches!(job, Job::Guest(_) | Job::Audio(_))) + 1;
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
        Job::AbiSplit => {
            vec![step("the published crates' versions", || abi_split(root))]
        }
        Job::GateStage => vec![step("what protects main", || gate_stage(root))],
        Job::HostFull => host_full(root),
        Job::Toolchain => vec![step("the toolchain release", || release::ensure_published(root))],
        Job::Guest(shard) => {
            guest(root, &suite_args(&["--shard", shard, "--jobs", "1", "--nightly"]))
        }
        Job::Tcg => guest(root, &suite_args(&["--jobs", "1", "process_stats"])),
        Job::Audio(shard) => guest(root, &suite_args(&["--audio-gate", "30", "--shard", shard])),
        Job::NightlyRed => vec![step("the nightly-red issue", nightly_red)],
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

/// The merge queue's whole gate: the build system's own tests, every member of
/// the host workspace, and clippy with warnings denied. None of the three
/// needs the ToyOS toolchain nightly.yml alone builds — the kernel and the
/// bootloader clippy against `x86_64-unknown-none`/`x86_64-unknown-uefi`,
/// targets any rustup installs, and `x86_64-unknown-toyos` (the one target
/// that does need the fork) is userland's alone, and userland carries no
/// clippy shape (`src/clippy.rs`).
fn host(root: &Path) -> Vec<Step> {
    let mut steps = vec![
        step("the build system", || cargo(root, &["test", "--lib"])),
        step("the host workspace", || {
            cargo(root, &["test", "--workspace", "--exclude", "toyos-build"])
        }),
    ];
    steps.push(step("clippy and the bare targets", || {
        for args in [
            &["component", "add", "clippy"][..],
            &["target", "add", "x86_64-unknown-none", "x86_64-unknown-uefi"],
        ] {
            let status = Command::new("rustup").args(args).status().map_err(|e| e.to_string())?;
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
    steps
}

/// What a model of the kernel's concurrency is shown able to catch: a feature
/// that takes away the one edge the model's property rests on, and the verdict
/// lines the model must then print.
pub(crate) struct Control {
    /// `cargo test` arguments that select the model's crate.
    krate: &'static [&'static str],
    pub(crate) feature: &'static str,
    test: Option<&'static str>,
    /// `false` is a case that catches its own panic and asserts on it: its
    /// teeth are a green run.
    must_red: bool,
    /// Every one must be in the output. An exit code alone would read a compile
    /// error as the model having teeth.
    verdicts: &'static [&'static str],
}

const KERNEL_LOOM: &[&str] = &["--manifest-path", "kernel-loom/Cargo.toml"];
const SCHED_LOOM: &[&str] = &["-p", "toyos-sched-loom"];
const SCHED_SIM: &[&str] = &["-p", "toyos-sched-sim"];
const PROCLIFE: &[&str] = &["-p", "toyos-proclife"];

const fn red(
    krate: &'static [&'static str],
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
    red(KERNEL_LOOM, "poison-overwrite", Some("poison_set"), &[
        "a_second_death_banks_beside_the_first ... FAILED",
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
    red(KERNEL_LOOM, "poll-fire-load-store", Some("poll_once"), &[
        "a_post_and_a_recheck_answer_a_poll_once ... FAILED",
        "a_withdrawal_and_a_post_never_both_take_a_poll ... FAILED",
    ]),
    red(KERNEL_LOOM, "sleeplock-acquire-off", Some("sleep_lock"), &[
        "a_parking_contender_observes_the_holders_writes ... FAILED",
        "two_holders_never_overlap ... FAILED",
    ]),
    red(KERNEL_LOOM, "durability-settle-blind", Some("durability"), &[
        "a_page_marked_clean_is_on_the_device ... FAILED",
        "a_settled_commit_covers_only_flushed_writes ... FAILED",
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
    red(SCHED_SIM, "placement-ignores-staleness", Some("policy"), &[
        "a_stopped_cpu_stops_taking_work ... FAILED",
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
    let mut args = vec!["test"];
    args.extend(control.krate);
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

/// The userland crates whose decisions are testable on the host. They name the
/// host triple because `userland/.cargo/config.toml` cross-compiles by default,
/// which is also why they cannot be host-workspace members.
const USERLAND_HOST_CRATES: &[&str] = &["sshd", "calc", "soundd", "logd", "pkg", "netd"];

fn host_full(root: &Path) -> Vec<Step> {
    let host_triple = crate::toolchain::host_triple();
    let mut steps = self::host(root);
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
    for name in USERLAND_HOST_CRATES {
        let manifest = format!("userland/{name}/Cargo.toml");
        steps.push(step(&format!("userland/{name}"), || {
            cargo(root, &["test", "--manifest-path", &manifest, "--target", &host_triple])
        }));
    }
    // The SDK compiles against the ToyOS sysroot everywhere but here, and this
    // build links no syscall.
    steps.push(step("the toyos SDK", || {
        cargo(root, &["test", "--manifest-path", "toyos/Cargo.toml", "--target", &host_triple])
    }));
    steps
}

/// A pull request against `main`, run as its own `ci.yml` job because it reads
/// the branch's own history against its merge base — a merge group's is
/// several branches', each already judged this way as a pull request, which is
/// why `abi-split` is not a job there at all. It keeps the name branch
/// protection requires.
fn abi_split(root: &Path) -> Result<String, String> {
    pr::git(root, &["fetch", "--quiet", "origin", "+refs/heads/main:refs/remotes/origin/main"])?;
    sdkversion::judge(root, "origin/main")
}

/// What protects `main` is configured outside the repository, so it is read
/// back: every [`REQUIRED_CHECKS`] name required, deletion and force-push
/// refused, and merge the only method.
fn gate_stage(root: &Path) -> Result<String, String> {
    let out = Command::new("gh")
        .args(["api", "repos/{owner}/{repo}/rules/branches/main"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!("gh api: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let rules: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("GitHub's rules: {e}"))?;
    let (bad, said) = protection(&rules);
    summary(&format!("### what protects main\n\n{}", said.join("\n")));
    println!("{}", said.join("\n"));
    if bad.is_empty() {
        Ok("main is protected".into())
    } else {
        Err(bad.join("; "))
    }
}

/// The refusals and the report, from `rules/branches/main`'s JSON.
fn protection(rules: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let all = rules.as_array().map(Vec::as_slice).unwrap_or_default();
    let of = |kind: &'static str| all.iter().filter(move |r| r["type"] == kind);
    let types: Vec<&str> = all.iter().filter_map(|r| r["type"].as_str()).collect();
    let live: Vec<&str> = of("required_status_checks")
        .flat_map(|r| r["parameters"]["required_status_checks"].as_array().into_iter().flatten())
        .filter_map(|c| c["context"].as_str())
        .collect();
    let mut methods: Vec<&str> = of("pull_request")
        .flat_map(|r| r["parameters"]["allowed_merge_methods"].as_array().into_iter().flatten())
        .filter_map(|m| m.as_str())
        .collect();
    methods.sort_unstable();

    let mut bad = Vec::new();
    for want in REQUIRED_CHECKS {
        if !live.contains(want) {
            bad.push(format!("main does not require the check `{want}`"));
        }
    }
    for want in ["deletion", "non_fast_forward", "pull_request", "required_status_checks"] {
        if !types.contains(&want) {
            bad.push(format!("main has no `{want}` rule"));
        }
    }
    if methods != ["merge"] {
        bad.push(format!("main allows merge methods {methods:?}, not merge alone"));
    }
    let mut said = vec![
        format!("- rules: `{}`", types.join(" ")),
        format!("- required checks: `{}`", live.join(" ")),
        format!(
            "- merge queue: `{}`, merge methods: `{}`",
            types.contains(&"merge_queue"),
            methods.join(",")
        ),
    ];
    for extra in live.iter().filter(|c| !REQUIRED_CHECKS.contains(c)) {
        said.push(format!("- `{extra}` is required at GitHub and not named in src/ci.rs"));
    }
    (bad, said)
}

// --- The guest jobs ------------------------------------------------------------

/// The harness's arguments for a CI lane: a runner is a whole host with one
/// suite on it, so the host's guest slots arbitrate nothing there.
fn suite_args(args: &[&str]) -> Vec<String> {
    let mut all = vec!["test", "--test", "toyos-build", "--"];
    all.extend(args);
    if on_runner() {
        all.extend(["--host-slots", "0"]);
    }
    all.into_iter().map(String::from).collect()
}

fn guest(root: &Path, suite: &[String]) -> Vec<Step> {
    let mut steps = vec![step("the instrument", || instrument(root))];
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
    steps
}

/// The suite's own count line and every line naming a verdict worth reading
/// without the log: a failure, whether it survived being run alone, and a
/// quarantined name that failed for something else.
fn verdicts(log: &str) -> String {
    let total = log
        .lines()
        .rfind(|l| l.contains("test result:") && l.contains(" total ("))
        .unwrap_or("no suite result line");
    let named: Vec<&str> = log
        .lines()
        .filter(|l| {
            l.starts_with("FAIL ")
                || l.starts_with("XFAIL ")
                || (l.starts_with(' ')
                    && ["STALL ", "INVL ", "ALONE "].iter().any(|v| l.trim_start().starts_with(v)))
                || l.contains("is quarantined for something else")
        })
        .collect();
    if named.is_empty() {
        total.to_string()
    } else {
        format!("{total}\n```\n{}\n```", named.join("\n"))
    }
}

/// The QEMU on `PATH` against `.github/qemu-version`, and whether `/dev/kvm`
/// opens where it is present — the two things a guest verdict must be read
/// against.
fn instrument(root: &Path) -> Result<String, String> {
    let want = declared_qemu_version(root).ok_or(".github/qemu-version declares no version")?;
    let out = Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .map_err(|e| format!("qemu-system-x86_64: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let have = parse_qemu_version(&said).ok_or_else(|| format!("QEMU said {said:?}"))?;
    let node = Path::new("/dev/kvm").exists();
    let accel = match (node, crate::kvm_usable()) {
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
    let line = format!("QEMU {have}, {accel}, {cpu}, {cores} core(s)");
    if have != want {
        return Err(format!(
            "{line}: this runs QEMU {have} and .github/qemu-version declares {want}. The \
             container image's digest is what pins it, so moving it is a commit that says the \
             instrument moved"
        ));
    }
    if node && !crate::kvm_usable() {
        return Err(format!("{line}: every boot would fall back to emulation in silence"));
    }
    Ok(line)
}

/// The QEMU every guest in CI runs, and the one this project's recorded numbers
/// were taken on. Comment lines and blanks are stripped, so the file can explain
/// itself.
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

/// `QEMU emulator version 11.0.3 (Debian 1:11.0.3+ds-1)` → `11.0.3`.
fn parse_qemu_version(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("QEMU emulator version ")?;
    let version = rest.split_whitespace().next()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// The line `cargo run` prints when this host is not the instrument the
/// project's numbers were taken on, and nothing at all when it is.
pub fn qemu_version_note(root: &Path) -> Option<String> {
    let want = declared_qemu_version(root)?;
    let out = Command::new("qemu-system-x86_64").arg("--version").output().ok()?;
    let have = parse_qemu_version(&String::from_utf8_lossy(&out.stdout))?;
    (have != want).then(|| {
        format!(
            "Note: this host runs QEMU {have} and .github/qemu-version declares {want} — \
             CI's guests and tests/audio-baseline.toml are on {want}, and the QEMU version \
             has been measured to decide test outcomes. Nothing here is broken; a comparison \
             across the two is."
        )
    })
}

// --- The nightly's alarm and the publisher -------------------------------------

/// The jobs `$NEEDS` (`toJSON(needs)`) says did not succeed, as `name(result)`.
fn failed_jobs(needs: &serde_json::Value) -> Vec<String> {
    let Some(jobs) = needs.as_object() else {
        return vec!["NEEDS is not an object".into()];
    };
    jobs.iter()
        .filter_map(|(name, job)| {
            let result = job["result"].as_str().unwrap_or("unknown");
            (result != "success").then(|| format!("{name}({result})"))
        })
        .collect()
}

/// One standing issue for a red nightly, found by title and commented on
/// rather than filed twice. Every red is adjudicated into a fix, a
/// `src/redlist.rs` row or a tier move; the issue is the alarm, not the record.
fn nightly_red() -> Result<String, String> {
    let needs = std::env::var("NEEDS").map_err(|_| "NEEDS carries no job results".to_string())?;
    let needs: serde_json::Value =
        serde_json::from_str(&needs).map_err(|e| format!("NEEDS is not JSON: {e}"))?;
    let failed = failed_jobs(&needs);
    if failed.is_empty() {
        return Ok("every job was green".into());
    }
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let body = format!(
        "Run: {}/{}/actions/runs/{}\nFailed jobs: {}",
        var("GITHUB_SERVER_URL"),
        var("GITHUB_REPOSITORY"),
        var("GITHUB_RUN_ID"),
        failed.join(" ")
    );
    let found = Command::new("gh")
        .args(["issue", "list", "--state", "open", "--limit", "30", "--json", "number,title"])
        .args(["--search", &format!("in:title \"{NIGHTLY_RED}\"")])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !found.status.success() {
        // An unanswered search read as "none open" would file a duplicate.
        return Err(format!("gh issue list: {}", String::from_utf8_lossy(&found.stderr).trim()));
    }
    let open: serde_json::Value =
        serde_json::from_slice(&found.stdout).map_err(|e| format!("gh issue list: {e}"))?;
    let number = open
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["title"] == NIGHTLY_RED)
        .and_then(|i| i["number"].as_u64());
    let mut gh = Command::new("gh");
    match number {
        Some(n) => gh.args(["issue", "comment", &n.to_string(), "--body", &body]),
        None => gh.args(["issue", "create", "--title", NIGHTLY_RED, "--body", &body]),
    };
    let status = gh.status().map_err(|e| format!("gh: {e}"))?;
    if !status.success() {
        return Err(format!("gh exited {status}"));
    }
    Ok(format!("reported {}", failed.join(" ")))
}

/// Each of the SDK crates the index does not already hold, in dependency
/// order, waiting for each to be readable before the next resolves it. Only
/// `main` publishes: a version is a name taken once.
fn publish(root: &Path) -> Result<String, String> {
    if on_runner() {
        if std::env::var("GITHUB_REF").ok().as_deref() != Some("refs/heads/main") {
            return Err("only a push to main publishes".into());
        }
    } else {
        pr::git(root, &["fetch", "--quiet", "origin"])?;
        if pr::git(root, &["rev-parse", "HEAD"])? != pr::git(root, &["rev-parse", "origin/main"])? {
            return Err("this checkout is not origin/main, and only main publishes".into());
        }
    }
    if std::env::var("CARGO_REGISTRY_TOKEN").map_or(true, |t| t.is_empty()) {
        return Err(
            "CARGO_REGISTRY_TOKEN is not set; the owner adds it under Settings → Secrets".into()
        );
    }
    let mut said = Vec::new();
    for (name, version, manifest) in sdkversion::versions(root) {
        if on_index(name, &version)? {
            said.push(format!("{name} {version} was there"));
            continue;
        }
        cargo(root, &["publish", "--manifest-path", &manifest])?;
        let mut seen = false;
        for _ in 0..60 {
            if on_index(name, &version)? {
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

/// Whether the crates.io sparse index holds `name` at `version`, unyanked. A
/// 404 is a crate never published.
fn on_index(name: &str, version: &str) -> Result<bool, String> {
    let url = format!("https://index.crates.io/{}/{}/{name}", &name[..2], &name[2..4]);
    let out = Command::new("curl")
        .args(["-sS", "-w", "\n%{http_code}", &url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", ""));
    match code {
        "404" => Ok(false),
        "200" => Ok(indexed(body, version)),
        other => Err(format!("the crates.io index answered {other:?} for {name}")),
    }
}

/// Whether one crate's index file holds `version`, unyanked: one JSON object a
/// line.
fn indexed(body: &str, version: &str) -> bool {
    body.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| v["vers"] == version && v["yanked"] != true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    /// The rules as `gh api repos/ToyOSOrg/ToyOS/rules/branches/main` answered,
    /// and the same with the protections this reads taken away.
    #[test]
    fn protection_is_read_back_and_a_loosened_rule_is_refused() {
        let live = r#"[{"type":"deletion"},{"type":"non_fast_forward"},
            {"type":"pull_request","parameters":{"allowed_merge_methods":["merge"]}},
            {"type":"required_status_checks","parameters":{"required_status_checks":[
              {"context":"host"},{"context":"abi-split"},{"context":"gate-stage"},
              {"context":"guest-suite"},{"context":"build"}]}},
            {"type":"merge_queue","parameters":{}}]"#;
        let (bad, said) = protection(&serde_json::from_str(live).unwrap());
        assert!(bad.is_empty(), "{bad:?}");
        assert!(said.iter().any(|l| l.contains("`guest-suite` is required at GitHub")), "{said:?}");

        let loose = r#"[{"type":"pull_request","parameters":{"allowed_merge_methods":["merge","squash"]}},
            {"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"build"}]}}]"#;
        let (bad, _) = protection(&serde_json::from_str(loose).unwrap());
        assert!(bad.iter().any(|b| b.contains("does not require the check `host`")), "{bad:?}");
        assert!(bad.iter().any(|b| b.contains("no `deletion` rule")), "{bad:?}");
        assert!(bad.iter().any(|b| b.contains("merge methods")), "{bad:?}");
        let (bad, _) = protection(&serde_json::json!({"message": "Not Found"}));
        assert!(!bad.is_empty());
    }

    #[test]
    fn the_nightly_names_every_job_that_did_not_succeed() {
        let needs = serde_json::json!({
            "host": {"result": "success", "outputs": {}},
            "guest": {"result": "failure", "outputs": {}},
            "tcg": {"result": "skipped", "outputs": {}},
        });
        assert_eq!(failed_jobs(&needs), ["guest(failure)", "tcg(skipped)"]);
        assert!(failed_jobs(&serde_json::json!({"host": {"result": "success"}})).is_empty());
    }

    #[test]
    fn the_summary_keeps_the_count_and_the_verdicts() {
        let log = "test result: ok. 3 passed\n\
                   FAIL rs::lan_talk: no exit code\n  ALONE lan_talk: GREEN\nnoise\n\
                   test result: FAILED. 40 passed; 1 failed, 41 total (300 s)\n";
        let said = verdicts(log);
        assert!(said.starts_with("test result: FAILED. 40 passed; 1 failed, 41 total"), "{said}");
        assert!(said.contains("FAIL rs::lan_talk") && said.contains("ALONE lan_talk"), "{said}");
        assert!(!said.contains("noise"), "{said}");
        assert_eq!(verdicts(""), "no suite result line");
    }

    #[test]
    fn the_index_holds_a_version_only_unyanked() {
        let body = "{\"name\":\"toyos-abi\",\"vers\":\"0.7.0\",\"yanked\":false}\n\
                    {\"name\":\"toyos-abi\",\"vers\":\"0.8.0\",\"yanked\":true}\n";
        assert!(indexed(body, "0.7.0"));
        assert!(!indexed(body, "0.8.0"));
        assert!(!indexed(body, "0.9.0"));
    }

    /// Every name `gate-stage` holds the ruleset to is a job `ci.yml` runs on a
    /// pull request and in the merge queue, so a required check always has
    /// something reporting it.
    #[test]
    fn every_required_check_is_a_job_on_every_pull_request() {
        let text = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml"))
            .expect("ci.yml is readable");
        assert!(text.contains("\n  pull_request:\n") && text.contains("\n  merge_group:"));
        for name in REQUIRED_CHECKS {
            assert!(text.contains(&format!("\n  {name}:")), "ci.yml runs no job `{name}`");
        }
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
