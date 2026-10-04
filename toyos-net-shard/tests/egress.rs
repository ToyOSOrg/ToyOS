//! The shard's egress: [ip]'s frames ahead of the data flows (`ip.md` §5.4), and a TCP flow that
//! builds nothing until its next hop resolves (`ip.md` §6.7, §9.6).

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_ip::Counter as IpCounter;
use toyos_net_ip::Nud;
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Counter, Error, Failure, SoftError, State};
use toyos_net_testnet::{Credit, Net};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::icmp::UnreachableCode;
use toyos_net_wire::Instant;

const HOST_UNREACHABLE: Failure = Failure::Unreachable(SoftError::Unreachable(UnreachableCode::Host));
/// A third node, 192.0.2.4.
const D: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);
const MAC_D: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0d]);

/// Frames one transmit opportunity of A's yields with room for `credit`.
fn opportunity(net: &mut Net, node: usize, credit: usize) -> Vec<String> {
    let now = net.now();
    let mut out = Vec::new();
    net.nodes[node].shard.transmit(now, credit, |frame| out.push(name(frame)));
    out
}

#[test]
fn s_ip_out_007_an_arp_request_goes_before_a_ready_segment() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    net.nodes[a].credit = Credit::None;
    let now = net.now();
    net.nodes[a].shard.connect(now, Some(port(49153)), ep(C, 80)).unwrap();
    net.nodes[a].shard.send(now, to_b, &[0x55; 4_000]).unwrap();

    // C's flow asks for its next hop, which queues the request; B's flow, resolved, sends.
    assert_eq!(opportunity(&mut net, a, 1), ["TCP 192.0.2.1:49152>192.0.2.2:80 ACK len 1448"]);
    // A segment is ready for B and a request is queued: the one frame of credit is the request's.
    assert_eq!(opportunity(&mut net, a, 1), ["ARP request 192.0.2.3"]);
    assert_eq!(opportunity(&mut net, a, 1), ["TCP 192.0.2.1:49152>192.0.2.2:80 ACK len 1448"]);
}

#[test]
fn s_pl_012_shard_a_connect_waits_for_its_next_hop_and_then_leaves() {
    let (mut net, a, b) = segment();
    net.nodes[b].shard.listen(B, Some(port(80)), || 0).unwrap();
    let start = net.wire().len();
    let now = net.now();
    let id = net.nodes[a].shard.connect(now, Some(port(49152)), ep(B, 80)).unwrap();
    let done = net.run_until(Duration::from_secs(1), |net| net.nodes[a].shard.status(id).unwrap().state == State::Established);
    assert!(done, "{}", dump(&net, start));

    // Pending: the request leaves and no SYN is built, so none can wait anywhere; the SYN is
    // built once the reply is in.
    let wire: Vec<(u64, usize, String)> = net.wire()[start..].iter().map(|c| ((c.at.nanos() - now.nanos()) / 1_000, c.from, name(&c.frame))).collect();
    assert_eq!(
        wire[..3],
        [
            (0, a, "ARP request 192.0.2.2".to_string()),
            (50, b, "ARP reply 192.0.2.2 to 192.0.2.1".to_string()),
            (100, a, "TCP 192.0.2.1:49152>192.0.2.2:80 SYN len 0".to_string()),
        ]
    );
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::Rto), 0, "no expiry for a SYN that never left");
}

#[test]
fn s_pl_013_shard_a_connect_to_no_one_fails_at_once_as_host_unreachable() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let id = net.nodes[a].shard.connect(now, Some(port(49152)), ep(C, 80)).unwrap();
    net.advance(Duration::from_millis(2_999));
    assert_eq!(net.nodes[a].shard.status(id).unwrap().state, State::SynSent, "resolution is still under way");

    // Three requests unanswered: FAILED at 3,000, and the connect with it (IP-D9).
    net.advance(Duration::from_millis(1));
    assert_eq!(net.nodes[a].shard.status(id).unwrap().failure, Some(HOST_UNREACHABLE));
    let (now, mut buf) = (net.now(), [0u8; 16]);
    assert_eq!(net.nodes[a].shard.recv(now, id, &mut buf), Err(Error::Failed(HOST_UNREACHABLE)));
    let counters = net.nodes[a].shard.tcp_counters();
    assert_eq!((counters.get(Counter::Rto), counters.get(Counter::NextHopFailed)), (0, 1));
    // The question peeked the FAILED entry: it refused no datagram (`ip.md` §6.7 (2)).
    assert_eq!(net.nodes[a].shard.ip().counters().get(IpCounter::NbFailedRefused), 0);
}

