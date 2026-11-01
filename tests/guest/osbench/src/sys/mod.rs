//! What std does not offer portably, as each OS offers it: the cheapest
//! syscall, a futex, a clock every process shares, anonymous mappings, IPC
//! between processes, a process's own accounting, the machine's
//! conditions and its power counters (`power`), and the CPU a thread runs on
//! and is pinned to. Both modules export the same names; Linux adds the
//! readers of its cgroup and autogroup, and ToyOS the diagnostics the tail
//! group reads (`diag`).

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(target_os = "toyos")]
#[path = "toyos.rs"]
mod imp;

pub use imp::*;

static ALLOWED: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();

/// Read the CPUs this process may run on, before any thread is pinned.
pub fn start() {
    ALLOWED.set(thread_cpus()).expect("sys::start runs once");
}

/// The CPUs this process may run on, as its main thread began with them. A
/// mask is per thread on both systems, so a placed thread's own names only its
/// CPU, and a placement made from one must not read it.
pub fn allowed_cpus() -> Vec<usize> {
    ALLOWED.get().expect("sys::start ran first").clone()
}

/// What a process can read of its own accounting: Linux its context
/// switches and page faults; ToyOS nothing.
pub struct Counts {
    pub switches: Option<u64>,
    pub faults: Option<u64>,
}

impl Counts {
    /// The accounting from `before` to now, per one of `ops` operations,
    /// in hundredths, as result fields.
    pub fn per_op(before: &Counts, ops: u64) -> Vec<(&'static str, String)> {
        let now = counts();
        let ops = ops.max(1);
        let hundredths = |d: u64| format!("{}.{:02}", d / ops, d * 100 / ops % 100);
        let mut fields = Vec::new();
        if let (Some(a), Some(b)) = (before.switches, now.switches) {
            fields.push(("switches_per_op", hundredths(b - a)));
        }
        if let (Some(a), Some(b)) = (before.faults, now.faults) {
            fields.push(("faults_per_op", hundredths(b - a)));
        }
        fields
    }
}

/// Why a call did not do its work: a facility the system does not offer, by
/// the one answer that says so; one it offers that this run may not use, such
/// as what only root reads; or any other failure.
#[derive(Clone)]
pub enum Refused {
    Absent(String),
    Withheld(String),
    Failed(String),
}

impl Refused {
    /// Report `bench` as unsupported for an absence, unmeasured for what this
    /// run was not granted, and refused for anything else.
    pub fn report(self, bench: &str) {
        match self {
            Refused::Absent(why) => crate::report::unsupported(bench, why),
            Refused::Withheld(why) => crate::report::unmeasured(bench, &why, &[]),
            Refused::Failed(why) => crate::report::refuse(bench, why),
        }
    }

    /// What it says, whichever it is, for a call that already succeeded once
    /// and so is failing now.
    pub fn why(self) -> String {
        match self {
            Refused::Absent(why) | Refused::Withheld(why) | Refused::Failed(why) => why,
        }
    }
}

/// The package's power counters as its registers hold them, read on one CPU:
/// that CPU's TSC beside them, the energy counter and its unit register
/// (`osbench_rules::energy`), and its C2, C3, C6 and C7 residency where the
/// CPU documents them, each `None` where it does not.
pub struct Package {
    pub tsc: u64,
    pub energy: Option<u64>,
    pub power_unit: Option<u64>,
    pub residency: [Option<u64>; 4],
}

/// One CPU's power counters as its registers hold them: its TSC beside them,
/// APERF and MPERF, its core's C6 and C7 residency and its core's energy, in
/// the package's unit, each `None` where the CPU does not have it.
pub struct CpuPower {
    pub cpu: u32,
    pub core: u32,
    pub tsc: u64,
    pub aperf: Option<u64>,
    pub mperf: Option<u64>,
    pub core_c6: Option<u64>,
    pub core_c7: Option<u64>,
    pub core_energy: Option<u64>,
}

/// One reading of the power counters, the package's and every CPU's
/// together: on either system it reaches every CPU.
pub struct Round {
    pub package: Package,
    pub cpus: Vec<CpuPower>,
}

/// A whole anonymous read-write mapping, unmapped on drop.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    /// A mapping backed by huge pages wherever the system gives them.
    pub fn huge(len: usize) -> Mapping {
        Mapping { ptr: map_anon_huge(len), len }
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// The mapping as bytes. Anonymous memory reads as zero, so every byte is
    /// initialised.
    #[cfg(target_os = "linux")]
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` is page-aligned, `len` bytes long and owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The mapping as words. Anonymous memory reads as zero, so every word is
    /// initialised.
    pub fn words(&mut self) -> &mut [u64] {
        // SAFETY: `ptr` is page-aligned, `len` bytes long and owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u64>(), self.len / 8) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: nothing borrows the mapping past `self`.
        unsafe { unmap(self.ptr, self.len) }
    }
}

/// The CPU package's temperature, in millidegrees Celsius, where a program
/// can read it.
pub struct Sensor {
    pub read: Box<dyn Fn() -> Option<i64> + Send + Sync>,
    /// The temperature the sensor calls critical, where it reports one.
    pub crit_mc: Option<i64>,
    /// Which sensor this is, for the conditions.
    pub what: String,
}
