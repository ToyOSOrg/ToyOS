mod kbd_close;
mod log_close;
mod log_gate;

use std::io::{self, BufRead, Write};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::process::Process;
use toyos::syscap::SysCap;

/// Tests that run **inside** this process rather than in a binary it spawns.
///
/// **Not a shortcut: a spawned binary cannot hold what these need.** This
/// process passes its whole namespace to every child, and a `SysCap` dup is not
/// a namespace entry — so a gate whose subject is a right on this program's own
/// capability has nowhere else to run. They answer the same
/// `===TEST_START===`/`===TEST_END===` protocol as a binary, so the host cannot
/// tell the difference and does not have to.
///
/// `kbd-close` is here for a second reason as well as that one: its subject is a
/// pending poll on **this process's own stdin**, which is a `Console`. A spawned
/// binary's stdin is a pipe (see the `Stdio::piped()` below), so the object the
/// collision is about does not exist in one.
const BUILTINS: &[(&str, fn(Option<&SysCap>) -> i32)] = &[
    ("log-gate", log_gate::run),
    ("log-close", log_close::run),
    ("kbd-close", kbd_close::run),
];

/// The job the runner is inside, and whether the list got through: written by
/// the loop, read by the deadline watching it.
static RUNNING: Mutex<String> = Mutex::new(String::new());
static FINISHED: AtomicBool = AtomicBool::new(false);

/// A second handle to the job now running, for the deadline to end it with, or
/// zero between jobs.
///
/// **Taken out with a swap, by whichever of the two gets there first**, so the
/// handle has exactly one owner and the loop closing it cannot leave the
/// deadline killing whatever the slot was handed to next. A duplicate and not
/// the `Child`'s own handle, because a `Child` has one owner and the loop is
/// blocked inside its `wait`.
static JOB: AtomicU32 = AtomicU32::new(0);

/// Take the running job, if it is still this thread's to take.
fn claim_the_job() -> Option<Process> {
    match JOB.swap(0, Ordering::AcqRel) {
        0 => None,
        // SAFETY: the swap is what makes this the only holder of the duplicate
        // `run_one` made; nothing else answers for it after this.
        raw => Some(unsafe { Process::from_raw(toyos::RawHandle(raw)) }),
    }
}

fn main() {
    // **The test estate's authority, and the one place least authority is not
    // enforced.** The guest binaries are not `[programs]` keys, so no manifest
    // row can name what any of them holds: a test binary holds what test-runner
    // holds. The namespace
    // travels by inheritance; this capability is handed over explicitly, as a
    // duplicate rather than the cap itself, because one boot runs several
    // binaries that each need the keyboard and a device claim moves.
    let cap: Option<SysCap> = Endowments::get().take(SYSCAP_LABEL);

    println!("===READY===");
    let _ = io::stdout().flush();

    // **A machine with no host on the other end runs the jobs its manifest
    // names**, one binary name per argument through the stdin path's own [`run_one`].
    let mut jobs: Vec<String> = std::env::args().skip(1).collect();
    // The one argument that is not a job: what the whole list gets.
    let mut bound_ms = toyos_tco::JOB_BOUND_MS;
    if let Some(asked) = jobs.first().and_then(|a| a.strip_prefix("--bound-ms=")) {
        let Ok(ms) = asked.parse() else { fatal(&format!("--bound-ms={asked}: not a number")) };
        bound_ms = ms;
        jobs.remove(0);
    }
    if !jobs.is_empty() {
        deadline(bound_ms, cap.as_ref());
        for job in &jobs {
            // Fatal by name: nobody is reading this console, so a job that did not run must end the boot.
            if job.split_whitespace().count() != 1 {
                give_the_machine_back(&format!("job {job:?} is not one binary name"), cap.as_ref());
            }
            *RUNNING.lock().expect("the deadline thread does not panic holding this") =
                job.clone();
            match run_one(job, &[], cap.as_ref()) {
                Ran::No => {
                    give_the_machine_back(&format!("job {job:?} did not run"), cap.as_ref())
                }
                // **A builtin's exit code reaches no kernel record.** A spawned
                // job's does — `process::exit_process` logs one — so a host
                // reading a stick can judge it; a builtin runs inside this
                // process and its code is console text, which on a machine with
                // no serial port is nothing at all. So a failing builtin ends
                // the boot: the missing `Rebooting.` is the only channel it has.
                Ran::Builtin(code) if code != 0 => give_the_machine_back(
                    &format!("the builtin {job:?} exited {code}"),
                    cap.as_ref(),
                ),
                Ran::Builtin(_) | Ran::Spawned => {}
            }
        }
        FINISHED.store(true, Ordering::Release);
        std::process::exit(0);
    }

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        command(&line, cap.as_ref());
    }
}

