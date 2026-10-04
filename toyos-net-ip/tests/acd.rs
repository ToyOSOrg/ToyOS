//! Address conflict detection, on RFC 5227's schedule scaled by the owner's ruling (`limits::acd`).

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_ip::limits::acd::{ANNOUNCE_WAIT, PROBE_MAX, PROBE_MIN, PROBE_WAIT};
use toyos_net_ip::{limits, AddrState, Counter, Event, IfIndex, Instant, Peer, Source};
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
    // The ruling: RFC 5227's 1 s and 2 s times 200/7,000, in whole nanoseconds.
    let (one, two) = (Duration::from_nanos(28_571_428), Duration::from_nanos(57_142_857));
    assert_eq!([PROBE_WAIT, PROBE_MIN, PROBE_MAX, ANNOUNCE_WAIT], [one, one, two, two]);
    let mut h = probing();
    let t0 = h.clock();
    let out = run_exact(&mut h, t0.after(Duration::from_secs(5)));
    let probes: Vec<Instant> = out.iter().filter(|(_, f)| *f == hex(V_ARP_PROBE)).map(|(at, _)| *at).collect();
    let announcements: Vec<Instant> = out.iter().filter(|(_, f)| *f == hex(V_ARP_ANNOUNCE)).map(|(at, _)| *at).collect();
    assert_eq!(probes.len(), 3);
    assert_eq!(announcements.len(), 2);
    assert!(probes[0].since(t0) <= one);
    for pair in probes.windows(2) {
        assert!((one..=two).contains(&pair[1].since(pair[0])));
    }
    assert_eq!(announcements[0].since(probes[2]), two);
    assert_eq!(announcements[1].since(announcements[0]), Duration::from_secs(2));
    let usable = announcements[0].since(t0);
    assert!(usable >= Duration::from_nanos(114_285_713) && usable <= Duration::from_millis(200), "{usable:?}");
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

/// if1 at 198.51.100.1/24 beside if0, which holds no address.
fn beside_if1() -> (H, IfIndex) {
    let mut h = H::raw();
    let if1 = h.add_if1();
    h.assign(if1, ip4(198, 51, 100, 1), 24, 0);
    h.settle();
    (h, if1)
}

#[test]
fn s_ip_acd_018_a_full_control_queue_holds_probes_and_announcements() {
    let (mut h, if1) = beside_if1();
    let t0 = h.clock();
    h.fill_control_queue(if1, t0, ip4(198, 51, 100, 1));
    h.ip.add_address(t0, h.if0, A, 24).unwrap();
    let at = t0.after(Duration::from_secs(1));
    while let Some(d) = h.ip.next_deadline().filter(|d| *d <= at) {
        h.ip.fire(d);
    }
    h.collect();
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Tentative), "never verified without its probes on the wire");
    assert!(!h.events.iter().any(|e| matches!(e, Event::Verified { .. })));
    assert_eq!(h.count(Counter::IpControlQueueFull), 0, "the probe waits; it is not dropped");

    let mut out = Vec::new();
    h.ip.transmit(at, limits::CONTROL_QUEUE, |iface, f| out.push((iface, f.to_vec())));
    assert_eq!(out.len(), limits::CONTROL_QUEUE);
    assert!(out.iter().all(|(iface, _)| *iface == if1), "the replies queued first");
    out.clear();
    h.ip.transmit(at, 1, |iface, f| out.push((iface, f.to_vec())));
    assert_eq!(out, [(h.if0, hex(V_ARP_PROBE))]);
    let rest: Vec<Vec<u8>> = run_exact(&mut h, at.after(Duration::from_secs(5))).into_iter().map(|(_, f)| f).collect();
    assert_eq!(rest, [hex(V_ARP_PROBE), hex(V_ARP_PROBE), hex(V_ARP_ANNOUNCE), hex(V_ARP_ANNOUNCE)]);
    assert!(h.events.contains(&Event::Verified { iface: h.if0, addr: A }));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));

    // A probes with credit until its third probe leaves; then if1 fills the queue, and the first
    // announcement it owes waits there, counted toward nothing until it leaves.
    let (mut h, if1) = beside_if1();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    let p3 = third_probe(&mut h);
    h.fill_control_queue(if1, p3, ip4(198, 51, 100, 1));
    let verified = p3.after(ANNOUNCE_WAIT);
    h.ip.fire(verified);
    h.collect();
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    assert!(h.events.contains(&Event::Verified { iface: h.if0, addr: A }));
    assert_eq!(h.count(Counter::IpControlQueueFull), 0, "the announcement waits; it is not dropped");
    let t1 = verified.after(Duration::from_secs(2));
    while let Some(d) = h.ip.next_deadline().filter(|d| *d <= t1) {
        h.ip.fire(d);
    }
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing), "not assigned while its first announcement waits");
    let mut out = Vec::new();
    h.ip.transmit(t1, usize::MAX, |iface, f| out.push((iface, f.to_vec())));
    assert_eq!(out.len(), limits::CONTROL_QUEUE + 1, "nothing more was queued");
    assert!(out[..limits::CONTROL_QUEUE].iter().all(|(iface, _)| *iface == if1), "the replies queued first");
    assert_eq!(out[limits::CONTROL_QUEUE], (h.if0, hex(V_ARP_ANNOUNCE)));
    assert!(run_exact(&mut h, t1.after(Duration::from_millis(1_999))).is_empty());
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing), "ANNOUNCE_INTERVAL runs from the hand-off");
    let second = t1.after(Duration::from_secs(2));
    assert_eq!(h.ip.next_deadline(), Some(second));
    h.ip.fire(second);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing), "not assigned while its second announcement waits");
    out.clear();
    h.ip.transmit(second, usize::MAX, |iface, f| out.push((iface, f.to_vec())));
    assert_eq!(out, [(h.if0, hex(V_ARP_ANNOUNCE))]);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));

    // A defence due while the queue is full waits too, one at a time.
    let mut h = H::fixture_i();
    h.fill_control_queue(h.if0, h.clock(), A);
    h.frame(&hex(V_ARP_CONFLICT_B));
    h.at(10_001);
    h.frame(&hex(V_ARP_CONFLICT_B));
    assert_eq!(h.count(Counter::AcdDefended), 2);
    assert_eq!(h.count(Counter::IpControlQueueFull), 0);
    let out = h.out();
    assert_eq!(out.len(), limits::CONTROL_QUEUE + 1);
    assert!(out[..limits::CONTROL_QUEUE].iter().all(|o| o.arp().is_some_and(|a| a.operation == toyos_net_wire::arp::Operation::Reply)));
    assert_eq!(out[limits::CONTROL_QUEUE].frame, hex(V_ARP_ANNOUNCE));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
}