#[test]
fn s_pl_013_shard_a_connect_with_no_route_fails_at_once() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let id = net.nodes[a].shard.connect(now, Some(port(49152)), ep(B, 80)).unwrap();
    net.nodes[a].shard.link_down(now).unwrap();
    net.advance(Duration::from_secs(5));
    assert_eq!(net.nodes[a].shard.status(id).unwrap().failure, Some(HOST_UNREACHABLE));
    let counters = net.nodes[a].shard.tcp_counters();
    assert_eq!((counters.get(Counter::Rto), counters.get(Counter::NextHopFailed)), (0, 1));
    assert_eq!(routes_refused(&net, a), 1, "one question, refused once");
}

/// Route lookups [ip] refused for want of a usable source, which a link that is down leaves.
fn routes_refused(net: &Net, node: usize) -> u64 {
    net.nodes[node].shard.ip().counters().get(IpCounter::RouteNoSourceAddress)
}

/// A forgets every neighbour: a link down and up again (`ip.md` §6.10).
fn forget(net: &mut Net, node: usize) {
    let now = net.now();
    net.nodes[node].shard.link_down(now).unwrap();
    net.nodes[node].shard.link_up(now).unwrap();
    let iface = net.nodes[node].shard.iface();
    assert!(net.nodes[node].shard.ip().neighbour(iface, B).is_none());
}

/// What `node` put on the wire from record `start` on, by name.
fn sent(net: &Net, node: usize, start: usize) -> Vec<String> {
    net.wire()[start..].iter().filter(|c| c.from == node).map(|c| name(&c.frame)).collect()
}

#[test]
fn s_pl_015_shard_an_owed_reset_waits_for_its_next_hop() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    forget(&mut net, a);
    let start = net.wire().len();
    let now = net.now();
    net.nodes[a].shard.abort(now, to_b).unwrap();
    net.advance(Duration::from_millis(10));
    let a_sent = sent(&net, a, start);
    let request = a_sent.iter().position(|f| f == "ARP request 192.0.2.2").expect("a request");
    let reset = a_sent.iter().position(|f| f.contains("RST")).expect("the reset left");
    assert!(request < reset, "{a_sent:?}");
}

#[test]
fn s_pl_015_shard_a_reset_to_a_peer_that_never_answers_is_dropped() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    forget(&mut net, a);
    net.link(b, a).dark = Some((net.now(), net.now().after(Duration::from_secs(3_600))));
    let start = net.wire().len();
    let now = net.now();
    net.nodes[a].shard.abort(now, to_b).unwrap();
    net.advance(Duration::from_millis(3_000));
    assert!(sent(&net, a, start).iter().all(|f| !f.contains("RST")), "the reset never left");
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed), 1);
}

#[test]
fn s_pl_014_shard_an_established_flow_records_host_unreachable() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    forget(&mut net, a);
    net.link(b, a).dark = Some((net.now(), net.now().after(Duration::from_secs(3_600))));
    let now = net.now();
    net.nodes[a].shard.send(now, to_b, &[0x55; 100]).unwrap();
    net.advance(Duration::from_millis(2_999));
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, None);
    net.advance(Duration::from_millis(1));
    let status = net.nodes[a].shard.status(to_b).unwrap();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::Unreachable(UnreachableCode::Host))));
    let failed = |net: &Net| net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed);
    assert_eq!((net.nodes[a].shard.tcp_counters().get(Counter::Rto), failed(&net)), (0, 1), "nothing left, so nothing expired");
    // The next question is at the hold-down's end, 20 s on, which resolves B afresh; that fails
    // 3 s later, a second segment not built.
    net.advance(Duration::from_millis(22_999));
    assert_eq!(failed(&net), 1);
    net.advance(Duration::from_millis(1));
    assert_eq!(failed(&net), 2);
    assert_eq!(net.nodes[a].shard.ip().counters().get(IpCounter::NbFailedRefused), 0);
}

