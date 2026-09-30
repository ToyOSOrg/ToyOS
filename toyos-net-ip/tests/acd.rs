//! §8: address conflict detection, on RFC 5227's schedule scaled by the IP-D1 ruling.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_ip::limits::acd::{ANNOUNCE_INTERVAL, ANNOUNCE_WAIT, PROBE_MAX, PROBE_MIN, PROBE_WAIT};
use toyos_net_ip::{AddrState, Counter, Event, Instant, Peer, Source};
use toyos_net_wire::ethernet::MacAddr;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

/// Frames with their exact hand-off instants, from a loop firing each deadline at its own time.
fn run_exact(h: &mut H, until: Instant) -> Vec<(Instant, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(d) = h.ip.next_deadline().filter(|d| *d <= until) {
        h.ip.fire(d);
        h.ip.transmit(d, usize::MAX, |_, f| out.push((d, f.to_vec())));
        h.collect();
    }
    out
}

fn probing() -> H {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    h
}

/// Probing until the first probe left; returns when.
fn first_probe(h: &mut H) -> Instant {
    let d = h.ip.next_deadline().unwrap();
    h.ip.fire(d);
    assert_eq!(h.ip.transmit(d, usize::MAX, |_, _| {}), 1);
    h.now = d.nanos() / 1_000_000 - BASE_MS;
    d
}

#[test]
fn s_ip_acd_001_the_ruled_schedule() {
    let mut h = probing();
    let t0 = h.clock();
    let out = run_exact(&mut h, t0.after(Duration::from_secs(5)));
    let probes: Vec<Instant> = out.iter().filter(|(_, f)| *f == hex(V_ARP_PROBE)).map(|(at, _)| *at).collect();
    let announcements: Vec<Instant> = out.iter().filter(|(_, f)| *f == hex(V_ARP_ANNOUNCE)).map(|(at, _)| *at).collect();
    assert_eq!(probes.len(), 3);
    assert_eq!(announcements.len(), 2);
    assert!(probes[0].since(t0) <= PROBE_WAIT);
    for pair in probes.windows(2) {
        assert!((PROBE_MIN..=PROBE_MAX).contains(&pair[1].since(pair[0])));
    }
    assert_eq!(announcements[0].since(probes[2]), ANNOUNCE_WAIT);
    assert_eq!(announcements[1].since(announcements[0]), ANNOUNCE_INTERVAL);
    let usable = announcements[0].since(t0);
    assert!(usable >= PROBE_MIN * 2 + ANNOUNCE_WAIT && usable <= Duration::from_millis(200), "{usable:?}");
    assert!(h.events.contains(&Event::Verified { iface: h.if0, addr: A }));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
}

#[test]
fn s_ip_acd_002_a_tentative_address_is_no_source() {
    let mut h = probing();
    first_probe(&mut h);
    assert_eq!(h.ip.route(B, Source::Bound(A), None), Err(Counter::RouteNoSourceAddress));
    assert_eq!(h.udp_to(B), Err(Counter::RouteNoSourceAddress));
    assert!(h.state(B).is_none());
}

fn conflict_while_probing(frame: Vec<u8>) {
    let mut h = probing();
    first_probe(&mut h);
    h.frame(&frame);
    assert_eq!(h.ip.address(h.if0, A), None);
    assert!(h.events.contains(&Event::Conflict { iface: h.if0, addr: A, mac: MAC_B }));
    assert_eq!(h.refusals(Counter::AcdConflict)[0].peer, Peer::Arp { ip: A, mac: MAC_B });
    assert!(h.run(10_000).is_empty(), "no more probes");
}

#[test]
fn s_ip_acd_003_an_answer_to_our_probe() {
    conflict_while_probing(eth(MAC_A, MAC_B, 0x0806, &arp_packet(2, MAC_B, A, MAC_A, ip4(0, 0, 0, 0))));
}

#[test]
fn s_ip_acd_004_an_announcement_of_our_candidate() {
    conflict_while_probing(hex(V_ARP_CONFLICT_B));
}

#[test]
fn s_ip_acd_005_a_simultaneous_prober() {
    conflict_while_probing(hex(V_ARP_PROBE_B));
}

#[test]
fn s_ip_acd_006_our_own_probe_reflected() {
    let mut h = probing();
    first_probe(&mut h);
    let mut reflected = hex(V_ARP_PROBE);
    reflected[6..12].copy_from_slice(&MAC_X.0);
    h.frame(&reflected);
    assert_eq!(h.count(Counter::ArpOwnSender), 1);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Tentative));
}

/// Probing through the third probe; returns when it left.
fn third_probe(h: &mut H) -> Instant {
    let mut probes = 0;
    let mut last = h.clock();
    while probes < 3 {
        let d = h.ip.next_deadline().unwrap();
        h.ip.fire(d);
        probes += h.ip.transmit(d, usize::MAX, |_, _| {});
        last = d;
    }
    last
}

#[test]
fn s_ip_acd_007_the_conflict_window_ends_with_announce_wait() {
    let mut h = probing();
    let third = third_probe(&mut h);
    let _ = h.ip.receive(third.after(ANNOUNCE_WAIT.saturating_sub(Duration::from_millis(1))), h.if0, &hex(V_ARP_CONFLICT_B));
    h.collect();
    assert_eq!(h.ip.address(h.if0, A), None);
    assert!(h.events.contains(&Event::Conflict { iface: h.if0, addr: A, mac: MAC_B }));

    let mut h = probing();
    let third = third_probe(&mut h);
    let at = third.after(ANNOUNCE_WAIT + Duration::from_millis(1));
    h.ip.fire(at);
    h.ip.transmit(at, usize::MAX, |_, _| {});
    let _ = h.ip.receive(at, h.if0, &hex(V_ARP_CONFLICT_B));
    h.collect();
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    let mut out = Vec::new();
    h.ip.transmit(at, usize::MAX, |_, f| out.push(f.to_vec()));
    assert_eq!(out, [hex(V_ARP_ANNOUNCE)], "defended with one announcement");
    assert_eq!(h.count(Counter::AcdDefended), 1);
}

