//! §10: the IGMP host, plus fragments at stage 5 (§11.1) and refusal visibility (§13).

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Counter, IgmpMode, Peer, RefusalLog};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::ipv4::MulticastAddr;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn group_mac(g: Ipv4Addr) -> MacAddr {
    MacAddr::multicast(group(g))
}

/// One IGMPv3 record: type, group, sources.
type Record = (u8, Ipv4Addr, Vec<Ipv4Addr>);

/// An IGMP message A sent, decoded by the test itself.
#[derive(Debug, PartialEq)]
enum Sent {
    V3(Vec<Record>),
    V2Report(Ipv4Addr),
    V2Leave(Ipv4Addr),
}

fn igmp(out: &Out) -> Option<Sent> {
    let ip = out.ip()?;
    if ip.protocol() != toyos_net_wire::ipv4::Protocol::Igmp {
        return None;
    }
    assert_eq!(oracle_sum(ip.payload()), 0xffff, "IGMP checksum");
    let p = ip.payload();
    let g = |at: usize| Ipv4Addr::new(p[at], p[at + 1], p[at + 2], p[at + 3]);
    Some(match p[0] {
        0x22 => {
            let n = usize::from(u16::from_be_bytes([p[6], p[7]]));
            let mut at = 8;
            let mut records = Vec::new();
            for _ in 0..n {
                let sources = usize::from(u16::from_be_bytes([p[at + 2], p[at + 3]]));
                let list = (0..sources).map(|s| g(at + 8 + 4 * s)).collect();
                records.push((p[at], g(at + 4), list));
                at += 8 + 4 * sources;
            }
            Sent::V3(records)
        }
        0x16 => Sent::V2Report(g(4)),
        0x17 => Sent::V2Leave(g(4)),
        other => panic!("type {other:#x}"),
    })
}

fn reports(out: &[Out]) -> Vec<(u64, Sent)> {
    out.iter().filter_map(|o| igmp(o).map(|s| (o.at, s))).collect()
}

fn to_ex(g: Ipv4Addr) -> Sent {
    Sent::V3(vec![(4, g, vec![])])
}

fn to_in(g: Ipv4Addr) -> Sent {
    Sent::V3(vec![(3, g, vec![])])
}

fn is_ex(g: Ipv4Addr) -> Sent {
    Sent::V3(vec![(2, g, vec![])])
}

/// Fixture I without 224.0.0.251 joined.
fn unjoined() -> H {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, A, 24, 0);
    h.ip.set_gateways(toyos_net_ip::Instant::from_millis(3_000), if0, &[R]).unwrap();
    h.settle();
    h
}

/// A query from R to `destination`, framed to its MAC.
fn query(h: &mut H, destination_mac: MacAddr, ip: &[u8]) {
    h.frame(&eth(destination_mac, MAC_R, 0x0800, ip));
}

fn general_v3(h: &mut H) {
    query(h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP3_QUERY_GEN));
}

/// A query datagram from R: `igmp` inside an IPv4 header with Router Alert, TTL 1, TOS 0xC0.
fn query_datagram(destination: Ipv4Addr, igmp: &[u8]) -> Vec<u8> {
    let mut ip = vec![0x46, 0xc0, 0, 0, 0, 0, 0x40, 0, 1, 2, 0, 0, 192, 0, 2, 254];
    ip.extend_from_slice(&destination.octets());
    ip.extend_from_slice(&[0x94, 4, 0, 0]);
    ip.extend_from_slice(igmp);
    let total = ip.len() as u16;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    fix(&mut ip);
    ip
}

fn v3_query(max_code: u8, group: Ipv4Addr, qrv: u8, sources: &[Ipv4Addr]) -> Vec<u8> {
    let mut q = vec![0x11, max_code, 0, 0];
    q.extend_from_slice(&group.octets());
    q.extend_from_slice(&[qrv, 125]);
    q.extend_from_slice(&(sources.len() as u16).to_be_bytes());
    for s in sources {
        q.extend_from_slice(&s.octets());
    }
    q
}

fn v2_query(max_code: u8, group: Ipv4Addr) -> Vec<u8> {
    let mut q = vec![0x11, max_code, 0, 0];
    q.extend_from_slice(&group.octets());
    q
}

#[test]
fn s_ip_igm_001_a_join_is_reported_twice() {
    let mut h = unjoined();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    let sent = reports(&h.run(10_000));
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], (0, to_ex(MDNS)));
    assert_eq!(sent[1].1, to_ex(MDNS));
    assert!(sent[1].0 > 0 && sent[1].0 <= 1_000);
}

