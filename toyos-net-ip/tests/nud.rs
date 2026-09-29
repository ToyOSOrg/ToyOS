//! §6: the reachability machine.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{limits, Advice, Counter, Event, Flow, Nud, Resolution};
use toyos_net_wire::Port;

fn flow_to(destination: Ipv4Addr) -> Flow {
    Flow { source: A, source_port: Port::new(5001).unwrap(), destination, destination_port: Port::new(5001).unwrap() }
}

fn payload(out: &Out) -> Vec<u8> {
    out.ip().unwrap().payload()[8..].to_vec()
}

#[test]
fn s_ip_nud_001_resolution_releases_the_datagram() {
    let mut h = H::fixture_i();
    assert_eq!(h.udp_to(B), Ok(None));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frame, hex(V_ARP_REQ));
    assert!(matches!(h.state(B), Some(Nud::Incomplete(s)) if s.queued() == 1));
    h.at(5);
    h.frame(&hex(V_ARP_REPLY));
    assert!(h.is_reachable(B));
    assert_eq!(h.mac_of(B), Some(MAC_B));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].to(), MAC_B);
    assert_eq!(payload(&out[0]), b"hi");
}

#[test]
fn s_ip_nud_002_three_requests_then_failed() {
    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    let mut out = h.out();
    out.extend(h.run(3_000));
    let times: Vec<u64> = out.iter().filter(|o| o.requests(B)).map(|o| o.at).collect();
    assert_eq!(times, [0, 1_000, 2_000]);
    assert_eq!(out.len(), 3);
    assert!(matches!(h.state(B), Some(Nud::Failed(_))));
    assert_eq!(h.count(Counter::NbFailed), 1);
    assert!(h.events.contains(&Event::Unreachable(flow_to(B))));
    assert!(h.run(10_000).is_empty());
}

#[test]
fn s_ip_nud_003_release_keeps_arrival_order() {
    let mut h = H::fixture_i();
    for data in [b"1", b"2", b"3"] {
        h.send(A, B, 5001, 5001, data).unwrap();
    }
    h.out();
    h.frame(&hex(V_ARP_REPLY));
    let fourth = h.send(A, B, 5001, 5001, b"4").unwrap().unwrap();
    let mut out: Vec<Vec<u8>> = h.out().iter().map(payload).collect();
    out.push(ip_of(&fourth).unwrap().payload()[8..].to_vec());
    assert_eq!(out, [b"1", b"2", b"3", b"4"]);
}

#[test]
fn s_ip_nud_004_overflow_drops_the_oldest() {
    let mut h = H::fixture_i();
    for n in 1..=10u8 {
        h.send(A, B, 5001, 5001, &[b'0' + n % 10]).unwrap();
    }
    h.out();
    h.frame(&hex(V_ARP_REPLY));
    let out: Vec<Vec<u8>> = h.out().iter().map(payload).collect();
    let expected: Vec<Vec<u8>> = (3..=10u8).map(|n| vec![b'0' + n % 10]).collect();
    assert_eq!(out, expected);
    assert_eq!(h.count(Counter::NbPendingOverflow), 2);
}

#[test]
fn s_ip_nud_005_the_interface_holds_at_most_64() {
    let mut h = H::fixture_i();
    for n in 0..8u8 {
        for _ in 0..8 {
            assert_eq!(h.udp_to(Ipv4Addr::new(192, 0, 2, 10 + n)), Ok(None));
        }
    }
    assert_eq!(h.udp_to(Ipv4Addr::new(192, 0, 2, 30)), Err(Counter::NbPendingFull));
    assert_eq!(h.count(Counter::NbPendingFull), 1);
    assert!(h.state(Ipv4Addr::new(192, 0, 2, 30)).is_none());
    assert!(h.events.contains(&Event::Unreachable(flow_to(Ipv4Addr::new(192, 0, 2, 30)))));
}

