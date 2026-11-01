use std::fs;
use std::sync::atomic::AtomicU32;
use std::sync::OnceLock;
use std::time::Duration;

use crate::report::condition;

pub fn null_syscall() {
    // SAFETY: getpid takes no arguments and cannot fail. Raw, so no libc
    // cache answers in its place.
    unsafe { libc::syscall(libc::SYS_getpid) };
}

/// `CLOCK_MONOTONIC`, which `Instant` reads and every process shares.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-pointer.
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) }, 0);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Block while `word` holds `expected`.
pub fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: `word` is a live, aligned u32; a null timeout waits unbounded.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            std::ptr::null::<libc::timespec>(),
        )
    };
}

/// Wake up to `count` waiters on `word`; answers how many woke.
pub fn futex_wake(word: &AtomicU32, count: u32) -> u32 {
    let count = i32::try_from(count).unwrap_or(i32::MAX);
    // SAFETY: `word` is a live, aligned u32.
    let woken =
        unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, count) };
    u32::try_from(woken).expect("FUTEX_WAKE on a valid word answers a count")
}

pub fn map_anon(len: usize) -> *mut u8 {
    // SAFETY: a fresh anonymous mapping aliases nothing.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap of {len} bytes: {}", std::io::Error::last_os_error());
    ptr.cast()
}

/// # Safety
/// `ptr` and `len` are one whole mapping from [`map_anon`], and nothing
/// reaches it afterwards.
pub unsafe fn unmap(ptr: *mut u8, len: usize) {
    // SAFETY: as the caller promises.
    assert_eq!(unsafe { libc::munmap(ptr.cast(), len) }, 0);
}

/// An anonymous mapping with transparent huge pages asked for;
/// [`huge_backing`] says what the kernel gave.
pub fn map_anon_huge(len: usize) -> *mut u8 {
    let ptr = map_anon(len);
    // SAFETY: an advice changes no contents.
    unsafe { libc::madvise(ptr.cast(), len, libc::MADV_HUGEPAGE) };
    ptr
}

/// This process's anonymous memory in huge pages, from
/// `/proc/self/smaps_rollup`.
pub fn huge_backing() -> String {
    let kib = fs::read_to_string("/proc/self/smaps_rollup").ok().and_then(|rollup| {
        let line = rollup.lines().find(|l| l.starts_with("AnonHugePages:"))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok()
    });
    kib.map_or_else(|| "unknown".into(), |kib| format!("{kib}KiB"))
}

/// The CPU that ran this, by Linux's numbering. Stale as soon as the
/// scheduler moves the thread.
pub fn current_cpu() -> u32 {
    // SAFETY: sched_getcpu takes nothing.
    let cpu = unsafe { libc::sched_getcpu() };
    u32::try_from(cpu).unwrap_or_else(|_| panic!("sched_getcpu: {}", std::io::Error::last_os_error()))
}

