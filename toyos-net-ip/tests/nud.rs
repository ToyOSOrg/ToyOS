//! §6: the reachability machine.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{limits, Advice, Counter, Event, Flow, IfIndex, Nud, Resolution};
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
    assert!(matches!(h.state(B), Some(Nud::Incomplete(s)) if s.pending.queued() == 1));
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
    assert!(matches!(h.state(B), Some(Nud::Failed)));
    assert_eq!(h.count(Counter::NbFailed), 1);
    assert_eq!(h.count(Counter::NbPendingDropped), 1);
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
    assert!(matches!(h.state(B), Some(Nud::Failed)));
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

    // Advice that reaches a confirmed entry within RETRANS of its last request.
    let mut h = H::fixture_i();
    let _ = h.ip.resolve(h.clock(), h.if0, R);
    assert_eq!(h.out().len(), 1, "the request at 0");
    h.at(5);
    h.reply_from(R, MAC_R);
    assert!(h.is_reachable(R));
    h.at(100);
    h.ip.advise(h.clock(), R, Advice::Reverify);
    assert!(matches!(h.state(R), Some(Nud::Probe(_))));
    assert!(h.out().is_empty(), "100 ms after the last request");
    let later = h.run(1_000);
    assert_eq!(later.iter().map(|o| (o.at, o.frame.clone())).collect::<Vec<_>>(), [(1_000, hex(V_ARP_POLL_R))]);
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
    assert!(matches!(h.state(B), Some(Nud::Failed)));
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
    assert!(matches!(h.state(B), Some(Nud::Failed)));
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

const WIDE_A: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

/// if0 at 10.0.0.1/16, with room for a full table of neighbours.
fn wide() -> H {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, WIDE_A, 16, 0);
    h.settle();
    h
}

fn neighbour(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(Ipv4Addr::new(10, 0, 1, 0)) + n)
}

fn wide_mac(addr: Ipv4Addr) -> MacAddr {
    MacAddr([2, 1, 0, 0, (u32::from(addr) >> 8) as u8, u32::from(addr) as u8])
}

fn reach_wide(h: &mut H, addr: Ipv4Addr) {
    let now = h.clock();
    let _ = h.ip.resolve(now, h.if0, addr);
    h.out();
    h.frame(&eth(MAC_A, wide_mac(addr), 0x0806, &arp_packet(2, wide_mac(addr), addr, MAC_A, WIDE_A)));
    assert!(h.is_reachable(addr));
}

/// `addr` asks who has 10.0.0.1: learned into STALE if new, and answered once the queue drains.
fn ask_wide(h: &mut H, addr: Ipv4Addr) {
    h.frame(&eth(MacAddr::BROADCAST, wide_mac(addr), 0x0806, &arp_packet(1, wide_mac(addr), addr, MacAddr::ZERO, WIDE_A)));
}

/// `addr` announces itself: an assertion, which resolves it into STALE.
fn announce_wide(h: &mut H, addr: Ipv4Addr) {
    h.frame(&eth(MacAddr::BROADCAST, wide_mac(addr), 0x0806, &arp_packet(1, wide_mac(addr), addr, MacAddr::ZERO, addr)));
}

