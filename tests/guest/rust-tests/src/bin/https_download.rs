//! How fast this machine downloads over HTTPS through ToyOS's own stack: one
//! large public file ([`HOST`], [`PATH`]) with the client `https_get` builds,
//! every byte hashed. The `internet_download` metal row runs it, holds a whole
//! body to the file's pin and reports the rate, never judging it.
//!
//! It runs only in a job list, and its window is the bound the runner gave
//! that list (`toyos_tco::LIST_BOUND_ENV`) less [`MARGIN_MS`]. Inside it, it
//! waits for netstack's lease on the `log` port, since on the T14 it is the
//! boot's first job; times [`PROBES`] TCP handshakes to the file's host; and
//! reads the body until it ends or the window closes.
//!
//! It says one line and ends 0: `https_download: whole bytes=<n>
//! sha256=<hex> <timing>` for a body read to its end, or `https_download: cut
//! bytes=<n> <timing>` for what came inside the window, and `https_download:
//! cut before the first byte of the body` for nothing. `<timing>` is
//! `secs=<s> mbps=<rate> rtt_ms=[<each>] busy=<fraction> cpus=[<each>]`: the
//! time from the first read of the body to the last and the rate over the
//! bytes after the first; each handshake's time from `connect` called to
//! returned, which is one round trip to the host and netstack's own time for a
//! connect; and the busy fraction each CPU's MPERF ran of its stamp across the
//! transfer (`busy=unread` where no CPU counts MPERF). An I/O or TLS error
//! panics.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall::{self, ProcessStats, SELF_PROCESS};
use toyos_logstream::program_line;

#[path = "../https_client.rs"]
#[allow(dead_code)]
mod https_client;
#[path = "../netperf.rs"]
mod netperf;
#[path = "../netperf_table.rs"]
#[allow(dead_code)]
mod netperf_table;
#[path = "../served_log.rs"]
mod served_log;

use netperf::{Machine, Snap};

/// A release archive Rust's distribution server never rewrites; the row holds
/// its length and SHA-256.
const HOST: &str = "static.rust-lang.org";
const PATH: &str = "/dist/2024-09-05/rust-1.81.0-x86_64-unknown-linux-gnu.tar.xz";
const ROOTS: &str = "/system/etc/ssl/cert.pem";

/// Handshakes timed before the transfer, one after another.
const PROBES: usize = 5;

/// MEASUREMENT ONLY: downloads of the file, one after another, each with the
/// cadence's stretch `S` netstack is told before it: a palindrome, so drift
/// across the boot weighs every `S` alike. A netstack with no stretch refuses
/// the word, and the label is then the run's place in the order.
const K_ORDER: [u32; 6] = [8, 16, 32, 32, 16, 8];

/// MEASUREMENT ONLY: the word that sets netstack's stretch: `S=` and the value.
fn set_k(k: u32) {
    use toyos::ipc::{FrameRx, RxStep};
    let conn = toyos::endow::service("netstack").expect("the job holds a netstack connector");
    conn.signal(u32::from_le_bytes([b'S', b'=', 0, 0]) | k << 16).expect("netstack takes the word");
    let mut rx: Box<FrameRx<64>> = Box::new(FrameRx::new());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match rx.pump(&conn) {
            RxStep::Idle => std::thread::sleep(Duration::from_millis(1)),
            _ => return,
        }
    }
    panic!("netstack did not answer S={k} in 2 s");
}

/// MEASUREMENT ONLY: one lookup of the file's host, timed: a round trip to
/// the LAN's resolver, which netstack asks every time.
fn lan_probe() -> f64 {
    let at = Instant::now();
    let _ = (HOST, 443).to_socket_addrs().unwrap_or_else(|e| panic!("look up {HOST}: {e}"));
    at.elapsed().as_secs_f64() * 1e3
}

/// MEASUREMENT ONLY: min, median, 90th percentile and max of `ms`, and how many.
fn spread(mut ms: Vec<f64>) -> String {
    if ms.is_empty() {
        return "none".into();
    }
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    format!("{:.2}/{:.2}/{:.2}/{:.2} n={}", ms[0], at(0.5), at(0.9), ms[ms.len() - 1], ms.len())
}

