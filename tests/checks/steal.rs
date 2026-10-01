use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::Instant;

use super::*;
use common::qemu::Liveness;
use common::steal::{demand, served, spent, Clock};

/// What [`one_thread_spins`] says once its spinner runs.
pub const SPINNING: &str = "steal: one thread spins until stdin closes";

/// The span each reading below is taken across.
const WINDOW: Duration = Duration::from_millis(400);

/// [`served`] against the cases it is the derivation of, staged with no guest.
pub fn served_self_check() -> Result<(), String> {
    let ms = Duration::from_millis;
    let second = ms(1000);
    let cases = [
        ("a guest that wanted nothing had the whole span", ms(0), ms(0), second),
        ("light demand lost the moments it waited", ms(100), ms(200), ms(800)),
        ("one thread busy throughout had what it ran", ms(150), ms(850), ms(150)),
        ("four busy threads served a quarter had a quarter", ms(1000), ms(3000), ms(250)),
        ("a guest the host never ran had nothing", ms(0), ms(2000), ms(0)),
    ];
    for (what, ran, waited, want) in cases {
        let got = served(second, ran, waited);
        if got != want {
            return Err(format!("{what}: ran {ran:?} and waited {waited:?} of a second gave {got:?}, not {want:?}"));
        }
    }
    Ok(())
}

/// The host's accounting, held to the identity it has to satisfy whatever the
/// host's load: a thread runnable throughout a span either ran or waited for
/// all of it. Read from outside, on a child whose one runnable thread is a
/// spinner, since this process's other tests run beside it; and once the child
/// is reaped it has no accounting at all.
pub fn reading_self_check() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("this test's own binary: {e}"))?;
    let mut child = Command::new(exe)
        .args(["--ignored", "--exact", "checks::one_thread_spins", "--nocapture", "--test-threads=1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn a child that spins: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped");
    let (said, spinning) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains(SPINNING) {
                let _ = said.send(());
            }
        }
    });
    let heard = spinning.recv_timeout(Duration::from_secs(30));
    let measured = heard.map_err(|e| format!("pid {pid} never said {SPINNING:?}: {e}")).and_then(|()| {
        let before = demand(pid).ok_or(format!("pid {pid} has no accounting while it spins"))?;
        let wall = Instant::now();
        thread::sleep(WINDOW);
        let after = demand(pid).ok_or(format!("pid {pid} has no accounting while it spins"))?;
        Ok((spent(&before, &after), wall.elapsed()))
    });
    drop(child.stdin.take());
    let status = child.wait().map_err(|e| format!("reap pid {pid}: {e}"))?;
    reader.join().map_err(|_| "the child's reader panicked")?;
    let ((ran, waited), took) = measured?;
    if !status.success() {
        return Err(format!("the spinning child ended {status}"));
    }
    let slack = took / 4 + Duration::from_millis(20);
    if ran + waited + slack < took || ran + waited > took + slack {
        return Err(format!(
            "one thread runnable for {took:?} ran {ran:?} and waited {waited:?}: the two are its \
             whole span, give or take {slack:?}"
        ));
    }
    if let Some(said) = demand(pid) {
        return Err(format!("pid {pid} is reaped and its accounting still answered {said:?}"));
    }
    eprintln!("  [steal] one thread runnable for {took:?} ran {ran:?} and waited {waited:?}");
    Ok(())
}

/// [`reading_self_check`]'s child: one thread spins until stdin closes.
pub fn one_thread_spins() {
    let stop = Arc::new(AtomicBool::new(false));
    let spinner = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        })
    };
    println!("{SPINNING}");
    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("read stdin to its end");
    stop.store(true, Ordering::Relaxed);
    spinner.join().expect("the spinner panicked");
}

/// A wait's decision on the guest's clock, against the two guests it has to
/// tell apart: one the host starves keeps its wait past the wall's span of
/// silence, and one that is gone is called stopped at that span.
pub fn wait_self_check() -> Result<(), String> {
    const QUIET: Duration = Duration::from_millis(250);
    let total = Duration::from_secs(60);

    // Twice as many spinners as cores: each is served half the time at most,
    // and a host busy with anything else serves them less.
    let cores = thread::available_parallelism().map_err(|e| format!("this host's cores: {e}"))?;
    let spinners = 2 * cores.get();
    let (stop, started) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(spinners + 1)));
    let threads: Vec<_> = (0..spinners)
        .map(|_| {
            let (stop, started) = (Arc::clone(&stop), Arc::clone(&started));
            thread::spawn(move || {
                started.wait();
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    started.wait();
    let clock = Clock::of(std::process::id());
    let mut starved = Liveness::new(clock.clone(), QUIET, total);
    let (wall, moment) = (Instant::now(), clock.now());
    thread::sleep(QUIET + QUIET / 2);
    let (working, had, took) = (starved.working(""), clock.since(moment), wall.elapsed());
    stop.store(true, Ordering::Relaxed);
    for spinner in threads {
        spinner.join().map_err(|_| "a spinner panicked")?;
    }
    if !working {
        return Err(format!(
            "{spinners} spinners on {cores} cores had {had:?} of {took:?}, and the wait called \
             them stopped after {QUIET:?} of silence"
        ));
    }

    let exe = std::env::current_exe().map_err(|e| format!("this test's own binary: {e}"))?;
    let mut gone = Command::new(exe)
        .arg("--list")
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn a child that lists and leaves: {e}"))?;
    let pid = gone.id();
    gone.wait().map_err(|e| format!("reap pid {pid}: {e}"))?;
    let mut stopped = Liveness::new(Clock::of(pid), QUIET, total);
    let wall = Instant::now();
    thread::sleep(QUIET + QUIET / 2);
    if stopped.working("") {
        return Err(format!("a guest that is gone was still working after {:?} of silence", wall.elapsed()));
    }
    eprintln!("  [steal] {spinners} spinners had {had:?} of {took:?} and their wait went on; a gone guest's ended");
    Ok(())
}
