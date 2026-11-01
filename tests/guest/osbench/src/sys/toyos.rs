use std::sync::atomic::AtomicU32;

use toyos_abi::syscall::{self, VmAt, VmProt};

pub use crate::arch::{cpu_model, threads_per_core};

/// The kernel's index of the CPU running the caller: what a mask, the
/// kernel's own records and every CPU field this program prints name it by.
pub fn current_cpu() -> u32 {
    syscall::current_cpu()
}

/// The core CPU `cpu` belongs to: two CPUs of one core are SMT siblings.
/// ToyOS has no call that says, so each CPU reads its APIC ID once, from a
/// thread pinned there.
pub fn core_of(cpu: u32) -> u32 {
    static APICS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    let apics = APICS.get_or_init(|| {
        (0..syscall::cpu_count() as usize)
            .map(|on| {
                std::thread::spawn(move || {
                    pin_self(on);
                    assert_eq!(current_cpu() as usize, on, "pinned to CPU {on}");
                    crate::arch::apic_id()
                })
                .join()
                .expect("a CPU's APIC ID reader panicked")
            })
            .collect()
    });
    crate::arch::core_of_apic(apics[cpu as usize])
}

/// A thread of this process, as [`thread_set_affinity`](syscall::thread_set_affinity) names it.
pub type Tid = u64;

pub fn tid() -> Tid {
    u64::from(toyos_abi::current_tid().0)
}

/// The CPUs this process may run on: the calling thread's mask.
pub fn allowed_cpus() -> Vec<usize> {
    let mask = syscall::thread_affinity(0).expect("the calling thread's own mask");
    (0..mask.len() * 64).filter(|&cpu| mask[cpu / 64] & 1 << (cpu % 64) != 0).collect()
}

fn set_affinity(tid: Tid, cpus: &[usize]) {
    let mut mask = [0u64; syscall::CPU_MASK_WORDS];
    for &cpu in cpus {
        mask[cpu / 64] |= 1 << (cpu % 64);
    }
    syscall::thread_set_affinity(tid, &mask).unwrap_or_else(|e| panic!("thread_set_affinity({tid}, {cpus:?}): {e:?}"));
}

/// Keep the calling thread on CPU `cpu`.
pub fn pin_self(cpu: usize) {
    set_affinity(0, &[cpu]);
}

/// Let thread `tid` run anywhere this process may.
pub fn unpin(tid: Tid) {
    set_affinity(tid, &allowed_cpus());
}

pub fn null_syscall() {
    std::hint::black_box(syscall::getpid());
}

/// Nanoseconds since boot off the clock page, which `Instant` reads and
/// every process shares.
pub fn monotonic_ns() -> u64 {
    toyos_abi::clock::nanos_since_boot()
}

/// Block while `word` holds `expected`.
pub fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: `word` is a live, aligned u32.
    unsafe { syscall::futex_wait(word.as_ptr(), expected, None) };
}

/// Wake up to `count` waiters on `word`; answers how many woke.
pub fn futex_wake(word: &AtomicU32, count: u32) -> u32 {
    // SAFETY: `word` is a live, aligned u32.
    let woken = unsafe { syscall::futex_wake(word.as_ptr(), count) };
    u32::try_from(woken).expect("a wake count fits the count asked for")
}

/// ToyOS commits an anonymous range whole and populates it a 4 KiB page at a
/// time as it is touched.
pub fn map_anon(len: usize) -> *mut u8 {
    // SAFETY: a fresh anonymous mapping aliases nothing.
    unsafe { syscall::vm_reserve(len, VmProt::ReadWrite, VmAt::ANYWHERE) }
        .unwrap_or_else(|e| panic!("vm_reserve of {len} bytes was refused: {e:?}"))
}

/// ToyOS maps no user memory in large pages yet: the same 4 KiB mapping.
pub fn map_anon_huge(len: usize) -> *mut u8 {
    map_anon(len)
}

pub fn huge_backing() -> String {
    "0KiB".into()
}

/// # Safety
/// `ptr` and `len` are one whole mapping from [`map_anon`], and nothing
/// reaches it afterwards.
pub unsafe fn unmap(ptr: *mut u8, len: usize) {
    // SAFETY: as the caller promises.
    unsafe { syscall::vm_release(ptr, len) }.expect("vm_release of a whole mapping");
}

