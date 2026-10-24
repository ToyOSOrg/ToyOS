//! Exploratory rows over the public internet, run only by name and never by
//! `all` or a judged boot, since no automated test depends on the public
//! internet: evidence of a path neither system owns, not a regression test.
//!
//! The one endpoint is Cloudflare's speed test at [`HOST`], over HTTPS:
//! `__down?bytes=N` answers N bytes and `__up` takes a POST's body, the
//! endpoints Cloudflare's own measurement engine drives
//! (<https://github.com/cloudflare/speedtest>). It refuses a download of
//! 100 MB. The round trip asks the same host's `cdn-cgi/trace`, which its edge
//! answers without the speed test's own processing. Answers name the client's
//! address and place, and nothing here keeps or prints them. TLS is rustls on
//! toyos-crypto's provider on both systems, so the cryptography is the same
//! code.

use std::io::{ErrorKind, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::measure::each;
use crate::report::{condition, refuse, unsupported};

const HOST: &str = "speed.cloudflare.com";
const USER_AGENT: &str = "toyos-build (https://github.com/ToyOSOrg/ToyOS)";
/// One download's bytes and one upload's, each timed whole: a slow uplink's
/// rate needs more resolution than whole MB/s.
const DOWN: u64 = 10_000_000;
const UP: usize = 10_000_000;
/// Timed transfers each way after one unread. The endpoint answers 429 past
/// a download volume it does not publish, so a session of runs stays well
/// under the volume that first drew one.
const ROUNDS: usize = 3;
/// The most a short answer is read to.
const ANSWER_MAX: u64 = 64 * 1024;
/// How long one connection or request may take before it is given up.
const PATIENCE: Duration = Duration::from_secs(30);
/// How long the address may take to arrive, as `lan`'s peer may.
const LEASE: Duration = Duration::from_secs(60);

const BENCHES: [&str; 4] = ["internet.tcp.connect", "internet.https.rtt", "internet.https.download", "internet.https.upload"];

pub fn suite() {
    condition("internet.endpoint", HOST);
    if let Err(e) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        if cfg!(target_os = "toyos") && e.kind() == ErrorKind::NotConnected {
            return BENCHES.iter().for_each(|b| unsupported(b, format!("no network stack answers: {e}")));
        }
        return refuse("internet", format!("no socket: {e}"));
    }
    let addr = match resolve() {
        Ok(addr) => addr,
        Err(why) => return refuse("internet", why),
    };
    match TcpStream::connect_timeout(&addr, PATIENCE).and_then(|s| s.local_addr()) {
        Ok(local) => match crate::sys::link_of(local.ip()) {
            Ok((_, said)) => condition("internet.link", said),
            Err(why) => return refuse("internet", why),
        },
        Err(e) => return refuse("internet", format!("{HOST} took no connection: {e}")),
    }
    let [connect, rtt, download, upload] = BENCHES;

    each(connect, 2, 20, &[], |_| TcpStream::connect_timeout(&addr, PATIENCE).map(drop).map_err(|e| format!("connect: {e}")));

    let agent = agent();
    let url = |path: &str| format!("https://{HOST}/{path}");
    let get = |path: &str, most: u64| -> Result<u64, String> {
        let mut answer = agent.get(url(path)).call().map_err(|e| format!("GET: {e}"))?;
        drain(answer.body_mut(), most)
    };
    each(rtt, 5, 50, &[("path", "cdn-cgi/trace".into())], |_| get("cdn-cgi/trace", ANSWER_MAX).map(drop));
    let down = format!("__down?bytes={DOWN}");
    each(download, 1, ROUNDS, &[("bytes", DOWN.to_string())], |_| match get(&down, DOWN)? {
        DOWN => Ok(()),
        n => Err(format!("the download ended at {n} of {DOWN} bytes")),
    });
    let sent = vec![0x5au8; UP];
    each(upload, 1, ROUNDS, &[("bytes", UP.to_string())], |_| {
        let mut answer = agent.post(url("__up")).send(&sent[..]).map_err(|e| format!("POST: {e}"))?;
        drain(answer.body_mut(), ANSWER_MAX).map(drop)
    });
}

/// An answer's body read to its end, unkept, so its connection serves the
/// next request: its length, refused past `most`.
fn drain(body: &mut ureq::Body, most: u64) -> Result<u64, String> {
    match std::io::copy(&mut body.as_reader().take(most + 1), &mut std::io::sink()) {
        Ok(n) if n <= most => Ok(n),
        Ok(_) => Err(format!("an answer ran past {most} bytes")),
        Err(e) => Err(format!("an answer's body: {e}")),
    }
}

/// [`HOST`]'s first IPv4 address. A ToyOS netstack answers `NotConnected`
/// until its lease lands and tells no one when it does, so a lookup it
/// answers so is asked again each second for up to [`LEASE`].
fn resolve() -> Result<SocketAddr, String> {
    let start = Instant::now();
    loop {
        match (HOST, 443).to_socket_addrs() {
            Ok(mut addrs) => return addrs.find(SocketAddr::is_ipv4).ok_or_else(|| format!("{HOST} has no IPv4 address")),
            Err(e) if e.kind() == ErrorKind::NotConnected && start.elapsed() < LEASE => std::thread::sleep(Duration::from_secs(1)),
            Err(e) => return Err(format!("{HOST} did not resolve: {e}")),
        }
    }
}

/// One agent, so every request after the first rides a connection it keeps.
fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().unversioned_rustls_crypto_provider(provider()).build();
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_global(Some(PATIENCE))
        .tls_config(tls)
        .build()
        .new_agent()
}

/// The system's random bytes, which the provider's key exchanges draw.
#[derive(Debug)]
struct SystemRandom;

impl rustls::crypto::SecureRandom for SystemRandom {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), rustls::crypto::GetRandomFailed> {
        getrandom::fill(bytes).map_err(|_| rustls::crypto::GetRandomFailed)
    }
}

static TLS: toyos_crypto::rustls_provider::Backend = toyos_crypto::rustls_provider::Backend::new(&SystemRandom);

/// toyos-crypto's provider, exchanging keys on X25519 alone, hybrid or not,
/// as the build's own requests do: no P-256 or P-384 secret meets a server's
/// input while the T14 measures those curves leaking
/// (`issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`).
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    let mut provider = TLS.provider();
    provider.kx_groups.retain(|group| matches!(group.name(), rustls::NamedGroup::X25519MLKEM768 | rustls::NamedGroup::X25519));
    Arc::new(provider)
}