/// The core CPU `cpu` belongs to, as sysfs gives it: two CPUs of one core
/// are SMT siblings.
pub fn core_of(cpu: u32) -> u32 {
    static CORES: OnceLock<Vec<u32>> = OnceLock::new();
    let cores = CORES.get_or_init(|| {
        let max = allowed_cpus().into_iter().max().unwrap_or(0);
        (0..=max)
            .map(|cpu| {
                let topology = |file: &str| -> u32 {
                    read(&format!("/sys/devices/system/cpu/cpu{cpu}/topology/{file}"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(u32::MAX)
                };
                topology("physical_package_id") << 16 | topology("core_id")
            })
            .collect()
    });
    cores[cpu as usize]
}

pub fn threads_per_core() -> u32 {
    let cpus = allowed_cpus();
    let first = core_of(cpus[0] as u32);
    cpus.iter().filter(|&&cpu| core_of(cpu as u32) == first).count() as u32
}

pub fn cpu_model() -> String {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name").and_then(|v| v.split_once(':')).map(|(_, v)| v.trim().to_string()));
    model.unwrap_or_else(|| "unknown".into())
}

/// The CPUs this process may run on, by Linux's numbering: its main
/// thread's mask, which nothing here pins.
pub fn allowed_cpus() -> Vec<usize> {
    // SAFETY: a zeroed cpu_set_t is an empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: getpid cannot fail; `set` is a valid out-pointer of the size given.
    let got = unsafe { libc::sched_getaffinity(libc::getpid(), std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    assert_eq!(got, 0, "sched_getaffinity: {}", std::io::Error::last_os_error());
    // SAFETY: CPU_ISSET reads the set within its size.
    (0..libc::CPU_SETSIZE as usize).filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) }).collect()
}

fn set_affinity(tid: libc::pid_t, cpus: &[usize]) {
    // SAFETY: a zeroed cpu_set_t is an empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &cpu in cpus {
        // SAFETY: `cpu` came from `allowed_cpus`, inside the set's size.
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    // SAFETY: `set` is valid for its size.
    let done = unsafe { libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    assert_eq!(done, 0, "sched_setaffinity({tid}, {cpus:?}): {}", std::io::Error::last_os_error());
}

/// A thread of this process, as `sched_setaffinity` names it.
pub type Tid = libc::pid_t;

pub fn tid() -> libc::pid_t {
    // SAFETY: gettid takes no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

/// Keep the calling thread on Linux CPU `cpu`.
pub fn pin_self(cpu: usize) {
    set_affinity(0, &[cpu]);
}

/// Let thread `tid` run anywhere this process may.
pub fn unpin(tid: libc::pid_t) {
    set_affinity(tid, &allowed_cpus());
}

/// The calling thread's timer slack, which pads each of its sleeps by up to
/// this much.
pub fn timer_slack_ns() -> u64 {
    // SAFETY: PR_GET_TIMERSLACK answers the slack as its return value.
    let slack = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
    u64::try_from(slack).expect("PR_GET_TIMERSLACK cannot fail")
}

pub fn set_timer_slack_ns(ns: u64) {
    // SAFETY: PR_SET_TIMERSLACK takes the slack by value.
    let done = unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, ns as libc::c_ulong, 0, 0, 0) };
    assert_eq!(done, 0, "PR_SET_TIMERSLACK: {}", std::io::Error::last_os_error());
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Every distinct value one per-CPU sysfs file holds, with how many CPUs hold
/// it: `powersave*8`.
fn per_cpu(file: &str) -> String {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for cpu in allowed_cpus() {
        let value = read(&format!("/sys/devices/system/cpu/cpu{cpu}/{file}")).unwrap_or_else(|| "absent".into());
        match seen.iter_mut().find(|(v, _)| *v == value) {
            Some((_, n)) => *n += 1,
            None => seen.push((value, 1)),
        }
    }
    seen.iter().map(|(v, n)| format!("{v}*{n}")).collect::<Vec<_>>().join(",")
}

/// The selected word of a sysfs choice file: `always [madvise] never`.
fn selected(path: &str) -> String {
    read(path)
        .map(|s| {
            s.split_whitespace()
                .find(|w| w.starts_with('['))
                .map_or(s.clone(), |w| w.trim_matches(['[', ']']).to_string())
        })
        .unwrap_or_else(|| "absent".into())
}

/// The whole machine's busy fraction over `span`, in permille, off
/// `/proc/stat`'s first line: everything but idle and iowait is busy.
fn busy_permille(span: Duration) -> Option<u64> {
    let sample = || -> Option<(u64, u64)> {
        let stat = fs::read_to_string("/proc/stat").ok()?;
        let fields: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
        let idle = fields.get(3)? + fields.get(4)?;
        Some((fields.iter().sum(), idle))
    };
    let (total0, idle0) = sample()?;
    std::thread::sleep(span);
    let (total1, idle1) = sample()?;
    let total = total1.checked_sub(total0)?;
    (total > 0).then(|| (total - (idle1 - idle0)) * 1000 / total)
}

/// What a Linux number depends on, read and never changed. `README.md` says
/// which of them a comparison sets.
pub fn conditions() {
    condition("kernel", read("/proc/sys/kernel/osrelease").unwrap_or_default());
    let distro = fs::read_to_string("/etc/os-release").ok().and_then(|text| {
        text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_string())
    });
    condition("distro", distro.unwrap_or_else(|| "unknown".into()));
    condition("cpus_online", read("/sys/devices/system/cpu/online").unwrap_or_default());
    condition("smt_active", read("/sys/devices/system/cpu/smt/active").unwrap_or_else(|| "absent".into()));
    condition("smt_control", read("/sys/devices/system/cpu/smt/control").unwrap_or_else(|| "absent".into()));
    condition("cpufreq_driver", per_cpu("cpufreq/scaling_driver"));
    condition("governor", per_cpu("cpufreq/scaling_governor"));
    condition("epp", per_cpu("cpufreq/energy_performance_preference"));
    condition("min_khz", per_cpu("cpufreq/scaling_min_freq"));
    condition("max_khz", per_cpu("cpufreq/scaling_max_freq"));
    condition(
        "intel_no_turbo",
        read("/sys/devices/system/cpu/intel_pstate/no_turbo").unwrap_or_else(|| "absent".into()),
    );
    condition("boost", read("/sys/devices/system/cpu/cpufreq/boost").unwrap_or_else(|| "absent".into()));
    condition("amd_pstate", read("/sys/devices/system/cpu/amd_pstate/status").unwrap_or_else(|| "absent".into()));
    condition("platform_profile", read("/sys/firmware/acpi/platform_profile").unwrap_or_else(|| "absent".into()));
    // What every timestamp here is read from: one the vDSO cannot read costs
    // each `Instant::now` a syscall.
    let clocksource = read("/sys/devices/system/clocksource/clocksource0/current_clocksource");
    condition("clocksource", clocksource.unwrap_or_else(|| "absent".into()));
    for supply in fs::read_dir("/sys/class/power_supply").into_iter().flatten().flatten() {
        let at = supply.path();
        let get = |f: &str| read(&at.join(f).to_string_lossy());
        match get("type").as_deref() {
            Some("Mains") => condition("ac_online", get("online").unwrap_or_default()),
            Some("Battery") => {
                condition("battery_status", get("status").unwrap_or_default());
                condition("battery_capacity", get("capacity").unwrap_or_default());
            }
            _ => {}
        }
    }
    condition("thp", selected("/sys/kernel/mm/transparent_hugepage/enabled"));
    condition("autogroup", read("/proc/sys/kernel/sched_autogroup_enabled").unwrap_or_else(|| "absent".into()));
    condition("timer_slack_ns", read("/proc/self/timerslack_ns").unwrap_or_default());
    for entry in fs::read_dir("/sys/devices/system/cpu/vulnerabilities").into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        condition(&format!("vuln_{name}"), read(&entry.path().to_string_lossy()).unwrap_or_default());
    }
    condition(
        "loadavg",
        read("/proc/loadavg").map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(",")).unwrap_or_default(),
    );
    condition("busy_permille_1s", busy_permille(Duration::from_secs(1)).map_or("unknown".into(), |p| p.to_string()));
}

