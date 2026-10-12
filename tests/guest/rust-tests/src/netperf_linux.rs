//! MEASUREMENT ONLY, never lands: `https_download`'s measurement on Linux, for
//! the same machine's Ubuntu reading. Not a bin of this crate: a scratch
//! project builds it for `x86_64-unknown-linux-gnu` with this crate's profile.
//!
//! Run as root, for `/dev/cpu/*/msr` (`modprobe msr` first): the same idle
//! window, suite probe and benches as on ToyOS, then [`RUNS`] downloads of the
//! same file on the same client, each said as `perf run`, and the per-MB
//! table of all of it. Every CPU's TSC, `IA32_APERF` and `IA32_MPERF` are read
//! through its `msr` device; `app.*` is `getrusage(RUSAGE_SELF)`; `lx.*` is
//! `/proc/stat`'s whole-machine `cpu` line in ms, `ctxt` and `intr`, and
//! `/proc/softirqs`' `NET_RX`.
//!
//! The connection-setup scout: each download also says `setup run=<n>
//! os=linux asked=<any|v4> <words>` (`netperf::Fetched::setup_words`), its
//! connection's `TCP_INFO` read as the TCP connector returns it. `netperf-linux
//! v4` connects only to IPv4 addresses (`ureq`'s `IpFamily::Ipv4Only`), and
//! `netperf-linux table <file>...` renders the setup rows of runs said before.

use std::fs::File;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::FileExt;
use std::time::Instant;

#[path = "https_client.rs"]
#[allow(dead_code)]
mod https_client;
#[path = "netperf.rs"]
mod netperf;
#[path = "netperf_table.rs"]
mod netperf_table;
#[path = "setup_table.rs"]
mod setup_table;

use netperf::{Machine, Snap};

const HOST: &str = "static.rust-lang.org";
const PATH: &str = "/dist/2024-09-05/rust-1.81.0-x86_64-unknown-linux-gnu.tar.xz";
const ROOTS: &str = "/etc/ssl/certs/ca-certificates.crt";
const RUNS: usize = 3;
const IDLE_SECS: f64 = 2.0;
/// `/proc/stat`'s unit, `USER_HZ`, in ms.
const TICK_MS: u64 = 10;

struct Linux {
    msrs: Vec<File>,
}

impl Linux {
    fn open() -> Self {
        let mut msrs = Vec::new();
        for cpu in 0.. {
            let path = format!("/dev/cpu/{cpu}/msr");
            match File::open(&path) {
                Ok(f) => msrs.push(f),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => panic!("{path}: {e} (run as root, after `modprobe msr`)"),
            }
        }
        assert!(!msrs.is_empty(), "no /dev/cpu/*/msr: `modprobe msr` first");
        Self { msrs }
    }
}

fn msr(f: &File, at: u64) -> u64 {
    let mut b = [0u8; 8];
    f.read_exact_at(&mut b, at).unwrap_or_else(|e| panic!("MSR {at:#x}: {e}"));
    u64::from_le_bytes(b)
}