#[test]
fn s_ip_igm_002_joins_before_and_after_the_first_address() {
    let mut h = H::bare();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    let before = h.run(2_000);
    let unspecified: Vec<Vec<u8>> = before.iter().filter(|o| igmp(o).is_some()).map(|o| ip_bytes(&o.frame)).collect();
    assert_eq!(unspecified, [hex(V_IGMP3_JOIN_UNSPEC), hex(V_IGMP3_JOIN_UNSPEC)]);
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    let after = h.run(3_000);
    let joins: Vec<(u64, Vec<u8>)> = after.iter().filter(|o| igmp(o).is_some()).map(|o| (o.at, o.frame.clone())).collect();
    assert_eq!(joins.len(), 2);
    assert!(joins.iter().all(|(_, f)| *f == hex(V_IGMP3_JOIN)));
    assert!(joins[1].0 > joins[0].0 && joins[1].0 - joins[0].0 <= 1_000);
}

#[test]
fn s_ip_igm_003_a_leave_is_reported_twice() {
    let mut h = H::fixture_i();
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    let sent = reports(&h.run(10_000));
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], (0, to_in(MDNS)));
    assert_eq!(sent[1].1, to_in(MDNS));
    assert!(sent[1].0 > 0 && sent[1].0 <= 1_000);
}

#[test]
fn s_ip_igm_004_a_leave_replaces_the_join() {
    let mut h = unjoined();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    let mut sent = reports(&h.out());
    h.at(10);
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    sent.extend(reports(&h.run(10_000)));
    assert_eq!(sent[0], (0, to_ex(MDNS)));
    assert_eq!(sent[1], (10, to_in(MDNS)));
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2].1, to_in(MDNS));
    assert!(sent[2].0 > 10 && sent[2].0 <= 1_010);
}

#[test]
fn s_ip_igm_005_membership_is_counted() {
    let mut h = unjoined();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    assert_eq!(reports(&h.run(5_000)).len(), 2, "one join, sent twice");
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    assert!(reports(&h.run(10_000)).is_empty());
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    assert_eq!(reports(&h.run(15_000))[0].1, to_in(MDNS));
}

#[test]
fn s_ip_igm_006_all_hosts_is_never_reported() {
    let mut h = H::fixture_i();
    h.ip.join(h.clock(), h.if0, group(ip4(224, 0, 0, 1))).unwrap();
    assert!(h.out().is_empty());
    general_v3(&mut h);
    let sent = reports(&h.run(10_000));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, is_ex(MDNS));
}

#[test]
fn s_ip_igm_007_a_general_query_is_answered_later() {
    let mut h = H::fixture_i();
    general_v3(&mut h);
    assert!(h.out().is_empty(), "never at once");
    let out = h.run(10_000);
    let current: Vec<&Out> = out.iter().filter(|o| igmp(o).is_some()).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(ip_bytes(&current[0].frame), hex(V_IGMP3_CURRENT));
    assert!(current[0].at > 0 && current[0].at <= 10_000);
}

#[test]
fn s_ip_igm_008_a_sooner_general_response_suffices() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &query_datagram(ip4(224, 0, 0, 1), &v3_query(10, ip4(0, 0, 0, 0), 2, &[])));
    h.at(100);
    general_v3(&mut h);
    let sent = reports(&h.run(20_000));
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0 <= 1_000);
}

#[test]
fn s_ip_igm_009_a_group_query() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(MDNS), &hex(V_IGMP3_QUERY_GROUP));
    let sent = reports(&h.run(5_000));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, is_ex(MDNS));
    assert!(sent[0].0 > 0 && sent[0].0 <= 1_000);
}

#[test]
fn s_ip_igm_010_a_query_for_a_group_not_joined() {
    let mut h = H::fixture_i();
    query(&mut h, MAC_A, &query_datagram(A, &v3_query(10, ip4(224, 0, 0, 252), 2, &[])));
    assert_eq!(h.count(Counter::IgmpQueryOtherGroup), 1);
    assert!(h.run(5_000).is_empty());
}

#[test]
fn s_ip_igm_011_a_group_and_source_query() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(MDNS), &query_datagram(MDNS, &hex(V_IGMP_QUERY_V3_SRC)));
    let sent = reports(&h.run(10_000));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, Sent::V3(vec![(1, MDNS, vec![ip4(192, 0, 2, 9), ip4(192, 0, 2, 10)])]));
}