#[test]
fn s_pl_014_shard_an_established_flow_with_no_route_records_host_unreachable() {
    let (mut net, a, b) = segment();
    let (to_b, from_a) = established(&mut net, a, b);
    let now = net.now();
    net.nodes[a].shard.link_down(now).unwrap();
    net.nodes[a].shard.send(now, to_b, &[0x55; 100]).unwrap();
    net.advance(Duration::from_secs(10));
    let status = net.nodes[a].shard.status(to_b).unwrap();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::Unreachable(UnreachableCode::Host))));
    assert_eq!((net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed), routes_refused(&net, a)), (1, 1), "ten seconds of opportunities ask once");

    // A route again: the flow asks, and its segment leaves; B's acknowledgment clears the error.
    let now = net.now();
    net.nodes[a].shard.link_up(now).unwrap();
    net.advance(Duration::from_secs(1));
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, None);
    let now = net.now();
    let mut buf = [0u8; 200];
    assert_eq!(net.nodes[b].shard.recv(now, from_a, &mut buf), Ok(toyos_net_tcp::Received::Data(100)));
}

/// A to B established and to D, a third node, established, each with its next hop resolved; then
/// B's entry FAILED, D's resolved, and nothing of either connection's owed.
fn b_failed_d_ready() -> (Net, usize, toyos_net_tcp::ConnId, toyos_net_tcp::ConnId) {
    let (mut net, a, b) = segment();
    let d = net.add_node(MAC_D, D, 24);
    assert!(net.run_until(Duration::from_secs(1), |net| net.nodes[d].events.contains(&toyos_net_shard::Event::Verified(D))));
    net.advance(toyos_net_ip::limits::acd::ANNOUNCE_INTERVAL * 2);
    let (to_b, _) = established(&mut net, a, b);
    let (to_d, _) = connected(&mut net, a, 49153, d, D);
    forget(&mut net, a);
    net.link(b, a).dark = Some((net.now(), net.now().after(Duration::from_secs(3_600))));
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, None, || 0).unwrap();
    net.nodes[a].shard.send_to(now, socket, B, 9, b"x").unwrap();
    net.nodes[a].shard.send_to(now, socket, D, 9, b"x").unwrap();
    net.advance(Duration::from_secs(4));
    let iface = net.nodes[a].shard.iface();
    assert!(matches!(net.nodes[a].shard.ip().neighbour(iface, B), Some(Nud::Failed)));
    assert!(net.nodes[a].shard.ip().neighbour(iface, D).is_some_and(|n| n.mac().is_some()));
    (net, a, to_b, to_d)
}

#[test]
fn s_pl_014_shard_a_failed_flow_is_asked_once_while_another_sends() {
    let (mut net, a, to_b, to_d) = b_failed_d_ready();
    net.nodes[a].credit = Credit::None;
    let now = net.now();
    net.nodes[a].shard.send(now, to_b, &[0x55; 100]).unwrap();
    net.nodes[a].shard.send(now, to_d, &[0x55; 4 * 1448]).unwrap();
    let before = net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed);
    let frames = opportunity(&mut net, a, 4);
    assert_eq!(frames, ["TCP 192.0.2.1:49153>192.0.2.4:80 ACK len 1448"; 4]);
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed) - before, 1, "B's flow is asked at the first frame, not at each");
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, Some(SoftError::Unreachable(UnreachableCode::Host)));
    assert_eq!(net.nodes[a].shard.ip().counters().get(IpCounter::NbFailedRefused), 0, "the question peeked the FAILED entry");
}

