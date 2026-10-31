//! Pacing (RFC 9002 §7.7) as a connection meets it. The bucket's arithmetic lives beside it in
//! `src/pace.rs`.

mod common;

use common::*;
use toyos_net_tcp::Counter;

/// Fixture E's window after the ACK of its initial ten segments at 10 ms: cwnd 29,200 in slow
/// start, paced at 2 · 29,200 / 10 ms. The bucket held the initial window, so ten leave with the
/// ACK and the rest once half the bucket has come back, 7,300 bytes in 1.25 ms each time.
fn released() -> H {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 60_000).len(), 10);
    assert_eq!(h.input(10, seg(5001).ack(15_601)).len(), 10, "the bucket holds the initial window");
    assert_eq!(h.info().cwnd, 29_200);
    h
}

#[test]
fn a_window_past_the_bucket_leaves_at_the_pacing_rate() {
    let mut h = released();
    assert_eq!(h.at(11).len(), 0, "nothing before 11.25 ms");
    assert_eq!(h.at(12).len(), 5);
    assert_eq!(h.at(13).len(), 5, "and cwnd is full");
    assert_eq!(h.at(20).len(), 0);
    assert_eq!(h.count(Counter::SendLimitedByPacingNanos), 2_500_000);
}

/// A pacing hold leaves the window in use as the send before it found it: the ACK that arrives
/// during it grows cwnd.
#[test]
fn an_ack_during_a_pacing_hold_grows_the_window() {
    let mut h = released();
    h.input_past(10, 500, seg(5001).ack(17_061));
    assert_eq!(h.info().cwnd, 30_660);
}

/// RFC 9002 §7.7: a segment carrying only an acknowledgment is not paced.
#[test]
fn an_acknowledgment_leaves_through_a_pacing_hold() {
    let mut h = released();
    h.arrive(seg(5001).ack(15_601).len(1460));
    expect(&h.deliver(seg(6461).ack(15_601).len(1460)), &["ACK=7921 LEN=0"]);
}