/// Model-specific register addresses, by [`crate::arch`]'s choice for the
/// CPU's vendor.
pub struct PowerRegisters {
    pub energy: u32,
    pub power_unit: u32,
    pub package_residency: [Option<u32>; 4],
    pub core_residency: [Option<u32>; 2],
    pub core_energy: Option<u32>,
}

/// The power counters through the `msr` driver's `/dev/cpu/<n>/msr`, which
/// takes root: the package's on whichever CPU the caller runs on, and each
/// CPU's on that CPU, an IPI to each but the caller's.
pub mod power {
    use std::fs::File;
    use std::os::unix::fs::FileExt;

    use super::super::{CpuPower, Package, Refused, Round};
    use super::PowerRegisters;

    const IA32_TIME_STAMP_COUNTER: u32 = 0x10;
    const IA32_MPERF: u32 = 0xe7;
    const IA32_APERF: u32 = 0xe8;

    fn registers() -> Result<PowerRegisters, Refused> {
        crate::arch::power_registers().ok_or_else(|| Refused::Absent("no CPU vendor whose power registers osbench reads".into()))
    }

    fn device(cpu: u32) -> Result<File, Refused> {
        let path = format!("/dev/cpu/{cpu}/msr");
        File::open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Refused::Withheld(format!("{path}: {e}: `sudo modprobe msr`")),
            std::io::ErrorKind::PermissionDenied => Refused::Withheld(format!("{path}: {e}: `sudo osbench energy` reads it")),
            _ => Refused::Failed(format!("{path}: {e}")),
        })
    }

    /// The register, or `None` where the CPU lacks it: the driver answers a
    /// `#GP` with `EIO`.
    fn msr(dev: &File, cpu: u32, at: u32) -> Result<Option<u64>, Refused> {
        let mut word = [0u8; 8];
        match dev.read_at(&mut word, u64::from(at)) {
            Ok(8) => Ok(Some(u64::from_le_bytes(word))),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(None),
            other => Err(Refused::Failed(format!("cpu{cpu}'s MSR {at:#x}: {other:?}"))),
        }
    }

    fn each(dev: &File, cpu: u32, at: Option<u32>) -> Result<Option<u64>, Refused> {
        at.map_or(Ok(None), |at| msr(dev, cpu, at))
    }

    /// The package's counters and then every CPU's, as ToyOS's round reads
    /// them: a read of every CPU each time.
    pub fn round() -> Result<Round, Refused> {
        let r = registers()?;
        Ok(Round { package: package(&r)?, cpus: cpus(&r)? })
    }

    fn package(r: &PowerRegisters) -> Result<Package, Refused> {
        let cpu = super::current_cpu();
        let dev = device(cpu)?;
        let tsc = msr(&dev, cpu, IA32_TIME_STAMP_COUNTER)?.ok_or_else(|| Refused::Failed(format!("cpu{cpu} read no TSC")))?;
        let mut residency = [None; 4];
        for (slot, at) in residency.iter_mut().zip(r.package_residency) {
            *slot = each(&dev, cpu, at)?;
        }
        Ok(Package { tsc, energy: msr(&dev, cpu, r.energy)?, power_unit: msr(&dev, cpu, r.power_unit)?, residency })
    }

    fn cpus(r: &PowerRegisters) -> Result<Vec<CpuPower>, Refused> {
        super::allowed_cpus()
            .into_iter()
            .map(|cpu| {
                let cpu = cpu as u32;
                let dev = device(cpu)?;
                Ok(CpuPower {
                    cpu,
                    core: super::core_of(cpu),
                    tsc: msr(&dev, cpu, IA32_TIME_STAMP_COUNTER)?
                        .ok_or_else(|| Refused::Failed(format!("cpu{cpu} read no TSC")))?,
                    aperf: msr(&dev, cpu, IA32_APERF)?,
                    mperf: msr(&dev, cpu, IA32_MPERF)?,
                    core_c6: each(&dev, cpu, r.core_residency[0])?,
                    core_c7: each(&dev, cpu, r.core_residency[1])?,
                    core_energy: each(&dev, cpu, r.core_energy)?,
                })
            })
            .collect()
    }

    /// The CPU time the whole machine has been busy for, in nanoseconds, off
    /// `/proc/stat` at its tick's resolution: everything but idle and iowait,
    /// this sleeping process included.
    pub fn busy_ns() -> Result<u64, Refused> {
        let stat = std::fs::read_to_string("/proc/stat").map_err(|e| Refused::Failed(format!("/proc/stat: {e}")))?;
        let fields: Vec<u64> =
            stat.lines().next().unwrap_or_default().split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
        let (Some(idle), Some(iowait)) = (fields.get(3), fields.get(4)) else {
            return Err(Refused::Failed("/proc/stat's first line has no idle and iowait".into()));
        };
        // SAFETY: sysconf takes a name and reads nothing else.
        let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100).max(1);
        Ok((fields.iter().sum::<u64>() - idle - iowait) * 1_000_000_000 / hz)
    }

    /// The panel and its light, the radios and the links, which the package's
    /// draw depends on: each connected DRM connector's DPMS, each backlight's
    /// level, each rfkill switch, and each network link's state.
    pub fn conditions() {
        use crate::report::condition;
        let listed = |dir: &str, said: &dyn Fn(&std::path::Path) -> Option<String>| -> String {
            let mut seen: Vec<String> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| said(&e.path()).map(|s| format!("{}:{s}", e.file_name().to_string_lossy())))
                .collect();
            seen.sort();
            if seen.is_empty() { "none".into() } else { seen.join(",") }
        };
        let get = |at: &std::path::Path, f: &str| super::read(&at.join(f).to_string_lossy());
        condition(
            "energy.display",
            listed("/sys/class/drm", &|at| (get(at, "status")? == "connected").then(|| get(at, "dpms").unwrap_or_default())),
        );
        condition(
            "energy.backlight",
            listed("/sys/class/backlight", &|at| Some(format!("{}/{}", get(at, "actual_brightness")?, get(at, "max_brightness")?))),
        );
        condition(
            "energy.radios",
            listed("/sys/class/rfkill", &|at| {
                let blocked = get(at, "soft")? == "1" || get(at, "hard")? == "1";
                Some(format!("{}_{}", get(at, "type")?, if blocked { "blocked" } else { "unblocked" }))
            }),
        );
        condition("energy.links", listed("/sys/class/net", &|at| if at.ends_with("lo") { None } else { get(at, "operstate") }));
    }
}

