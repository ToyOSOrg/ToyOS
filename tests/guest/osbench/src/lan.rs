//! The machine's link to the other bench, through std's `net` on both
//! systems (`osbench-lan`): this machine broadcasts for the `osbench-peer`
//! that bench's Linux runs as the fixed reference, then times TCP to it.
//! Every connection is this machine's own, so a system that accepts none
//! from its network is measured all the same.
//!
//! Each result is named for the link that carried it, `lan.wired.*` or
//! `lan.wifi.*`, and the link not measured says why: a Linux run measures
//! whichever link reaches the peer, and ToyOS drives Ethernet alone
//! (`issues/there-is-no-wifi.md`). `lan.link` and `lan.peer_link` describe
//! both ends' links; no address is printed.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use osbench_lan::link::Link;
use osbench_lan::Peer;

use crate::report::{condition, refuse, result, unmeasured, unsupported};

/// How long a peer is asked for, which a boot's lease lands inside too.
const DISCOVERY: Duration = Duration::from_secs(60);
/// Each transfer's span, and the rounds timed after one unread.
const SPAN: Duration = Duration::from_secs(3);
const ROUNDS: usize = 5;

fn benches(link: Link) -> [String; 4] {
    ["tcp.connect", "tcp.rtt.64", "tcp.upload", "tcp.download"].map(|b| format!("lan.{}.{b}", link.name()))
}

pub fn suite() {
    if cfg!(target_os = "toyos") {
        for bench in benches(Link::Wifi) {
            unsupported(&bench, "ToyOS has no Wi-Fi driver: issues/there-is-no-wifi.md");
        }
    }
    let broadcast = SocketAddr::from((Ipv4Addr::BROADCAST, osbench_lan::PORT));
    let started = Instant::now();
    let found = match osbench_lan::discover(broadcast, DISCOVERY) {
        Ok(Some(found)) => found,
        Ok(None) => return refuse("lan", format!("no osbench-peer answered on the link within {DISCOVERY:?}")),
        Err(e) if cfg!(target_os = "toyos") && e.kind() == std::io::ErrorKind::NotConnected => {
            return benches(Link::Wired).iter().for_each(|b| unsupported(b, format!("no network stack answers: {e}")));
        }
        Err(e) => return refuse("lan", format!("no socket to look for a peer with: {e}")),
    };
    condition("lan.peer_found_after_ms", started.elapsed().as_millis());
    condition("lan.peer_link", &found.link);
    let peer = Peer(SocketAddr::new(found.ip, osbench_lan::PORT));
    let link = match peer.local() {
        Ok(local) => crate::sys::link_of(local.ip()),
        Err(e) => return refuse("lan", format!("the peer answered and took no connection: {e}")),
    };
    let (measured, said) = match link {
        Ok((link, said)) => (link, said),
        Err(why) => return refuse("lan", why),
    };
    condition("lan.link", said);
    let other = if measured == Link::Wired { Link::Wifi } else { Link::Wired };
    if cfg!(target_os = "linux") {
        for bench in benches(other) {
            unmeasured(&bench, &format!("this run reached its peer over its {} link", measured.name()), &[]);
        }
    }
    measure(&peer, &benches(measured));
}

fn measure(peer: &Peer, [connect, rtt, upload, download]: &[String; 4]) {
    crate::measure::each(connect, 5, 50, &[], |_| peer.connect().map_err(|e| format!("connect: {e}")));
    match peer.echo(64) {
        Ok(mut echo) => crate::measure::each(rtt, 100, 2000, &[("nodelay", "1".into())], |_| {
            echo.round_trip().map_err(|e| format!("echo: {e}"))
        }),
        Err(e) => refuse(rtt, format!("the echo would not open: {e}")),
    }
    type Transfer = fn(&Peer, Duration, &mut dyn FnMut(&std::net::TcpStream)) -> std::io::Result<osbench_lan::Transfer>;
    let download_of: Transfer = |peer, span, _| peer.download(span);
    for (bench, run) in [(upload, Peer::upload as Transfer), (download, download_of)] {
        let mut stack = Vec::new();
        let mut ramps = Vec::new();
        let mut rounds = || -> Result<Vec<u64>, String> {
            run(peer, SPAN, &mut |_| {}).map_err(|e| e.to_string())?;
            crate::clock::settle();
            let before = crate::sys::net_counters();
            let mut log = crate::sys::StreamLog::new();
            let mbps = (0..ROUNDS)
                .map(|_| {
                    let moved = run(peer, SPAN, &mut |stream| log.look(stream)).map_err(|e| e.to_string())?;
                    ramps.extend(moved.ramp);
                    Ok(moved.bytes * 1000 / moved.ns)
                })
                .collect::<Result<Vec<u64>, String>>()?;
            stack = stack_moved(before, crate::sys::net_counters());
            stack.extend(log.fields());
            Ok(mbps)
        };
        match rounds() {
            Ok(mut mbps) => {
                let mut extra = vec![("span_ms".to_string(), SPAN.as_millis().to_string())];
                extra.extend(stack);
                extra.extend(ramp_means(&ramps));
                let extra: Vec<(&str, String)> = extra.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                result(bench, "MB/s", &mut mbps, &extra)
            }
            Err(why) => refuse(bench, why),
        }
    }
}

/// The bytes the peer had received by each of its ramp marks, as a mean over the rounds: how
/// fast each round's new connection opened, read at the receiver for every sender alike.
fn ramp_means(ramps: &[osbench_lan::Ramp]) -> Vec<(String, String)> {
    if ramps.is_empty() {
        return Vec::new();
    }
    osbench_lan::RAMP_MARKS_MS
        .iter()
        .enumerate()
        .map(|(at, ms)| {
            let sum: u64 = ramps.iter().map(|ramp| ramp.0[at]).sum();
            (format!("peer_received_by_{ms}ms_bytes_mean"), (sum / ramps.len() as u64).to_string())
        })
        .collect()
}

/// What the stack's counters moved by over a row's timed rounds, each as a field named
/// `stack_<counter>`; a reading that failed is one field saying why.
fn stack_moved(before: Result<Vec<(String, u64)>, String>, after: Result<Vec<(String, u64)>, String>) -> Vec<(String, String)> {
    match (before, after) {
        (Ok(before), Ok(after)) => after
            .iter()
            .filter_map(|(key, now)| {
                let was = before.iter().find(|(k, _)| k == key)?.1;
                let field = format!("stack_{}", key.replace(['.', '-'], "_"));
                Some((field, now.saturating_sub(was).to_string()))
            })
            .collect(),
        (Err(why), _) | (_, Err(why)) => vec![("stack_unread".to_string(), why)],
    }
}