/// The ceiling on netstack's word reaching this job: its own bound on saying
/// it, and two of logkeeper's rounds at its write budget
/// (`userland/logkeeper/src/policy.rs`, 5 s), since a served line is one the
/// stick already holds.
const LEASE_WAIT: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS + 10_000);

/// What the window leaves the list's bound for the last line to reach the
/// stick: one of logkeeper's rounds.
const MARGIN_MS: u64 = 5_000;

/// The transfer so far: its bytes, when its first read came, every CPU's
/// counters then and how many bytes that read brought, and when its last came.
struct Progress {
    bytes: u64,
    first: Option<(Instant, Vec<Record>, u64)>,
    last: Option<Instant>,
}

/// MEASUREMENT ONLY: the idle window before anything of the job's runs.
const IDLE_SECS: f64 = 2.0;

/// MEASUREMENT ONLY: every CPU's counters summed, and this process's own
/// accounting.
struct Toy<'a>(&'a SysCap);

impl Machine for Toy<'_> {
    fn snap(&self) -> Snap {
        let mut snap = Snap::new();
        for (index, r) in round(self.0).into_iter().enumerate() {
            for counter in Counter::ALL {
                let Some(v) = r.get(counter) else { continue };
                let key = match counter {
                    Counter::Stamp | Counter::Aperf | Counter::Mperf => counter.name().to_string(),
                    // Round 4: one package, which every CPU reads alike: the first CPU's.
                    Counter::PkgEnergyUj if index == 0 => counter.name().to_string(),
                    Counter::PkgEnergyUj => continue,
                    // The clock scout's: said per window by `clocked`, never summed.
                    Counter::CoreC6
                    | Counter::CoreC7
                    | Counter::PkgC2
                    | Counter::PkgC3
                    | Counter::PkgC6
                    | Counter::PkgC7
                    | Counter::PkgC8
                    | Counter::PkgC9
                    | Counter::PkgC10
                    | Counter::PerfStatus
                    | Counter::HwpStatus
                    | Counter::PerfLimitReasons
                    | Counter::HwpCapabilities
                    | Counter::PkgPowerLimit
                    | Counter::RaplPowerUnit
                    | Counter::PmuGlobalCtrl
                    | Counter::PowerCtl
                    | Counter::PkgCstConfig => continue,
                    Counter::Smi
                    | Counter::HwpRequest
                    | Counter::HwpRequestPkg
                    | Counter::EnergyPerfBias
                    | Counter::FirmwareCalls
                    | Counter::FirmwareNanos => continue,
                    _ => format!("k.{}", counter.name()),
                };
                *snap.entry(key).or_default() += v;
            }
        }
        let mut stats = ProcessStats::default();
        syscall::process_stats(SELF_PROCESS, &mut stats).expect("the job reads its own accounting");
        snap.insert("app.cpu_ns".into(), stats.cpu_ns);
        snap.insert("app.syscalls".into(), stats.syscall_total);
        snap.insert("app.syscall_ns".into(), stats.syscall_total_ns);
        snap.insert("app.runq_ns".into(), stats.runqueue_wait_ns);
        snap
    }

    fn tsc(&self) -> u64 {
        // SAFETY: reads the time-stamp counter, which Ring 3 may.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}

fn main() {
    // MEASUREMENT ONLY: `<url> <roots>` is the QEMU check's server, its lease
    // already waited on and a window of ten minutes.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (url, roots) = match &args[..] {
        [url, roots] => (url.clone(), roots.clone()),
        _ => (format!("https://{HOST}{PATH}"), ROOTS.to_string()),
    };
    let window = if args.is_empty() {
        let bound_ms: u64 = std::env::var(toyos_tco::LIST_BOUND_ENV)
            .unwrap_or_else(|e| panic!("{}: {e}; this job runs only in a job list", toyos_tco::LIST_BOUND_ENV))
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", toyos_tco::LIST_BOUND_ENV));
        let since_boot_ms = toyos_abi::clock::nanos_since_boot() / 1_000_000;
        Duration::from_millis(bound_ms.saturating_sub(MARGIN_MS).saturating_sub(since_boot_ms))
    } else {
        Duration::from_secs(600)
    };
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    let progress = Mutex::new(Progress { bytes: 0, first: None, last: None });
    let rtt = Mutex::new(String::from("unread"));
    // Dropped as the body ends whole, or as this panics.
    let (transferring, ended) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        let (watched, rtt, cap) = (&progress, &rtt, &cap);
        s.spawn(move || {
            if ended.recv_timeout(window) == Err(mpsc::RecvTimeoutError::Timeout) {
                let progress = watched.lock().unwrap_or_else(PoisonError::into_inner);
                // The body ended whole while this waited for the lock.
                if ended.try_recv() == Err(mpsc::TryRecvError::Disconnected) {
                    return;
                }
                let rtt = rtt.lock().unwrap_or_else(PoisonError::into_inner);
                println!("https_download: {}", report("cut", None, &progress, &rtt, cap));
                std::process::exit(0);
            }
        });
        let (host, port) = authority(&url);
        if args.is_empty() {
            leased();
        }
        *rtt.lock().unwrap_or_else(PoisonError::into_inner) = handshakes(&host, port);

        // MEASUREMENT ONLY: the machine idle, the suite this client's offer
        // gets, and what the job's own kernels cost on this CPU.
        let machine = Toy(cap);
        let cpus = syscall::cpu_count() as usize;
        let idle = clocked(cap, "window=idle", || netperf::idle(&machine, cpus, IDLE_SECS));
        println!("https_download: {idle}");
        println!("https_download: {}", hitm_control(cap));
        let pem = std::fs::read(&roots).unwrap_or_else(|e| panic!("read {roots}: {e}"));
        let path = url.splitn(4, '/').nth(3).map_or("/".to_string(), |p| format!("/{p}"));
        let suite = netperf::suite(&pem, &host, port, &path);
        println!("https_download: {}", netperf::benches(&machine, &suite, &idle));

        // MEASUREMENT ONLY: the LAN's round trip with nothing else moving.
        let idle_lan: Vec<f64> = (0..10)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(50));
                lan_probe()
            })
            .collect();
        println!("https_download: lan idle lan_ms={}", spread(idle_lan));

        // MEASUREMENT ONLY: the file once per `K`, back to back.
        for (index, k) in K_ORDER.into_iter().enumerate() {
            let run = index + 1;
            set_k(k);
            *progress.lock().unwrap_or_else(PoisonError::into_inner) = Progress { bytes: 0, first: None, last: None };
            let before = machine.snap();
            let at = Instant::now();
            // The LAN's round trip while the download moves, every 50 ms.
            let moving = std::sync::atomic::AtomicBool::new(true);
            let (fetched, lan) = clocked(cap, &format!("window=run run={run} k={k}"), || std::thread::scope(|probes| {
                let lan = probes.spawn(|| {
                    let mut ms = Vec::new();
                    while moving.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                        if progress.lock().unwrap_or_else(PoisonError::into_inner).first.is_some() {
                            ms.push(lan_probe());
                        }
                    }
                    ms
                });
                let fetched = netperf::fetch(https_client::config(&roots), &url, |bytes| {
                    let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                    progress.bytes = bytes;
                    progress.last = Some(Instant::now());
                    if progress.first.is_none() {
                        progress.first = Some((Instant::now(), round(cap), bytes));
                    }
                });
                moving.store(false, std::sync::atomic::Ordering::Relaxed);
                (fetched, lan.join().expect("the probe thread"))
            }));
            let secs = at.elapsed().as_secs_f64();
            println!("https_download: lan run={run} k={k} lan_ms={}", spread(lan));
            let d = netperf::delta(&before, &machine.snap());
            let progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
            let rtt = rtt.lock().unwrap_or_else(PoisonError::into_inner);
            println!(
                "https_download: {} tcp_ms={:.1} tls_ms={:.1} run={run} k={k}",
                report("whole", Some(&fetched.sha256), &progress, &rtt, cap),
                fetched.tcp_ms,
                fetched.tls_ms
            );
            println!("https_download: perf run={run} k={k} bytes={} secs={secs:.3} {}", fetched.bytes, netperf::words(&d));
        }
        drop(transferring);
    });
}

