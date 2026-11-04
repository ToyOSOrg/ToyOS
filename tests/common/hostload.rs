//! Whether the host did work besides this run's.
//!
//! A guest's vCPUs are admitted against the host's CPUs
//! (`buildlock::guest_cpus`); work that is not this run's takes them, and the
//! guest is descheduled at the host's pace. The kernel's own counters give the
//! host's busy CPU time (`/proc/stat`, Mach's `HOST_CPU_LOAD_INFO`), and
//! `getrusage` gives this run's: itself and every child it has reaped. The
//! difference is the other work.
//!
//! Read only once every child is reaped: a live child's time is not yet in
//! `RUSAGE_CHILDREN`, so it reads as other work.

use std::time::{Duration, Instant};

/// Both readings at one moment.
pub struct Mark {
    at: Instant,
    host: Host,
    own: Duration,
}

/// What other work used between a [`Mark`] and the reading.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub other: Duration,
    pub over: Duration,
}

impl Reading {
    /// Other work averaging at most one host CPU. Past that, at least one CPU
    /// the guest budget admitted guests against was not theirs for the whole
    /// run.
    pub fn quiet(&self) -> bool {
        self.other <= self.over
    }

    /// The other work's CPU time, as host CPUs busy on average.
    pub fn cpus(&self) -> f64 {
        self.other.as_secs_f64() / self.over.as_secs_f64().max(f64::MIN_POSITIVE)
    }
}

pub fn mark() -> Mark {
    Mark { at: Instant::now(), host: Host::read(), own: own() }
}

impl Mark {
    pub fn reading(&self) -> Reading {
        let (host, own) = (Host::read(), own());
        let (busy, _) = host.since(&self.host);
        Reading { other: busy.saturating_sub(own.saturating_sub(self.own)), over: self.at.elapsed() }
    }

    /// The host's busy and total CPU time since this mark, and the CPUs they
    /// sum over.
    #[allow(dead_code, reason = "the self-check's")]
    pub(crate) fn host_since(&self) -> (Duration, Duration, u32) {
        let now = Host::read();
        let (busy, total) = now.since(&self.host);
        (busy, total, now.cpus)
    }
}

/// This process's CPU time and every reaped descendant's.
fn own() -> Duration {
    let usage = |who| {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `ru` is a valid out-pointer for the call.
        assert_eq!(unsafe { libc::getrusage(who, &mut ru) }, 0, "getrusage");
        let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1_000);
        tv(ru.ru_utime) + tv(ru.ru_stime)
    };
    usage(libc::RUSAGE_SELF) + usage(libc::RUSAGE_CHILDREN)
}

/// `ticks` of the counters' unit, `USER_HZ` on Linux and the scheduler's tick
/// on macOS.
pub(crate) fn ticks(ticks: u64) -> Duration {
    // SAFETY: no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    assert!(hz > 0, "sysconf(_SC_CLK_TCK) said {hz}");
    Duration::from_nanos(ticks.saturating_mul(1_000_000_000 / hz as u64))
}

/// The host's cumulative CPU counters in ticks, summed over its CPUs.
pub(crate) struct Host {
    busy: u64,
    idle: u64,
    cpus: u32,
}

/// The width the counters wrap at: Linux's are 64-bit, Mach's `natural_t`.
#[cfg(target_os = "linux")]
const WRAP: u64 = u64::MAX;
#[cfg(target_os = "macos")]
const WRAP: u64 = u32::MAX as u64;

impl Host {
    /// Busy and total time since `earlier`.
    pub(crate) fn since(&self, earlier: &Host) -> (Duration, Duration) {
        let busy = self.busy.wrapping_sub(earlier.busy) & WRAP;
        let idle = self.idle.wrapping_sub(earlier.idle) & WRAP;
        (ticks(busy), ticks(busy + idle))
    }
}

/// `/proc/stat`'s aggregate line: user nice system idle iowait irq softirq
/// steal, then guest and guest_nice, which user and nice already include.
/// Steal is busy, because a hypervisor ran something else. Idle and iowait are
/// idle as one sum: the kernel moves time from iowait to idle, so iowait alone
/// can decrease (`Documentation/filesystems/proc.rst`, `/proc/stat`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "the checks read it on every host"))]
pub(crate) fn proc_stat(stat: &str) -> Host {
    let line = stat.lines().find(|l| l.starts_with("cpu ")).expect("/proc/stat has a cpu line");
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|f| f.parse().expect("/proc/stat's cpu fields are numbers"))
        .collect();
    assert_eq!(fields.len(), 8, "/proc/stat's cpu line: {line}");
    let idle = fields[3].wrapping_add(fields[4]);
    let busy = fields.iter().fold(0u64, |sum, f| sum.wrapping_add(*f)).wrapping_sub(idle);
    let cpus = stat
        .lines()
        .filter(|l| l.strip_prefix("cpu").is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit())))
        .count() as u32;
    Host { busy, idle, cpus }
}

#[cfg(target_os = "linux")]
impl Host {
    fn read() -> Host {
        proc_stat(&std::fs::read_to_string("/proc/stat").expect("/proc/stat is readable"))
    }
}

#[cfg(target_os = "macos")]
impl Host {
    fn read() -> Host {
        // libc's own declarations of these are deprecated for the `mach2` crate.
        extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn mach_host_self() -> libc::mach_port_t;
            fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::c_int;
        }
        let mut info = libc::host_cpu_load_info { cpu_ticks: [0; libc::CPU_STATE_MAX as usize] };
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: `info` is the flavour's structure and `count` its size in
        // `integer_t`s; the host port's send right is released after the call.
        let status = unsafe {
            let host = mach_host_self();
            let status = libc::host_statistics(
                host,
                libc::HOST_CPU_LOAD_INFO,
                (&raw mut info).cast(),
                &mut count,
            );
            mach_port_deallocate(mach_task_self_, host);
            status
        };
        assert_eq!(status, 0, "host_statistics(HOST_CPU_LOAD_INFO)");
        let ticks = info.cpu_ticks.map(u64::from);
        let idle = ticks[libc::CPU_STATE_IDLE as usize];
        let busy = ticks.iter().sum::<u64>() - idle;
        // SAFETY: no preconditions.
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as u32;
        Host { busy, idle, cpus }
    }
}
