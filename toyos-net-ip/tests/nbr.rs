//! ARP reception, and what a modern host accepts.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Counter, Nud, Peer, RefusalLog};
use toyos_net_wire::ethernet::MacAddr;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn request(sender_mac: MacAddr, sender: Ipv4Addr, target: Ipv4Addr) -> Vec<u8> {
    eth(MacAddr::BROADCAST, sender_mac, 0x0806, &arp_packet(1, sender_mac, sender, MacAddr::ZERO, target))
}

fn frames(out: &[Out]) -> Vec<Vec<u8>> {
    out.iter().map(|o| o.frame.clone()).collect()
}

#[test]
fn s_ip_nbr_001_our_own_mac_as_sender() {
    let mut h = H::fixture_i();
    let mut frame = hex(V_ARP_REQ_B);
    frame[22..28].copy_from_slice(&MAC_A.0);
    h.frame(&frame);
    assert_eq!(h.count(Counter::ArpOwnSender), 1);
    assert!(h.out().is_empty() && h.state(B).is_none());
}

#[test]
fn s_ip_nbr_002_prefix_edges_are_no_senders() {
    let mut h = H::fixture_i();
    for sender in [ip4(192, 0, 2, 255), ip4(192, 0, 2, 0)] {
        h.frame(&request(MAC_B, sender, A));
    }
    assert_eq!(h.count(Counter::ArpInvalidSenderAddress), 2);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_nbr_003_a_request_for_us() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_REQ_B));
    assert_eq!(frames(&h.out()), [hex(V_ARP_REPLY_A)]);
    assert!(h.is_stale(B));
    assert_eq!(h.mac_of(B), Some(MAC_B));
}

#[test]
fn s_ip_nbr_004_a_request_for_another_host() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_REQ_OTHER));
    assert!(h.out().is_empty() && h.state(B).is_none());
    assert_eq!(h.count(Counter::ArpNotForUs), 1);
}

#[test]
fn s_ip_nbr_005_a_known_host_asking_for_another() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.frame(&hex(V_ARP_REQ_OTHER));
    assert!(h.is_reachable(B));
    assert_eq!(h.mac_of(B), Some(MAC_B));
}

#[test]
fn s_ip_nbr_006_an_unsolicited_reply() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_REPLY));
    assert!(h.state(B).is_none());
    assert_eq!(h.count(Counter::ArpNotForUs), 1);
}

#[test]
fn s_ip_nbr_007_an_unknown_router_moving() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_ROUTER_MOVED));
    assert!(h.state(R).is_none());
}

#[test]
fn s_ip_nbr_008_a_known_router_moving() {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    h.frame(&hex(V_ARP_ROUTER_MOVED));
    assert!(h.is_stale(R));
    assert_eq!(h.mac_of(R), Some(MAC_X));
    assert_eq!(h.count(Counter::ArpMacChanged), 1);
    assert_eq!(h.refusals(Counter::ArpMacChanged)[0].peer, Peer::MacChange { ip: R, old: MAC_R, new: MAC_X });

    // A datagram released to R that still waits for room leaves to the MAC R holds then.
    let mut h = H::fixture_i();
    assert_eq!(h.send(A, R, 5001, 5001, b"1"), Ok(None));
    h.out();
    h.fill_control_queue(h.if0, h.clock(), A);
    h.frame(&request(MAC_R, R, A));
    assert!(matches!(h.state(R), Some(Nud::Stale(s)) if s.released.queued() == 1));
    h.frame(&hex(V_ARP_ROUTER_MOVED));
    assert_eq!(h.mac_of(R), Some(MAC_X));
    let out = h.out();
    assert_eq!(out.last().map(|o| (o.to(), o.ip().is_some())), Some((MAC_X, true)));
    assert!(out.iter().all(|o| o.to() != MAC_R));
}

#[test]
fn s_ip_nbr_009_locktime() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(500);
    h.frame(&hex(V_ARP_ROUTER_MOVED));
    assert!(h.is_reachable(R));
    assert_eq!(h.mac_of(R), Some(MAC_R));
    assert_eq!(h.refusals(Counter::ArpOverrideLocked).len(), 1);
    h.at(1_001);
    h.frame(&hex(V_ARP_ROUTER_MOVED));
    assert!(h.is_stale(R));
    assert_eq!(h.mac_of(R), Some(MAC_X));
}

