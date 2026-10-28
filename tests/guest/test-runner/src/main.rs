use std::io::{self, BufRead, Write};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use toyos::endow::{Endowments, FromHandle, SYSCAP_LABEL};
use toyos::power::{Mode, Pending, Refused, Stop, Update};
use toyos::process::Process;
use toyos::syscap::SysCap;
use toyos_session::LOGIN_PORT;

/// The job the runner is inside, and whether the list got through: written by
/// the loop, read by the deadline watching it.
static RUNNING: Mutex<String> = Mutex::new(String::new());
static FINISHED: AtomicBool = AtomicBool::new(false);

/// A second handle to the job now running, for the deadline to end it with, or
/// none between jobs.
///
/// **Taken out by whichever of the two gets there first**, so the loop closing
/// it cannot leave the deadline killing whatever the slot was handed to next. A
/// duplicate and not the `Child`'s own handle, because a `Child` has one owner
/// and the loop is blocked inside its `wait`.
static JOB: Mutex<Option<Process>> = Mutex::new(None);

/// What this row is endowed as a login authority, where it is one: its `login`
/// connector, which each job is handed a copy of, and the launcher of the
/// session it opens for its jobs, which a login authority's launches are in.
struct Login {
    connector: toyos::namespace::Namespace,
    session: toyos::namespace::Namespace,
    _control: toyos::ipc::Connection,
}

static LOGIN: std::sync::OnceLock<Option<Login>> = std::sync::OnceLock::new();

fn login() -> Option<&'static Login> {
    LOGIN
        .get_or_init(|| {
            let connector = Endowments::get().take::<toyos::namespace::Namespace>(LOGIN_PORT)?;
            match toyos_session::open(&connector) {
                Ok((session, control)) => Some(Login { connector, session, _control: control }),
                Err(why) => panic!("test-runner: a login authority with no session for its jobs: {why}"),
            }
        })
        .as_ref()
}

/// Set by the deadline before it kills the job it was watching, so the loop
/// that kill releases does not start another one.
///
/// **A kill releases the `wait` the loop is inside.** That is what the kill is
/// for — the driver gets to finish or abandon the transfer it had in flight
/// before the reset — but the loop then runs on, and the next job it spawns
/// lands under a shutdown that has already written the boot's last word. A metal
/// boot's verdict is that word being the log's last line, so that spawn costs
/// the boot its verdict.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Nothing this process does can reach a log whose last line is already
/// written, so the loop stops where it is.
///
/// **Parks rather than exits**: the runner is what the supervisor waits on, so an exit
/// here would end the boot underneath the shutdown that is running on the other
/// thread. A reboot that is refused ends this process from that thread instead.
fn stand_down() -> ! {
    loop {
        std::thread::park();
    }
}

