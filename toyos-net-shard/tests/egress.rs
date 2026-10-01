//! The shard's egress: [ip]'s frames ahead of the data flows (`ip.md` §5.4), and a TCP flow that
//! builds nothing until its next hop resolves (`ip.md` §6.7, §9.6).

mod common;

use std::time::Duration;

use common::*;
use toyos_net_tcp::{Counter, Error, Failure, SoftError, State};
use toyos_net_ip::Nud;
use toyos_net_testnet::Credit;
use toyos_net_wire::icmp::UnreachableCode;

/// Frames one transmit opportunity of A's yields with room for `credit`.
fn opportunity(net: &mut toyos_net_testnet::Net, node: usize, credit: usize) -> Vec<String> {
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
    let unreachable = Failure::Unreachable(SoftError::Unreachable(UnreachableCode::Host));
    assert_eq!(net.nodes[a].shard.status(id).unwrap().failure, Some(unreachable));
    let (now, mut buf) = (net.now(), [0u8; 16]);
    assert_eq!(net.nodes[a].shard.recv(now, id, &mut buf), Err(Error::Failed(unreachable)));
    let counters = net.nodes[a].shard.tcp_counters();
    assert_eq!((counters.get(Counter::Rto), counters.get(Counter::NextHopFailed)), (0, 1));
}

/// A forgets every neighbour: a link down and up again (`ip.md` §6.10).
fn forget(net: &mut toyos_net_testnet::Net, node: usize) {
    let now = net.now();
    net.nodes[node].shard.link_down(now).unwrap();
    net.nodes[node].shard.link_up(now).unwrap();
    let iface = net.nodes[node].shard.iface();
    assert!(net.nodes[node].shard.ip().neighbour(iface, B).is_none());
}

/// What `node` put on the wire from record `start` on, by name.
fn sent(net: &toyos_net_testnet::Net, node: usize, start: usize) -> Vec<String> {
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
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::Rto), 0, "nothing left, so nothing expired");
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
fn stale_peer() -> (toyos_net_testnet::Net, usize, toyos_net_tcp::ConnId) {
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

fn state_of_b(net: &toyos_net_testnet::Net, a: usize) -> Option<&Nud> {
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
fn s_ip_nud_026_shard_a_flow_with_nothing_to_send_leaves_stale_alone() {
    let (mut net, a, to_b) = stale_peer();
    let now = net.now();
    assert_eq!(net.nodes[a].shard.recv(now, to_b, &mut [0; 16]), Err(Error::WouldBlock));
    net.settle();
    assert!(matches!(state_of_b(&net, a), Some(Nud::Stale(_))), "{:?}", state_of_b(&net, a));
}

// No id: the alternation stands until architecture §3.3's deficit round-robin (DRR-01 to DRR-03)
// replaces it.
#[test]
fn tcp_and_udp_take_turns_frame_by_frame() {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    net.nodes[a].credit = Credit::None;
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, None, || 0).unwrap();
    for _ in 0..3 {
        net.nodes[a].shard.send_to(now, socket, B, 9, b"x").unwrap();
    }
    net.nodes[a].shard.send(now, to_b, &[0x55; 8_000]).unwrap();
    let kinds: Vec<char> = opportunity(&mut net, a, 6).iter().map(|f| f.chars().next().unwrap()).collect();
    assert!(kinds.windows(2).all(|w| w[0] != w[1]), "{kinds:?}");
    assert_eq!(kinds.iter().filter(|&&k| k == 'I').count(), 3, "three datagrams among six frames: {kinds:?}");
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