/// MEASUREMENT ONLY: `url`'s host and port.
fn authority(url: &str) -> (String, u16) {
    let rest = url.strip_prefix("https://").unwrap_or_else(|| panic!("{url} is no https URL"));
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or_else(|e| panic!("{url}'s port: {e}"))),
        None => (authority.to_string(), 443),
    }
}

/// Wait for netstack to say it holds a lease; panics where it says it took
/// none.
fn leased() {
    served_log::Log::open().until("netstack's word on its lease", LEASE_WAIT, |line| {
        let Some(said) = program_line(line).filter(|said| said.tag == "netstack") else { return false };
        assert!(!said.text.starts_with(toyos_tco::NO_LEASE_SAID), "{}", said.text);
        said.text.starts_with(toyos_tco::LEASE_SAID)
    });
}

/// [`PROBES`] handshakes with `host`'s first address, each closed as it is
/// made, as the `rtt_ms` the module header gives.
fn handshakes(host: &str, port: u16) -> String {
    let addr = (host, port)
        .to_socket_addrs()
        .unwrap_or_else(|e| panic!("look up {host}: {e}"))
        .next()
        .unwrap_or_else(|| panic!("{host} has no address"));
    let each: Vec<String> = (0..PROBES)
        .map(|_| {
            let at = Instant::now();
            drop(TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to {host}: {e}")));
            format!("{:.1}", at.elapsed().as_secs_f64() * 1e3)
        })
        .collect();
    format!("[{}]", each.join(" "))
}

/// Every CPU's counters now.
fn round(cap: &SysCap) -> Vec<Record> {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw).expect("test-runner's capability reads the counters");
    raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect()
}