#[test]
fn s_ip_nud_022_a_full_table_evicts_in_order() {
    let mut h = wide();
    let (failed, unreachable, stale) = (neighbour(0), neighbour(1), neighbour(2));
    let _ = h.ip.resolve(h.clock(), h.if0, failed);
    reach_wide(&mut h, unreachable);
    h.ip.advise(h.clock(), unreachable, Advice::Reverify);
    ask_wide(&mut h, stale);
    h.run(5_000);
    assert!(matches!(h.state(failed), Some(Nud::Failed)));
    assert!(matches!(h.state(unreachable), Some(Nud::Unreachable(u)) if u.quiescent()));
    assert!(h.is_stale(stale));
    for n in 3..limits::nud::TABLE_MAX as u32 {
        reach_wide(&mut h, neighbour(n));
    }
    let mut left = vec![failed, unreachable, stale];
    for n in 0..3 {
        let new = neighbour(9_000 + n);
        assert_eq!(h.send(WIDE_A, new, 5001, 5001, b"x"), Ok(None));
        assert!(matches!(h.state(new), Some(Nud::Incomplete(_))));
        let gone = left.remove(0);
        assert!(h.state(gone).is_none(), "§6.8's order: {gone} next");
        assert!(left.iter().all(|a| h.state(*a).is_some()));
    }
    assert_eq!(h.send(WIDE_A, neighbour(9_003), 5001, 5001, b"x"), Err(Counter::NbTableFull));

    let mut h = wide();
    for n in 0..limits::nud::TABLE_MAX as u32 {
        reach_wide(&mut h, neighbour(n));
    }
    let new = neighbour(9_000);
    assert_eq!(h.send(WIDE_A, new, 5001, 5001, b"x"), Err(Counter::NbTableFull));
    assert_eq!(h.count(Counter::NbTableFull), 1);
    let asker = neighbour(9_001);
    h.frame(&eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp_packet(1, MAC_X, asker, MacAddr::ZERO, WIDE_A)));
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
    assert_eq!(h.count(Counter::NbPendingDropped), 3);
    h.ip.link_up(h.clock(), h.if0).unwrap();
    assert!(h.state(B).is_none() && h.state(DNS).is_none());

    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    h.out();
    h.frame(&hex(V_ARP_REPLY));
    h.ip.link_down(h.clock(), h.if0).unwrap();
    h.collect();
    assert_eq!(h.count(Counter::NbPendingDropped), 1, "released, never sent");
    assert!(h.events.contains(&Event::Unreachable(flow_to(B))));
    h.ip.link_up(h.clock(), h.if0).unwrap();
    assert!(h.out().iter().all(|o| o.ip().is_none_or(|ip| ip.protocol() != toyos_net_wire::ipv4::Protocol::Udp)));
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

/// wide() with B, neighbour(0), holding 1 and 2 unresolved, and the other 511 entries made one per
/// ms behind it: REACHABLE, or STALE.
fn b_and_511(reachable: bool) -> H {
    let mut h = wide();
    for data in [b"1", b"2"] {
        assert_eq!(h.send(WIDE_A, neighbour(0), 5001, 5001, data), Ok(None));
    }
    h.out();
    for n in 1..limits::nud::TABLE_MAX as u32 {
        h.at(u64::from(n));
        if reachable {
            reach_wide(&mut h, neighbour(n));
        } else {
            ask_wide(&mut h, neighbour(n));
            h.out();
        }
    }
    h
}

