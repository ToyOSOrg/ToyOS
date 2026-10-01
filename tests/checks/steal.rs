use super::*;
use common::steal::{demand, served, Clock};

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

/// The host's accounting read where the clock reads it: this process's own
/// threads, more of them than the host has cores, both run and wait, and the
/// clock that reads them runs slower than the wall while they do.
pub fn demand_self_check() -> Result<(), String> {
    let me = std::process::id();
    let (ran_before, waited_before) = demand(me).ok_or("this process has no accounting")?;
    let clock = Clock::of(me);
    let (wall, moment) = (std::time::Instant::now(), clock.now());
    let spinners = 2 * common::qemu::host_cores();
    let until = std::time::Instant::now() + Duration::from_millis(300);
    let threads: Vec<_> = (0..spinners)
        .map(|_| {
            thread::spawn(move || {
                while std::time::Instant::now() < until {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    // Read while the spinners stand: an exited thread's share leaves Linux's sum.
    while std::time::Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    let (ran, waited) = demand(me).ok_or("this process has no accounting")?;
    let (had, took) = (clock.since(moment), wall.elapsed());
    for spinner in threads {
        spinner.join().map_err(|_| "a spinner panicked")?;
    }
    let (ran, waited) = (ran.saturating_sub(ran_before), waited.saturating_sub(waited_before));
    if ran.is_zero() || waited.is_zero() {
        return Err(format!("{spinners} spinners on {} cores ran {ran:?} and waited {waited:?}", spinners / 2));
    }
    if had >= took {
        return Err(format!("the clock gave {had:?} of {took:?} to a process whose threads waited {waited:?}"));
    }
    eprintln!("  [steal] {spinners} spinners ran {ran:?} and waited {waited:?}; the clock gave {had:?} of {took:?}");
    Ok(())
}

/// A process that is gone wants nothing, so its clock is the wall's.
pub fn gone_self_check() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("this test's own binary: {e}"))?;
    let mut child = std::process::Command::new(exe)
        .arg("--list")
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn a child that lists and leaves: {e}"))?;
    let pid = child.id();
    child.wait().map_err(|e| format!("reap it: {e}"))?;
    match demand(pid) {
        None => Ok(()),
        Some(said) => Err(format!("pid {pid} is reaped and its accounting still answered {said:?}")),
    }
}
