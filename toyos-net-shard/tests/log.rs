//! Refusals reach the shell through the shard's per-rule limiter: at most one line per rule in
//! any 10 s, carrying how many it stood for (`tcp.md` §14.2, `ip.md` §13.1, `udp-dhcp.md`
//! §U12.2).

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_ip::Peer;
use toyos_net_shard::{Event, Refusal};

/// `wire.md` §12: B to A, UDP with a Loose Source Route option.
const V_IP_LSRR: &str = "
47 00 00 26 00 00 40 00 40 11 2e f9 c0 00 02 02
c0 00 02 01 83 07 04 c0 00 02 fe 00 13 88 13 89
00 0a ec 5b 68 69";

fn refusals(events: &[Event]) -> Vec<(Refusal, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Refused { refusal, suppressed } => Some((*refusal, *suppressed)),
            _ => None,
        })
        .collect()
}

#[test]
fn s_ip_mod_001_shard_one_line_per_rule_per_10_s() {
    let (mut net, a, _) = segment();
    let start = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.drain_events().for_each(drop);
    let frame = from_b(&hex(V_IP_LSRR));
    for n in 0..100 {
        shard.receive(start.after(Duration::from_millis(n * 10)), &frame);
    }
    shard.receive(start.after(Duration::from_millis(10_001)), &frame);
    let lines = refusals(&shard.drain_events().collect::<Vec<_>>());
    let rules: Vec<(toyos_net_ip::Counter, Peer, u64)> = lines
        .iter()
        .map(|(r, n)| match r {
            Refusal::Ip(r) => (r.rule, r.peer, *n),
            other => panic!("{other:?}"),
        })
        .collect();
    let rule = toyos_net_ip::Counter::IpSourceRoute;
    assert_eq!(rules, [(rule, Peer::Ip(B), 0), (rule, Peer::Ip(B), 99)]);
    assert_eq!(shard.ip().counters().get(rule), 101);
}

#[test]
fn s_mod_005_shard_one_line_per_rule_per_10_s() {
    let (mut net, a, _) = segment();
    let start = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.drain_events().for_each(drop);
    let syn_fin = |n: u8| from_b(&segment_bytes((Ipv4Addr::new(192, 0, 2, 10 + n), 1234), (A, 80), 5_000, 0x03));
    for n in 0..50 {
        shard.receive(start.after(Duration::from_millis(u64::from(n))), &syn_fin(n));
    }
    shard.receive(start.after(Duration::from_millis(10_000)), &syn_fin(50));
    let lines: Vec<(Ipv4Addr, u16, u64)> = refusals(&shard.drain_events().collect::<Vec<_>>())
        .into_iter()
        .map(|(r, n)| match r {
            Refusal::Tcp(r) => {
                assert_eq!(r.rule, toyos_net_tcp::Counter::SynFin);
                (r.remote.addr, r.remote.port.get(), n)
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(lines, [(Ipv4Addr::new(192, 0, 2, 10), 1234, 0), (Ipv4Addr::new(192, 0, 2, 60), 1234, 49)]);
    assert_eq!(shard.tcp_counters().get(toyos_net_tcp::Counter::SynFin), 51);
}

#[test]
fn s_udp_us_020_shard_sending_to_this_host_is_logged() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.drain_events().for_each(drop);
    let socket = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(50001)), || 0).unwrap();
    let rule = toyos_net_udp::Counter::SendUnspecifiedDestination;
    assert_eq!(shard.send_to(now, socket, Ipv4Addr::UNSPECIFIED, 53, b"hi"), Err(toyos_net_udp::Error::Refused(rule)));
    let lines = refusals(&shard.drain_events().collect::<Vec<_>>());
    assert!(matches!(lines[..], [(Refusal::Udp(r), 0)] if r.rule == rule && r.peer == (Ipv4Addr::UNSPECIFIED, 53)), "{lines:?}");
}