/// Nothing a program can read: the kernel's frequency request and the
/// machine's power source are the boot log's and the operator's to record.
pub fn conditions() {}

/// Nothing: a process's handle to itself carries no `READ`, which reading
/// its accounting takes (`kernel/src/loader/start.rs`'s `own_handle`). The
/// kernel's exit record of each job counts its syscalls instead.
pub fn counts() -> super::Counts {
    super::Counts { switches: None, faults: None }
}

/// A log line's time for a reading of [`monotonic_ns`]: the kernel log's
/// stamps count from the counter's zero, not from boot.
pub fn log_time_ns(monotonic: u64) -> u64 {
    monotonic + toyos_abi::clock::page().stamp_at_boot
}

/// ToyOS gives a thread the stack its builder asks for, in whole pages.
pub const STACK_FLOOR: &str = "none";

/// What [`current_cpu`] names a CPU by, for the fields that print one.
pub const CPU_NAMING: &str = "cpu";

/// Native IPC: a connection through a port, between this process and a child
/// it endows the port's acceptor.
pub mod ipc {
    use std::io;
    use std::os::toyos::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use toyos::endow::{Endowments, FromHandle};
    use toyos::ipc::{Connection, MAX_FRAME_LEN};
    use toyos::port::{self, Acceptor};
    use toyos::{OwnedHandle, Pipe};

    const LABEL: &str = "osbench-ipc";
    const NAME: &str = "osbench";
    const FRAME: usize = MAX_FRAME_LEN as usize;

    /// One end of a connection. A message longer than one frame
    /// ([`MAX_FRAME_LEN`]) goes as consecutive frames: a ToyOS connection
    /// frames each message, and a frame carries at most that much.
    pub struct Peer(Connection);

    /// What a message carries beside its bytes: one end of a pipe.
    pub type Token = Pipe;

    fn failed(what: &str, e: impl std::fmt::Debug) -> io::Error {
        io::Error::other(format!("{what}: {e:?}"))
    }

    /// Frames one message takes.
    pub fn frames(len: usize) -> usize {
        len.div_ceil(FRAME).max(1)
    }

    impl Peer {
        pub fn send(&self, data: &[u8]) -> io::Result<()> {
            if data.is_empty() {
                return self.0.send_bytes(1, data).map_err(|e| failed("send", e));
            }
            for chunk in data.chunks(FRAME) {
                self.0.send_bytes(1, chunk).map_err(|e| failed("send", e))?;
            }
            Ok(())
        }

        pub fn recv(&self, buf: &mut [u8]) -> io::Result<()> {
            let mut at = 0;
            loop {
                at += self.take(&mut buf[at..])?;
                if at >= buf.len() {
                    return Ok(());
                }
            }
        }

        /// One frame into the front of `buf`, straight from the kernel: the
        /// bytes it held.
        fn take(&self, buf: &mut [u8]) -> io::Result<usize> {
            let (frame, _) = self.0.recv_into(buf).map_err(|e| failed("receive", e))?;
            Ok(frame.payload_len)
        }

        /// One frame of `data` (at most one frame's worth) and `token`, moved.
        pub fn send_with(&self, data: &[u8], token: Token) -> io::Result<()> {
            self.0.send_bytes_with(1, data, [OwnedHandle::from(token)]).map_err(|e| failed("send", e))
        }

        pub fn recv_with(&self, buf: &mut [u8]) -> io::Result<Token> {
            let (_, handles) = self.0.recv_into(buf).map_err(|e| failed("receive", e))?;
            let [handle] = handles.exact::<1>().ok_or_else(|| io::Error::other("no handle came"))?;
            // The peer sends only pipe ends.
            Ok(Pipe::from_handle(handle))
        }
    }

    pub fn token() -> io::Result<Token> {
        let (read, _write) = toyos::pipe_pair().map_err(|e| failed("pipe", e))?;
        Ok(read)
    }