/// Ten addresses lost while probing on if0; returns when the tenth's first probe left.
fn ten_conflicts(h: &mut H) -> Instant {
    let mut started = h.clock();
    for n in 0..10u8 {
        let candidate = ip4(192, 0, 2, 10 + n);
        h.ip.add_address(h.clock(), h.if0, candidate, 24).unwrap();
        started = first_probe(h);
        let conflict = eth(MacAddr::BROADCAST, MAC_B, 0x0806, &arp_packet(1, MAC_B, candidate, MacAddr::ZERO, candidate));
        h.frame(&conflict);
        assert_eq!(h.ip.address(h.if0, candidate), None);
    }
    assert_eq!(h.count(Counter::AcdConflict), 10);
    started
}

#[test]
fn s_ip_acd_008_ten_conflicts_rate_limit_new_candidates() {
    let mut h = H::bare();
    let tenth = ten_conflicts(&mut h);
    let added = tenth.after(Duration::from_secs(5));
    h.ip.add_address(added, h.if0, ip4(192, 0, 2, 40), 24).unwrap();
    assert_eq!(h.count(Counter::AcdRateLimited), 1);
    assert_eq!(h.ip.next_deadline(), Some(tenth.after(Duration::from_millis(60_000))));
}

#[test]
fn s_ip_acd_009_ten_quiet_minutes_lift_the_limit() {
    let mut h = H::bare();
    ten_conflicts(&mut h);
    let later = h.clock().after(Duration::from_secs(600));
    h.ip.add_address(later, h.if0, ip4(192, 0, 2, 40), 24).unwrap();
    assert_eq!(h.count(Counter::AcdRateLimited), 0);
    assert!(h.ip.next_deadline().unwrap().since(later) <= PROBE_WAIT);
}

#[test]
fn s_ip_acd_010_one_defence() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_CONFLICT_B));
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_ANNOUNCE)]);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
    assert_eq!(h.refusals(Counter::AcdDefended)[0].peer, Peer::Arp { ip: A, mac: MAC_B });
}

#[test]
fn s_ip_acd_011_a_second_conflict_inside_the_interval_loses() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_CONFLICT_B));
    h.out();
    h.at(9_999);
    h.frame(&hex(V_ARP_CONFLICT_B));
    assert_eq!(h.ip.address(h.if0, A), None);
    assert!(h.events.contains(&Event::Lost { iface: h.if0, addr: A, mac: MAC_B }));
    assert_eq!(h.count(Counter::AcdConflict), 1);
}

#[test]
fn s_ip_acd_012_a_second_conflict_after_the_interval_is_defended() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_CONFLICT_B));
    h.out();
    h.at(10_001);
    h.frame(&hex(V_ARP_CONFLICT_B));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
    assert_eq!(h.count(Counter::AcdDefended), 2);
}

#[test]
fn s_ip_acd_013_an_announcing_address_answers_probes() {
    let mut h = probing();
    third_probe(&mut h);
    let d = h.ip.next_deadline().unwrap();
    h.ip.fire(d);
    h.ip.transmit(d, usize::MAX, |_, _| {});
    h.now = d.nanos() / 1_000_000 - BASE_MS + 1;
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    h.frame(&hex(V_ARP_PROBE_B));
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_REPLY_TO_PROBE)]);
}

#[test]
fn s_ip_acd_014_a_renewal_is_not_probed() {
    let mut h = H::fixture_i();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    assert!(h.run(10_000).is_empty());
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
}

#[test]
fn s_ip_acd_015_link_down_abandons_probing() {
    let mut h = probing();
    first_probe(&mut h);
    h.ip.link_down(h.clock(), h.if0).unwrap();
    h.collect();
    assert!(h.events.contains(&Event::NotVerified { iface: h.if0, addr: A }));
    assert_eq!(h.ip.address(h.if0, A), None);
}

#[test]
fn s_ip_acd_016_link_up_announces_without_probing() {
    let mut h = H::fixture_i();
    h.ip.link_down(h.clock(), h.if0).unwrap();
    h.ip.link_up(h.clock(), h.if0).unwrap();
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
    let arp: Vec<(u64, Vec<u8>)> = h.run(5_000).into_iter().filter(|o| o.arp().is_some()).map(|o| (o.at, o.frame)).collect();
    assert_eq!(arp, [(0, hex(V_ARP_ANNOUNCE)), (2_000, hex(V_ARP_ANNOUNCE))]);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
}

#[test]
fn s_ip_acd_017_detection_starts_at_the_first_announcement() {
    let mut h = probing();
    third_probe(&mut h);
    let d = h.ip.next_deadline().unwrap();
    h.ip.fire(d);
    h.ip.transmit(d, usize::MAX, |_, _| {});
    h.now = d.nanos() / 1_000_000 - BASE_MS + 1;
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    h.frame(&hex(V_ARP_CONFLICT_B));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    assert_eq!(h.count(Counter::AcdDefended), 1);
}