#[test]
fn s_pl_015_shard_a_reset_with_no_route_is_dropped() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    let now = net.now();
    net.nodes[a].shard.link_down(now).unwrap();
    let start = net.wire().len();
    net.nodes[a].shard.abort(now, to_b).unwrap();
    net.advance(Duration::from_secs(5));
    assert!(sent(&net, a, start).iter().all(|f| !f.starts_with("TCP")), "{}", dump(&net, start));
    assert_eq!((net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed), routes_refused(&net, a)), (1, 1));
}

#[test]
fn s_hs_034_shard_a_child_whose_peer_never_answers_gives_up() {
    let (mut net, a, b) = segment();
    net.nodes[a].shard.listen(A, Some(port(80)), || 0).unwrap();
    let start = net.wire().len();
    let now = net.now();
    net.link(b, a).dark = Some((now, now.after(Duration::from_secs(3_600))));
    net.nodes[a].shard.receive(now, &from_b(&segment_bytes((B, 40_000), (A, 80), 5_000, 0x02)));
    let given_up = |net: &Net| net.nodes[a].shard.tcp_counters().get(Counter::SynAckGiveUp);
    net.advance(Duration::from_millis(59_999));
    assert_eq!(given_up(&net), 0);
    net.advance(Duration::from_millis(1));
    assert_eq!(given_up(&net), 1, "60 s from the SYN, though no SYN-ACK ever left");
    assert!(sent(&net, a, start).iter().all(|f| !f.starts_with("TCP")), "{}", dump(&net, start));
    // Each failure of B's resolution is one segment not built: at 3 s, and again 3 s after each
    // hold-down's end, at 26 s and 49 s.
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed), 3);
}

#[test]
fn s_hs_035_shard_a_directed_broadcast_is_not_a_unicast_remote() {
    let (mut net, a, _) = segment();
    let start = net.wire().len();
    let now = net.now();
    let broadcast = ep(Ipv4Addr::new(192, 0, 2, 255), 80);
    assert_eq!(net.nodes[a].shard.connect(now, None, broadcast), Err(ConnectError::NotUnicast));
    net.advance(Duration::from_secs(1));
    assert!(sent(&net, a, start).is_empty());
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed), 0);
}

#[test]
fn s_pl_015_shard_a_reset_for_no_socket_waits_for_its_next_hop() {
    let (mut net, a, b) = segment();
    established(&mut net, a, b);
    forget(&mut net, a);
    let start = net.wire().len();
    let now = net.now();
    let refused = net.nodes[b].shard.connect(now, None, ep(A, 81)).unwrap();
    net.advance(Duration::from_millis(10));
    let a_sent = sent(&net, a, start);
    let request = a_sent.iter().position(|f| f == "ARP request 192.0.2.2").expect("a request");
    let reset = a_sent.iter().position(|f| f.contains("RST")).expect("the reset left");
    assert!(request < reset, "{a_sent:?}");
    assert_eq!(net.nodes[b].shard.status(refused).unwrap().failure, Some(Failure::Refused));
}

#[test]
fn s_pl_015_shard_a_time_wait_ack_waits_for_its_next_hop() {
    let (mut net, a, b) = segment();
    let (to_b, from_a) = established(&mut net, a, b);
    let now = net.now();
    net.nodes[a].shard.close(now, to_b).unwrap();
    net.advance(Duration::from_millis(10));
    assert_eq!(net.nodes[b].shard.status(from_a).unwrap().state, State::CloseWait);

    // A's answer to B's FIN is lost, and A forgets B in TIME-WAIT.
    let now = net.now();
    net.link(a, b).dark = Some((now, now.after(Duration::from_millis(10))));
    net.nodes[b].shard.close(now, from_a).unwrap();
    net.advance(Duration::from_millis(10));
    forget(&mut net, a);
    let start = net.wire().len();

    // B's FIN again: TIME-WAIT owes the ACK, which leaves once B's address is in, and B's
    // LAST-ACK ends with it.
    net.advance(Duration::from_secs(5));
    let a_sent = sent(&net, a, start);
    let request = a_sent.iter().position(|f| f == "ARP request 192.0.2.2").expect("a request");
    let ack = a_sent.iter().position(|f| f.starts_with("TCP") && f.contains("ACK")).expect("the ACK left");
    assert!(request < ack, "{a_sent:?}");
    let fins = sent(&net, b, start).iter().filter(|f| f.contains("FIN")).count();
    assert_eq!(fins, 1, "B retransmitted its FIN once: {}", dump(&net, start));
}