    /// `exe ipc-serve`, endowed a fresh port's acceptor, and this process's
    /// connection to it.
    pub fn start_server(exe: &std::path::Path) -> io::Result<(Child, Peer)> {
        let (acceptor, connector) = port::create().map_err(|e| failed("port", e))?;
        let names = toyos::namespace::build().add(NAME, &connector).finish().map_err(|e| failed("namespace", e))?;
        let conn = names.open(NAME).map_err(|e| failed("connect", e))?;
        let child = Command::new(exe)
            .arg("ipc-serve")
            .endow(LABEL, acceptor.into())
            .stdin(Stdio::null())
            .spawn()?;
        Ok((child, Peer(conn)))
    }

    /// The child's end: the connection its parent made to the port it was
    /// endowed.
    pub fn served() -> io::Result<Peer> {
        let acceptor: Acceptor = Endowments::get().take(LABEL).ok_or_else(|| io::Error::other("no port endowed"))?;
        acceptor.accept().map(Peer).map_err(|e| failed("accept", e))
    }

    /// A port this process both connects to and accepts on, for timing a
    /// connection's setup alone.
    pub struct Rendezvous {
        acceptor: Acceptor,
        names: toyos::namespace::Namespace,
    }

    impl Rendezvous {
        pub fn new() -> io::Result<Self> {
            let (acceptor, connector) = port::create().map_err(|e| failed("port", e))?;
            let names = toyos::namespace::build().add(NAME, &connector).finish().map_err(|e| failed("namespace", e))?;
            Ok(Self { acceptor, names })
        }

        /// Open a connection and accept it; both ends close on return.
        pub fn connect_accept(&self) -> io::Result<()> {
            let client = self.names.open(NAME).map_err(|e| failed("connect", e))?;
            let server = self.acceptor.accept().map_err(|e| failed("accept", e))?;
            drop((client, server));
            Ok(())
        }
    }
}

/// The capability the runner endowed this process, `None` where the boot
/// endows none. The endowment is taken once per process, so every row that
/// uses the capability borrows it here.
pub fn syscap() -> Option<&'static toyos::syscap::SysCap> {
    static CAP: std::sync::OnceLock<Option<toyos::syscap::SysCap>> = std::sync::OnceLock::new();
    CAP.get_or_init(|| toyos::endow::Endowments::get().take(toyos::endow::SYSCAP_LABEL)).as_ref()
}

/// What the tail group reads to say where a late wake's time went, through
/// the runner's capability: each CPU's firmware interrupts and clock, the
/// scheduler's diary and the roster's names. Read-only observation rights;
/// `None` where this boot did not grant them.
pub mod diag {
    use std::collections::BTreeMap;

    use toyos::syscap::{SysCap, TraceCursor};
    use toyos_abi::counters::{Counter, RawRecord, Record};
    use toyos_abi::syscall::{SYSINFO_ENTRY_SIZE, SYSINFO_HEADER_SIZE};
    use toyos_abi::trace::TraceRecord;
    use toyos_trace::{Entry, Event};

    pub struct Diag {
        cap: &'static SysCap,
        cursor: Option<TraceCursor<Vec<u64>>>,
    }

    /// One CPU's counters, by the kernel's index of the CPU, which is what
    /// [`current_cpu`](super::current_cpu) names it by, and the APIC id it
    /// read off itself.
    #[derive(Clone)]
    pub struct CpuCounts {
        /// The CPU did not answer within the read's bound: its values are the
        /// last it published.
        pub stale: bool,
        pub cpu: u32,
        pub apic: u32,
        /// The CPU's own counter when it read the rest (the TSC).
        pub stamp: Option<u64>,
        pub smi: Option<u64>,
        pub aperf: Option<u64>,
        pub mperf: Option<u64>,
        pub kicks: Option<u64>,
        /// Core C6 and C7 residency in TSC ticks, where the CPU counts it.
        pub core_c6: Option<u64>,
        pub core_c7: Option<u64>,
        /// Idle periods the kernel chose a declared deep state for, counted
        /// as each ends.
        pub deep_idles: Option<u64>,
        /// The package's counters, on the CPU that reads them.
        pub package_energy: Option<u64>,
        pub rapl_power_unit: Option<u64>,
        pub package_residency: [Option<u64>; 4],
        pub core_energy: Option<u64>,
    }