impl Machine for Linux {
    fn snap(&self) -> Snap {
        let mut snap = Snap::new();
        for f in &self.msrs {
            for (key, at) in [("stamp", 0x10), ("mperf", 0xE7), ("aperf", 0xE8)] {
                *snap.entry(key.into()).or_default() += msr(f, at);
            }
        }
        let stat = std::fs::read_to_string("/proc/stat").expect("/proc/stat");
        for line in stat.lines() {
            let mut w = line.split_whitespace();
            match w.next() {
                Some("cpu") => {
                    let v: Vec<u64> = w.map(|x| x.parse().unwrap()).collect();
                    for (i, name) in ["user", "nice", "sys", "idle", "iowait", "irq", "softirq"].iter().enumerate() {
                        snap.insert(format!("lx.{name}_ms"), v[i] * TICK_MS);
                    }
                }
                Some("ctxt") => {
                    snap.insert("lx.switches".into(), w.next().unwrap().parse().unwrap());
                }
                Some("intr") => {
                    snap.insert("lx.irqs".into(), w.next().unwrap().parse().unwrap());
                }
                _ => {}
            }
        }
        let softirqs = std::fs::read_to_string("/proc/softirqs").expect("/proc/softirqs");
        for line in softirqs.lines() {
            let mut w = line.split_whitespace();
            if w.next() == Some("NET_RX:") {
                snap.insert("lx.net_rx_softirqs".into(), w.map(|x| x.parse::<u64>().unwrap()).sum());
            }
        }
        // SAFETY: `getrusage` writes the one struct it is handed.
        let ru = unsafe {
            let mut ru = std::mem::zeroed::<libc::rusage>();
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut ru), 0);
            ru
        };
        let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000;
        snap.insert("app.cpu_ns".into(), ns(ru.ru_utime) + ns(ru.ru_stime));
        snap.insert("app.user_ns".into(), ns(ru.ru_utime));
        snap.insert("app.sys_ns".into(), ns(ru.ru_stime));
        snap.insert("app.nvcsw".into(), ru.ru_nvcsw as u64);
        snap.insert("app.nivcsw".into(), ru.ru_nivcsw as u64);
        snap
    }

    fn tsc(&self) -> u64 {
        // SAFETY: reads the time-stamp counter, which Ring 3 may.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}

/// The established TCP connection to one of `addrs`, as the kernel holds it:
/// its peer's family, whether that peer is the first address, and the round
/// trip the handshake measured.
fn tcp_info(addrs: &[SocketAddr]) -> String {
    for fd in 3..1024 {
        // SAFETY: each call writes at most the length it is handed into the
        // struct it is handed, and an fd that is no socket is refused.
        let (info, peer) = unsafe {
            let mut info = std::mem::zeroed::<libc::tcp_info>();
            let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
            if libc::getsockopt(fd, libc::IPPROTO_TCP, libc::TCP_INFO, &mut info as *mut _ as *mut libc::c_void, &mut len) != 0 {
                continue;
            }
            let mut ss = std::mem::zeroed::<libc::sockaddr_storage>();
            let mut sl = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            if libc::getpeername(fd, &mut ss as *mut _ as *mut libc::sockaddr, &mut sl) != 0 {
                continue;
            }
            let peer = match i32::from(ss.ss_family) {
                libc::AF_INET => {
                    let sin = &*(&ss as *const _ as *const libc::sockaddr_in);
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))), u16::from_be(sin.sin_port))
                }
                libc::AF_INET6 => {
                    let sin6 = &*(&ss as *const _ as *const libc::sockaddr_in6);
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)), u16::from_be(sin6.sin6_port))
                }
                _ => continue,
            };
            (info, peer)
        };
        // TCP_ESTABLISHED.
        if info.tcpi_state != 1 {
            continue;
        }
        let Some(at) = addrs.iter().position(|a| a.ip() == peer.ip() && a.port() == peer.port()) else { continue };
        return format!(
            " peer_family={} peer_index={at} tcpi_rtt_us={} tcpi_rttvar_us={} tcpi_rto_us={}",
            if peer.is_ipv4() { "v4" } else { "v6" },
            info.tcpi_rtt,
            info.tcpi_rttvar,
            info.tcpi_rto
        );
    }
    " tcpi=none".to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("table") {
        let mut log = String::new();
        for path in &args[1..] {
            log.push_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}")));
        }
        print!("{}", setup_table::table(&log));
        return;
    }
    let (family, asked) = match args.first().map(String::as_str) {
        None => (ureq::config::IpFamily::Any, "any"),
        Some("v4") => (ureq::config::IpFamily::Ipv4Only, "v4"),
        Some(other) => panic!("{other:?}: the one argument is `v4`, or `table` and files"),
    };
    let machine = Linux::open();
    let mut said = String::new();
    let mut say = |line: String| {
        println!("{}{line}", netperf_table::PREFIX);
        said.push_str(&format!("{}{line}\n", netperf_table::PREFIX));
    };
    let idle = netperf::idle(&machine, machine.msrs.len(), IDLE_SECS);
    say(idle.clone());
    let pem = std::fs::read(ROOTS).unwrap_or_else(|e| panic!("read {ROOTS}: {e}"));
    let suite = netperf::suite(&pem, HOST, 443, PATH);
    say(netperf::benches(&machine, &suite, &idle));
    let url = format!("https://{HOST}{PATH}");
    let mut previous = None;
    for run in 1..=RUNS {
        let before = machine.snap();
        let at = Instant::now();
        let fetched = netperf::fetch(https_client::config_family(ROOTS, family), &url, tcp_info, |_| {});
        let secs = at.elapsed().as_secs_f64();
        let d = netperf::delta(&before, &machine.snap());
        say(format!("setup run={run} os=linux asked={asked} {}", fetched.setup_words(previous)));
        previous = fetched.addrs.first().copied();
        say(format!(
            "whole bytes={} sha256={} secs={secs:.3} mbps={:.1} tcp_ms={:.1} tls_ms={:.1}",
            fetched.bytes,
            fetched.sha256,
            fetched.bytes as f64 * 8.0 / secs / 1e6,
            fetched.tcp_ms,
            fetched.tls_ms
        ));
        say(format!("perf run={run} bytes={} secs={secs:.3} {}", fetched.bytes, netperf::words(&d)));
    }
    print!("{}", netperf_table::table(&said));
    print!("{}", setup_table::table(&said));
}
