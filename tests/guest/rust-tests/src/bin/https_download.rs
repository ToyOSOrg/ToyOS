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

/// MEASUREMENT ONLY: downloads of the file, one after another.
const RUNS: usize = 2;

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
        for r in round(self.0) {
            for counter in Counter::ALL {
                let Some(v) = r.get(counter) else { continue };
                let key = match counter {
                    Counter::Stamp | Counter::Aperf | Counter::Mperf => counter.name().to_string(),
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
        let idle = netperf::idle(&machine, cpus, IDLE_SECS);
        println!("https_download: {idle}");
        let pem = std::fs::read(&roots).unwrap_or_else(|e| panic!("read {roots}: {e}"));
        let path = url.splitn(4, '/').nth(3).map_or("/".to_string(), |p| format!("/{p}"));
        let suite = netperf::suite(&pem, &host, port, &path);
        println!("https_download: {}", netperf::benches(&machine, &suite, &idle));

        // MEASUREMENT ONLY: the file twice, back to back, so a CDN's cold first fetch is told apart.
        for run in 1..=RUNS {
            *progress.lock().unwrap_or_else(PoisonError::into_inner) = Progress { bytes: 0, first: None, last: None };
            let before = machine.snap();
            let at = Instant::now();
            let fetched = netperf::fetch(https_client::config(&roots), &url, |bytes| {
                let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                progress.bytes = bytes;
                progress.last = Some(Instant::now());
                if progress.first.is_none() {
                    progress.first = Some((Instant::now(), round(cap), bytes));
                }
            });
            let secs = at.elapsed().as_secs_f64();
            let d = netperf::delta(&before, &machine.snap());
            let progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
            let rtt = rtt.lock().unwrap_or_else(PoisonError::into_inner);
            println!(
                "https_download: {} tcp_ms={:.1} tls_ms={:.1}",
                report("whole", Some(&fetched.sha256), &progress, &rtt, cap),
                fetched.tcp_ms,
                fetched.tls_ms
            );
            println!("https_download: perf run={run} bytes={} secs={secs:.3} {}", fetched.bytes, netperf::words(&d));
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