    /// The capability's diagnostics for the rows that read the counters
    /// around them ([`crate::around`]): opened once a process, and `None`
    /// where the boot endows no capability.
    pub fn shared() -> Option<&'static Diag> {
        static SHARED: std::sync::OnceLock<Option<Diag>> = std::sync::OnceLock::new();
        SHARED.get_or_init(open).as_ref()
    }

    pub fn open() -> Option<Diag> {
        let cap = super::syscap()?;
        let cursor = cap.trace_cursor_words().ok().map(|n| TraceCursor::new(vec![0u64; n]));
        Some(Diag { cap, cursor })
    }

    impl Diag {
        /// Every CPU's counters, or why the read failed.
        pub fn counters(&self) -> Result<Vec<CpuCounts>, String> {
            let mut raw = vec![RawRecord::EMPTY; toyos_abi::syscall::cpu_count() as usize];
            let read = self.cap.counters(&mut raw).map_err(|e| format!("the counters read was refused: {e:?}"))?;
            raw[..read]
                .iter()
                .map(|r| {
                    let r = Record::decode(r).map_err(|e| format!("a counters record did not decode: {e:?}"))?;
                    Ok(CpuCounts {
                        stale: r.stale,
                        cpu: r.cpu,
                        apic: r.hardware_id,
                        stamp: r.get(Counter::Stamp),
                        smi: r.get(Counter::Smi),
                        aperf: r.get(Counter::Aperf),
                        mperf: r.get(Counter::Mperf),
                        kicks: r.get(Counter::Kicks),
                        core_c6: r.get(Counter::CoreC6),
                        core_c7: r.get(Counter::CoreC7),
                        deep_idles: r.get(Counter::DeepIdles),
                        package_energy: r.get(Counter::PackageEnergy),
                        rapl_power_unit: r.get(Counter::RaplPowerUnit),
                        package_residency: [Counter::PackageC2, Counter::PackageC3, Counter::PackageC6, Counter::PackageC7]
                            .map(|c| r.get(c)),
                        core_energy: r.get(Counter::CoreEnergy),
                    })
                })
                .collect()
        }

        pub fn has_diary(&self) -> bool {
            self.cursor.is_some()
        }

        /// Every diary record since the last read, decoded, with its time as
        /// nanoseconds since boot, oldest first; how many the rings had
        /// overwritten before this read reached them; and why the read ended
        /// early, if it did.
        pub fn diary(&mut self) -> (Vec<(u64, Entry)>, u64, Option<String>) {
            let Some(cursor) = self.cursor.as_mut() else { return (Vec::new(), 0, None) };
            let page = toyos_abi::clock::page();
            let lost_before = cursor.lost();
            let mut out = Vec::new();
            let mut buf = vec![TraceRecord::EMPTY; 4096];
            let mut failed = None;
            loop {
                let n = match self.cap.trace(cursor, &mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        failed = Some(format!("the diary read was refused: {e:?}"));
                        break;
                    }
                };
                for raw in &buf[..n] {
                    if let Ok(entry) = Entry::decode(raw) {
                        let since_boot = page.nanos_since_boot(entry.stamp);
                        out.push((since_boot, entry));
                    }
                }
                if n < buf.len() {
                    break;
                }
            }
            (out, cursor.lost() - lost_before, failed)
        }

        /// Every live thread's process and name, and the CPU nanoseconds it
        /// has run, by `(pid, tid)`.
        pub fn roster(&self) -> BTreeMap<(u32, u32), (String, u64)> {
            const ENTRIES: usize = 2048;
            let mut buf = vec![0u8; SYSINFO_HEADER_SIZE + SYSINFO_ENTRY_SIZE * ENTRIES];
            let n = self.cap.roster(&mut buf).min(buf.len());
            let mut rows = BTreeMap::new();
            if n < SYSINFO_HEADER_SIZE {
                return rows;
            }
            let mut pos = SYSINFO_HEADER_SIZE;
            while pos + SYSINFO_ENTRY_SIZE <= n {
                let u32_at = |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().expect("four"));
                let name = &buf[pos + 32..pos + 60];
                let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let cpu_ns = u64::from_le_bytes(buf[pos + 24..pos + 32].try_into().expect("eight"));
                rows.insert((u32_at(pos), u32_at(pos + 4)), (String::from_utf8_lossy(&name[..len]).into_owned(), cpu_ns));
                pos += SYSINFO_ENTRY_SIZE;
            }
            rows
        }
    }

    /// An event's short name and its thread, for a note.
    pub fn describe(entry: &Entry) -> String {
        let what = match entry.event {
            Event::Pick => "pick",
            Event::Wake => "wake",
            Event::Park => "park",
            Event::Preempt => "preempt",
            Event::Migrate { .. } => "migrate",
            Event::Adopt => "adopt",
            Event::Retire => "retire",
            Event::IdleEnter => "idle",
            Event::TimerArm { .. } => "arm",
            Event::TimerStop => "stop",
            Event::TimerFire => "fire",
            Event::TimerFireBurst { .. } => "fires",
            Event::Mark { .. } => "mark",
        };
        match entry.thread {
            Some(t) => format!("{what}:{}/{}", t.pid, t.tid),
            None => what.to_string(),
        }
    }
}