/// The cgroup whose `cpu` controller governs this process: the deepest one
/// on its path whose parent enables `cpu` for its children, or `/` when
/// none does.
pub fn cpu_cgroup() -> String {
    let own = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| text.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string)))
        .unwrap_or_default();
    let mut governing = String::from("/");
    let mut at = String::new();
    for part in own.split('/').filter(|p| !p.is_empty()) {
        let parent = format!("/sys/fs/cgroup{at}/cgroup.subtree_control");
        at.push('/');
        at.push_str(part);
        if read(&parent).is_some_and(|c| c.split_whitespace().any(|w| w == "cpu")) {
            governing.clone_from(&at);
        }
    }
    governing
}

pub fn autogroup_enabled() -> bool {
    read("/proc/sys/kernel/sched_autogroup_enabled").as_deref() == Some("1")
}

/// This process's autogroup, as `/proc/self/autogroup` names it.
pub fn autogroup() -> String {
    read("/proc/self/autogroup").and_then(|s| s.split_whitespace().next().map(str::to_string)).unwrap_or_default()
}

/// This process's own accounting, from `getrusage`: Linux counts no
/// syscalls for a process to read.
pub fn counts() -> super::Counts {
    // SAFETY: a zeroed rusage is a valid out-buffer.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid out-pointer.
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    super::Counts {
        switches: Some((usage.ru_nvcsw + usage.ru_nivcsw) as u64),
        faults: Some((usage.ru_minflt + usage.ru_majflt) as u64),
    }
}

