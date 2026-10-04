//! UDP through the shard: what nobody takes is answered by [ip], and what the network says about
//! a connected socket's datagrams reaches it.

mod common;

use std::time::Duration;

use common::*;
use toyos_net_testnet::Fate;
use toyos_net_udp::{Error, SocketError};

#[test]
fn s_udp_us_035_shard_no_socket_is_a_port_unreachable() {
    let (mut net, a, b) = segment();
    let now = net.now();
    let socket = net.nodes[b].shard.bind(B, Some(port(5000)), || 0).unwrap();
    net.nodes[b].shard.send_to(now, socket, A, 5001, b"hi").unwrap();
    let start = net.wire().len();
    net.advance(Duration::from_millis(1));
    let a_sent: Vec<String> = net.wire()[start..].iter().filter(|c| c.from == a).map(|c| name(&c.frame)).filter(|f| !f.starts_with("ARP")).collect();
    assert_eq!(a_sent, ["IPv4 Icmp to 192.0.2.2"]);
}

#[test]
fn s_udp_us_046_shard_a_port_unreachable_refuses_the_connected_socket_once() {
    let (mut net, a, b) = segment();
    net.link(a, b).rule = Some(Box::new(|frame| if name(frame).starts_with("IPv4 Udp") { Fate::Drop } else { Fate::Pass }));
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, Some(port(50001)), || 0).unwrap();
    net.nodes[a].shard.udp_connect(now, socket, B, 53).unwrap();
    net.nodes[a].shard.send_to(now, socket, B, 53, b"hi").unwrap();
    let start = net.wire().len();
    net.advance(Duration::from_millis(1));
    let datagram = net.wire()[start..].iter().find(|c| c.from == a && name(&c.frame).starts_with("IPv4 Udp")).expect("the datagram left").frame.clone();
    let now = net.now();
    net.nodes[a].shard.receive(now, &icmp_about(&datagram, 3, 3, [0; 4]));
    let mut buf = [0u8; 16];
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Err(Error::Failed(SocketError::Refused)));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Ok(None));
}

#[test]
fn s_ip_icd_012_shard_a_resolution_failure_reaches_the_connected_socket() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, Some(port(50001)), || 0).unwrap();
    net.nodes[a].shard.udp_connect(now, socket, C, 53).unwrap();
    net.nodes[a].shard.send_to(now, socket, C, 53, b"hi").unwrap();
    let mut buf = [0u8; 16];
    net.advance(Duration::from_millis(2_999));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Ok(None));
    net.advance(Duration::from_millis(1));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Err(Error::Failed(SocketError::Unreachable)));
}
