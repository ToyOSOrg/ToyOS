//! The shard's deficit round-robin over flows (DRR-01–03): A at 192.0.2.1
//! against B, C and D at .2, .3 and .4, each resolved in A's table, with A's credit only at the
//! opportunities a scenario names. Each expected order is derived by hand from the shard's egress
//! rules with Q = 1,514 (RFC 8290 §5.2.4), the way RFC 8290 §4.2's scheduler charges a flow, its
//! deficit allowed below zero.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_shard::Event;
use toyos_net_tcp::{ConnId, State};
use toyos_net_testnet::{Credit, Net};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::Instant;

const D: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);
const MAC_C: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0c]);
const MAC_D: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0d]);
const KIB_64: usize = 64 * 1024;

const B_BULK: &str = "TCP 192.0.2.1:49152>192.0.2.2:80 ACK len 1448";
const C_BULK: &str = "TCP 192.0.2.1:49153>192.0.2.3:80 ACK len 1448";
const D_BULK: &str = "TCP 192.0.2.1:49154>192.0.2.4:80 ACK len 1448";

struct Fixture {
    net: Net,
    a: usize,
    /// A's connections, established and idle, from 49152 to B, 49153 to C, 49154 to D, 49155 to
    /// B and 49156 to C, each to port 80.
    conns: [ConnId; 5],
}

/// A, B, C and D on one segment, A's five connections established, then A's device offering
/// nothing.
fn fixture() -> Fixture {
    let mut net = Net::new(Instant::from_millis(3_600_000));
    let a = net.add_node(MAC_A, A, 24);
    let peers = [(net.add_node(MAC_B, B, 24), B), (net.add_node(MAC_C, C, 24), C), (net.add_node(MAC_D, D, 24), D)];
    let verified = |net: &mut Net| [(a, A)].into_iter().chain(peers).all(|(n, addr)| net.nodes[n].events.contains(&Event::Verified(addr)));
    assert!(net.run_until(Duration::from_secs(1), verified), "every address verified");
    net.advance(toyos_net_ip::limits::acd::ANNOUNCE_INTERVAL * 2);
    for (n, addr) in peers {
        net.nodes[n].shard.listen(addr, Some(port(80)), || 0).unwrap();
    }
    let now = net.now();
    let conns = [(49152, B), (49153, C), (49154, D), (49155, B), (49156, C)].map(|(p, addr)| net.nodes[a].shard.connect(now, Some(port(p)), ep(addr, 80)).unwrap());
    let established = |net: &mut Net| conns.iter().all(|&id| net.nodes[a].shard.status(id).unwrap().state == State::Established);
    assert!(net.run_until(Duration::from_secs(1), established), "the handshakes complete");
    net.nodes[a].credit = Credit::None;
    Fixture { net, a, conns }
}

/// Frames one opportunity of A's yields with room for `credit`, by name and length.
fn opportunity(f: &mut Fixture, credit: usize) -> Vec<(String, usize)> {
    let now = f.net.now();
    let mut out = Vec::new();
    f.net.nodes[f.a].shard.transmit(now, credit, |frame| out.push((name(frame), frame.len())));
    out
}

fn names(out: &[(String, usize)]) -> Vec<&str> {
    out.iter().map(|(n, _)| n.as_str()).collect()
}

fn send(f: &mut Fixture, conn: usize, len: usize) {
    let now = f.net.now();
    let sent = f.net.nodes[f.a].shard.send(now, f.conns[conn], &vec![0x55; len]).unwrap();
    assert!(sent >= len.min(14_480), "a cwnd's worth queued");
}

const U: &str = "IPv4 Udp to 192.0.2.3";
const T: &str = B_BULK;
/// DRR-01's 18 frames: each round T one quantum, U what its deficit, debt included, allows.
const DRR_01: [&str; 18] = [T, U, U, T, U, T, U, U, T, U, T, U, U, T, T, T, T, T];

/// DRR-01's two flows: T, A's connection to B, writes 64 KiB, then U queues 8 datagrams of
/// 1,000 bytes to C.
fn bulk_and_datagrams() -> Fixture {
    let mut f = fixture();
    send(&mut f, 0, KIB_64);
    let now = f.net.now();
    let u = f.net.nodes[f.a].shard.bind(A, None, || 0).unwrap();
    for _ in 0..8 {
        f.net.nodes[f.a].shard.send_to(now, u, C, 9, &[0xaa; 1_000]).unwrap();
    }
    f
}

#[test]
fn s_shard_drr_001_a_negative_deficit_is_repaid() {
    let mut f = bulk_and_datagrams();
    let out = opportunity(&mut f, 18);
    assert_eq!(names(&out), DRR_01);
    for (name, len) in &out {
        assert_eq!(*len, if name == U { 1_042 } else { 1_514 }, "{name}");
    }
}

/// Credit that runs out mid-turn: the turn resumes at the next opportunity with no second
/// quantum, so DRR-01 one frame an opportunity is DRR-01.
#[test]
fn s_shard_drr_001_a_turn_spans_opportunities_on_one_quantum() {
    let mut f = bulk_and_datagrams();
    let out: Vec<(String, usize)> = (0..18).flat_map(|_| opportunity(&mut f, 1)).collect();
    assert_eq!(names(&out), DRR_01);
}

/// DRR-02's three bulk flows, B's, C's and D's, after its first opportunity.
fn bulk() -> (Fixture, Vec<(String, usize)>) {
    let mut f = fixture();
    for conn in 0..3 {
        send(&mut f, conn, KIB_64);
    }
    let first = opportunity(&mut f, 4);
    (f, first)
}