/// `CLOCK_MONOTONIC` is the clock Linux's own timestamps use.
pub fn log_time_ns(monotonic: u64) -> u64 {
    monotonic
}

/// Linux gives a thread the stack its builder asks for.
pub const STACK_FLOOR: &str = "none";

/// What [`current_cpu`] names a CPU by, for the fields that print one.
pub const CPU_NAMING: &str = "cpu";

pub fn protect(ptr: *mut u8, len: usize, writable: bool) -> Result<(), super::Refused> {
    let prot = if writable { libc::PROT_READ | libc::PROT_WRITE } else { libc::PROT_READ };
    // SAFETY: the caller's mapping; a protection change moves no memory.
    match unsafe { libc::mprotect(ptr.cast(), len, prot) } {
        0 => Ok(()),
        _ => Err(super::Refused::Failed(format!("mprotect: {}", std::io::Error::last_os_error()))),
    }
}

/// # Safety
/// `ptr` and `len` lie inside one mapping from [`map_anon`] that nothing
/// reaches in that range afterwards.
pub unsafe fn unmap_part(ptr: *mut u8, len: usize) -> Result<(), super::Refused> {
    // SAFETY: as the caller promises.
    match unsafe { libc::munmap(ptr.cast(), len) } {
        0 => Ok(()),
        _ => Err(super::Refused::Failed(format!("munmap: {}", std::io::Error::last_os_error()))),
    }
}

/// An anonymous mapping that faults in 4 KiB pages: transparent huge pages
/// are refused for it.
pub fn map_anon_4k(len: usize) -> *mut u8 {
    let ptr = map_anon(len);
    // SAFETY: an advice changes no contents.
    unsafe { libc::madvise(ptr.cast(), len, libc::MADV_NOHUGEPAGE) };
    ptr
}

/// IPC between this process and a child: a Unix stream socket pair, the
/// child's end its stdin; a descriptor rides as `SCM_RIGHTS`.
pub mod ipc {
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::process::{Child, Command, Stdio};

    pub struct Peer(UnixStream);

    /// What a message carries beside its bytes: one end of a pipe.
    pub type Token = OwnedFd;

    /// A stream carries a message of any length as one write.
    pub fn frames(_len: usize) -> usize {
        1
    }

    const CONTROL: usize = 64;

    impl Peer {
        pub fn send(&self, data: &[u8]) -> io::Result<()> {
            (&self.0).write_all(data)
        }

        pub fn recv(&self, buf: &mut [u8]) -> io::Result<()> {
            (&self.0).read_exact(buf)
        }

