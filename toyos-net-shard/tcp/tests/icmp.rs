//! ICMP errors, delivered already classified by [ip].

mod common;

use std::num::NonZeroU16;

use common::*;
use toyos_net_tcp::{Counter, Failure, IcmpKind, SoftError, State};
use toyos_net_wire::icmp::UnreachableCode;

fn connecting() -> H {
    let mut h = H::new(65_535);
    h.start(0);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    expect(&h.transmit(), &["SEQ=1000 CTL=SYN"]);
    h
}

fn code(n: u8) -> IcmpKind {
    IcmpKind::Unreachable(unreachable(n))
}

fn too_big(mtu: u16, quoted_length: u16) -> IcmpKind {
    IcmpKind::PacketTooBig { next_hop_mtu: NonZeroU16::new(mtu), quoted_length }
}

fn ten_outstanding() -> H {
    let mut h = fixture_e();
    ten_out(&mut h);
    h
}

#[test]
fn s_ic_001_port_unreachable_refuses_a_connect() {
    let mut h = connecting();
    h.icmp(1, code(3), 1000);
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::Refused)));
}

#[test]
fn s_ic_002_a_stale_quote() {
    let mut h = connecting();
    h.icmp(1, code(3), 1234);
    assert_eq!(h.count(Counter::IcmpStale), 1);
    assert_eq!(h.status().state, State::SynSent);
}

#[test]
fn s_ic_003_host_unreachable_is_soft() {
    let mut h = connecting();
    h.icmp(500, code(1), 1000);
    assert_eq!(h.count(Counter::IcmpSoft), 1);
    assert_eq!(h.status().soft_error, Some(SoftError::Unreachable(UnreachableCode::Host)));
    expect(&h.at(1000), &["CTL=SYN"]);
    h.at(183_000);
    let status = h.status();
    assert_eq!(status.failure, Some(Failure::Unreachable(SoftError::Unreachable(UnreachableCode::Host))));
}

#[test]
fn s_ic_004_administratively_prohibited() {
    let mut h = connecting();
    h.icmp(1, code(13), 1000);
    assert_eq!(h.status().failure, Some(Failure::Prohibited));
}

#[test]
fn s_ic_005_soft_in_a_synchronized_state() {
    let mut h = ten_outstanding();
    h.icmp(1, code(1), 2461);
    assert_eq!(h.status().soft_error, Some(SoftError::Unreachable(UnreachableCode::Host)));
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ic_006_port_unreachable_is_soft_when_synchronized() {
    let mut h = ten_outstanding();
    h.icmp(1, code(3), 2461);
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.count(Counter::IcmpSoft), 1);
    assert_eq!(h.count(Counter::IcmpHardAsSoft), 1);
    assert_eq!(h.refusals(Counter::IcmpHardAsSoft).len(), 1);
}

#[test]
fn s_ic_007_quotes_outside_the_flight() {
    let mut h = ten_outstanding();
    h.icmp(1, code(1), 15_601);
    h.icmp(2, code(1), 1000);
    assert_eq!(h.count(Counter::IcmpStale), 2);
    assert_eq!(h.status().soft_error, None);
}

#[test]
fn s_ic_008_time_exceeded_is_soft() {
    let mut h = ten_outstanding();
    h.icmp(1, IcmpKind::TimeExceeded, 1001);
    assert_eq!(h.status().soft_error, Some(SoftError::TimeExceeded));
}

#[test]
fn s_ic_009_parameter_problem_is_soft() {
    let mut h = ten_outstanding();
    h.icmp(1, IcmpKind::ParameterProblem, 1001);
    assert_eq!(h.status().soft_error, Some(SoftError::ParameterProblem));
}