#[test]
fn s_ip_nud_006_reachable_goes_stale_silently() {
    let mut h = H::fixture_i_with(R, MAC_R);
    assert!(h.run(14_999).is_empty());
    assert!(h.is_reachable(R));
    assert!(h.run(45_000).is_empty());
    assert!(h.is_stale(R));
}

#[test]
fn s_ip_nud_007_positive_advice_moves_the_confirmation() {
    let mut h = H::fixture_i_with(R, MAC_R);
    let reachable = h.ip.reachable_time(h.if0).unwrap();
    let ms = |d: std::time::Duration| d.as_millis() as u64;
    h.at(10_000);
    h.ip.advise(h.clock(), REMOTE, Advice::Confirmed);
    h.run(ms(reachable) + 1);
    assert!(h.is_reachable(R), "the original deadline re-arms");
    h.run(10_000 + ms(reachable) - 1);
    assert!(h.is_reachable(R));
    h.run(10_000 + ms(reachable) + 1);
    assert!(h.is_stale(R));
}

#[test]
fn s_ip_nud_008_delay_probe_unreachable() {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    assert!(matches!(h.udp_to(REMOTE), Ok(Some(frame)) if destination_of(&frame) == MAC_R));
    assert!(matches!(h.state(R), Some(Nud::Delay(_))));
    let out = h.run(8_000);
    let polls: Vec<u64> = out.iter().filter(|o| o.frame == hex(V_ARP_POLL_R)).map(|o| o.at).collect();
    assert_eq!(polls, [5_000, 6_000, 7_000]);
    assert_eq!(out.len(), 3);
    assert!(matches!(h.state(R), Some(Nud::Unreachable(u)) if u.quiescent()));
    assert_eq!(h.count(Counter::NbUnreachable), 1);
}

#[test]
fn s_ip_nud_009_advice_ends_delay() {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    h.udp_to(REMOTE).unwrap();
    h.at(4_000);
    h.ip.advise(h.clock(), REMOTE, Advice::Confirmed);
    assert!(h.is_reachable(R));
    assert!(h.run(5_000).is_empty());
}

#[test]
fn s_ip_nud_010_a_reply_confirms_probe() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(100);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    assert!(matches!(h.state(R), Some(Nud::Probe(_))));
    h.out();
    h.frame(&hex(V_ARP_REPLY_R));
    assert!(h.is_reachable(R));
}

/// R UNREACHABLE after PROBE's three unanswered polls, at the returned time.
fn unreachable_r() -> (H, u64) {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    h.udp_to(REMOTE).unwrap();
    h.run(8_000);
    assert!(matches!(h.state(R), Some(Nud::Unreachable(_))));
    h.rebase();
    (h, 8_000)
}

#[test]
fn s_ip_nud_011_unreachable_backs_off_while_traffic_flows() {
    let (mut h, t0) = unreachable_r();
    let mut requests = Vec::new();
    let mut t = t0;
    while t <= t0 + 160_000 {
        requests.extend(h.run(t).iter().filter(|o| o.requests(R)).map(|o| o.at - t0));
        h.at(t);
        let frame = h.udp_to(REMOTE).unwrap().expect("sent to the cached MAC");
        assert_eq!(destination_of(&frame), MAC_R);
        requests.extend(h.out().iter().filter(|o| o.requests(R)).map(|o| o.at - t0));
        t += 100;
    }
    assert_eq!(requests, [0, 3_000, 12_000, 39_000, 99_000, 159_000]);
    assert!(h.out().iter().all(|o| o.frame != hex(V_ARP_REQ_R) || o.to() == MacAddr::BROADCAST));
}

use toyos_net_wire::ethernet::MacAddr;

