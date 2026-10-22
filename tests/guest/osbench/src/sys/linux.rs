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

/// An anonymous mapping with transparent huge pages asked for, which
/// ToyOS's anonymous mappings always are; [`huge_backing`] says what the
/// kernel gave.
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

/// What a Linux number depends on, read and never changed.
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
    condition("max_khz", per_cpu("cpufreq/scaling_max_freq"));
    condition(
        "intel_no_turbo",
        read("/sys/devices/system/cpu/intel_pstate/no_turbo").unwrap_or_else(|| "absent".into()),
    );
    condition("boost", read("/sys/devices/system/cpu/cpufreq/boost").unwrap_or_else(|| "absent".into()));
    condition("amd_pstate", read("/sys/devices/system/cpu/amd_pstate/status").unwrap_or_else(|| "absent".into()));
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