#[test]
fn s_ip_igm_012_too_many_sources_make_a_group_response() {
    let mut h = H::fixture_i();
    let sources: Vec<Ipv4Addr> = (0..70u32).map(|n| Ipv4Addr::from(u32::from(ip4(198, 51, 100, 0)) + n)).collect();
    query(&mut h, group_mac(MDNS), &query_datagram(MDNS, &v3_query(100, MDNS, 2, &sources)));
    assert_eq!(reports(&h.run(10_000)).into_iter().map(|(_, s)| s).collect::<Vec<_>>(), [is_ex(MDNS)]);
}

#[test]
fn s_ip_igm_013_queries_need_router_alert() {
    let mut h = H::fixture_i();
    general_v3(&mut h);
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP3_QUERY_NORA));
    let mut v2 = hex(V_IGMP_QUERY_V2);
    v2.drain(20..24);
    v2[0] = 0x45;
    v2[3] = 28;
    fix(&mut v2);
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &v2);
    assert_eq!(h.count(Counter::IgmpQueryNoRouterAlert), 2);
    assert_eq!(h.refusals(Counter::IgmpQueryNoRouterAlert)[0].peer, Peer::Ip(R));
    assert!(h.run(20_000).is_empty());
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V3));
}

#[test]
fn s_ip_igm_014_general_query_destinations() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(MDNS), &edit(hex(V_IGMP3_QUERY_GEN), |ip| set_destination(ip, MDNS)));
    assert_eq!(h.refusals(Counter::IgmpGeneralQueryDestination).len(), 1);
    assert!(h.run(20_000).is_empty());
    query(&mut h, MAC_A, &edit(hex(V_IGMP3_QUERY_GEN), |ip| set_destination(ip, A)));
    assert_eq!(reports(&h.run(40_000)).len(), 1);
}

#[test]
fn s_ip_igm_015_a_group_query_to_another_group() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &edit(hex(V_IGMP3_QUERY_GROUP), |ip| set_destination(ip, ip4(224, 0, 0, 1))));
    h.ip.join(h.clock(), h.if0, group(ip4(224, 0, 0, 252))).unwrap();
    h.run(5_000);
    query(&mut h, group_mac(ip4(224, 0, 0, 252)), &edit(hex(V_IGMP3_QUERY_GROUP), |ip| set_destination(ip, ip4(224, 0, 0, 252))));
    assert_eq!(h.count(Counter::IgmpQueryDestination), 2);
    assert!(h.run(20_000).is_empty());
}

#[test]
fn s_ip_igm_016_the_robustness_is_learned() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &query_datagram(ip4(224, 0, 0, 1), &v3_query(10, ip4(0, 0, 0, 0), 3, &[])));
    h.run(5_000);
    h.ip.join(h.clock(), h.if0, group(ip4(239, 1, 2, 3))).unwrap();
    let joins = reports(&h.run(10_000)).into_iter().filter(|(_, s)| *s == to_ex(ip4(239, 1, 2, 3))).count();
    assert_eq!(joins, 3);
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &query_datagram(ip4(224, 0, 0, 1), &v3_query(10, ip4(0, 0, 0, 0), 0, &[])));
    h.run(15_000);
    h.ip.join(h.clock(), h.if0, group(ip4(239, 1, 2, 4))).unwrap();
    let joins = reports(&h.run(20_000)).into_iter().filter(|(_, s)| *s == to_ex(ip4(239, 1, 2, 4))).count();
    assert_eq!(joins, 3);
}

#[test]
fn s_ip_igm_017_a_v2_querier() {
    let mut h = H::fixture_i();
    general_v3(&mut h);
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP_QUERY_V2));
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V2));
    assert_eq!(h.count(Counter::IgmpModeV2), 1);
    let out = h.run(10_000);
    let sent = reports(&out);
    assert_eq!(sent.len(), 1, "the v3 response was cancelled");
    assert_eq!(sent[0].1, Sent::V2Report(MDNS));
    let report = out.iter().find(|o| igmp(o).is_some()).unwrap();
    let ip = report.ip().unwrap();
    assert_eq!((ip.destination(), ip.ttl(), ip.traffic_class().byte()), (MDNS, 1, 0xc0));
    assert!(matches!(ip.options().iter().next(), Some(toyos_net_wire::ipv4::Ipv4Option::RouterAlert(0))));
    assert!(sent[0].0 > 0 && sent[0].0 <= 10_000);
}

fn v2_mode() -> H {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP_QUERY_V2));
    h.run(10_000);
    h.rebase();
    h
}

