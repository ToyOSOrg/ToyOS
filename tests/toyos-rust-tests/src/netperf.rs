//! MEASUREMENT ONLY, never lands: where a download's CPU goes, said the same
//! way on ToyOS (`https_download`) and on Linux (`netperf_linux.rs`), and the
//! per-MB table the `internet_download` judge and the Linux job both render
//! from those words.
//!
//! A [`Snap`] is every counter a machine gives, summed over its CPUs, by name:
//! `stamp`, `aperf` and `mperf` are the CPUs' own (TSC, `IA32_APERF`,
//! `IA32_MPERF`); `app.*` is the job's own accounting; `k.*` is ToyOS's kernel
//! counters and `lx.*` Linux's `/proc/stat`. The job says three kinds of line:
//!
//! ```text
//! https_download: perf idle secs=<s> cpus=<n> tsc_hz=<hz> <delta words>
//! https_download: bench suite=<suite> aead=<alg> <x>_aperf_per_b=<c> <x>_tsc_per_b=<c> ...
//! https_download: perf run=<k> bytes=<n> secs=<s> <delta words>
//! ```
//!
//! and netstack says `netstack: prof <words>` as each stream ends.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{ConnectionDetails, Connector, RustlsConnector, TcpConnector, Transport};

use crate::netperf_table::field;

pub type Snap = BTreeMap<String, u64>;

/// What a machine reads of itself.
pub trait Machine {
    fn snap(&self) -> Snap;
    /// The CPU's free-running counter on the calling CPU.
    fn tsc(&self) -> u64;
}

/// Every key both hold, `b - a`.
pub fn delta(a: &Snap, b: &Snap) -> Snap {
    a.iter().filter_map(|(k, va)| Some((k.clone(), b.get(k)?.wrapping_sub(*va)))).collect()
}

pub fn words(snap: &Snap) -> String {
    snap.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

/// The idle window: nothing of the job's runs for `secs`.
pub fn idle(machine: &dyn Machine, cpus: usize, secs: f64) -> String {
    let before = machine.snap();
    let at = Instant::now();
    // A window of nothing, measured: not a wait on any event.
    std::thread::sleep(Duration::from_secs_f64(secs));
    let after = machine.snap();
    let secs = at.elapsed().as_secs_f64();
    let d = delta(&before, &after);
    let tsc_hz = d.get("stamp").map_or(0.0, |s| *s as f64 / cpus as f64 / secs);
    format!("perf idle secs={secs:.3} cpus={cpus} tsc_hz={tsc_hz:.0} {}", words(&d))
}

/// One kernel, timed by the whole machine's APERF less the idle window's rate
/// and by the calling CPU's own counter, per byte.
fn bench(machine: &dyn Machine, idle_aperf_per_s: f64, mut each: impl FnMut() -> usize) -> (f64, f64) {
    const LEAST: Duration = Duration::from_millis(300);
    let before = machine.snap();
    let at = Instant::now();
    let t0 = machine.tsc();
    let mut bytes = 0u64;
    while at.elapsed() < LEAST {
        for _ in 0..16 {
            bytes += each() as u64;
        }
    }
    let ticks = machine.tsc().wrapping_sub(t0);
    let secs = at.elapsed().as_secs_f64();
    let after = machine.snap();
    let d = delta(&before, &after);
    let aperf = d.get("aperf").map_or(f64::NAN, |a| *a as f64 - idle_aperf_per_s * secs);
    (aperf / bytes as f64, ticks as f64 / bytes as f64)
}

/// The negotiated suite's AEAD sealing a TLS record's 16 KiB in place, SHA-256
/// over the job's 64 KiB reads, and a 64 KiB copy.
pub fn benches(machine: &dyn Machine, suite: &str, idle_line: &str) -> String {
    let idle = field(idle_line, "aperf").unwrap_or(0.0) / field(idle_line, "secs").unwrap_or(1.0);
    let (alg, name): (&'static ring::aead::Algorithm, &str) = if suite.contains("AES_128_GCM") {
        (&ring::aead::AES_128_GCM, "aes128gcm")
    } else if suite.contains("AES_256_GCM") {
        (&ring::aead::AES_256_GCM, "aes256gcm")
    } else if suite.contains("CHACHA20") {
        (&ring::aead::CHACHA20_POLY1305, "chacha20poly1305")
    } else {
        panic!("no AEAD for suite {suite}")
    };
    let key = ring::aead::LessSafeKey::new(ring::aead::UnboundKey::new(alg, &[7u8; 32][..alg.key_len()]).unwrap());
    let mut record = vec![0x5au8; 16 * 1024];
    let mut n = 0u64;
    let aead = bench(machine, idle, || {
        n += 1;
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&n.to_be_bytes());
        let tag = key
            .seal_in_place_separate_tag(ring::aead::Nonce::assume_unique_for_key(nonce), ring::aead::Aad::empty(), &mut record)
            .unwrap();
        let _ = black_box(tag);
        record.len()
    });
    let chunk = vec![0xa5u8; 64 * 1024];
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let sha = bench(machine, idle, || {
        digest.update(black_box(&chunk));
        chunk.len()
    });
    black_box(digest.finish());
    let mut to = vec![0u8; 64 * 1024];
    let copy = bench(machine, idle, || {
        black_box(&mut to).copy_from_slice(black_box(&chunk));
        chunk.len()
    });
    format!(
        "bench suite={suite} aead={name} aead_aperf_per_b={:.3} aead_tsc_per_b={:.3} sha_aperf_per_b={:.3} sha_tsc_per_b={:.3} \
         memcpy_aperf_per_b={:.3} memcpy_tsc_per_b={:.3}",
        aead.0, aead.1, sha.0, sha.1, copy.0, copy.1
    )
}