#[test]
fn s_ip_nud_029_released_datagrams_are_evicted_last() {
    let b = neighbour(0);
    let flow = Flow { source: WIDE_A, source_port: Port::new(5001).unwrap(), destination: b, destination_port: Port::new(5001).unwrap() };
    // The other 511 STALE; then REACHABLE, twice: for a later entry at B's address, and for the
    // interface's bound.
    for arm in 0..3 {
        let others_reachable = arm > 0;
        let mut h = b_and_511(others_reachable);
        for n in 1..=limits::CONTROL_QUEUE as u32 {
            ask_wide(&mut h, neighbour(n));
        }
        announce_wide(&mut h, b);
        assert!(matches!(h.state(b), Some(Nud::Stale(s)) if s.released.queued() == 2));
        let new = neighbour(9_000);
        assert_eq!(h.send(WIDE_A, new, 5001, 5001, b"x"), Ok(None));
        assert!(matches!(h.state(new), Some(Nud::Incomplete(_))));
        if !others_reachable {
            assert!(h.state(neighbour(1)).is_none(), "the longest unused of the others");
            assert!(matches!(h.state(b), Some(Nud::Stale(s)) if s.released.queued() == 2), "B keeps 1 and 2");
            let to_b: Vec<Vec<u8>> = h.out().iter().filter(|o| o.to() == wide_mac(b)).map(payload).collect();
            assert_eq!(to_b, [b"1", b"2"]);
            assert!(h.is_stale(b), "its idle lifetime has not passed: it stays once they have left");
            assert_eq!(h.count(Counter::NbPendingEvicted), 0);
            continue;
        }
        assert!(h.state(b).is_none());
        assert_eq!(h.count(Counter::NbPendingEvicted), 2);
        assert_eq!(h.events.iter().filter(|e| **e == Event::Unreachable(flow)).count(), 2);
        for t in [1_511, 2_511, 3_511] {
            h.ip.fire(H::instant(t));
        }
        assert!(matches!(h.state(new), Some(Nud::Failed)));
        if arm == 1 {
            // A later entry at B's address resolves: B's turns went with B, so its datagram waits
            // for its own, behind a defence queued first.
            h.at(3_511);
            h.frame(&eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp_packet(1, MAC_X, WIDE_A, MacAddr::ZERO, WIDE_A)));
            assert_eq!(h.send(WIDE_A, b, 5001, 5001, b"3"), Ok(None));
            h.frame(&eth(MAC_A, wide_mac(b), 0x0806, &arp_packet(2, wide_mac(b), b, MAC_A, WIDE_A)));
            let out = h.out();
            assert_eq!(out.len(), limits::CONTROL_QUEUE + 2);
            assert!(out[..limits::CONTROL_QUEUE].iter().all(|o| o.to() != wide_mac(b)), "1 and 2 never leave");
            assert!(out[limits::CONTROL_QUEUE].arp().is_some_and(|a| a.sender_ip == WIDE_A && a.target_ip == WIDE_A), "the defence");
            assert_eq!(out[limits::CONTROL_QUEUE + 1].to(), wide_mac(b));
            assert_eq!(payload(&out[limits::CONTROL_QUEUE + 1]), b"3");
        } else {
            // B's two datagrams left the count with B, though the control queue has reached
            // nothing since. By 46,000 the FAILED entry is gone and the others are STALE, so
            // entries can be made.
            for t in [23_511, 46_000] {
                h.ip.fire(H::instant(t));
            }
            h.at(46_000);
            let mut accepted = 0;
            while h.send(WIDE_A, neighbour(10_000 + accepted / 8), 5001, 5001, b"y") == Ok(None) {
                accepted += 1;
            }
            assert_eq!(h.count(Counter::NbPendingFull), 1);
            assert_eq!(accepted as usize, limits::nud::PENDING_TOTAL);
        }
    }

    // B resolves while the control queue has room, so its turns are inside it; 62 replies fill it
    // behind them, and a defence waits. The room B's eviction frees is the defence's, so it leaves
    // ahead of the new entry's request.
    let mut h = b_and_511(true);
    announce_wide(&mut h, b);
    for n in 1..=limits::CONTROL_QUEUE as u32 - 2 {
        ask_wide(&mut h, neighbour(n));
    }
    h.frame(&eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp_packet(1, MAC_X, WIDE_A, MacAddr::ZERO, WIDE_A)));
    let new = neighbour(9_000);
    assert_eq!(h.send(WIDE_A, new, 5001, 5001, b"x"), Ok(None));
    assert!(h.state(b).is_none());
    let out = h.out();
    assert_eq!(out.len(), limits::CONTROL_QUEUE);
    assert!(out[limits::CONTROL_QUEUE - 2].arp().is_some_and(|a| a.sender_ip == WIDE_A && a.target_ip == WIDE_A), "the defence");
    assert!(out[limits::CONTROL_QUEUE - 1].requests(new));

    // RTE-004's prefixes put X on both interfaces. X on if1 holds 1 and 2, whose turns wait, when
    // X on if0 ends its FAILED hold-down: if0's entry goes, and if1's datagrams still leave to X.
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    let a1 = Ipv4Addr::new(192, 0, 2, 129);
    h.assign(if0, A, 24, 0);
    h.assign(if1, a1, 25, 5_000);
    h.settle();
    let x = Ipv4Addr::new(192, 0, 2, 200);
    let _ = h.ip.resolve(h.clock(), if0, x);
    h.run(3_000);
    assert!(matches!(h.ip.neighbour(if0, x), Some(Nud::Failed)));
    for data in [b"1", b"2"] {
        assert_eq!(h.send(a1, x, 5001, 5001, data), Ok(None));
    }
    h.out();
    let _ = h.ip.receive(h.clock(), if1, &eth(MAC_A1, MAC_X, 0x0806, &arp_packet(2, MAC_X, x, MAC_A1, a1)));
    assert!(matches!(h.ip.neighbour(if1, x), Some(Nud::Reachable(r)) if r.released.queued() == 2));
    h.ip.fire(H::instant(23_000));
    assert!(h.ip.neighbour(if0, x).is_none());
    h.at(23_000);
    let to_x: Vec<(IfIndex, Vec<u8>)> = h.out().iter().filter(|o| o.to() == MAC_X).map(|o| (o.iface, payload(o))).collect();
    assert_eq!(to_x, [(if1, b"1".to_vec()), (if1, b"2".to_vec())]);
}

