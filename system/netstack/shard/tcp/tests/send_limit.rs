//! What held a sender back, and for how long (RFC 4898 §4's `tcpEStatsPerfSndLimTime*`): the time
//! from a transmit opportunity that left new data unsent to the next that sent some, counted by
//! whether the sender itself, the peer's window or congestion held it.

mod common;

use common::*;
use toyos_net_tcp::Counter;

const MS: u64 = 1_000_000;

fn held(h: &H) -> [u64; 3] {
    [
        h.count(Counter::SendLimitedByUserNanos),
        h.count(Counter::SendLimitedByWindowNanos),
        h.count(Counter::SendLimitedByCongestionNanos),
    ]
}

#[test]
fn a_sender_at_its_congestion_window_counts_the_wait_until_the_ack_that_opens_it() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 30_000).len(), 10, "the initial window");
    assert_eq!(held(&h), [0; 3], "nothing counts until the wait ends");
    assert!(!h.input(50, seg(5001).ack(15_601)).is_empty(), "the ACK opens the window");
    assert_eq!(held(&h), [0, 0, 50 * MS]);
    assert_eq!(h.count(Counter::CongestionStops), 1, "the rest of the 30 000 bytes fit the grown window");
    assert_eq!(h.count(Counter::CongestionStopsCwndBytes), 14_600);
}

#[test]
fn a_sender_at_the_peers_window_counts_the_wait_as_the_windows() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(2920));
    assert_eq!(h.send(0, 30_000).len(), 2, "the peer's window, under the congestion window");
    assert!(!h.input(30, seg(5001).ack(3921).wnd(2920)).is_empty());
    assert_eq!(held(&h), [0, 30 * MS, 0]);
}

#[test]
fn a_sender_with_nothing_unsent_counts_the_wait_as_the_users() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 1460).len(), 1);
    h.input(20, seg(5001).ack(2461));
    assert_eq!(held(&h), [0; 3], "an ACK that sends nothing ends no wait");
    assert_eq!(h.send(50, 1460).len(), 1);
    assert_eq!(held(&h), [50 * MS, 0, 0]);
}

#[test]
fn a_wait_that_changes_cause_counts_each_cause_its_own_span() {
    let mut h = fixture_e();
    h.send(0, 30_000);
    // Everything acknowledged, but the window shut to less than a segment: congestion held the
    // sender for 40 ms, and the peer's window from then on.
    assert!(h.input(40, seg(5001).ack(15_601).wnd(100)).iter().all(|o| o.len() <= 100));
    assert_eq!(held(&h)[2], 40 * MS);
    h.input(70, seg(5001).ack(15_601).wnd(65_535));
    assert_eq!(held(&h), [0, 30 * MS, 40 * MS]);
}

#[test]
fn a_retransmission_is_not_counted_as_sent_again() {
    let mut h = fixture_e();
    h.send(0, 30_000);
    assert_eq!(h.count(Counter::SentBytes), 14_600);
    h.at(1000);
    assert!(h.count(Counter::RetransmitBytes) > 0, "the timeout resent");
    assert_eq!(h.count(Counter::SentBytes), 14_600);
}

#[test]
fn a_nagle_hold_is_the_senders_own() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 2000).len(), 1, "Nagle holds the 540-byte tail behind the segment in flight");
    assert!(!h.input(30, seg(5001).ack(2461)).is_empty(), "the ACK releases the tail");
    assert_eq!(held(&h), [30 * MS, 0, 0]);
}

#[test]
fn a_wait_that_ends_in_a_timeout_is_congestions() {
    let mut h = fixture_e();
    h.send(0, 1460);
    // Nothing unsent: the sender's own wait, until the retransmission timer fires and makes the
    // span congestion's; the wait after it is the sender's again.
    let mut t = 0;
    while h.count(Counter::Rto) == 0 {
        t += 1;
        h.at(t);
    }
    let fired = u64::try_from(t).unwrap();
    assert_eq!(held(&h), [0, 0, fired * MS]);
    h.input(t + 1, seg(5001).ack(2461));
    assert_eq!(h.send(t + 11, 1460).len(), 1);
    assert_eq!(held(&h), [11 * MS, 0, fired * MS]);
}

/// Each ACK outside recovery is counted by whether cwnd ended the opportunity before it, with
/// what it grew cwnd by.
#[test]
fn each_ack_is_counted_by_its_limit_with_what_it_grew() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 16_060).len(), 10, "cwnd stops the sender with a segment waiting");
    assert_eq!(h.input(10, seg(5001).ack(3921)).len(), 1, "the user's data ends the next");
    h.input(20, seg(5001).ack(5381));
    assert_eq!(h.count(Counter::AcksCwndLimited), 1);
    assert_eq!(h.count(Counter::AcksNotLimited), 1);
    assert_eq!(h.count(Counter::CwndGrownBytes), 2920);
}