        /// `data` and `token` in one `sendmsg`, then this process's copy
        /// closed, so the descriptor ends up the peer's alone as a ToyOS
        /// handle moved would.
        pub fn send_with(&self, data: &[u8], token: Token) -> io::Result<()> {
            let mut control = [0u8; CONTROL];
            let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() };
            // SAFETY: a zeroed msghdr is an empty one.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            // SAFETY: CMSG_SPACE is arithmetic.
            msg.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<libc::c_int>() as u32) } as _;
            // SAFETY: `msg`'s control buffer holds one header and one int,
            // which CMSG_SPACE just sized.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&msg);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(size_of::<libc::c_int>() as u32) as _;
                libc::CMSG_DATA(header).cast::<libc::c_int>().write_unaligned(token.as_raw_fd());
            }
            // SAFETY: every pointer in `msg` is live for the call.
            let sent = unsafe { libc::sendmsg(self.0.as_raw_fd(), &msg, 0) };
            if sent < 0 {
                return Err(io::Error::last_os_error());
            }
            if sent as usize != data.len() {
                return Err(io::Error::other(format!("sendmsg sent {sent} of {} bytes", data.len())));
            }
            drop(token);
            Ok(())
        }

        pub fn recv_with(&self, buf: &mut [u8]) -> io::Result<Token> {
            let mut control = [0u8; CONTROL];
            let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
            // SAFETY: a zeroed msghdr is an empty one.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = CONTROL as _;
            // SAFETY: every pointer in `msg` is live for the call.
            let got = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_WAITALL) };
            if got < 0 {
                return Err(io::Error::last_os_error());
            }
            if got as usize != buf.len() {
                return Err(io::Error::other(format!("recvmsg read {got} of {} bytes", buf.len())));
            }
            // SAFETY: the kernel wrote `msg_controllen` bytes of headers.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&msg);
                if header.is_null() || (*header).cmsg_type != libc::SCM_RIGHTS {
                    return Err(io::Error::other("no descriptor came"));
                }
                let fd = libc::CMSG_DATA(header).cast::<libc::c_int>().read_unaligned();
                Ok(OwnedFd::from_raw_fd(fd))
            }
        }
    }

    pub fn token() -> io::Result<Token> {
        let (read, _write) = std::io::pipe()?;
        Ok(OwnedFd::from(read))
    }

    pub fn start_server(exe: &std::path::Path) -> io::Result<(Child, Peer)> {
        let (ours, theirs) = UnixStream::pair()?;
        let child = Command::new(exe).arg("ipc-serve").stdin(Stdio::from(OwnedFd::from(theirs))).spawn()?;
        Ok((child, Peer(ours)))
    }

    /// The child's end, which its parent made its stdin.
    pub fn served() -> io::Result<Peer> {
        // SAFETY: the parent made descriptor 0 a socket this process owns
        // and nothing else here reads stdin.
        Ok(Peer(UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) })))
    }

    /// A listening socket in the abstract namespace, which this process both
    /// connects to and accepts on, for timing a connection's setup alone.
    pub struct Rendezvous {
        listener: UnixListener,
        addr: SocketAddr,
    }

    impl Rendezvous {
        pub fn new() -> io::Result<Self> {
            let addr = SocketAddr::from_abstract_name(format!("osbench-{}", std::process::id()))?;
            let listener = UnixListener::bind_addr(&addr)?;
            Ok(Self { listener, addr })
        }

        pub fn connect_accept(&self) -> io::Result<()> {
            let client = UnixStream::connect_addr(&self.addr)?;
            let (server, _) = self.listener.accept()?;
            drop((client, server));
            Ok(())
        }
    }
}

/// The mount holding `path`, off `/proc/self/mountinfo`: its filesystem
/// type, its source, its mount's options and its superblock's, where a
/// filesystem's own options such as ext4's `nobarrier` are.
fn mount_of(path: &str) -> Option<(String, String, String, String)> {
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    // `id parent major:minor root mountpoint options... - fstype source superoptions`
    mounts
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mut left = left.split_whitespace();
            let point = left.nth(4)?;
            let options = left.next()?;
            let mut right = right.split_whitespace();
            let (fstype, source) = (right.next()?, right.next()?);
            let superblock = right.next().unwrap_or_default();
            let under = path == point || point == "/" || path.starts_with(&format!("{point}/"));
            under.then(|| (point.len(), (fstype.to_string(), source.to_string(), options.to_string(), superblock.to_string())))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, mount)| mount)
}

/// How `dir` is mounted, for a condition: its filesystem, its mount's
/// options and its superblock's.
pub fn mount_options(dir: &str) -> String {
    mount_of(dir).map_or_else(|| "unknown".into(), |(fstype, _, options, superblock)| format!("{fstype} {options} {superblock}"))
}