/// `word`, then the transfer from its first byte to now in the words the
/// module header gives them.
fn report(word: &str, sha256: Option<&str>, progress: &Progress, rtt: &str, cap: &SysCap) -> String {
    let (Some((at, before, first)), Some(last)) = (&progress.first, progress.last) else {
        return format!("{word} before the first byte of the body");
    };
    let after = round(cap);
    let secs = last.duration_since(*at).as_secs_f64();
    let busy: Option<Vec<f64>> = before
        .iter()
        .zip(&after)
        .map(|(a, b)| {
            let ran = b.get(Counter::Mperf)? - a.get(Counter::Mperf)?;
            let stamp = b.get(Counter::Stamp)? - a.get(Counter::Stamp)?;
            Some(ran as f64 / stamp as f64)
        })
        .collect();
    let mb = (progress.bytes - first) as f64 / 1e6;
    let busy = match busy {
        // MEASUREMENT ONLY: cpu_s, every CPU's busy fraction times the transfer's time, summed.
        Some(each) => {
            let cpu_s = each.iter().sum::<f64>() * secs;
            format!(
                "busy={:.3} cpus=[{}] cpu_s={cpu_s:.3} cpu_ms_per_mb={:.2}",
                each.iter().sum::<f64>() / each.len() as f64,
                each.iter().map(|b| format!("{b:.3}")).collect::<Vec<_>>().join(" "),
                cpu_s * 1e3 / mb
            )
        }
        None => "busy=unread".to_string(),
    };
    let mbps = mb * 8.0 / secs;
    let sha256 = sha256.map(|hex| format!(" sha256={hex}")).unwrap_or_default();
    format!("{word} bytes={}{sha256} secs={secs:.3} mbps={mbps:.1} rtt_ms={rtt} {busy}", progress.bytes)
}

/// MEASUREMENT ONLY: how often `clocked` reads every CPU's counters inside its
/// window. Each read kicks every CPU, so it is also the scout's own load.
const SAMPLE: Duration = Duration::from_millis(250);

