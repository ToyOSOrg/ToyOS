//! A guest's own time: the host's wall clock with every stretch the host kept
//! the guest's QEMU waiting for a CPU taken out.
//!
//! A wait on a guest is a hang detector, and a hang is a guest that does not
//! progress while it has the host. A loaded host stretches a guest's work by
//! however long its threads sat runnable and unserved; this clock does not run
//! across that stretch. A guest that wanted nothing had nothing withheld, so an
//! idle guest's clock and a stopped one's are the wall's, and a guest that
//! stopped is called stopped on time however loaded the host is.

use std::collections::BTreeMap;
use std::ops::Sub;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A point on one guest's [`Clock`]; two guests' moments are not comparable.
#[derive(Clone, Copy)]
pub struct Moment(Duration);

impl Sub for Moment {
    type Output = Duration;

    fn sub(self, earlier: Moment) -> Duration {
        self.0.saturating_sub(earlier.0)
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

/// Each of a process's threads' time on a CPU and runnable waiting for one,
/// over its life, by an id that names the same thread in every reading.
pub type Threads = BTreeMap<u64, (Duration, Duration)>;

/// What `threads` ran and waited since `before`. A thread `before` lacks was
/// born inside the span; one `threads` lacks exited inside it, and only its
/// last span is lost.
pub fn spent(before: &Threads, threads: &Threads) -> (Duration, Duration) {
    threads.iter().fold((Duration::ZERO, Duration::ZERO), |(ran, waited), (id, &(r, w))| {
        let (r0, w0) = before.get(id).copied().unwrap_or_default();
        (ran + r.saturating_sub(r0), waited + w.saturating_sub(w0))
    })
}

/// One guest's clock, reading its QEMU process; clones read the same clock.
#[derive(Clone)]
pub struct Clock(Arc<Mutex<Reading>>);

struct Reading {
    pid: u32,
    wall: Instant,
    threads: Threads,
    had: Duration,
}

/// The least wall time between two readings of a process's accounting: a
/// console flood reads the clock once a line.
const RESAMPLE: Duration = Duration::from_millis(5);

/// Every guest's read spans this run, in nanoseconds: the wall clock, and what
/// of it the guest had.
static WALL_NS: AtomicU64 = AtomicU64::new(0);
static HAD_NS: AtomicU64 = AtomicU64::new(0);

/// The wall clock this run's guests' waits read across, and what of it they
/// had: the host's load where it fell. `None` before any guest's clock has
/// been read.
pub fn run_share() -> Option<(Duration, Duration)> {
    let wall = WALL_NS.load(Ordering::Relaxed);
    (wall != 0).then(|| (Duration::from_nanos(wall), Duration::from_nanos(HAD_NS.load(Ordering::Relaxed))))
}

impl Clock {
    /// The clock of process `pid`, at zero now.
    pub fn of(pid: u32) -> Self {
        let reading = Reading { pid, wall: Instant::now(), threads: demand(pid).unwrap_or_default(), had: Duration::ZERO };
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
            Some(threads) => {
                let (ran, waited) = spent(&reading.threads, &threads);
                reading.threads = threads;
                served(span, ran, waited)
            }
            None => span,
        };
        reading.wall = wall;
        reading.had += had;
        WALL_NS.fetch_add(u64::try_from(span.as_nanos()).expect("a span fits u64 nanoseconds"), Ordering::Relaxed);
        HAD_NS.fetch_add(u64::try_from(had.as_nanos()).expect("a span fits u64 nanoseconds"), Ordering::Relaxed);
        Moment(reading.had)
    }

    /// Time this guest has had since `earlier`.
    pub fn since(&self, earlier: Moment) -> Duration {
        self.now() - earlier
    }
}

/// Process `pid`'s threads ([`Threads`]); `None` once the process is gone.
///
/// The task's totals, under one id: they keep what its exited threads ran.
/// `ri_runnable_time` counts a thread running as well as waiting to, so the
/// wait is what it holds past the run.
#[cfg(target_os = "macos")]
pub fn demand(pid: u32) -> Option<Threads> {
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
    let ran = info.ri_user_time + info.ri_system_time;
    let ticks = |count: u64| Duration::from_nanos(mach_nanos(count));
    Some(Threads::from([(0, (ticks(ran), ticks(info.ri_runnable_time.saturating_sub(ran))))]))
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

/// Each `/proc/<pid>/task/<tid>/schedstat`, by tid: nanoseconds on a CPU, then
/// nanoseconds waiting on a run queue.
#[cfg(target_os = "linux")]
pub fn demand(pid: u32) -> Option<Threads> {
    use std::io::ErrorKind::NotFound;
    // Without it every thread's file is absent and a starved guest reads as one
    // that wanted nothing.
    assert!(
        std::path::Path::new("/proc/self/schedstat").is_file(),
        "this kernel keeps no per-task schedstat (CONFIG_SCHED_INFO), which a guest's clock reads"
    );
    let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(e) if e.kind() == NotFound => return None,
        Err(e) => panic!("/proc/{pid}/task: {e}"),
    };
    let mut threads = Threads::new();
    for task in tasks {
        let task = task.unwrap_or_else(|e| panic!("/proc/{pid}/task: {e}"));
        let tid = task.file_name().to_str().and_then(|t| t.parse::<u64>().ok());
        let tid = tid.unwrap_or_else(|| panic!("/proc/{pid}/task holds {:?}, which is no tid", task.file_name()));
        let path = task.path().join("schedstat");
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
        threads.insert(tid, (Duration::from_nanos(on_cpu), Duration::from_nanos(queued)));
    }
    Some(threads)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn demand(_: u32) -> Option<Threads> {
    unimplemented!("this host's scheduler accounting is not read here")
}
