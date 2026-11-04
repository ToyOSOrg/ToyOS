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
        Reading { other: busy.saturating_sub(own - self.own), over: self.at.elapsed() }
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

/// The host's cumulative CPU counters, summed over its CPUs.
struct Host {
    /// Linux: user nice system idle iowait irq softirq steal. macOS: user
    /// system idle nice, 32-bit and wrapping.
    fields: Vec<u64>,
    cpus: u32,
}

#[cfg(target_os = "linux")]
impl Host {
    /// `/proc/stat`'s aggregate line. Its guest and guest_nice fields are
    /// left out because user and nice already include them.
    fn read() -> Host {
        let stat = std::fs::read_to_string("/proc/stat").expect("/proc/stat is readable");
        let line = stat.lines().find(|l| l.starts_with("cpu ")).expect("/proc/stat has a cpu line");
        let fields: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(|f| f.parse().expect("/proc/stat's cpu fields are numbers"))
            .collect();
        assert_eq!(fields.len(), 8, "/proc/stat's cpu line: {line}");
        let cpus = stat
            .lines()
            .filter(|l| l.strip_prefix("cpu").is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit())))
            .count() as u32;
        Host { fields, cpus }
    }

    /// Busy and total time since `earlier`. Idle and iowait are idle; steal is
    /// busy, because a hypervisor ran something else.
    fn since(&self, earlier: &Host) -> (Duration, Duration) {
        let d: Vec<u64> = self.fields.iter().zip(&earlier.fields).map(|(a, b)| a - b).collect();
        let total: u64 = d.iter().sum();
        (ticks(total - d[3] - d[4]), ticks(total))
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
        // SAFETY: no preconditions.
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as u32;
        Host { fields: info.cpu_ticks.map(u64::from).to_vec(), cpus }
    }

    fn since(&self, earlier: &Host) -> (Duration, Duration) {
        let d: Vec<u64> = self
            .fields
            .iter()
            .zip(&earlier.fields)
            .map(|(&a, &b)| u64::from((a as u32).wrapping_sub(b as u32)))
            .collect();
        let total: u64 = d.iter().sum();
        (ticks(total - d[libc::CPU_STATE_IDLE as usize]), ticks(total))
    }
}
