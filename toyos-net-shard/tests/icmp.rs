//! ICMP errors [ip] validated reach TCP in its own terms.

mod common;

use std::time::Duration;

use common::*;
use toyos_net_tcp::{ConnId, Counter, Failure, SoftError, State};
use toyos_net_testnet::{Fate, Net};

/// B never sees A's TCP segments; the first of them A sent from record `start` on.
fn silence_tcp_to_b(net: &mut Net, a: usize, b: usize) {
    net.link(a, b).rule = Some(Box::new(|frame| if name(frame).starts_with("TCP") { Fate::Drop } else { Fate::Pass }));
}

fn first_tcp(net: &Net, a: usize, start: usize) -> Vec<u8> {
    net.wire()[start..].iter().find(|c| c.from == a && name(&c.frame).starts_with("TCP")).expect("a segment left").frame.clone()
}

/// A's connection to B with 1,000 bytes outstanding that B never receives; the segment carrying
/// them.
fn outstanding() -> (Net, usize, ConnId, Vec<u8>) {
    let (mut net, a, b) = segment();
    let (to_b, _) = established(&mut net, a, b);
    silence_tcp_to_b(&mut net, a, b);
    let start = net.wire().len();
    let now = net.now();
    net.nodes[a].shard.send(now, to_b, &[0x55; 1_000]).unwrap();
    net.settle();
    let segment = first_tcp(&net, a, start);
    (net, a, to_b, segment)
}

fn inject(net: &mut Net, node: usize, frame: &[u8]) {
    let now = net.now();
    net.nodes[node].shard.receive(now, frame);
}

#[test]
fn s_ic_001_shard_port_unreachable_refuses_a_connect() {
    let (mut net, a, b) = segment();
    silence_tcp_to_b(&mut net, a, b);
    let start = net.wire().len();
    let now = net.now();
    let id = net.nodes[a].shard.connect(now, Some(port(49152)), ep(B, 80)).unwrap();
    net.advance(Duration::from_millis(1));
    let syn = first_tcp(&net, a, start);
    inject(&mut net, a, &icmp_about(&syn, 3, 3, [0; 4]));
    assert_eq!(net.nodes[a].shard.status(id).unwrap().failure, Some(Failure::Refused));
}

#[test]
fn s_ic_008_shard_time_exceeded_is_soft() {
    let (mut net, a, to_b, segment) = outstanding();
    inject(&mut net, a, &icmp_about(&segment, 11, 0, [0; 4]));
    let status = net.nodes[a].shard.status(to_b).unwrap();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::TimeExceeded)));
}

#[test]
fn s_ic_009_shard_parameter_problem_is_soft() {
    let (mut net, a, to_b, segment) = outstanding();
    inject(&mut net, a, &icmp_about(&segment, 12, 0, [20, 0, 0, 0]));
    let status = net.nodes[a].shard.status(to_b).unwrap();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::ParameterProblem)));
}

#[test]
fn s_ic_012_shard_fragmentation_needed_without_an_mtu_is_ignored() {
    let (mut net, a, to_b, segment) = outstanding();
    inject(&mut net, a, &icmp_about(&segment, 3, 4, [0; 4]));
    assert_eq!(net.nodes[a].shard.tcp_counters().get(Counter::PmtuNoMtu), 1);
    assert_eq!(net.nodes[a].shard.status(to_b).unwrap().soft_error, None);
}
