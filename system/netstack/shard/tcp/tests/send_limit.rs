//! What held a sender back, and for how long (RFC 4898 §2's sender limitation states): the time
//! from a transmit opportunity that left new data unsent to the next that sent some, counted by
//! whether the user, the peer's window or congestion held it.

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