#[test]
fn s_ic_010_an_unassigned_code_is_soft() {
    let mut h = ten_outstanding();
    h.icmp(1, code(16), 1001);
    assert_eq!(h.status().soft_error, Some(SoftError::Unreachable(unreachable(16))));
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ic_011_path_mtu_lowered() {
    let mut h = ten_outstanding();
    let ssthresh = h.info().ssthresh;
    expect(&h.icmp(1, too_big(1400, 1500), 1001), &["SEQ=1001 LEN=1360"]);
    let info = h.info();
    assert_eq!((info.smss, info.cwnd, info.ssthresh), (1360, 13_600, ssthresh));
    assert_eq!(h.count(Counter::PmtuLowered), 1);
    h.input(10, seg(5001).ack(15_601));
    assert!(h.send(11, 5000).iter().all(|o| o.payload.len() <= 1360));
}

#[test]
fn s_ic_012_no_next_hop_mtu() {
    let mut h = ten_outstanding();
    nothing(&h.icmp(1, IcmpKind::PacketTooBig { next_hop_mtu: None, quoted_length: 1500 }, 1001));
    assert_eq!(h.count(Counter::PmtuNoMtu), 1);
    assert_eq!(h.refusals(Counter::PmtuNoMtu).len(), 1);
    assert_eq!(h.info().smss, 1460);
}

#[test]
fn s_ic_013_an_mtu_that_is_no_smaller() {
    let mut h = ten_outstanding();
    nothing(&h.icmp(1, too_big(1500, 1500), 1001));
    assert_eq!(h.count(Counter::PmtuBogus), 1);
}

#[test]
fn s_ic_014_the_mtu_floor() {
    let mut h = ten_outstanding();
    h.icmp(1, too_big(300, 1500), 1001);
    assert_eq!(h.info().smss, 536);
    assert_eq!(h.count(Counter::PmtuFloored), 1);
}

#[test]
fn s_ic_015_an_mtu_not_below_the_refused_datagram() {
    let mut h = ten_outstanding();
    nothing(&h.icmp(1, too_big(1400, 1400), 1001));
    assert_eq!(h.count(Counter::PmtuBogus), 1);
}

#[test]
fn s_ic_016_one_retransmission_per_lowering() {
    let mut h = ten_outstanding();
    h.icmp(1, too_big(1400, 1500), 1001);
    nothing(&h.icmp(2, too_big(1400, 1500), 1001));
    assert_eq!(h.count(Counter::PmtuLowered), 1);
}

#[test]
fn s_ic_017_time_wait_is_stale() {
    let mut h = fixture_e();
    h.close(0);
    h.input(10, seg(5001).ack(1002).fin());
    h.icmp(20, code(1), 1001);
    assert_eq!(h.count(Counter::IcmpStale), 1);
}

#[test]
fn s_ic_018_a_listener_is_never_addressed() {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.tcp.listen(A, Some(port(80)), || 0).unwrap();
    h.icmp(0, code(3), 1000);
    assert_eq!(h.count(Counter::IcmpNoSocket), 1);
}

#[test]
fn s_ic_019_a_pending_child_is_deleted() {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    let listener = h.tcp.listen(A, Some(port(80)), || 0).unwrap();
    h.input(0, seg(5000).syn().mss(1460));
    nothing(&h.icmp(1, code(3), 1000));
    assert_eq!(h.tcp.next_deadline(), None);
    nothing(&h.input(2, seg(5001).ack(1001)).into_iter().filter(|o| !o.is(RST)).collect::<Vec<_>>());
    assert_eq!(h.tcp.accept(listener).unwrap(), None);
}

#[test]
fn s_ic_020_progress_clears_a_soft_error() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.icmp(1, code(1), 1001);
    h.input(10, seg(5001).ack(1101));
    assert_eq!(h.status().soft_error, None);
    h.send(10, 100);
    h.at(900_010);
    assert_eq!(h.status().failure, Some(Failure::TimedOut));
}

#[test]
fn s_ic_021_path_mtu_with_timestamps() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    h.icmp(1, too_big(1400, 1500), 1001);
    assert_eq!(h.info().smss, 1348);
}