/// What a transfer row reports of the stack beneath it: the kernel's TCP segment counts and
/// its loss recovery, from `/proc/net/snmp` and `/proc/net/netstat`.
pub fn net_counters() -> Result<Vec<(String, u64)>, String> {
    const READ: [(&str, &str); 6] = [
        ("Tcp", "OutSegs"),
        ("Tcp", "RetransSegs"),
        ("TcpExt", "TCPTimeouts"),
        ("TcpExt", "TCPSackRecovery"),
        ("TcpExt", "TCPRenoRecovery"),
        ("TcpExt", "TCPLossProbes"),
    ];
    let mut table = std::collections::BTreeMap::new();
    for file in ["/proc/net/snmp", "/proc/net/netstat"] {
        let text = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        let lines: Vec<&str> = text.lines().collect();
        // Each table is a line of names and a line of values, both led by its name.
        for [names, values] in lines.as_chunks::<2>().0 {
            let (Some((name, keys)), Some((again, values))) = (names.split_once(':'), values.split_once(':')) else {
                return Err(format!("{file}: a table's line has no name"));
            };
            if name != again {
                return Err(format!("{file}: {name}'s names are followed by {again}'s values"));
            }
            for (key, value) in keys.split_whitespace().zip(values.split_whitespace()) {
                if let Ok(value) = value.parse::<u64>() {
                    table.insert(format!("{name}.{key}"), value);
                }
            }
        }
    }
    Ok(READ.iter().filter_map(|(name, key)| table.get(&format!("{name}.{key}")).map(|v| (format!("{name}.{key}"), *v))).collect())
}

/// How a reading adds up over a row's rounds: a time its total, a state its mean.
#[derive(Clone, Copy)]
enum Over {
    Total,
    Mean,
}

type Reading = Vec<(&'static str, u64, Over)>;

/// What a row's own connections say of themselves in `TCP_INFO`, each read just before its
/// end: RFC 4898's sender limits as Linux keeps them (`tcpi_busy_time`, `tcpi_rwnd_limited`,
/// `tcpi_sndbuf_limited`, microseconds), its congestion window in segments and its smoothed round
/// trip in microseconds.
pub struct StreamLog(Vec<Result<Reading, String>>);

impl StreamLog {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn look(&mut self, stream: &std::net::TcpStream) {
        self.0.push(tcp_info(stream));
    }

    /// Each reading as `stream_<name>_total` or `stream_<name>_mean` over the rounds; a reading
    /// that failed is one field saying why.
    pub fn fields(&self) -> Vec<(String, String)> {
        let mut sums: Reading = Vec::new();
        for reading in &self.0 {
            match reading {
                Ok(fields) => {
                    for &(name, value, over) in fields {
                        match sums.iter_mut().find(|(n, _, _)| *n == name) {
                            Some((_, sum, _)) => *sum = sum.saturating_add(value),
                            None => sums.push((name, value, over)),
                        }
                    }
                }
                Err(why) => return vec![("stream_unread".to_string(), why.clone())],
            }
        }
        let rounds = self.0.len().max(1) as u64;
        sums.into_iter()
            .map(|(name, sum, over)| match over {
                Over::Total => (format!("stream_{name}_total"), sum.to_string()),
                Over::Mean => (format!("stream_{name}_mean"), (sum / rounds).to_string()),
            })
            .collect()
    }
}

fn tcp_info(stream: &std::net::TcpStream) -> Result<Reading, String> {
    use std::os::fd::AsRawFd;
    use Over::{Mean, Total};
    // SAFETY: plain old data, which the kernel fills.
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    // SAFETY: `info` is `len` writable bytes, and the descriptor is the stream's own.
    let rc = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_INFO, (&raw mut info).cast(), &mut len)
    };
    if rc != 0 {
        return Err(format!("TCP_INFO: {}", std::io::Error::last_os_error()));
    }
    let full = std::mem::offset_of!(libc::tcp_info, tcpi_sndbuf_limited) + std::mem::size_of::<u64>();
    if (len as usize) < full {
        return Err(format!("TCP_INFO answered {len} bytes, short of the sender limits' {full}"));
    }
    Ok(vec![
        ("busy_us", info.tcpi_busy_time, Total),
        ("rwnd_limited_us", info.tcpi_rwnd_limited, Total),
        ("sndbuf_limited_us", info.tcpi_sndbuf_limited, Total),
        ("snd_cwnd_segments", u64::from(info.tcpi_snd_cwnd), Mean),
        ("srtt_us", u64::from(info.tcpi_rtt), Mean),
    ])
}