#[test]
fn s_ip_igm_018_a_join_in_v2_mode() {
    let mut h = v2_mode();
    let t = h.now;
    h.ip.join(h.clock(), h.if0, group(ip4(224, 0, 0, 252))).unwrap();
    let sent = reports(&h.run(t + 20_000));
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], (t, Sent::V2Report(ip4(224, 0, 0, 252))));
    assert_eq!(sent[1].1, Sent::V2Report(ip4(224, 0, 0, 252)));
    assert!(sent[1].0 > t && sent[1].0 <= t + 10_000);
}

#[test]
fn s_ip_igm_019_another_hosts_report_suppresses_ours() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP_QUERY_V2));
    let mut report = hex(V_IGMP_REPORT);
    report[6..12].copy_from_slice(&MAC_B.0);
    let mut ip = report[14..46].to_vec();
    set_source(&mut ip, B);
    fix(&mut ip);
    h.frame(&eth(group_mac(MDNS), MAC_B, 0x0800, &ip));
    assert!(reports(&h.run(20_000)).is_empty(), "the delay was cancelled");
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    assert!(reports(&h.run(40_000)).is_empty(), "not the last reporter: no leave");
}

#[test]
fn s_ip_igm_020_a_v1_query_is_refused() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP1_QUERY));
    assert_eq!(h.refusals(Counter::IgmpV1Query).len(), 1);
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V3));
    assert!(h.run(20_000).is_empty());
}

#[test]
fn s_ip_igm_021_v2_mode_ends() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &hex(V_IGMP_QUERY_V2));
    h.run(259_999);
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V2));
    h.run(260_001);
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V3));
    assert_eq!(h.count(Counter::IgmpModeV3), 1);
    general_v3(&mut h);
    let out = h.run(280_001);
    assert!(out.iter().any(|o| igmp(o).is_some() && ip_bytes(&o.frame) == hex(V_IGMP3_CURRENT)));
}

#[test]
fn s_ip_igm_022_a_v2_group_query_in_v3_mode() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(MDNS), &query_datagram(MDNS, &v2_query(10, MDNS)));
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V3));
    let sent = reports(&h.run(5_000));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, is_ex(MDNS));
    assert!(sent[0].0 > 0 && sent[0].0 <= 1_000);
}

#[test]
fn s_ip_igm_023_the_last_reporter_leaves() {
    let mut h = v2_mode();
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    let out = h.out();
    assert_eq!(reports(&out).into_iter().map(|(_, s)| s).collect::<Vec<_>>(), [Sent::V2Leave(MDNS)]);
    let ip = out[0].ip().unwrap();
    assert_eq!((ip.destination(), ip.ttl()), (ip4(224, 0, 0, 2), 1));
    assert_eq!(ip.payload(), hex(V_IGMP_LEAVE));
}

#[test]
fn s_ip_igm_024_other_hosts_reports_are_ignored_in_v3() {
    let mut h = H::fixture_i();
    general_v3(&mut h);
    let v3_report = query_datagram(ip4(224, 0, 0, 22), &[0x22, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, 224, 0, 0, 251]);
    h.frame(&eth(group_mac(ip4(224, 0, 0, 22)), MAC_B, 0x0800, &edit(v3_report, |ip| set_source(ip, B))));
    let v2 = edit(hex(V_IGMP_REPORT)[14..46].to_vec(), |ip| set_source(ip, B));
    h.frame(&eth(group_mac(MDNS), MAC_B, 0x0800, &v2));
    let v1 = edit(v2, |ip| ip[24] = 0x12);
    h.frame(&eth(group_mac(MDNS), MAC_B, 0x0800, &v1));
    assert_eq!(h.count(Counter::IgmpIgnored), 2);
    assert_eq!(h.count(Counter::EthNotForUs), 1, "a v3 report goes to 224.0.0.22, which a host never joins");
    assert_eq!(reports(&h.run(10_000)).into_iter().map(|(_, s)| s).collect::<Vec<_>>(), [is_ex(MDNS)]);
}

#[test]
fn s_ip_igm_025_thirty_two_groups() {
    let mut h = H::fixture_i();
    for n in 1..32u8 {
        h.ip.join(h.clock(), h.if0, group(ip4(239, 0, 0, n))).unwrap();
    }
    assert_eq!(h.ip.join(h.clock(), h.if0, group(ip4(239, 0, 0, 99))), Err(Counter::IgmpTooManyGroups));
}