/// The power counters, through the runner's capability: `SYS_COUNTERS`
/// under `Rights::TRACE`, the package's held by the boot processor. Every read
/// kicks every CPU (`toyos_abi::counters`), the package's too.
pub mod power {
    use super::super::{CpuPower, Package, Refused, Round};
    use super::diag::{self, CpuCounts};

    fn read() -> Result<Vec<CpuCounts>, Refused> {
        let d = diag::shared().ok_or_else(|| Refused::Withheld("this boot endows osbench no capability".into()))?;
        d.counters().map_err(Refused::Failed)
    }

    /// One `SYS_COUNTERS` round, which kicks every CPU.
    pub fn round() -> Result<Round, Refused> {
        let read = read()?;
        Ok(Round { package: package(&read)?, cpus: cpus(read)? })
    }

    fn package(cpus: &[CpuCounts]) -> Result<Package, Refused> {
        let held = cpus.iter().find(|c| c.package_energy.is_some() || c.package_residency.iter().any(Option::is_some));
        let Some(c) = held else {
            return Err(Refused::Absent(
                "no CPU's record carries the package's counters: the kernel's `counters: cpu0 reads` line says whether it \
                 reads them, or the capability holds no TRACE"
                    .into(),
            ));
        };
        if c.stale {
            return Err(Refused::Failed(format!("cpu{}, which reads the package, answered stale", c.cpu)));
        }
        Ok(Package {
            tsc: c.stamp.ok_or_else(|| Refused::Failed(format!("cpu{}'s record carries no stamp", c.cpu)))?,
            energy: c.package_energy,
            power_unit: c.rapl_power_unit,
            residency: c.package_residency,
        })
    }

    fn cpus(read: Vec<CpuCounts>) -> Result<Vec<CpuPower>, Refused> {
        read.into_iter()
            .map(|c| {
                if c.stale {
                    return Err(Refused::Failed(format!("cpu{} answered stale", c.cpu)));
                }
                Ok(CpuPower {
                    cpu: c.cpu,
                    core: crate::arch::core_of(c.apic),
                    tsc: c.stamp.ok_or_else(|| Refused::Failed(format!("cpu{}'s record carries no stamp", c.cpu)))?,
                    aperf: c.aperf,
                    mperf: c.mperf,
                    core_c6: c.core_c6,
                    core_c7: c.core_c7,
                    core_energy: c.core_energy,
                })
            })
            .collect()
    }

    /// The CPU time every process but this one has run for, in nanoseconds,
    /// by the roster.
    pub fn busy_ns() -> Result<u64, Refused> {
        let d = diag::shared().ok_or_else(|| Refused::Withheld("this boot endows osbench no capability".into()))?;
        let me = std::process::id();
        Ok(d.roster().iter().filter(|((pid, _), _)| *pid != me).map(|(_, (_, ns))| ns).sum())
    }
}

pub fn protect(ptr: *mut u8, len: usize, writable: bool) -> Result<(), super::Refused> {
    let prot = if writable { VmProt::ReadWrite } else { VmProt::Read };
    // SAFETY: the caller's mapping; a protection change moves no memory.
    unsafe { syscall::vm_protect(ptr, len, prot) }.map_err(|e| super::Refused::Failed(format!("vm_protect: {e:?}")))
}