/// A established to B, then A holding B only as STALE: both forget, and B's datagram to A's
/// socket on port 9 makes B ask for A, which is how A learns B (`ip.md` §6.8 (b)).
fn stale_peer() -> (Net, usize, toyos_net_tcp::ConnId) {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    forget(&mut net, a);
    let now = net.now();
    net.nodes[b].shard.link_down(now).unwrap();
    net.nodes[b].shard.link_up(now).unwrap();
    net.nodes[a].shard.bind(A, Some(port(9)), || 0).unwrap();
    let socket = net.nodes[b].shard.bind(B, None, || 0).unwrap();
    net.nodes[b].shard.send_to(now, socket, A, 9, b"x").unwrap();
    net.advance(Duration::from_millis(1));
    let iface = net.nodes[a].shard.iface();
    assert!(matches!(net.nodes[a].shard.ip().neighbour(iface, B), Some(Nud::Stale(_))));
    (net, a, to_b)
}

fn state_of_b(net: &Net, a: usize) -> Option<&Nud> {
    net.nodes[a].shard.ip().neighbour(net.nodes[a].shard.iface(), B)
}

#[test]
fn s_ip_nud_026_shard_a_segment_to_a_stale_neighbour_moves_it_to_delay() {
    let (mut net, a, to_b) = stale_peer();
    let now = net.now();
    net.nodes[a].shard.send(now, to_b, b"y").unwrap();
    net.settle();
    assert!(matches!(state_of_b(&net, a), Some(Nud::Delay(_))), "{:?}", state_of_b(&net, a));
}

#[test]
fn s_ip_nud_009_shard_an_acknowledgment_ends_delay() {
    let (mut net, a, to_b) = stale_peer();
    let now = net.now();
    net.nodes[a].shard.send(now, to_b, b"y").unwrap();
    net.settle();
    assert!(matches!(state_of_b(&net, a), Some(Nud::Delay(_))));
    // B's ACK advances SND.UNA: TCP's positive advice reaches [ip] (`ip.md` §6.9).
    net.advance(Duration::from_millis(100));
    assert!(matches!(state_of_b(&net, a), Some(Nud::Reachable(_))), "{:?}", state_of_b(&net, a));
}

#[test]
fn s_ip_nud_020_shard_a_stalled_connect_reverifies_the_gateway() {
    let (mut net, a, _) = segment();
    let r = net.add_node(MAC_R, R, 24);
    assert!(net.run_until(Duration::from_secs(1), |net| net.nodes[r].events.contains(&toyos_net_shard::Event::Verified(R))));
    net.advance(toyos_net_ip::limits::acd::ANNOUNCE_INTERVAL * 2);
    let now = net.now();
    net.nodes[a].shard.set_gateways(now, &[R]).unwrap();
    let start = net.wire().len();
    net.nodes[a].shard.connect(now, Some(port(49152)), ep(std::net::Ipv4Addr::new(198, 51, 100, 7), 80)).unwrap();
    assert!(net.run_until(Duration::from_secs(1), |net| sent(net, a, start).iter().any(|f| f.contains("SYN"))));
    // SYNs at 0, 1 and 3 s; the third expiry, at 7 s, is R1: negative advice for the gateway.
    let first_syn = net.now();
    net.advance(first_syn.after(Duration::from_millis(6_999)).since(net.now()));
    assert!(!sent(&net, a, start).contains(&"ARP poll 192.0.2.254".to_string()), "{}", dump(&net, start));
    net.advance(Duration::from_millis(1));
    assert!(sent(&net, a, start).contains(&"ARP poll 192.0.2.254".to_string()), "{}", dump(&net, start));
    let iface = net.nodes[a].shard.iface();
    assert!(net.nodes[a].shard.ip().neighbour(iface, std::net::Ipv4Addr::new(198, 51, 100, 7)).is_none());
}