/// MEASUREMENT ONLY: `f`, with every CPU's counters read as it starts, every
/// [`SAMPLE`] while it runs and as it ends, and the `clock <window>` line of
/// what they say printed after it.
fn clocked<R>(cap: &SysCap, window: &str, f: impl FnOnce() -> R) -> R {
    let (done, sampled) = mpsc::channel::<()>();
    let at = Instant::now();
    let (r, rounds) = std::thread::scope(|s| {
        let sampler = s.spawn(move || {
            let mut rounds = vec![round(cap)];
            // Woken early, and for good, as `f` returns.
            while sampled.recv_timeout(SAMPLE) == Err(mpsc::RecvTimeoutError::Timeout) {
                rounds.push(round(cap));
            }
            rounds.push(round(cap));
            rounds
        });
        let r = f();
        drop(done);
        (r, sampler.join().expect("the clock sampler"))
    });
    println!("https_download: clock {window} {}", clock_words(&rounds, at.elapsed().as_secs_f64()));
    r
}

/// MEASUREMENT ONLY: the clock line's words over `rounds`, the first read
/// `secs` before the last:
///
/// - `cpu<i>=c0:c1:c6:c7:ghz`: each CPU's share of its stamp in C0 (MPERF),
///   in core C6 and C7 (the core's residency, so both threads of a core read
///   it alike), C1 as the rest (`turbostat`'s `CPU%c1`), and its clock while
///   in C0 (APERF over MPERF at the stamp's rate);
/// - `pkg=pc2:pc3:pc6:pc7:pc8:pc9:pc10`: the package's shares of CPU 0's stamp;
/// - `pkg_w`, `pl1_w`/`pl2_w` (each with its enable bit), `energy_uj`;
/// - `ratio=[<ratio>:<reads>]`: `IA32_PERF_STATUS` bits 15:8 over every CPU's
///   every read after the first;
/// - `limits_active=[<bit>:<reads>]`: of those reads, in how many some CPU's
///   `MSR_CORE_PERF_LIMIT_REASONS` had that status bit (15:0) set;
///   `limits_logged`: its log bits (31:16, shifted down) over every read
///   after the first, which is what was logged inside the window;
/// - `hwp_status`: `IA32_HWP_STATUS` over the same reads;
/// - `hwp_caps`, `hwp_request`, `power_ctl` (C1E), `pkg_cst_config`,
///   `pmu_global`: CPU 0's, as read last, with
///   `pmu_global_agree` whether every CPU held CPU 0's;
/// - the window's sums over every CPU: `instructions`, `aperf`, `mperf`,
///   `hitm_loads`, `hitm_rfos`, `pmm_takes`, `pmm_pin_takes`, `pmm_spin`,
///   `pmm_hold` (stamp ticks), and `stale` reads.
fn clock_words(rounds: &[Vec<Record>], secs: f64) -> String {
    let (first, last) = (&rounds[0], &rounds[rounds.len() - 1]);
    let d = |cpu: usize, c: Counter| -> Option<u64> { Some(last.get(cpu)?.get(c)?.wrapping_sub(first.get(cpu)?.get(c)?)) };
    let sum = |c: Counter| -> String {
        let each: Option<Vec<u64>> = (0..last.len()).map(|cpu| d(cpu, c)).collect();
        each.map_or("unread".into(), |each| each.iter().sum::<u64>().to_string())
    };
    let mut words = vec![format!("secs={secs:.3} reads={}", rounds.len())];
    for cpu in 0..last.len() {
        let cell = (|| {
            let tsc = d(cpu, Counter::Stamp)? as f64;
            let mperf = d(cpu, Counter::Mperf)? as f64;
            let aperf = d(cpu, Counter::Aperf)? as f64;
            let (c6, c7) = (d(cpu, Counter::CoreC6)? as f64, d(cpu, Counter::CoreC7)? as f64);
            let c0 = mperf / tsc;
            let ghz = aperf / mperf * tsc / secs / 1e9;
            Some(format!("{c0:.4}:{:.4}:{:.4}:{:.4}:{ghz:.3}", 1.0 - c0 - c6 / tsc - c7 / tsc, c6 / tsc, c7 / tsc))
        })();
        words.push(format!("cpu{cpu}={}", cell.unwrap_or_else(|| "unread".into())));
    }
    let pkg = (|| {
        let tsc = d(0, Counter::Stamp)? as f64;
        let each: Option<Vec<String>> = [
            Counter::PkgC2,
            Counter::PkgC3,
            Counter::PkgC6,
            Counter::PkgC7,
            Counter::PkgC8,
            Counter::PkgC9,
            Counter::PkgC10,
        ]
        .into_iter()
        .map(|c| Some(format!("{:.4}", d(0, c)? as f64 / tsc)))
        .collect();
        Some(each?.join(":"))
    })();
    words.push(format!("pkg={}", pkg.unwrap_or_else(|| "unread".into())));
    let energy = d(0, Counter::PkgEnergyUj);
    words.push(format!(
        "pkg_w={} energy_uj={}",
        energy.map_or("unread".into(), |uj| format!("{:.3}", uj as f64 / 1e6 / secs)),
        energy.map_or("unread".into(), |uj| uj.to_string())
    ));
    let limit = (|| {
        let raw = last.first()?.get(Counter::PkgPowerLimit)?;
        let unit = 1.0 / (1u64 << (last.first()?.get(Counter::RaplPowerUnit)? & 0xF)) as f64;
        Some(format!(
            "pl1_w={:.3}/{} pl2_w={:.3}/{} pkg_power_limit={raw:#x}",
            (raw & 0x7FFF) as f64 * unit,
            raw >> 15 & 1,
            (raw >> 32 & 0x7FFF) as f64 * unit,
            raw >> 47 & 1
        ))
    })();
    words.push(limit.unwrap_or_else(|| "pl1_w=unread".into()));
    let mut ratio = std::collections::BTreeMap::<u64, u64>::new();
    let mut active = std::collections::BTreeMap::<u32, u64>::new();
    let (mut logged, mut hwp_status, mut stale) = (0u64, 0u64, 0u64);
    for r in &rounds[1..] {
        let mut now = 0u64;
        for record in r {
            stale += u64::from(record.stale);
            if let Some(v) = record.get(Counter::PerfStatus) {
                *ratio.entry(v >> 8 & 0xFF).or_default() += 1;
            }
            if let Some(v) = record.get(Counter::PerfLimitReasons) {
                now |= v & 0xFFFF;
                logged |= v >> 16 & 0xFFFF;
            }
            hwp_status |= record.get(Counter::HwpStatus).unwrap_or(0);
        }
        for bit in 0..16 {
            if now & 1 << bit != 0 {
                *active.entry(bit).or_default() += 1;
            }
        }
    }
    let map = |m: Vec<String>| format!("[{}]", m.join(" "));
    words.push(format!("ratio={}", map(ratio.iter().map(|(r, n)| format!("{r}:{n}")).collect())));
    words.push(format!("limits_active={}", map(active.iter().map(|(b, n)| format!("{b}:{n}")).collect())));
    words.push(format!("limits_logged={logged:#x} hwp_status={hwp_status:#x}"));
    let cpu0 = |c: Counter| last.first().and_then(|r| r.get(c)).map_or("unread".into(), |v| format!("{v:#x}"));
    let global = last.first().and_then(|r| r.get(Counter::PmuGlobalCtrl));
    words.push(format!(
        "hwp_caps={} hwp_request={} power_ctl={} pkg_cst_config={} pmu_global={} pmu_global_agree={}",
        cpu0(Counter::HwpCapabilities),
        cpu0(Counter::HwpRequest),
        cpu0(Counter::PowerCtl),
        cpu0(Counter::PkgCstConfig),
        cpu0(Counter::PmuGlobalCtrl),
        last.iter().all(|r| r.get(Counter::PmuGlobalCtrl) == global)
    ));
    for (name, c) in [
        ("instructions", Counter::Instructions),
        ("aperf", Counter::Aperf),
        ("mperf", Counter::Mperf),
        ("hitm_loads", Counter::HitmLoads),
        ("hitm_rfos", Counter::HitmRfos),
        ("pmm_takes", Counter::PmmLockTakes),
        ("pmm_pin_takes", Counter::PmmPinTakes),
        ("pmm_spin", Counter::PmmLockSpinCycles),
        ("pmm_hold", Counter::PmmLockHoldCycles),
    ] {
        words.push(format!("{name}={}", sum(c)));
    }
    words.push(format!("stale={stale}"));
    words.join(" ")
}

