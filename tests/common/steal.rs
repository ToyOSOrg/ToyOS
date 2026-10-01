//! A guest's own time: the host's wall clock with every stretch the host kept
//! the guest's QEMU waiting for a CPU taken out.
//!
//! A wait on a guest is a hang detector, and a hang is a guest that does not
//! progress while it has the host. A loaded host stretches a guest's work by
//! however long its threads sat runnable and unserved, and a wait on the wall
//! clock reads that stretch as a hang; this clock does not run across it. What
//! the host withheld is steal, as a hypervisor accounts it to a vCPU. A guest
//! that wanted nothing had nothing withheld, so an idle guest's clock is the
//! wall's, and so is a stopped one's: a guest that stopped is called stopped on
//! time however loaded the host is.

use std::ops::Add;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A point on one guest's [`Clock`]; two guests' moments are not comparable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Moment(Duration);

impl Moment {
    pub fn checked_duration_since(self, earlier: Moment) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    pub fn saturating_duration_since(self, earlier: Moment) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

impl Add<Duration> for Moment {
    type Output = Moment;

    fn add(self, span: Duration) -> Moment {
        Moment(self.0 + span)
    }
}

/// The share of `wall` a guest had, given the time its threads spent on a CPU
/// (`ran`) and runnable waiting for one (`waited`) across it.
///
/// Demand under one thread's worth lost the moments it waited and nothing
/// else; demand over it had the share of itself the host served.
pub fn served(wall: Duration, ran: Duration, waited: Duration) -> Duration {
    let wanted = ran + waited;
    if wanted <= wall {
        return wall.saturating_sub(waited);
    }
    let share = wall.as_nanos() * ran.as_nanos() / wanted.as_nanos();
    Duration::from_nanos(u64::try_from(share).expect("a share of a wall span fits the span"))
}

/// One guest's clock, reading its QEMU process; clones read the same clock.
#[derive(Clone)]
pub struct Clock(Arc<Mutex<Reading>>);

struct Reading {
    pid: u32,
    wall: Instant,
    ran: Duration,
    waited: Duration,
    had: Duration,
}

/// The least wall time between two readings of a process's accounting: a
/// console flood reads the clock once a line.
const RESAMPLE: Duration = Duration::from_millis(5);

impl Clock {
    /// The clock of process `pid`, at zero now.
    pub fn of(pid: u32) -> Self {
        let (ran, waited) = demand(pid).unwrap_or_default();
        let reading = Reading { pid, wall: Instant::now(), ran, waited, had: Duration::ZERO };
        Self(Arc::new(Mutex::new(reading)))
    }

    pub fn now(&self) -> Moment {
        let mut reading = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let wall = Instant::now();
        let span = wall.duration_since(reading.wall);
        if span < RESAMPLE {
            return Moment(reading.had);
        }
        // A process that is gone wants nothing, so the span was all its own.
        let had = match demand(reading.pid) {
            Some((ran, waited)) => {
                let had =
                    served(span, ran.saturating_sub(reading.ran), waited.saturating_sub(reading.waited));
                (reading.ran, reading.waited) = (ran, waited);
                had
            }
            None => span,
        };
        reading.wall = wall;
        reading.had += had;
        Moment(reading.had)
    }

    /// Time this guest has had since `earlier`.
    pub fn since(&self, earlier: Moment) -> Duration {
        self.now().saturating_duration_since(earlier)
    }
}

/// What process `pid`'s threads have spent on a CPU and runnable waiting for
/// one, summed over its life; `None` once the process is gone.
#[cfg(target_os = "macos")]
pub fn demand(pid: u32) -> Option<(Duration, Duration)> {
    // SAFETY: integers and a byte array, for which all zeroes is a value.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is the `rusage_info_v4` that `RUSAGE_INFO_V4` names, and
    // the call writes that struct and nothing past it.
    let answered = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast(),
        )
    };
    if answered != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return None;
        }
        panic!("proc_pid_rusage({pid}): {error}");
    }
    let ticks = |count: u64| Duration::from_nanos(mach_nanos(count));
    Some((ticks(info.ri_user_time + info.ri_system_time), ticks(info.ri_runnable_time)))
}

/// Mach absolute time units, which `proc_pid_rusage` counts in, as nanoseconds.
///
/// `libc` deprecates its binding of the timebase for the `mach2` crate's
/// binding of the same call, which this tree does not take.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn mach_nanos(count: u64) -> u64 {
    static TIMEBASE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut base = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: one out-parameter the call fills.
        let answered = unsafe { libc::mach_timebase_info(&mut base) };
        assert_eq!(answered, 0, "mach_timebase_info answered {answered}");
        (u64::from(base.numer), u64::from(base.denom))
    });
    u64::try_from(u128::from(count) * u128::from(numer) / u128::from(denom))
        .expect("a process's CPU time in nanoseconds fits a u64")
}

/// The same, summed over `/proc/<pid>/task/*/schedstat`: nanoseconds on a CPU,
/// then nanoseconds waiting on a run queue. A thread that has exited takes its
/// share with it, which [`Clock::now`] reads as no demand.
#[cfg(target_os = "linux")]
pub fn demand(pid: u32) -> Option<(Duration, Duration)> {
    use std::io::ErrorKind::NotFound;
    let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(e) if e.kind() == NotFound => return None,
        Err(e) => panic!("/proc/{pid}/task: {e}"),
    };
    let (mut ran, mut waited) = (0u64, 0u64);
    for task in tasks {
        let path = task.unwrap_or_else(|e| panic!("/proc/{pid}/task: {e}")).path().join("schedstat");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == NotFound => continue,
            Err(e) => panic!("{}: {e}", path.display()),
        };
        let mut fields = text.split_whitespace().map(|field| {
            field.parse::<u64>().unwrap_or_else(|e| panic!("{}: {field:?}: {e}", path.display()))
        });
        let (Some(on_cpu), Some(queued)) = (fields.next(), fields.next()) else {
            panic!("{}: {text:?} is not `<on cpu> <queued> <slices>`", path.display());
        };
        ran += on_cpu;
        waited += queued;
    }
    Some((Duration::from_nanos(ran), Duration::from_nanos(waited)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn demand(_: u32) -> Option<(Duration, Duration)> {
    unimplemented!("this host's scheduler accounting is not read here")
}
