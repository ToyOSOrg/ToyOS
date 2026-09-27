//! Pull egress (tcp.md §18.13): segments are built at a transmit opportunity, and timers start at
//! hand-off. PL-11, the `many_up` shape, runs on the test network in `tests/net.rs`.

mod common;

use common::*;
use toyos_net_tcp::{Counter, Failure, State};

#[test]
fn s_pl_001_the_timer_starts_at_hand_off() {
    let mut h = fixture_e();
    h.credit = Some(0);
    nothing(&h.send(0, 1000));
    nothing(&h.at(499));
    h.credit = None;
    expect(&h.at(500), &["SEQ=1001 LEN=1000"]);
    nothing(&h.at(699));
    expect(&h.at(700), &["SEQ=1001 LEN=1000"]);
}

#[test]
fn s_pl_002_no_expiry_for_a_retransmission_that_never_left() {
    let mut h = fixture_e();
    expect(&h.send(0, 1000), &["LEN=1000"]);
    h.credit = Some(0);
    nothing(&h.at(4999));
    assert_eq!(h.count(Counter::Rto), 1);
    h.credit = None;
    expect(&h.at(5000), &["SEQ=1001 LEN=1000"]);
    nothing(&h.at(5399));
    expect(&h.at(5400), &["SEQ=1001 LEN=1000"]);
    assert_eq!(h.count(Counter::Rto), 2);
    assert_eq!(h.count(Counter::RtoUnsent), 0);
}

#[test]
fn s_pl_003_unsent_is_not_in_flight() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 30_000);
    let info = h.info();
    assert_eq!(info.snd_nxt, info.snd_una);
    h.credit = Some(4);
    assert_eq!(h.at(1).len(), 4);
    let info = h.info();
    assert_eq!(info.snd_nxt.get() - info.snd_una.get(), 5840);
}

#[test]
fn s_pl_004_owed_acks_collapse() {
    let mut h = fixture_e();
    h.credit = Some(0);
    for k in 0..5u32 {
        h.input(1, seg(5001 + 100 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(2), &["ACK=5501"]);
}

#[test]
fn s_pl_005_at_most_three_duplicate_acks_are_held() {
    let mut h = fixture_e();
    h.credit = Some(0);
    for k in 0..3u32 {
        h.input(1, seg(6001 + 200 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(2), &["ACK=5001", "ACK=5001", "ACK=5001"]);
    h.credit = Some(0);
    for k in 3..8u32 {
        h.input(3, seg(6001 + 200 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(4), &["ACK=5001", "ACK=5001", "ACK=5001"]);
}

#[test]
fn s_pl_006_a_segment_carries_the_state_of_its_moment() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 100);
    h.input(5, seg(5001).ack(1001).len(50));
    h.credit = None;
    expect(&h.at(10), &["SEQ=1001 ACK=5051 LEN=100"]);
    let mut h = fixture_ef();
    h.credit = Some(0);
    h.send(0, 100);
    h.input_full(5, seg(5001).ack(1001).len(50));
    h.credit = None;
    expect(&h.at(10), &["SEQ=1001 ACK=5051 LEN=100 TS=1010/*"]);
}

#[test]
fn s_pl_007_credit_bounds_an_opportunity() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 5 * 1460);
    h.credit = Some(2);
    expect(&h.at(1), &["SEQ=1001", "SEQ=2461"]);
    h.credit = None;
    expect(&h.at(2), &["SEQ=3921", "SEQ=5381", "SEQ=6841"]);
}

#[test]
fn s_pl_008_resets_go_first() {
    let mut h = fixture_e();
    let other = h.tcp.connect(h.now(), A, Some(port(49153)), ep(B, 81)).unwrap();
    h.transmit();
    h.input(1, seg(7000).ack(1001).syn().mss(1460).from(B, 81).to(A, 49153));
    h.credit = Some(0);
    let now = h.now();
    h.tcp.send(now, other, &[1; 100]).unwrap();
    h.call(2, |tcp, now, id| tcp.abort(now, id)).0.unwrap();
    h.credit = Some(1);
    let outs = h.at(3);
    expect(&outs, &["CTL=RST,ACK"]);
    assert_eq!(outs[0].dst, (B, 80));
    h.credit = None;
    let outs = h.at(4);
    assert_eq!(outs.len(), 1);
    assert_eq!((outs[0].dst, outs[0].payload.len()), ((B, 81), 100));
}

#[test]
fn s_pl_009_give_up_without_credit() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.credit = Some(0);
    nothing(&h.at(900_000));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)));
    h.credit = None;
    expect(&h.at(900_001), &["SEQ=1101 ACK=5001 CTL=RST,ACK"]);
}

#[test]
fn s_pl_010_give_up_before_retransmission() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.at(162_199);
    h.credit = Some(0);
    nothing(&h.at(179_999));
    h.credit = None;
    expect(&h.at(180_000), &["SEQ=1001 LEN=100"]);
    let times: Vec<i64> = h.at(899_999).iter().map(|o| o.t).collect();
    assert_eq!(times.first(), Some(&240_000));
    assert_eq!(times.last(), Some(&840_000), "an expiry every 60 s from 180 s");
    let rtos = h.count(Counter::Rto);
    expect(&h.at(900_000), &["CTL=RST,ACK"]);
    assert_eq!(h.count(Counter::Rto), rtos, "the give-up came first; no retransmission was made due");
}
