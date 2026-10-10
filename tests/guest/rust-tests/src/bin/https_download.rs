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

use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall;
use toyos_logstream::program_line;

#[path = "../https_client.rs"]
mod https_client;
#[path = "../served_log.rs"]
mod served_log;

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

fn main() {
    let bound_ms: u64 = std::env::var(toyos_tco::LIST_BOUND_ENV)
        .unwrap_or_else(|e| panic!("{}: {e}; this job runs only in a job list", toyos_tco::LIST_BOUND_ENV))
        .parse()
        .unwrap_or_else(|e| panic!("{}: {e}", toyos_tco::LIST_BOUND_ENV));
    let since_boot_ms = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    let window = Duration::from_millis(bound_ms.saturating_sub(MARGIN_MS).saturating_sub(since_boot_ms));
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
        leased();
        *rtt.lock().unwrap_or_else(PoisonError::into_inner) = handshakes();

        // MEASUREMENT ONLY: the file twice, back to back, so a CDN's cold first fetch is told apart.
        for _ in 0..RUNS {
            *progress.lock().unwrap_or_else(PoisonError::into_inner) = Progress { bytes: 0, first: None, last: None };
            let url = format!("https://{HOST}{PATH}");
            let response = https_client::agent(ROOTS).get(&url).call().unwrap_or_else(|e| panic!("GET {url}: {e}"));
            let mut body = response.into_body().into_reader();
            let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
            let mut chunk = vec![0u8; 64 * 1024];
            let mut bytes = 0u64;
            loop {
                let n = body.read(&mut chunk).unwrap_or_else(|e| panic!("the body of {url} after {bytes} bytes: {e}"));
                if n == 0 {
                    break;
                }
                digest.update(&chunk[..n]);
                bytes += n as u64;
                let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                progress.bytes = bytes;
                progress.last = Some(Instant::now());
                if progress.first.is_none() {
                    progress.first = Some((Instant::now(), round(&cap), bytes));
                }
            }
            drop(body);
            let progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
            let rtt = rtt.lock().unwrap_or_else(PoisonError::into_inner);
            let sha256 = https_client::hex(digest.finish());
            println!("https_download: {}", report("whole", Some(&sha256), &progress, &rtt, &cap));
        }
        drop(transferring);
    });
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

/// [`PROBES`] handshakes with [`HOST`]'s first address, each closed as it is
/// made, as the `rtt_ms` the module header gives.
fn handshakes() -> String {
    let addr = (HOST, 443)
        .to_socket_addrs()
        .unwrap_or_else(|e| panic!("look up {HOST}: {e}"))
        .next()
        .unwrap_or_else(|| panic!("{HOST} has no address"));
    let each: Vec<String> = (0..PROBES)
        .map(|_| {
            let at = Instant::now();
            drop(TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to {HOST}: {e}")));
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