#[test]
fn s_ip_igm_026_link_flap() {
    let mut h = H::fixture_i();
    general_v3(&mut h);
    h.ip.link_down(h.clock(), h.if0).unwrap();
    h.ip.link_up(h.clock(), h.if0).unwrap();
    let sent = reports(&h.run(20_000));
    assert_eq!(sent.len(), 2, "the pending response is gone");
    assert_eq!(sent[0], (0, to_ex(MDNS)));
    assert_eq!(sent[1].1, to_ex(MDNS));
    assert!(sent[1].0 > 0 && sent[1].0 <= 1_000);
}

#[test]
fn s_ip_igm_027_one_report_for_every_group() {
    let mut h = H::fixture_i();
    for g in [ip4(239, 1, 2, 3), ip4(239, 1, 2, 4)] {
        h.ip.join(h.clock(), h.if0, group(g)).unwrap();
    }
    h.run(5_000);
    general_v3(&mut h);
    let sent = reports(&h.run(20_000));
    assert_eq!(sent.len(), 1);
    let Sent::V3(records) = &sent[0].1 else { panic!() };
    let mut groups: Vec<(u8, Ipv4Addr)> = records.iter().map(|r| (r.0, r.1)).collect();
    groups.sort();
    assert_eq!(groups, [(2, MDNS), (2, ip4(239, 1, 2, 3)), (2, ip4(239, 1, 2, 4))]);
}

#[test]
fn s_ip_igm_028_the_join_byte_for_byte() {
    let mut h = unjoined();
    h.ip.join(h.clock(), h.if0, group(MDNS)).unwrap();
    assert_eq!(h.out()[0].frame, hex(V_IGMP3_JOIN));
}

#[test]
fn s_igmp_023_no_router_alert_is_refused() {
    let mut h = H::fixture_i();
    let mut v2 = hex(V_IGMP_QUERY_V2);
    v2.drain(20..24);
    v2[0] = 0x45;
    v2[3] = 28;
    fix(&mut v2);
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &v2);
    assert_eq!(h.count(Counter::IgmpQueryNoRouterAlert), 1);
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V3));
}

#[test]
fn s_igmp_024_ttl_2_is_accepted() {
    let mut h = H::fixture_i();
    query(&mut h, group_mac(ip4(224, 0, 0, 1)), &edit(hex(V_IGMP_QUERY_V2), |ip| ip[8] = 2));
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V2));
}

#[test]
fn s_igmp_025_a_general_query_to_our_address() {
    let mut h = H::fixture_i();
    query(&mut h, MAC_A, &edit(hex(V_IGMP_QUERY_V2), |ip| set_destination(ip, A)));
    assert_eq!(h.ip.igmp_mode(h.if0), Some(IgmpMode::V2));
    assert_eq!(reports(&h.run(20_000)).len(), 1);
}

#[test]
fn s_igmp_029_joining_all_hosts_sends_nothing() {
    let mut h = H::fixture_i();
    h.ip.join(h.clock(), h.if0, MulticastAddr::ALL_HOSTS).unwrap();
    assert!(h.run(5_000).is_empty());
}

#[test]
fn s_ip_frg_001_a_fragment_is_refused_and_logged() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert!(h.datagram(&hex(V_FRAG_1)).is_none());
    assert_eq!(h.refusals(Counter::IpFragment)[0].peer, Peer::Ip(B));
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_mod_001_one_line_per_rule_per_10_s() {
    let mut h = H::fixture_i();
    for n in 0..100u64 {
        h.at(n * 10);
        h.datagram(&hex(V_IP_LSRR));
    }
    h.at(10_001);
    h.datagram(&hex(V_IP_LSRR));
    assert_eq!(h.count(Counter::IpSourceRoute), 101);
    let refusals = h.refusals(Counter::IpSourceRoute);
    assert!(refusals.iter().all(|r| r.peer == Peer::Ip(B)));
    let mut log = RefusalLog::default();
    let times = (0..100u64).map(|n| n * 10).chain([10_001]);
    let lines: Vec<u64> = refusals.iter().zip(times).filter_map(|(r, t)| log.admit(H::instant(t), r.rule)).collect();
    assert_eq!(lines, [0, 99]);
}

#[test]
fn s_ip_mod_002_ordinary_refusals_are_not_logged() {
    let mut h = H::fixture_i();
    for _ in 0..1_000 {
        h.datagram(&edit(hex(V_UDP_HI), |ip| set_destination(ip, ip4(192, 0, 2, 7))));
    }
    assert_eq!(h.count(Counter::IpNotForUs), 1_000);
    assert!(h.events.iter().all(|e| !matches!(e, toyos_net_ip::Event::Refused(_))));
}
