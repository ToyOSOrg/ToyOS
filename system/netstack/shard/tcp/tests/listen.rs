//! Listening and accepting. LS-12, the SYN flood, runs in `tests/net.rs`.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_tcp::{limits, Counter, Error, Options, Received, State};

fn listening() -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(0);
    h.listener = Some(h.tcp.listen(A, Some(port(80)), || 0).unwrap());
    h
}

fn syn_from(h: &mut H, t: i64, peer: (Ipv4Addr, u16)) -> Vec<O> {
    h.input(t, seg(5000).syn().mss(1460).from(peer.0, peer.1))
}

fn ack_from(h: &mut H, t: i64, peer: (Ipv4Addr, u16)) -> Vec<O> {
    h.peer = peer;
    let outs = h.input(t, seg(5001).ack(1001).from(peer.0, peer.1));
    h.peer = (B, 40_000);
    outs
}

fn peer(i: u32) -> (Ipv4Addr, u16) {
    (Ipv4Addr::from(0xC633_6400 + (i >> 12)), 1024 + (i & 0xfff) as u16)
}

fn accept(h: &mut H) -> Option<toyos_net_tcp::ConnId> {
    h.tcp.accept(h.listener.unwrap()).unwrap()
}

#[test]
fn s_ls_001_one_listener_per_endpoint() {
    let mut h = listening();
    assert_eq!(h.tcp.listen(A, Some(port(80)), || 0), Err(Error::AddrInUse));
}

#[test]
fn s_ls_002_connections_do_not_hold_the_port() {
    let mut h = fixture_es();
    let now = h.now();
    h.tcp.close_listener(now, h.listener.unwrap()).unwrap();
    assert!(h.tcp.listen(A, Some(port(80)), || 0).is_ok());
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ls_003_the_pending_bound() {
    let mut h = listening();
    for i in 0..limits::LISTEN_PENDING as u32 {
        expect(&syn_from(&mut h, 0, peer(i)), &["CTL=SYN,ACK"]);
    }
    nothing(&syn_from(&mut h, 1, peer(9999)));
    assert_eq!(h.count(Counter::ListenOverflow), 1);
}

#[test]
fn s_ls_004_the_ready_bound() {
    let mut h = listening();
    for i in 0..=limits::LISTEN_READY as u32 {
        syn_from(&mut h, 0, peer(i));
    }
    for i in 0..limits::LISTEN_READY as u32 {
        ack_from(&mut h, 10, peer(i));
    }
    let last = peer(limits::LISTEN_READY as u32);
    nothing(&ack_from(&mut h, 20, last));
    assert_eq!(h.count(Counter::AcceptQueueFull), 1);
    accept(&mut h).unwrap();
    let outs = h.at(1000);
    assert!(outs.iter().any(|o| o.dst == last && o.is(SYN | ACK)), "the child's SYN-ACK is retransmitted");
    ack_from(&mut h, 1010, last);
    let mut accepted = 0;
    while accept(&mut h).is_some() {
        accepted += 1;
    }
    assert_eq!(accepted, limits::LISTEN_READY);
}

#[test]
fn s_ls_005_first_in_first_out() {
    let mut h = listening();
    for i in 0..3 {
        syn_from(&mut h, 0, peer(i));
    }
    for (k, i) in [2, 0, 1].into_iter().enumerate() {
        ack_from(&mut h, 10 + k as i64, peer(i));
    }
    let ids: Vec<_> = (0..3).map(|_| accept(&mut h).unwrap()).collect();
    let order: Vec<u16> = ids.into_iter().map(|id| h.tcp.tuple(id).unwrap().remote.port.get()).collect();
    assert_eq!(order, [peer(2).1, peer(0).1, peer(1).1]);
}

#[test]
fn s_ls_006_a_closed_child_is_accepted() {
    let mut h = listening();
    syn_from(&mut h, 0, (B, 40_000));
    h.input(1, seg(5001).ack(1001).fin());
    h.conn = accept(&mut h);
    assert_eq!(h.info().state, State::CloseWait);
    let (r, _) = h.call(2, |tcp, now, id| tcp.recv(now, id, &mut [0u8; 8]));
    assert_eq!(r, Ok(Received::End));
}

#[test]
fn s_ls_007_a_reset_child_is_never_returned() {
    let mut h = listening();
    syn_from(&mut h, 0, (B, 40_000));
    h.input(1, seg(5001).ack(1001));
    h.input(2, seg(5001).rst());
    assert_eq!(accept(&mut h), None);
    assert_eq!(h.count(Counter::AcceptResetDropped), 1);
}

#[test]
fn s_ls_008_closing_a_listener_resets_its_children() {
    let mut h = listening();
    syn_from(&mut h, 0, (B, 40_000));
    syn_from(&mut h, 0, (B, 40_001));
    ack_from(&mut h, 1, (B, 40_001));
    let now = h.now();
    h.tcp.close_listener(now, h.listener.unwrap()).unwrap();
    let outs = h.transmit();
    assert_eq!(outs.len(), 2);
    let pending = outs.iter().find(|o| o.dst == (B, 40_000)).unwrap();
    check(pending, "SEQ=1001 ACK=5001 CTL=RST,ACK");
    let ready = outs.iter().find(|o| o.dst == (B, 40_001)).unwrap();
    check(ready, "ACK=5001 CTL=RST,ACK");
    assert_eq!(h.count(Counter::ListenerClosedReset), 2);
}

#[test]
fn s_ls_009_the_specific_address_first() {
    let mut h = listening();
    let any = h.tcp.listen(Ipv4Addr::UNSPECIFIED, Some(port(80)), || 0).unwrap();
    syn_from(&mut h, 0, (B, 40_000));
    h.input(1, seg(5001).ack(1001));
    assert_eq!(h.tcp.accept(any).unwrap(), None);
    assert!(accept(&mut h).is_some());
}

#[test]
fn s_ls_010_children_inherit_options() {
    let mut h = listening();
    h.tcp.set_listener_options(h.listener.unwrap(), Options { nodelay: true, ..Options::default() }).unwrap();
    syn_from(&mut h, 0, (B, 40_000));
    h.input(1, seg(5001).ack(1001));
    h.conn = accept(&mut h);
    expect(&h.send(2, 10), &["LEN=10"]);
    expect(&h.send(3, 10), &["LEN=10"]);
}

#[test]
fn s_ls_011_a_bound_port_without_a_listener() {
    let mut h = fixture_e();
    let outs = h.input(1, seg(7000).syn().from(B, 81));
    expect(&outs, &["SEQ=0 ACK=7001 CTL=RST,ACK"]);
}