/// The suite `host` at `port` negotiates with this client's offer: ureq's own
/// rustls configuration on ring, trusting `roots`; a `HEAD` of `path` is read
/// to its end, so the server ends the connection as it would any other.
pub fn suite(roots: &[u8], host: &str, port: u16, path: &str) -> String {
    use rustls::pki_types::{CertificateDer, ServerName};
    let mut store = rustls::RootCertStore::empty();
    for item in ureq::tls::parse_pem(roots) {
        if let Ok(ureq::tls::PemItem::Certificate(cert)) = item {
            store.add(CertificateDer::from(cert.der().to_vec())).ok();
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).unwrap();
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
    let mut sock = std::net::TcpStream::connect((host, port)).unwrap_or_else(|e| panic!("connect to {host}: {e}"));
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).unwrap_or_else(|e| panic!("handshake with {host}: {e}"));
    }
    let suite = format!("{:?}", conn.negotiated_cipher_suite().expect("a suite once the handshake is done").suite());
    let mut stream = rustls::Stream::new(&mut conn, &mut sock);
    let head = format!(
        "HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: toyos-build (https://github.com/ToyOSOrg/ToyOS)\r\nConnection: close\r\n\r\n"
    );
    std::io::Write::write_all(&mut stream, head.as_bytes()).unwrap_or_else(|e| panic!("HEAD {path}: {e}"));
    let mut sink = Vec::new();
    // The end of the answer, however the server ends it.
    let _ = stream.read_to_end(&mut sink);
    suite
}

/// A link of an agent's connector chain that passes its transport on and
/// keeps when it did.
#[derive(Clone, Debug, Default)]
pub struct Mark(pub Arc<Mutex<Option<Instant>>>);

impl<In: Transport> Connector<In> for Mark {
    type Out = In;

    fn connect(&self, _: &ConnectionDetails, chained: Option<In>) -> Result<Option<In>, ureq::Error> {
        if chained.is_some() {
            *self.0.lock().unwrap() = Some(Instant::now());
        }
        Ok(chained)
    }
}

pub struct Fetched {
    pub bytes: u64,
    pub sha256: String,
    pub tcp_ms: f64,
    pub tls_ms: f64,
}

/// One GET of `url` on an agent of `config`, every 64 KiB read hashed, `each`
/// told the bytes so far; the agent, and with it the connection, is gone on
/// return.
pub fn fetch(config: ureq::config::Config, url: &str, mut each: impl FnMut(u64)) -> Fetched {
    let (tcp, tls) = (Mark::default(), Mark::default());
    let connector = ().chain(TcpConnector::default()).chain(tcp.clone()).chain(RustlsConnector::default()).chain(tls.clone());
    let agent = ureq::Agent::with_parts(config, connector, DefaultResolver::default());
    let called = Instant::now();
    let response = agent.get(url).call().unwrap_or_else(|e| panic!("GET {url}: {e}"));
    let ready = |mark: &Mark| mark.0.lock().unwrap().map_or(-1.0, |at| at.duration_since(called).as_secs_f64() * 1e3);
    let (tcp_ms, tls_ms) = (ready(&tcp), ready(&tls));
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
        each(bytes);
    }
    drop(body);
    drop(agent);
    let sha256 = digest.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    Fetched { bytes, sha256, tcp_ms, tls_ms }
}

