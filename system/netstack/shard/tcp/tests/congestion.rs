//! The congestion window. CC-07 to CC-11 and CC-13 to CC-15 exercise the
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

/// RFC 9406 §4.2: an ACK of two segments grows the initial slow start by both.
#[test]
fn s_cc_005_slow_start() {
    let mut h = fixture_e();
    h.send(0, 30_000);
    let outs = h.input(10, seg(5001).ack(3921));
    assert_eq!(h.info().cwnd, 17_520);
    expect(&outs, &["SEQ=15601", "SEQ=17061", "SEQ=18521", "SEQ=19981"]);
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

/// RFC 5681 §3.1 grows cwnd by each ACK, and a receive pass that brings ten ACKs before the next
/// transmit opportunity is ten ACKs: the window grows as it does when each comes alone.
#[test]
fn s_cc_016_acks_in_one_pass_each_grow_slow_start() {
    let one_at_a_time = {
        let mut h = fixture_e();
        h.send(0, 60_000);
        for i in 1..=10u32 {
            h.input(10, seg(5001).ack(1001 + i * 1460));
        }
        h.info().cwnd
    };
    let mut h = fixture_e();
    h.send(0, 60_000);
    for i in 1..=10u32 {
        h.arrive(seg(5001).ack(1001 + i * 1460));
    }
    h.transmit();
    assert_eq!((one_at_a_time, h.info().cwnd), (29_200, 29_200));
}

/// RFC 7661 §4.3, which RFC 9438 §4.6 admits: an opportunity the caller's credit ends with half
/// of cwnd in flight leaves the window in use, and the ACK after it grows cwnd; a segment under
/// half, it is application limited (§5.8), and grows nothing.
#[test]
fn s_cc_017_a_sender_the_card_holds_grows_a_window_half_in_flight() {
    for (credit, grown) in [(10, 1460), (9, 0)] {
        let mut h = fixture_e();
        assert_eq!(h.send(0, 60_000).len(), 10, "a stop at the initial window");
        h.credit = Some(credit);
        assert_eq!(h.input(10, seg(5001).ack(15_601)).len(), credit);
        let cwnd = h.info().cwnd;
        assert_eq!(cwnd, 29_200, "the ACK after cwnd's stop grows it");
        h.credit = Some(0);
        h.deliver(seg(5001).ack(17_061));
        assert_eq!(h.info().cwnd - cwnd, grown, "{credit} of the half's 10 segments in flight");
    }
}

/// RFC 9406 §4.2 with §4.3's L = infinity for a paced sender: an ACK of the whole flight grows the
/// initial slow start by the whole flight, where RFC 5681 §3.1 grows it by one segment.
#[test]
fn s_cc_018_a_stretch_ack_grows_the_initial_slow_start_by_all_it_acknowledged() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 60_000).len(), 10);
    h.input(10, seg(5001).ack(15_601));
    assert_eq!(h.info().cwnd, 29_200);
    assert_eq!(h.at(19).len(), 10, "the pacer releases the rest of the window");
    h.input(20, seg(5001).ack(44_801));
    assert_eq!(h.info().cwnd, 58_400);
}