#[test]
fn s_ip_nud_022_shard_a_flow_refused_by_a_full_table_asks_again_once_an_entry_may_go() {
    let mut net = Net::new(Instant::from_millis(3_600_000));
    let a = net.add_node(MAC_A, A, 22);
    let b = net.add_node(MAC_B, B, 22);
    let verified = |net: &mut Net, n: usize, addr| net.nodes[n].events.contains(&toyos_net_shard::Event::Verified(addr));
    assert!(net.run_until(Duration::from_secs(1), |net| verified(net, a, A) && verified(net, b, B)));
    net.advance(toyos_net_ip::limits::acd::ANNOUNCE_INTERVAL * 2);
    let (to_b, from_a) = established(&mut net, a, b);
    forget(&mut net, a);

    // Connects to as many absent neighbours as the table holds, each asking ahead of B's flow:
    // every entry is INCOMPLETE, so none can go.
    let now = net.now();
    let absent = (1..=1_022u32).map(|n| Ipv4Addr::from(u32::from(Ipv4Addr::new(192, 0, 0, 0)) + n)).filter(|&addr| addr != A && addr != B);
    for addr in absent.take(toyos_net_ip::limits::nud::TABLE_MAX) {
        net.nodes[a].shard.connect(now, None, ep(addr, 80)).unwrap();
    }
    net.nodes[a].shard.send(now, to_b, &[0x55; 100]).unwrap();
    net.advance(Duration::from_millis(1));
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, Some(SoftError::Unreachable(UnreachableCode::Host)));
    let asked = |net: &Net| (net.nodes[a].shard.ip().counters().get(IpCounter::NbTableFull), net.nodes[a].shard.tcp_counters().get(Counter::NextHopFailed));
    assert_eq!(asked(&net), (1, 1));

    // [ip]'s timers fire at 1 s and 2 s, for the second and third requests: no entry may go yet,
    // and B's flow is not asked.
    net.advance(Duration::from_millis(2_998));
    assert_eq!(asked(&net), (1, 1));

    // At 3 s the requests fail and their entries may go: B's flow asks again, and leaves.
    net.advance(Duration::from_secs(4));
    let now = net.now();
    let mut buf = [0u8; 200];
    assert_eq!(net.nodes[b].shard.recv(now, from_a, &mut buf), Ok(toyos_net_tcp::Received::Data(100)));
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, None);
}

#[test]
fn s_ip_nud_024_shard_a_segments_request_prefers_its_source() {
    let (mut net, a, _) = segment();
    let (second, peer) = (Ipv4Addr::new(198, 51, 100, 1), Ipv4Addr::new(198, 51, 100, 9));
    let now = net.now();
    net.nodes[a].shard.add_address(now, second, 24).unwrap();
    assert!(net.run_until(Duration::from_secs(1), |net| net.nodes[a].events.contains(&toyos_net_shard::Event::Verified(second))));
    net.nodes[a].shard.listen(A, Some(port(80)), || 0).unwrap();
    let start = net.wire().len();
    let mut syn = from_b(&segment_bytes((peer, 40_000), (A, 80), 5_000, 0x02));
    syn[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 0x99]);
    let now = net.now();
    net.nodes[a].shard.receive(now, &syn);
    net.advance(Duration::from_millis(1));
    let request = net.wire()[start..].iter().find(|c| c.from == a && name(&c.frame) == format!("ARP request {peer}")).expect("a request");
    let frame = toyos_net_wire::ethernet::Frame::parse(&request.frame).unwrap();
    let arp = toyos_net_wire::arp::Arp::parse(frame.body()).unwrap();
    assert_eq!(arp.sender_ip, A, "the SYN-ACK's source, not {second}, which the prefix would pick");
}