/// Take the running job, if it is still this thread's to take.
fn claim_the_job() -> Option<Process> {
    JOB.lock().unwrap_or_else(PoisonError::into_inner).take()
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
    // names**, one job per argument — a binary name and its arguments —
    // through the stdin path's own [`run_one`].
    let mut jobs: Vec<String> = std::env::args().skip(1).collect();
    // The one argument that is not a job: what the whole list gets.
    let mut bound_ms = toyos_tco::JOB_BOUND_MS;
    if let Some(asked) = jobs.first().and_then(|a| a.strip_prefix("--bound-ms=")) {
        let Ok(ms) = asked.parse() else { fatal(&format!("--bound-ms={asked}: not a number")) };
        bound_ms = ms;
        jobs.remove(0);
    }
    if !jobs.is_empty() {
        deadline(bound_ms);
        for job in &jobs {
            // A binary name and its arguments. Fatal by name: nobody is
            // reading this console, so a job that did not run must end the boot.
            let mut words = job.split_whitespace();
            let Some(name) = words.next() else { give_the_machine_back("an empty job") };
            let args: Vec<&str> = words.collect();
            *RUNNING.lock().expect("the deadline thread does not panic holding this") =
                job.clone();
            match run_one(name, &args, cap.as_ref()) {
                Ran::No => {
                    give_the_machine_back(&format!("job {job:?} did not run"))
                }
                Ran::Spawned => {}
            }
            if STOPPING.load(Ordering::Acquire) {
                stand_down();
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
/// **The line below is this program's own text**, in the log under its name;
/// the evidence a judge reads that this fired is the kernel's own reboot line
/// and the boot's elapsed time, not this.
fn deadline(bound_ms: u64) {
    // A boot list with no way back to the firmware would sit here forever
    // whatever this thread did, so it is refused where it is asked for.
    if toyos::endow::service(toyos::power::PORT).is_err() {
        fatal("no `power` connector to reboot with, so the job list has no deadline");
    }
    std::thread::spawn(move || {
        // A deadline that fired before the loop named a job would say the bound
        // and nothing about what was inside it, so it waits for the name.
        let job = loop {
            if FINISHED.load(Ordering::Acquire) {
                return;
            }
            let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
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
        // **The job goes before the reboot.** the reboot syncs the machine
        // and then resets it, and a job still running is a process that can
        // start a device transfer after that sync. Killing it and waiting for
        // it to be gone leaves the driver to finish or abandon what it had in
        // flight; the kernel's own stop then covers what the kill left.
        // Before the kill, because the kill is what releases the loop.
        STOPPING.store(true, Ordering::Release);
        if let Some(job) = claim_the_job() {
            let _ = job.kill();
            let _ = job.wait();
        }
        fatal(&format!("the reboot was refused: {:?}", reboot()));
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
fn give_the_machine_back(why: &str) -> ! {
    println!("test-runner: {why}");
    let _ = io::stdout().flush();
    let refused = reboot();
    println!("test-runner: the reboot was refused: {refused:?}");
    let _ = io::stdout().flush();
    std::process::exit(1);
}

/// The machine back to firmware, at once: the harness owns it, so its stop
/// is the explicit decision and waits for no session. Comes back only where
/// the machine did not stop.
fn reboot() -> Result<Update, Refused> {
    toyos::power::stop(Stop::Reboot, Mode::Terminate).map(Pending::outcome)
}

/// Say why, on a console that may have nobody on it, and end this process —
/// which on the job path ends the boot, because the runner is what the supervisor waits
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

enum Ran {
    /// It never started.
    No,
    Spawned,
}

/// Run `/system/bin/<name>` between the host's markers.
fn run_one(name: &str, args: &[&str], cap: Option<&SysCap>) -> Ran {
    let path = format!("/system/bin/{name}");

    println!("===TEST_START {name}===");
    let _ = io::stdout().flush();

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
            command.endow(SYSCAP_LABEL, dup.into());
        }
        Some(Err(toyos_abi::syscall::SyscallError::PermissionDenied)) | None => {}
        Some(Err(e)) => {
            println!("===TEST_END {name} error=the capability would not duplicate: {e:?}===");
            let _ = io::stdout().flush();
            return Ran::No;
        }
    }
    // Its launcher too, where its row starts anything, and its `login`
    // connector where its row is a login authority, whose launcher is the
    // session it opened for its jobs: a job is a helper, and a helper
    // inherits neither.
    let login = login();
    let launcher = login.map(|l| &l.session).or(toyos::endow::launcher());
    for (label, held) in [(toyos::launch::LAUNCHER, launcher), (LOGIN_PORT, login.map(|l| &l.connector))] {
        let Some(held) = held else { continue };
        match toyos::AsHandle::as_handle(held).try_clone_to_owned() {
            Ok(dup) => {
                command.endow(label, dup.into());
            }
            Err(e) => {
                println!("===TEST_END {name} error=its {label} would not duplicate: {e:?}===");
                let _ = io::stdout().flush();
                return Ran::No;
            }
        }
    }
    let ran = match command.spawn() {
        Ok(mut child) => {
            drop(child.stdin.take());
            // Published before the wait below blocks this thread: from here the
            // deadline can end this job rather than resetting around it.
            //
            // **Refused by name rather than left at zero.** A duplicate this
            // does not get leaves the deadline with nothing to kill, so the job
            // outlives its bound and the machine is reset around it — and on a
            // machine with no console nothing anywhere says why. The list ends
            // here instead, by handing the machine back.
            match toyos::AsHandle::as_handle(&child).try_clone_to_owned() {
                Ok(watch) => *JOB.lock().unwrap_or_else(PoisonError::into_inner) = Some(Process::from_handle(watch)),
                Err(e) => give_the_machine_back(&format!(
                    "job {name:?} started and its handle would not duplicate ({e:?}), so the \
                     deadline has nothing to end it with"
                )),
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