/// # Safety
/// `ptr` and `len` lie inside one mapping from [`map_anon`] that nothing
/// reaches in that range afterwards.
pub unsafe fn unmap_part(ptr: *mut u8, len: usize) -> Result<(), super::Refused> {
    // SAFETY: as the caller promises.
    unsafe { syscall::vm_release(ptr, len) }.map_err(|e| super::Refused::Failed(format!("vm_release: {e:?}")))
}

/// A mapping that faults in 4 KiB pages, which every ToyOS anonymous mapping does.
pub fn map_anon_4k(len: usize) -> *mut u8 {
    map_anon(len)
}

/// DATA's `/home`, what serves it, and its medium. A program cannot ask
/// which disk DATA is on, or whether it is memory instead; the `osbench` row
/// refuses a boot whose DATA was not the NVMe's partition, so that is the
/// medium a measured run's lines name.
pub fn persistent_dir() -> Option<(String, String, String)> {
    let served_by = "DATA volume, bcachefs crate's format, DATA file server over the block service";
    Some(("/home".into(), served_by.into(), "nvme".into()))
}

/// What a transfer row reports of the stack beneath it, as netstack's `inspect` answers it: [tcp]'s
/// counters, and the cards' summed over netstack's links. Those no card reports are left out.
pub fn net_counters() -> Result<Vec<(String, u64)>, String> {
    const READ: [&str; 25] = [
        "net.tcp.sent.bytes",
        "net.tcp.retransmit.bytes",
        "net.tcp.rto",
        "net.tcp.fast-recovery",
        "net.tcp.sack-recovery",
        "net.tcp.loss-probe",
        "net.tcp.loss-probe-recovery",
        "net.tcp.dsack-rcvd",
        "net.tcp.rtt.departure",
        "net.tcp.rtt.echo",
        "net.tcp.send-limited.user.ns",
        "net.tcp.send-limited.window.ns",
        "net.tcp.send-limited.congestion.ns",
        "net.tcp.send-limited.pacing.ns",
        "net.tcp.congestion-stops",
        "net.tcp.congestion-stops.cwnd.bytes",
        "net.tcp.congestion-stops.srtt.us",
        "net.tcp.acks.cwnd-limited",
        "net.tcp.acks.not-limited",
        "net.tcp.cwnd.grown.bytes",
        "net.tcp.hystart.css",
        "net.tcp.hystart.css.entry-rounds",
        "net.tcp.hystart.resumed",
        "net.tcp.hystart.exits",
        "net.tcp.hystart.exit.cwnd.bytes",
    ];
    /// Each link's, under `net.link.<n>.`.
    const CARD: [&str; 8] = [
        "transmit.full",
        "transmit.wake_armed",
        "transmit.wake_taken",
        "transmit.full.ns",
        "transmit.unreported",
        "errors.missed",
        "wire.sent",
        "wire.received",
    ];
    let snap = inspect::ask(toyos_inspect::NET)?;
    let number = |key: &str| match snap.get(key) {
        Some(toyos_inspect::Value::U64(v)) => Some(*v),
        _ => None,
    };
    let links = number("net.links").unwrap_or(0);
    let tcp = READ.iter().filter_map(|key| number(key).map(|v| (key.to_string(), v)));
    let cards = CARD.iter().filter_map(|key| {
        let each: Vec<u64> = (0..links).filter_map(|n| number(&format!("net.link.{n}.{key}"))).collect();
        (!each.is_empty()).then(|| (format!("net.{key}"), each.iter().sum()))
    });
    Ok(tcp.chain(cards).collect())
}

/// What a row's own connections say of themselves: nothing on ToyOS, whose
/// stack's counters ([`net_counters`]) say it for every connection.
pub struct StreamLog;

impl StreamLog {
    pub fn new() -> Self {
        Self
    }

    pub fn look(&mut self, _stream: &std::net::TcpStream) {}