#[test]
fn s_ip_nud_030_released_datagrams_keep_their_entry() {
    // B STALE since t = 0, and B quiescent UNREACHABLE since t = 4,000: each idle lifetime passes
    // while B holds 1 and 2.
    for unreachable in [false, true] {
        let mut h = H::fixture_i();
        for data in [b"1", b"2"] {
            assert_eq!(h.send(A, B, 5001, 5001, data), Ok(None));
        }
        h.out();
        for n in 0..limits::CONTROL_QUEUE as u8 {
            let m = MacAddr([2, 1, 0, 0, 0, n]);
            h.frame(&eth(MacAddr::BROADCAST, m, 0x0806, &arp_packet(1, m, Ipv4Addr::new(192, 0, 2, 100 + n), MacAddr::ZERO, A)));
        }
        let held = |h: &H| match h.state(B) {
            Some(Nud::Stale(s)) if !unreachable => Some(s.released.queued()),
            Some(Nud::Unreachable(u)) if unreachable && u.quiescent() => Some(u.released.queued()),
            _ => None,
        };
        let idle = if unreachable {
            h.frame(&hex(V_ARP_REPLY));
            h.ip.advise(h.clock(), B, Advice::Reverify);
            for t in [1_000, 2_000, 3_000, 4_000] {
                h.ip.fire(H::instant(t));
            }
            4_000 + 600_000
        } else {
            h.frame(&eth(MacAddr::BROADCAST, MAC_B, 0x0806, &arp_packet(1, MAC_B, B, MacAddr::ZERO, B)));
            600_000
        };
        assert_eq!(held(&h), Some(2));
        h.ip.fire(H::instant(idle));
        h.at(idle);
        assert_eq!(held(&h), Some(2), "not idle while it holds 1 and 2");
        assert!(h.out_with(limits::CONTROL_QUEUE).iter().all(|o| o.to() != MAC_B), "the queued frames first");
        assert_eq!(held(&h), Some(2), "1 and 2 are not out yet");
        assert_eq!(h.out_with(1).iter().map(payload).collect::<Vec<_>>(), [b"1"]);
        assert_eq!(held(&h), Some(1));
        assert_eq!(h.out_with(1).iter().map(payload).collect::<Vec<_>>(), [b"2"]);
        assert!(h.state(B).is_none(), "deleted as its queue drains");
    }

    // B STALE at MAC X since 1,000 holds 1 and 2, with a request queued behind them by the advice
    // before: its idle lifetime passes, and it goes as 2 leaves.
    let mut h = H::fixture_i();
    for data in [b"1", b"2"] {
        assert_eq!(h.send(A, B, 5001, 5001, data), Ok(None));
    }
    h.out();
    h.at(5);
    h.frame(&hex(V_ARP_REPLY));
    h.at(1_000);
    h.ip.advise(h.clock(), B, Advice::Reverify);
    assert!(matches!(h.state(B), Some(Nud::Probe(_))));
    h.frame(&eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp_packet(1, MAC_X, B, MacAddr::ZERO, B)));
    h.ip.fire(H::instant(601_000));
    h.at(601_000);
    assert!(matches!(h.state(B), Some(Nud::Stale(s)) if s.released.queued() == 2), "not idle while it holds 1 and 2");
    let out: Vec<(MacAddr, Vec<u8>)> = h.out_with(2).iter().map(|o| (o.to(), payload(o))).collect();
    assert_eq!(out, [(MAC_X, b"1".to_vec()), (MAC_X, b"2".to_vec())]);
    assert!(h.state(B).is_none(), "deleted as 2 leaves");
    assert!(h.out().is_empty(), "the request finds no entry");
}