#[test]
fn s_ip_nbr_010_a_second_answer_is_locked_out() {
    let mut h = H::fixture_i();
    let _ = h.ip.resolve(h.clock(), h.if0, R, A);
    h.out();
    h.at(5);
    h.frame(&hex(V_ARP_REPLY_R));
    assert!(h.is_reachable(R));
    h.at(10);
    h.frame(&hex(V_ARP_REPLY_R_MOVED));
    assert_eq!(h.mac_of(R), Some(MAC_R));
    assert_eq!(h.count(Counter::ArpOverrideLocked), 1);
}

#[test]
fn s_ip_nbr_011_an_unsolicited_same_mac_extends_nothing() {
    let mut h = H::fixture_i_with(R, MAC_R);
    h.at(20_000);
    let before = (format!("{:?}", h.state(R)), h.ip.next_deadline());
    h.frame(&hex(V_ARP_REPLY_R));
    assert_eq!((format!("{:?}", h.state(R)), h.ip.next_deadline()), before);
}

#[test]
fn s_ip_nbr_012_an_off_link_sender_is_answered_not_cached() {
    let mut h = H::fixture_i();
    let asker = ip4(198, 51, 100, 9);
    h.frame(&request(MAC_B, asker, A));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].arp().unwrap().target_ip, asker);
    assert!(h.state(asker).is_none());
    assert_eq!(h.count(Counter::ArpSenderOffLink), 1);
}

// RFC 3927 §2.6.2: a host in 169.254/16 is on this link whatever address the interface holds, so
// its request is a neighbour's: answered, and cached to be verified before it is trusted. Before
// the interface has an address nothing is on its link. §2.5 has that host broadcast its request,
// as every request here is.
#[test]
fn rfc_3927_2_6_2_a_link_local_sender_is_a_neighbour() {
    let asker = ip4(169, 254, 3, 4);
    let mut h = H::fixture_i();
    h.frame(&request(MAC_B, asker, A));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].to(), out[0].arp().unwrap().target_ip), (MAC_B, asker));
    assert!(h.is_stale(asker));
    assert_eq!(h.mac_of(asker), Some(MAC_B));
    assert_eq!(h.count(Counter::ArpSenderOffLink), 0);

    let mut h = H::bare();
    h.frame(&request(MAC_B, asker, A));
    assert!(h.out().is_empty() && h.state(asker).is_none());
    assert_eq!(h.count(Counter::ArpSenderOffLink), 1);
}

// 169.254.0.0 and 169.254.255.255 are the network and the broadcast address of that prefix
// (RFC 3927 §2.6.2 names the second), and no host's on any interface: as a sender, in a request
// for our address or in a reply to a request of ours, each is refused as our own prefixes' edges
// are, answered nothing and cached nowhere.
#[test]
fn rfc_3927_the_edges_of_the_link_local_prefix_are_no_senders() {
    for edge in [ip4(169, 254, 0, 0), ip4(169, 254, 255, 255)] {
        let mut h = H::fixture_i();
        h.frame(&request(MAC_B, edge, A));
        h.frame(&eth(MAC_A, MAC_B, 0x0806, &arp_packet(2, MAC_B, edge, MAC_A, A)));
        assert_eq!(h.count(Counter::ArpInvalidSenderAddress), 2, "{edge}");
        assert!(h.out().is_empty() && h.state(edge).is_none(), "{edge}");
    }
}

#[test]
fn s_ip_nbr_013_a_probe_is_answered_and_not_cached() {
    let mut h = H::fixture_i();
    h.frame(&hex(V_ARP_PROBE_B));
    assert_eq!(frames(&h.out()), [hex(V_ARP_REPLY_TO_PROBE)]);
    assert!(h.state(ip4(0, 0, 0, 0)).is_none());
}

#[test]
fn s_ip_nbr_014_a_reply_for_another_target_asserts() {
    let mut h = H::fixture_i();
    h.udp_to(REMOTE).unwrap();
    h.out();
    h.frame(&eth(MAC_A, MAC_R, 0x0806, &arp_packet(2, MAC_R, R, MAC_A, ip4(192, 0, 2, 7))));
    assert!(h.is_stale(R));
    assert_eq!(h.mac_of(R), Some(MAC_R));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].to(), MAC_R);
}