#[test]
fn s_ip_nud_012_unreachable_is_quiet_without_traffic() {
    let (mut h, t0) = unreachable_r();
    assert!(h.run(t0 + 20_000).is_empty());
    let t1 = t0 + 20_000;
    assert!(h.udp_to(REMOTE).unwrap().is_some());
    let out = h.out();
    assert_eq!(out.iter().filter(|o| o.frame == hex(V_ARP_REQ_R)).count(), 1);
    assert!(h.run(t1 + 3_000).is_empty());
    assert!(matches!(h.state(R), Some(Nud::Unreachable(u)) if u.quiescent()));
}

#[test]
fn s_ip_nud_013_a_moved_router_answers() {
    let (mut h, t0) = unreachable_r();
    h.at(t0 + 100);
    h.udp_to(REMOTE).unwrap();
    h.out();
    h.frame(&hex(V_ARP_REPLY_R_MOVED));
    assert!(h.is_reachable(R));
    assert_eq!(h.mac_of(R), Some(MAC_X));
    assert_eq!(h.count(Counter::ArpMacChanged), 1);
    let logged = h.refusals(Counter::ArpMacChanged);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].peer, toyos_net_ip::Peer::MacChange { ip: R, old: MAC_R, new: MAC_X });
}

#[test]
fn s_ip_nud_014_failed_is_held_down() {
    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    h.out();
    h.run(3_000);
    h.at(5_000);
    assert_eq!(h.udp_to(B), Err(Counter::NbFailedRefused));
    assert_eq!(h.count(Counter::NbFailedRefused), 1);
    assert!(h.out().is_empty());
    assert!(h.run(22_999).is_empty());
    assert!(matches!(h.state(B), Some(Nud::Failed(_))));
    h.run(23_000);
    assert!(h.state(B).is_none());
    h.at(23_001);
    assert_eq!(h.udp_to(B), Ok(None));
    assert!(h.out()[0].requests(B));
}

#[test]
fn s_ip_nud_015_an_assertion_revives_failed() {
    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    h.out();
    h.run(3_000);
    h.at(4_000);
    h.frame(&hex(V_ARP_REQ_B));
    assert!(h.is_stale(B));
    assert_eq!(h.mac_of(B), Some(MAC_B));
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_REPLY_A)]);
}

#[test]
fn s_ip_nud_016_requests_are_spaced() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(100);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    let first = h.out();
    h.at(600);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    let second = h.out();
    let later = h.run(1_100);
    assert_eq!(first.iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_POLL_R)]);
    assert!(second.is_empty());
    assert_eq!(later.iter().map(|o| (o.at, o.frame.clone())).collect::<Vec<_>>(), [(1_100, hex(V_ARP_POLL_R))]);
}

#[test]
fn s_ip_nud_017_negative_advice_probes() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(100);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    assert!(matches!(h.state(R), Some(Nud::Probe(_))));
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_POLL_R)]);
}

#[test]
fn s_ip_nud_018_negative_advice_leaves_probe_incomplete_and_failed() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(100);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    h.out();
    h.at(200);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    assert!(matches!(h.state(R), Some(Nud::Probe(p)) if p.requests() == 1));
    assert!(h.out().is_empty());

    let mut h = H::fixture_i();
    h.ip.set_gateways(h.clock(), h.if0, &[]).unwrap();
    h.udp_to(B).unwrap();
    h.out();
    h.ip.advise(h.clock(), B, Advice::Reverify);
    assert!(matches!(h.state(B), Some(Nud::Incomplete(s)) if s.requests() == 1));
    assert!(h.out().is_empty());
    h.run(3_000);
    h.at(3_500);
    h.ip.advise(h.clock(), B, Advice::Reverify);
    assert!(matches!(h.state(B), Some(Nud::Failed(_))));
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_nud_019_positive_advice_needs_a_mac() {
    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    h.out();
    h.ip.advise(h.clock(), B, Advice::Confirmed);
    assert!(matches!(h.state(B), Some(Nud::Incomplete(_))));
    h.run(3_000);
    h.ip.advise(h.clock(), B, Advice::Confirmed);
    assert!(matches!(h.state(B), Some(Nud::Failed(_))));
    let (mut h, t0) = unreachable_r();
    h.at(t0 + 10);
    h.ip.advise(h.clock(), REMOTE, Advice::Confirmed);
    assert!(h.is_reachable(R));
    assert_eq!(h.mac_of(R), Some(MAC_R));
}

