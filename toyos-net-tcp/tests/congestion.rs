//! The congestion window (tcp.md §18.11). CC-07 to CC-11 and CC-13 to CC-15 exercise the
//! controller directly and live beside it in `src/cc.rs`.

mod common;

use common::*;

#[test]
fn s_cc_001_ten_segments() {
    let mut h = fixture_e();
    let outs = h.send(0, 30_000);
    assert_eq!(outs.len(), 10);
    assert!(outs.iter().all(|o| o.payload.len() == 1460));
}

#[test]
fn s_cc_002_ten_segments_with_timestamps() {
    let mut h = fixture_ef();
    let outs = h.send(0, 30_000);
    assert_eq!(outs.len(), 10);
    assert!(outs.iter().all(|o| o.payload.len() == 1448));
    assert_eq!(h.info().cwnd, 14_480);
}

#[test]
fn s_cc_003_ten_segments_of_536() {
    let mut h = client(65_535, seg(5000).ack(1001).syn());
    assert_eq!(h.info().cwnd, 5360);
    let outs = h.send(0, 30_000);
    assert_eq!(outs.len(), 10);
    assert!(outs.iter().all(|o| o.payload.len() == 536));
}

#[test]
fn s_cc_004_one_segment_after_two_syn_retransmissions() {
    let mut h = H::new(65_535);
    h.start(0);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    h.transmit();
    h.at(3000);
    h.input(3010, seg(5000).ack(1001).syn().mss(1460));
    expect(&h.send(3010, 30_000), &["LEN=1460"]);
}

#[test]
fn s_cc_005_slow_start() {
    let mut h = fixture_e();
    h.send(0, 30_000);
    let outs = h.input(10, seg(5001).ack(3921));
    assert_eq!(h.info().cwnd, 16_060);
    expect(&outs, &["SEQ=15601", "SEQ=17061", "SEQ=18521"]);
}

#[test]
fn s_cc_006_no_growth_when_not_cwnd_limited() {
    let mut h = fixture_e();
    h.send(0, 1460);
    h.input(10, seg(5001).ack(2461));
    assert_eq!(h.info().cwnd, 14_600);
}

#[test]
fn s_cc_012_restart_after_idle() {
    let mut h = fixture_e();
    h.send(0, 60_000);
    let mut t = 1;
    while h.info().cwnd < 29_200 {
        let una = h.info().snd_una.get();
        h.input(t, seg(5001).ack(una + 1460));
        let room = 65_535 - h.info().queued;
        h.send(t, room);
        t += 1;
    }
    let nxt = h.info().snd_nxt.get();
    h.input(t, seg(5001).ack(nxt));
    while h.info().queued > 0 {
        t += 1;
        let nxt = h.info().snd_nxt.get();
        h.input(t, seg(5001).ack(nxt));
    }
    assert!(h.info().cwnd >= 29_200, "cwnd grew to {}", h.info().cwnd);
    let outs = h.send(t + 1000, 30_000);
    assert_eq!(outs.len(), 10);
    assert_eq!(h.info().cwnd, 14_600);
}