/// MEASUREMENT ONLY: laps of [`hitm_control`]'s ring.
const LAPS: u64 = 100_000;
/// MEASUREMENT ONLY: its threads: four on a machine of two threads per core
/// span at least two cores, so at least two of every lap's hops cross one.
const RING: u64 = 4;
/// MEASUREMENT ONLY: the hang ceiling on the ring's laps.
const RING_CEILING: Duration = Duration::from_secs(3);

/// MEASUREMENT ONLY: the HITM counters' positive control. A token goes round
/// a ring of [`RING`] spinning threads [`LAPS`] times; each holder reads the
/// token another thread just wrote, a load that hits a line modified in the
/// writer's core wherever the two are on different cores, and stores to a
/// scratch line the previous holder modified and nobody read since, a demand
/// RFO that does the same. At least half of the hops cross a core, so a live
/// pair of counters reads at least a quarter of the hops each.
fn hitm_control(cap: &SysCap) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    #[repr(align(64))]
    struct Line(AtomicU64);
    let (token, scratch) = (Line(AtomicU64::new(0)), Line(AtomicU64::new(0)));
    // Set by the first thread past the ceiling: the line then says so, and the
    // download it comes before still runs.
    let gave_up = std::sync::atomic::AtomicBool::new(false);
    let hops = LAPS * RING;
    let before = round(cap);
    let at = Instant::now();
    std::thread::scope(|s| {
        for me in 0..RING {
            let (token, scratch, gave_up) = (&token, &scratch, &gave_up);
            s.spawn(move || {
                let mut mine = me;
                while mine < hops {
                    let mut spins = 0u64;
                    while token.0.load(Ordering::Acquire) != mine {
                        core::hint::spin_loop();
                        spins += 1;
                        // A holder that shares this CPU runs only once this one gives it up.
                        if spins % 1024 == 0 {
                            std::thread::yield_now();
                        }
                        if spins % 4096 == 0 && (gave_up.load(Ordering::Relaxed) || at.elapsed() > RING_CEILING) {
                            gave_up.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    scratch.0.store(mine, Ordering::Relaxed);
                    token.0.store(mine + 1, Ordering::Release);
                    mine += RING;
                }
            });
        }
    });
    let secs = at.elapsed().as_secs_f64();
    let after = round(cap);
    let hops = token.0.load(Ordering::Relaxed);
    let ended = if gave_up.load(Ordering::Relaxed) { "gave up at the ceiling" } else { "whole" };
    let sum = |c: Counter| -> Option<u64> {
        before.iter().zip(&after).map(|(a, b)| Some(b.get(c)?.wrapping_sub(a.get(c)?))).sum()
    };
    let word = |v: Option<u64>| v.map_or("unread".into(), |v| v.to_string());
    let (loads, rfos) = (sum(Counter::HitmLoads), sum(Counter::HitmRfos));
    let live = |v: Option<u64>| v.map_or("unread", |v| if v >= hops / 4 { "live" } else { "dead" });
    format!(
        "hitm control {ended} hops={hops} secs={secs:.3} ns_per_hop={:.1} hitm_loads={} ({}) hitm_rfos={} ({}) aperf={} instructions={}",
        secs * 1e9 / hops as f64,
        word(loads),
        live(loads),
        word(rfos),
        live(rfos),
        word(sum(Counter::Aperf)),
        word(sum(Counter::Instructions)),
    )
}