#[test]
fn s_shard_drr_002_a_bulk_flow_starves_no_other_beyond_its_quantum() {
    let (mut f, first) = bulk();
    assert_eq!(names(&first), [B_BULK, C_BULK, D_BULK, B_BULK]);

    send(&mut f, 3, 100);
    let second = opportunity(&mut f, 4);
    assert_eq!(names(&second), [C_BULK, D_BULK, B_BULK, "TCP 192.0.2.1:49155>192.0.2.2:80 ACK len 100"]);
    assert_eq!(second[3].1, 166, "100 bytes behind 54 of headers and 12 of timestamps");

    let third = opportunity(&mut f, 23);
    let rounds = [C_BULK, D_BULK, B_BULK].repeat(7);
    assert_eq!(names(&third), [rounds.as_slice(), &[C_BULK, D_BULK]].concat());
}

/// An ICMP echo request from B to A, as the frame B's device would carry.
fn echo_request() -> Vec<u8> {
    let mut icmp = vec![8, 0, 0, 0, 0x12, 0x34, 0, 1];
    icmp.extend_from_slice(b"drr-03");
    let check = sum(&icmp);
    icmp[2..4].copy_from_slice(&check.to_be_bytes());
    let len = u16::try_from(20 + icmp.len()).unwrap().to_be_bytes();
    let mut ip = vec![0x45, 0, len[0], len[1], 0, 0, 0x40, 0, 64, 1, 0, 0];
    ip.extend_from_slice(&B.octets());
    ip.extend_from_slice(&A.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    from_b(&[ip, icmp].concat())
}

/// D asking who has 192.0.2.1 (RFC 826).
fn arp_request_from_d() -> Vec<u8> {
    let mut frame = [MacAddr::BROADCAST.0, MAC_D.0].concat();
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 1]);
    frame.extend_from_slice(&MAC_D.0);
    frame.extend_from_slice(&D.octets());
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&A.octets());
    frame
}

#[test]
fn s_shard_drr_003_ip_and_owed_resets_go_first_and_the_round_resumes() {
    let (mut f, first) = bulk();
    assert_eq!(names(&first), [B_BULK, C_BULK, D_BULK, B_BULK]);
    let now = f.net.now();
    f.net.nodes[f.a].shard.receive(now, &echo_request());
    f.net.nodes[f.a].shard.receive(now, &arp_request_from_d());
    f.net.nodes[f.a].shard.abort(now, f.conns[4]).unwrap();
    let out = opportunity(&mut f, 6);
    assert_eq!(
        names(&out),
        ["IPv4 Icmp to 192.0.2.2", "ARP reply 192.0.2.1 to 192.0.2.4", "TCP 192.0.2.1:49156>192.0.2.3:80 RST,ACK len 0", C_BULK, D_BULK, B_BULK]
    );
}

// No id: a connection that takes the slot of one freed while still in the round has one turn,
// not its predecessor's as well.
#[test]
fn a_connection_that_takes_a_freed_slot_takes_one_turn() {
    let mut f = fixture();
    let (a, c) = (f.a, 2);
    f.net.nodes[c].shard.listen(C, Some(port(81)), || 0).unwrap();
    let now = f.net.now();
    let freed = f.net.nodes[a].shard.connect(now, Some(port(49157)), ep(B, 81)).unwrap();
    f.net.nodes[a].shard.abort(now, freed).unwrap();
    let third = f.net.nodes[a].shard.connect(now, Some(port(49158)), ep(C, 81)).unwrap();
    let syn = opportunity_frames(&mut f, 1);
    assert_eq!(syn.len(), 1);
    f.net.nodes[c].shard.receive(now, &syn[0]);
    let established = |net: &mut Net| net.nodes[a].shard.status(third).unwrap().state == State::Established;
    assert!(f.net.run_until(Duration::from_secs(1), established), "the SYN-ACK arrives");
    let now = f.net.now();
    f.net.nodes[a].shard.send(now, third, &[7; 3 * 1460]).unwrap();
    send(&mut f, 1, 3 * 1460);
    let ports: Vec<u16> = opportunity_frames(&mut f, 6).iter().filter_map(|frame| data(frame).map(|(port, ..)| port)).collect();
    assert_eq!(ports, [49158, 49153, 49158, 49153, 49158, 49153], "one turn each");
}

fn opportunity_frames(f: &mut Fixture, credit: usize) -> Vec<Vec<u8>> {
    let now = f.net.now();
    let mut out = Vec::new();
    f.net.nodes[f.a].shard.transmit(now, credit, |frame| out.push(frame.to_vec()));
    out
}

// No id: a sender leaves the round as its last datagram leaves, its debt with it. U ends
// its turn on its last datagram owing 570 bytes, a connection with 100 bytes becomes eligible, and
// U refills: U rejoins behind it with a fresh deficit and sends two frames on one quantum.
#[test]
fn a_sender_leaves_the_round_with_its_last_datagram() {
    let mut f = fixture();
    let now = f.net.now();
    let u = f.net.nodes[f.a].shard.bind(A, None, || 0).unwrap();
    for _ in 0..2 {
        f.net.nodes[f.a].shard.send_to(now, u, C, 9, &[0xaa; 1_000]).unwrap();
    }
    assert_eq!(names(&opportunity(&mut f, 2)), [U, U], "1,514 - 2 × 1,042 = -570");
    send(&mut f, 1, 100);
    for _ in 0..2 {
        f.net.nodes[f.a].shard.send_to(now, u, C, 9, &[0xaa; 1_000]).unwrap();
    }
    assert_eq!(names(&opportunity(&mut f, 3)), ["TCP 192.0.2.1:49153>192.0.2.3:80 ACK len 100", U, U]);
}