/// The runner's home, the filesystem and disk holding it, as `ext4, NVMe
/// disk, model <model>, write cache write back` (the block layer's
/// `queue/write_cache`: whether a sync must flush the device's cache), and
/// the disk's kind as a medium: `nvme`, `usb`,
/// `sata`, `mmc`, `virtio`, `other`, or `none` for no block device. The path
/// itself names the user and is never printed.
pub fn persistent_dir() -> Option<(String, String, String)> {
    let home = std::env::var("HOME").ok()?;
    let (fstype, source, _, _) = mount_of(&home)?;
    // btrfs reports an anonymous `0:NN` that names no block device; its
    // source device's own number does.
    let sysfs = |devno: String| fs::canonicalize(format!("/sys/dev/block/{devno}")).ok();
    let device = device_number_of(&home).and_then(sysfs).or_else(|| {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let meta = fs::metadata(source.strip_prefix("/dev/").map(|_| source.as_str())?).ok()?;
        let rdev = meta.file_type().is_block_device().then(|| meta.rdev())?;
        sysfs(format!("{}:{}", libc::major(rdev), libc::minor(rdev)))
    });
    let Some(device) = device else {
        return Some((home, format!("{fstype}, on no block device"), "none".into()));
    };
    let (disk, mapped) = backing_disk(device);
    let model = read(&disk.join("device/model").to_string_lossy()).unwrap_or_else(|| "unknown".into());
    let cache = read(&disk.join("queue/write_cache").to_string_lossy()).unwrap_or_else(|| "unknown".into());
    let name = disk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A SCSI disk is a USB one where sysfs puts it under a USB device.
    let usb = disk.to_string_lossy().contains("/usb");
    let sd = if usb { ("USB", "usb") } else { ("SCSI or SATA", "sata") };
    let (kind, medium) = [("nvme", ("NVMe", "nvme")), ("sd", sd), ("mmcblk", ("MMC", "mmc")), ("vd", ("virtio", "virtio"))]
        .into_iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map_or(("other", "other"), |(_, kind)| kind);
    let through = if mapped { " through device-mapper" } else { "" };
    // The device's own name is never printed: an installer or a user chose
    // a mapper's (`luks-<UUID>`, a volume group named for the host).
    Some((home, format!("{fstype}{through}, {kind} disk, model {model}, write cache {cache}"), medium.into()))
}

/// The `major:minor` of the filesystem holding `path`, off mountinfo's third
/// field for the deepest mount over it.
fn device_number_of(path: &str) -> Option<String> {
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let devno = fields.nth(2)?;
            let point = fields.nth(1)?;
            let under = path == point || point == "/" || path.starts_with(&format!("{point}/"));
            under.then(|| (point.len(), devno.to_string()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, devno)| devno)
}

/// The whole disk under a sysfs block device: through each device-mapper
/// layer's first `slaves` entry, then from a partition to its disk; and
/// whether a mapper was crossed.
fn backing_disk(mut device: std::path::PathBuf) -> (std::path::PathBuf, bool) {
    let mut mapped = false;
    while let Some(lower) = fs::read_dir(device.join("slaves")).ok().and_then(|mut d| d.next()).and_then(Result::ok) {
        match fs::canonicalize(lower.path()) {
            Ok(next) => {
                device = next;
                mapped = true;
            }
            Err(_) => break,
        }
    }
    if device.join("partition").exists() {
        if let Some(parent) = device.parent() {
            device = parent.to_path_buf();
        }
    }
    (device, mapped)
}

pub use osbench_lan::link::of as link_of;

/// The package's temperature off hwmon: `coretemp`'s `Package id 0`, or
/// `k10temp`'s `Tdie` where it has one and its `Tctl` otherwise.
pub fn package_sensor() -> Result<super::Sensor, String> {
    let mut chips: Vec<std::path::PathBuf> =
        fs::read_dir("/sys/class/hwmon").map_err(|e| format!("/sys/class/hwmon: {e}"))?.flatten().map(|e| e.path()).collect();
    chips.sort();
    for chip in chips {
        let name = read(&chip.join("name").to_string_lossy()).unwrap_or_default();
        let wanted: &[&str] = match name.as_str() {
            "coretemp" => &["Package id 0"],
            "k10temp" => &["Tdie", "Tctl"],
            _ => continue,
        };
        let labels: Vec<(String, String)> = (1..=64)
            .filter_map(|i| read(&chip.join(format!("temp{i}_label")).to_string_lossy()).map(|l| (format!("temp{i}"), l)))
            .collect();
        for label in wanted {
            if let Some((temp, _)) = labels.iter().find(|(_, l)| l == label) {
                let input = chip.join(format!("{temp}_input"));
                let crit_mc = read(&chip.join(format!("{temp}_crit")).to_string_lossy()).and_then(|v| v.parse().ok());
                let reader = move || read(&input.to_string_lossy()).and_then(|v| v.parse().ok());
                return Ok(super::Sensor { read: Box::new(reader), crit_mc, what: format!("{name} {label}") });
            }
        }
    }
    Err("no coretemp or k10temp package sensor in /sys/class/hwmon".into())
}