/// Watch the job list for `bound_ms` measured from boot, which is the only
/// bound over a kernel that is alive while a job never finishes.
///
/// **The line below is console output and reaches no log record**: a userland
/// `println!` is a write to a `ConsoleObject` and `logd` never sees it, so on a
/// machine with no serial port the evidence that this fired is the kernel's own
/// reboot line and the boot's elapsed time, not this.
fn deadline(bound_ms: u64, cap: Option<&SysCap>) {
    let Some(power) = cap.and_then(|cap| cap.duplicate().ok()) else {
        // A boot list with no way back to the firmware would sit here forever
        // whatever this thread did, so it is refused where it is asked for.
        fatal("no capability to reboot with, so the job list has no deadline");
    };
    std::thread::spawn(move || {
        // A deadline that fired before the loop named a job would say the bound
        // and nothing about what was inside it, so it waits for the name.
        let job = loop {
            if FINISHED.load(Ordering::Acquire) {
                return;
            }
            let since_boot = toyos_abi::syscall::clock_nanos() / 1_000_000;
            let named = RUNNING.lock().map(|job| job.clone()).unwrap_or_default();
            match bound_ms.checked_sub(since_boot) {
                Some(left) => std::thread::sleep(Duration::from_millis(left.max(1))),
                None if !named.is_empty() => break named,
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        // Spelled here and read from `toyos_build::bootlog::JOB_DEADLINE_SAID`,
        // the way the kernel spells `Rebooting.` for the same reader.
        println!(
            "test-runner: the job list ran past its bound, and the job it was inside is {job} \
             ({bound_ms} ms)"
        );
        let _ = io::stdout().flush();
        // **The job goes before the reboot.** `SYS_REBOOT` syncs the machine
        // and then resets it, and a job still running is a process that can
        // start a device transfer after that sync. Killing it and waiting for
        // it to be gone leaves the driver to finish or abandon what it had in
        // flight; the kernel's own stop then covers what the kill left.
        if let Some(job) = claim_the_job() {
            let _ = job.kill();
            let _ = job.wait();
        }
        fatal(&format!("the reboot was refused: {:?}", power.reboot()));
    });
}

/// End a job list that cannot go on, **by returning the machine to firmware**.
///
/// Exiting is not enough and that was a real defect: a boot whose list stopped
/// early never reached its `reboot` job, and nothing else ended it — the
/// deadline thread dies with this process, so the machine sat idle. Measured on
/// a metal-shaped guest: `test_rs_std_tls` failed to spawn at 17 s and the guest
/// was still up, saying nothing, at 291 s. On a machine with no console that is
/// the loop waiting out `return_secs` and then refusing, with no line anywhere
/// saying which job it was.
///
/// A reset the firmware sees is the one channel a job list has left. What went
/// wrong is on the console for a QEMU run, and on the stick it is the *absence*
/// of the boot's own last records — which is why the boot ends here rather than
/// hanging: an absence at a known point is a verdict, and a hang is not.
fn give_the_machine_back(why: &str, cap: Option<&SysCap>) -> ! {
    println!("test-runner: {why}");
    let _ = io::stdout().flush();
    if let Some(power) = cap.and_then(|cap| cap.duplicate().ok()) {
        let refused = power.reboot();
        println!("test-runner: the reboot was refused: {refused:?}");
        let _ = io::stdout().flush();
    }
    std::process::exit(1);
}

/// Say why, on a console that may have nobody on it, and end this process —
/// which on the job path ends the boot, because the runner is what init waits
/// on.
fn fatal(why: &str) -> ! {
    println!("test-runner: {why}");
    let _ = io::stdout().flush();
    std::process::exit(1);
}

/// One command line off stdin. The only parser in this program.
fn command(line: &str, cap: Option<&SysCap>) {
    let cmd = line.trim();
    if cmd.is_empty() {
        return;
    }

    if cmd == "quit" {
        std::process::exit(0);
    }

    let Some(name) = cmd.strip_prefix("run ") else {
        eprintln!("unknown command: {cmd}");
        return;
    };
    // `run <name> [args...]`: the markers still carry only the binary
    // name, so the host protocol is unchanged for the argument-less case.
    let mut words = name.split_whitespace();
    let Some(name) = words.next() else { return };
    let args: Vec<&str> = words.collect();
    run_one(name, &args, cap);
}

/// What became of one job. The two ways it can have run are told apart because
/// only one of them leaves the kernel a record: a spawned binary's exit is
/// `exit: <name> pid=… code=…`, and a builtin's is a line on this console.
enum Ran {
    /// It never started.
    No,
    Spawned,
    Builtin(i32),
}

/// Run `/system/bin/<name>` or that name's builtin, between the host's markers.
fn run_one(name: &str, args: &[&str], cap: Option<&SysCap>) -> Ran {
    let path = format!("/system/bin/{name}");

    println!("===TEST_START {name}===");
    let _ = io::stdout().flush();

    if let Some((_, builtin)) = BUILTINS.iter().find(|(n, _)| *n == name) {
        let code = builtin(cap);
        println!("===TEST_END {name} exit={code}===");
        let _ = io::stdout().flush();
        return Ran::Builtin(code);
    }

    // Piped stdin so the child does not consume the serial commands.
    let mut command = Command::new(&path);
    command.args(args).stdin(Stdio::piped());
    // **A refused dup is an answer and not a failure — but only one
    // refusal is.** `duplicate` needs `DUP` on the capability, which a
    // manifest grants by name, so `PermissionDenied` says this cap is one
    // the program holds *for itself* and the child gets the namespace and
    // no capability at all. `logread` is exactly such a cap, as `realtime`
    // is: the estate does not hand either down. The `expect` here
    // assumed every cap was dup-able and took the whole boot down on the
    // first config that endowed one without `dup`.
    //
    // **Every other refusal stays loud**, and `.ok()` swallowed them with
    // the intended one: a table that cannot hold another handle is a test
    // estate that has leaked, and a child silently started without the
    // capability its test needs reds somewhere else entirely, on a
    // assertion about the log rather than about the handle.
    match cap.map(SysCap::duplicate) {
        Some(Ok(dup)) => {
            command.endow(SYSCAP_LABEL, dup.into_raw().0);
        }
        Some(Err(toyos_abi::syscall::SyscallError::PermissionDenied)) | None => {}
        Some(Err(e)) => {
            println!("===TEST_END {name} error=the capability would not duplicate: {e:?}===");
            let _ = io::stdout().flush();
            return Ran::No;
        }
    }
    let ran = match command.spawn() {
        Ok(mut child) => {
            drop(child.stdin.take());
            // Published before the wait below blocks this thread: from here the
            // deadline can end this job rather than resetting around it.
            if let Ok(watch) = toyos_abi::syscall::dup(toyos_abi::RawHandle(child.as_raw_handle()))
            {
                JOB.store(watch.0, Ordering::Release);
            }
            let outcome = child.wait();
            drop(claim_the_job());
            match outcome {
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    println!("===TEST_END {name} exit={code}===");
                    Ran::Spawned
                }
                Err(e) => {
                    println!("===TEST_END {name} error={e}===");
                    Ran::No
                }
            }
        }
        Err(e) => {
            println!("===TEST_END {name} error={e}===");
            Ran::No
        }
    };
    let _ = io::stdout().flush();
    ran
}