#[test]
fn s_ip_nud_020_advice_lands_on_the_gateway() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(100);
    h.ip.advise(h.clock(), REMOTE, Advice::Reverify);
    assert!(matches!(h.state(R), Some(Nud::Probe(_))));
    assert!(h.state(REMOTE).is_none());
}

#[test]
fn s_ip_nud_021_stale_is_deleted_when_idle() {
    let mut h = H::fixture_i();
    h.stale(B, MAC_B);
    h.run(599_999);
    assert!(h.is_stale(B));
    h.run(600_000);
    assert!(h.state(B).is_none());
    let mut h = H::fixture_i();
    h.stale(B, MAC_B);
    h.run(599_999);
    h.udp_to(B).unwrap();
    assert!(matches!(h.state(B), Some(Nud::Delay(_))));
}

/// if0 at 10.0.0.1/16, with room for a full table of neighbours.
fn wide() -> H {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, Ipv4Addr::new(10, 0, 0, 1), 16, 0);
    h.settle();
    h
}

fn neighbour(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(Ipv4Addr::new(10, 0, 1, 0)) + n)
}

fn reach_wide(h: &mut H, addr: Ipv4Addr) {
    let now = h.clock();
    let _ = h.ip.resolve(now, h.if0, addr);
    h.out();
    let m = MacAddr([2, 1, 0, 0, (u32::from(addr) >> 8) as u8, u32::from(addr) as u8]);
    h.frame(&eth(MAC_A, m, 0x0806, &arp_packet(2, m, addr, MAC_A, Ipv4Addr::new(10, 0, 0, 1))));
    assert!(h.is_reachable(addr));
}

#[test]
fn s_ip_nud_022_a_full_table_evicts_failed_first() {
    let mut h = wide();
    let failed = neighbour(0);
    let _ = h.ip.resolve(h.clock(), h.if0, failed);
    h.out();
    h.run(3_000);
    assert!(matches!(h.state(failed), Some(Nud::Failed(_))));
    for n in 1..limits::nud::TABLE_MAX as u32 {
        reach_wide(&mut h, neighbour(n));
    }
    let new = neighbour(9_000);
    assert_eq!(h.send(Ipv4Addr::new(10, 0, 0, 1), new, 5001, 5001, b"x"), Ok(None));
    assert!(h.state(failed).is_none());
    assert!(matches!(h.state(new), Some(Nud::Incomplete(_))));

    let mut h = wide();
    for n in 0..limits::nud::TABLE_MAX as u32 {
        reach_wide(&mut h, neighbour(n));
    }
    let new = neighbour(9_000);
    assert_eq!(h.send(Ipv4Addr::new(10, 0, 0, 1), new, 5001, 5001, b"x"), Err(Counter::NbTableFull));
    assert_eq!(h.count(Counter::NbTableFull), 1);
    let asker = neighbour(9_001);
    h.frame(&eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp_packet(1, MAC_X, asker, MacAddr::ZERO, Ipv4Addr::new(10, 0, 0, 1))));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert!(out[0].arp().is_some_and(|a| a.target_ip == asker));
    assert!(h.state(asker).is_none());
}

#[test]
fn s_ip_nud_023_link_down_drops_the_table() {
    let mut h = H::fixture_i();
    for _ in 0..2 {
        h.udp_to(B).unwrap();
    }
    h.udp_to(DNS).unwrap();
    h.out();
    h.ip.link_down(h.clock(), h.if0).unwrap();
    h.collect();
    assert!(h.state(B).is_none() && h.state(DNS).is_none());
    let told = h.events.iter().filter(|e| matches!(e, Event::Unreachable(_))).count();
    assert_eq!(told, 3);
    h.ip.link_up(h.clock(), h.if0).unwrap();
    assert!(h.state(B).is_none() && h.state(DNS).is_none());
}