#[test]
fn s_ip_nbr_015_a_broadcast_reply_is_still_solicited() {
    let mut h = H::fixture_i();
    let _ = h.ip.resolve(h.clock(), h.if0, R, A);
    h.out();
    let mut frame = hex(V_ARP_REPLY_R);
    frame[..6].copy_from_slice(&MacAddr::BROADCAST.0);
    h.frame(&frame);
    assert!(h.is_reachable(R));
}

#[test]
fn s_ip_nbr_016_the_strong_model_for_arp() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.settle();
    let arp = request(MAC_X, ip4(198, 51, 100, 9), A);
    let _ = h.ip.receive(h.clock(), if1, &eth(MacAddr::BROADCAST, MAC_X, 0x0806, &arp[14..]));
    assert_eq!(h.ip.transmit(h.clock(), usize::MAX, |_, _| {}), 0);
    assert_eq!(h.count(Counter::ArpNotForUs), 1);
}

#[test]
fn s_ip_nbr_017_a_tentative_address_is_not_answered() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    h.frame(&hex(V_ARP_REQ_B));
    assert!(h.out().iter().all(|o| o.arp().is_some_and(|a| a.sender_ip.is_unspecified())));
    assert_eq!(h.count(Counter::AcdConflict), 0);
}

#[test]
fn s_ip_nbr_018_fifty_mac_changes_one_line() {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    for n in 0..50u8 {
        h.at(u64::from(n) * 20);
        let m = MacAddr([2, 0, 0, 0, 1, n]);
        h.frame(&eth(MacAddr::BROADCAST, m, 0x0806, &arp_packet(1, m, R, MacAddr::ZERO, R)));
    }
    assert_eq!(h.count(Counter::ArpMacChanged), 50);
    let refusals = h.refusals(Counter::ArpMacChanged);
    let mut log = RefusalLog::default();
    let lines: Vec<u64> = refusals.iter().enumerate().filter_map(|(n, r)| log.admit(H::instant(n as u64 * 20), r.rule)).collect();
    assert_eq!(lines, [0]);
    assert_eq!(log.admit(H::instant(10_000), Counter::ArpMacChanged), Some(49));
}

#[test]
fn s_arp_016_the_sender_mac_is_what_is_learned() {
    let mut h = H::host(MAC_B, B, 24);
    let mut frame = hex(V_ARP_REQ);
    frame[6..12].copy_from_slice(&MAC_X.0);
    h.frame(&frame);
    assert_eq!(h.mac_of(A), Some(MAC_A));
}

#[test]
fn s_arp_017_a_group_sender_mac() {
    let mut h = H::fixture_i();
    for sender in [MacAddr::BROADCAST, MacAddr([1, 0, 0x5e, 0, 0, 1])] {
        h.frame(&eth(MacAddr::BROADCAST, MAC_B, 0x0806, &arp_packet(1, sender, B, MacAddr::ZERO, A)));
    }
    assert_eq!(h.count(Counter::ArpGroupSenderHardware), 2);
    assert!(h.out().is_empty());
}

#[test]
fn s_arp_018_invalid_sender_addresses() {
    let mut h = H::fixture_i();
    for sender in [ip4(255, 255, 255, 255), ip4(224, 0, 0, 1), ip4(127, 0, 0, 1), ip4(240, 0, 0, 1)] {
        h.frame(&request(MAC_B, sender, A));
    }
    h.frame(&eth(MAC_A, MAC_B, 0x0806, &arp_packet(2, MAC_B, ip4(0, 0, 0, 0), MAC_A, A)));
    assert_eq!(h.count(Counter::ArpInvalidSenderAddress), 5);
    assert!(h.out().is_empty());
    h.frame(&request(MAC_B, ip4(0, 0, 0, 0), ip4(192, 0, 2, 7)));
    assert_eq!(h.count(Counter::ArpInvalidSenderAddress), 5, "a probe is a request from 0.0.0.0");
    assert!(h.state(ip4(0, 0, 0, 0)).is_none());
    assert!(matches!(h.state(B), None | Some(Nud::Failed)));
}
