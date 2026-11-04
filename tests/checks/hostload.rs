//! The other-work reading, against a child that spins: other work until it is
//! reaped, this run's after.

use std::time::Duration;

use toyos_build::eprintln;

use crate::common::hostload;

/// The CPU time the staged other work spins for.
const SPIN: Duration = Duration::from_secs(1);

pub fn self_check() -> Result<(), String> {
    let start = hostload::mark();
    // SAFETY: the child calls only `clock_gettime` and `_exit`, both
    // async-signal-safe, so a fork of this threaded process is sound.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        loop {
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut now) };
            if now.tv_sec as u64 >= SPIN.as_secs() {
                unsafe { libc::_exit(0) };
            }
        }
    }
    if pid < 0 {
        return Err(format!("fork: {}", std::io::Error::last_os_error()));
    }
    // Exited, and not yet reaped: its time is in no `getrusage` of ours.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out-pointer and `pid` is this process's child.
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) } != 0 {
        return Err(format!("waitid: {}", std::io::Error::last_os_error()));
    }
    let unreaped = start.reading();
    let (_, total, cpus) = start.host_since();
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let mut status = 0;
    // SAFETY: valid out-pointers, and the child is exited.
    if unsafe { libc::wait4(pid, &mut status, 0, &mut ru) } != pid {
        return Err(format!("wait4: {}", std::io::Error::last_os_error()));
    }
    let reaped = start.reading();
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1_000);
    let spun = tv(ru.ru_utime) + tv(ru.ru_stime);

    // The counters' unit: every CPU accrues a tick of some kind per tick of
    // wall clock, whatever the load.
    let per_cpu = total.as_secs_f64() / f64::from(cpus) / unreaped.over.as_secs_f64();
    if !(0.9..=1.1).contains(&per_cpu) {
        return Err(format!(
            "the host's counters summed {total:?} over {cpus} CPU(s) in {:?}, {per_cpu:.2} of \
             wall clock per CPU: they are not in the unit read",
            unreaped.over
        ));
    }
    // A tick per CPU at each end is what the counters can be off by.
    let slack = hostload::ticks(2) * cpus;
    if spun < SPIN / 2 {
        return Err(format!("the child spun {spun:?} of CPU, and it was to spin {SPIN:?}"));
    }
    if unreaped.other + slack < spun {
        return Err(format!(
            "an unreaped child spun {spun:?} and the reading saw {:?} of other work",
            unreaped.other
        ));
    }
    if reaped.other + spun > unreaped.other + slack {
        return Err(format!(
            "reaping a child that spun {spun:?} took the other work from {:?} to {:?}: its time \
             is not counted as this run's",
            unreaped.other, reaped.other
        ));
    }
    eprintln!(
        "  [hostload] a child's {spun:.2?} read as other work until reaped ({:.2?} → {:.2?})",
        unreaped.other, reaped.other
    );
    Ok(())
}