    pub fn fields(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// What carries a connection on ToyOS: the netstack link whose lease holds its local address.
pub fn link_of(local: std::net::IpAddr) -> Result<(osbench_lan::link::Link, String), String> {
    let snap = inspect::ask(toyos_inspect::NET)?;
    let text = |key: String| match snap.get(&key) {
        Some(toyos_inspect::Value::Text(t)) => Some(t.clone()),
        _ => None,
    };
    let links = match snap.get("net.links") {
        Some(toyos_inspect::Value::U64(n)) => *n,
        _ => 0,
    };
    let held = (0..links).find(|n| text(format!("net.link.{n}.lease.address")).is_some_and(|a| a.split('/').next() == Some(&local.to_string())));
    let Some(n) = held else { return Err(format!("no netstack link holds {local}")) };
    let driver = text(format!("net.link.{n}.driver")).unwrap_or_default();
    match text(format!("net.link.{n}.medium")).as_deref() {
        Some("wired") => Ok((osbench_lan::link::Link::Wired, format!("netstack's wired link, {driver}"))),
        Some("wireless") => Ok((osbench_lan::link::Link::Wifi, format!("netstack's wireless link, {driver}"))),
        other => Err(format!("netstack's link {n} is carried over {other:?}")),
    }
}

/// ToyOS reads no temperature: the kernel counts none
/// (`toyos_abi::counters::Counter`) and no driver serves one.
pub fn package_sensor() -> Result<super::Sensor, String> {
    Err("no counter or service carries the package's temperature".into())
}

/// Requests straight through the file server's client (`toyos::fs::Dir`),
/// under none of std's descriptor table and path routing: beside the same
/// rows through std, what that layer costs. Only for a directory a file
/// server serves; the kernel's `/tmp` has no client to go around.
pub mod direct {
    use toyos::fs::{Base, Dir, Error, O_READ, SEEK_START};

    use crate::measure::each;
    use crate::report::refuse;

    /// `dir`'s capability, connected: `None` where no file server serves it,
    /// and why where its server would not take this second connection.
    pub fn connect(dir: &str) -> Result<Option<Dir>, String> {
        let Some(names) = toyos::endow::namespace() else { return Ok(None) };
        match Dir::connect(names, &format!("{}{dir}", toyos::fs::CAPABILITY_PREFIX)) {
            Ok(d) => Ok(Some(d)),
            Err(Error::NotHeld) => Ok(None),
            Err(e) => Err(format!("{dir}'s file server would not take a second connection: {e:?}")),
        }
    }

    fn word(e: impl core::fmt::Debug) -> String {
        format!("{e:?}")
    }

    /// The `direct` rows, each on the file its std twin uses: `small`, with
    /// the target's `fields`, and `big`, with `big_fields`; each a path
    /// under the directory `dir` serves.
    pub fn rows(
        dir: &mut Dir,
        name: &dyn Fn(&str) -> String,
        fields: &[(&'static str, String)],
        big_fields: &[(&'static str, String)],
        small: &str,
        big: &str,
        at: impl Fn(usize) -> u64,
    ) {
        let opened = match dir.open(Base::Root, big, O_READ) {
            Ok(o) => o.fid,
            Err(e) => return refuse(&name("direct"), word(e)),
        };
        let held = match dir.open(Base::Root, small, O_READ) {
            Ok(o) => o.fid,
            Err(e) => {
                dir.close(opened);
                return refuse(&name("direct"), word(e));
            }
        };
        let mut buf = vec![0u8; 4096];
        each(&name("direct.seek"), 100, 2000, big_fields, |i| dir.seek(opened, SEEK_START, at(i) as i64).map(drop).map_err(word));
        each(&name("direct.read_4k_at"), 100, 2000, big_fields, |i| {
            dir.seek(opened, SEEK_START, at(i) as i64).map_err(word)?;
            dir.read(opened, &mut buf).map(drop).map_err(word)
        });
        each(&name("direct.pread_4k"), 100, 2000, big_fields, |i| dir.pread(opened, at(i), &mut buf).map(drop).map_err(word));
        each(&name("direct.fstat"), 100, 2000, fields, |_| dir.fstat(held).map(drop).map_err(word));
        each(&name("direct.stat"), 100, 2000, fields, |_| dir.stat(Base::Root, small, true).map(drop).map_err(word));
        dir.close(opened);
        dir.close(held);
    }
}