#[test]
fn s_ip_nud_024_requests_speak_from_the_prompting_source() {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, A, 24, 0);
    h.assign(if0, Ipv4Addr::new(198, 51, 100, 1), 24, 5_000);
    h.settle();
    h.send(Ipv4Addr::new(198, 51, 100, 1), Ipv4Addr::new(198, 51, 100, 9), 5001, 5001, b"x").unwrap();
    let out = h.out();
    assert_eq!(out[0].arp().unwrap().sender_ip, Ipv4Addr::new(198, 51, 100, 1));

    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    assert_eq!(h.udp_to(B), Err(Counter::RouteNoSourceAddress));
    assert!(h.state(B).is_none());
    assert!(h.out().iter().all(|o| o.arp().is_none_or(|a| a.target_ip != B)));
}

#[test]
fn s_ip_nud_025_a_flow_waits_outside() {
    let mut h = H::fixture_i();
    assert_eq!(h.ip.resolve(h.clock(), h.if0, B), Resolution::Pending);
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_REQ)]);
    h.frame(&hex(V_ARP_REPLY));
    assert!(h.events.contains(&Event::Resolved { iface: h.if0, next_hop: B }));
    assert_eq!(h.ip.resolve(h.clock(), h.if0, B), Resolution::Resolved(MAC_B));
    assert!(h.out().is_empty(), "no segment ever waited in [ip]");

    let mut h = H::fixture_i();
    assert_eq!(h.ip.resolve(h.clock(), h.if0, B), Resolution::Pending);
    h.out();
    h.run(2_999);
    assert!(!h.events.iter().any(|e| matches!(e, Event::Failed { .. })));
    h.run(3_000);
    assert!(h.events.contains(&Event::Failed { iface: h.if0, next_hop: B }));
    assert_eq!(h.ip.resolve(h.clock(), h.if0, B), Resolution::Failed);
}

#[test]
fn s_ip_nud_026_a_flow_moves_stale_to_delay() {
    let mut h = H::fixture_i();
    h.stale(B, MAC_B);
    assert_eq!(h.ip.resolve(h.clock(), h.if0, B), Resolution::Resolved(MAC_B));
    assert!(matches!(h.state(B), Some(Nud::Delay(_))));
}

#[test]
fn s_ip_nud_027_prop_reachable_time_is_drawn_every_two_hours() {
    let two_hours = limits::nud::REACHABLE_REDRAW;
    for seed in 1..=8u8 {
        let mut h = H::fixture_i();
        let mut rng = Rng(u64::from(seed) * 7919);
        let mut last = (h.ip.reachable_time(h.if0).unwrap(), h.clock());
        let mut changes = 0;
        let mut t = 0;
        while t < 10 * 3_600_000 {
            t += 1 + rng.below(1_200_000);
            h.at(t);
            let addr = Ipv4Addr::new(192, 0, 2, 2 + (t % 200) as u8);
            let _ = h.ip.resolve(h.clock(), h.if0, addr);
            h.out();
            h.reply_from(addr, MAC_B);
            h.fire(t);
            let reachable = h.ip.reachable_time(h.if0).unwrap();
            assert!((limits::nud::REACHABLE_MIN..=limits::nud::REACHABLE_MAX).contains(&reachable));
            if reachable != last.0 {
                assert!(h.clock().since(last.1) >= two_hours, "seed {seed}: redrawn early at {t}");
                last = (reachable, h.clock());
                changes += 1;
            }
        }
        assert!((3..=5).contains(&changes), "seed {seed}: {changes} redraws in ten hours");
    }
}
