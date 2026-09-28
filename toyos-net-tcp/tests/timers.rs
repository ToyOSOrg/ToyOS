//! Keepalive, give-up and the user timeout.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_tcp::{Event, Failure, Keepalive, Options, State};

fn keepalive(h: &mut H, t: i64, idle: Duration) {
    let options = Options { keepalive: Some(Keepalive { idle, ..Keepalive::default() }), ..Options::default() };
    h.call(t, |tcp, now, id| tcp.set_options(now, id, options)).0.unwrap();
}

fn timed_out(h: &mut H) {
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)));
}

#[test]
fn s_ka_001_off_by_default() {
    let mut h = fixture_e();
    nothing(&h.at(3 * 3_600_000));
}

#[test]
fn s_ka_002_a_probe_after_two_hours() {
    let mut h = fixture_e();
    keepalive(&mut h, 0, Duration::from_secs(7200));
    nothing(&h.at(7_199_999));
    expect(&h.at(7_200_000), &["SEQ=1000 ACK=5001 CTL=ACK LEN=0"]);
    nothing(&h.input(7_200_010, seg(5001).ack(1001)));
    nothing(&h.at(14_400_009));
    expect(&h.at(14_400_010), &["SEQ=1000 CTL=ACK"]);
}

#[test]
fn s_ka_003_nine_unanswered_probes() {
    let mut h = fixture_e();
    keepalive(&mut h, 0, Duration::from_secs(7200));
    let outs = h.at(7_875_000);
    let probes: Vec<i64> = outs.iter().filter(|o| o.seq == 1000).map(|o| o.t).collect();
    assert_eq!(probes, (0..9).map(|k| 7_200_000 + k * 75_000).collect::<Vec<_>>());
    let last = outs.last().unwrap();
    check(last, "SEQ=1001 ACK=5001 CTL=RST,ACK");
    assert_eq!(last.t, 7_875_000);
    timed_out(&mut h);
}

#[test]
fn s_ka_004_no_keepalive_with_data_outstanding() {
    let mut h = fixture_e();
    keepalive(&mut h, 0, Duration::from_secs(10));
    h.send(0, 100);
    let outs = h.at(100_000);
    assert!(outs.iter().all(|o| o.seq == 1001 && o.payload.len() == 100), "retransmissions only");
}

#[test]
fn s_ka_005_the_peer_keepalive_is_answered() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5000).ack(1001)), &["SEQ=1001 ACK=5001"]);
    expect(&h.input(11, seg(5000).ack(1001)), &["SEQ=1001 ACK=5001"]);
}

#[test]
fn s_ka_006_the_idle_time_is_settable() {
    let mut h = fixture_e();
    keepalive(&mut h, 0, Duration::from_secs(10));
    nothing(&h.at(9_999));
    expect(&h.at(10_000), &["SEQ=1000 CTL=ACK"]);
}

#[test]
fn s_ka_007_close_wait_and_fin_wait_2() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    keepalive(&mut h, 0, Duration::from_secs(7200));
    expect(&h.at(7_200_000), &["SEQ=1000 ACK=5002 CTL=ACK"]);
    let mut h = fixture_e();
    h.call(0, |tcp, now, id| tcp.shutdown_write(now, id)).0.unwrap();
    h.input(0, seg(5001).ack(1002));
    keepalive(&mut h, 0, Duration::from_secs(7200));
    assert_eq!(h.info().state, State::FinWait2);
    expect(&h.at(7_200_000), &["SEQ=1001 ACK=5001 CTL=ACK"]);
}

#[test]
fn s_gu_001_three_expiries_raise_advice() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.at(1399);
    assert!(!h.status().delivery_problem);
    assert!(!h.events.contains(&Event::Reverify(B)));
    h.at(1400);
    assert!(h.status().delivery_problem);
    assert_eq!(h.events.iter().filter(|e| **e == Event::Reverify(B)).count(), 1);
}

#[test]
fn s_gu_002_give_up() {
    let mut h = fixture_e();
    h.send(0, 100);
    let outs = h.at(900_000);
    let last = outs.last().unwrap();
    check(last, "SEQ=1101 ACK=5001 CTL=RST,ACK");
    assert_eq!(last.t, 900_000);
    timed_out(&mut h);
}

#[test]
fn s_gu_003_progress_restarts_the_give_up_clock() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.input(500_000, seg(5001).ack(1101));
    h.send(500_000, 100);
    h.at(1_399_999);
    assert_eq!(h.status().state, State::Established);
    let outs = h.at(1_400_000);
    check(outs.last().unwrap(), "SEQ=1201 CTL=RST,ACK");
    timed_out(&mut h);
}

fn user_timeout(h: &mut H, t: i64) {
    let options = Options { user_timeout: Some(Duration::from_secs(30)), ..Options::default() };
    h.call(t, |tcp, now, id| tcp.set_options(now, id, options)).0.unwrap();
}

#[test]
fn s_gu_004_user_timeout() {
    let mut h = fixture_e();
    user_timeout(&mut h, 0);
    h.send(0, 100);
    h.at(29_999);
    assert_eq!(h.status().state, State::Established);
    check(h.at(30_000).last().unwrap(), "SEQ=1101 CTL=RST,ACK");
    timed_out(&mut h);
}

#[test]
fn s_gu_005_user_timeout_ends_an_answered_persist() {
    let mut h = fixture_e();
    user_timeout(&mut h, 0);
    h.input(0, seg(5001).ack(1001).wnd(0));
    h.send(0, 100);
    h.at(200);
    h.input(210, seg(5001).ack(1001).wnd(0));
    let outs = h.at(30_000);
    let last = outs.last().unwrap();
    check(last, "SEQ=1001 ACK=5001 CTL=RST,ACK");
    assert_eq!(last.t, 30_000);
    timed_out(&mut h);
}

#[test]
fn s_gu_006_progress_confirms_the_next_hop() {
    let mut h = fixture_e();
    h.events.clear();
    h.call(0, |tcp, now, id| tcp.set_options(now, id, Options { nodelay: true, ..Options::default() })).0.unwrap();
    h.send(0, 1460);
    h.send(0, 1460);
    h.input(10, seg(5001).ack(2461));
    h.input(11, seg(5001).ack(2461));
    h.input(12, seg(5001).ack(3921));
    assert_eq!(h.events.iter().filter(|e| **e == Event::Reachable(B)).count(), 2);
}

/// [ip] reads the advice in order: an advance after a pending re-verify confirms the next hop again.
#[test]
fn s_gu_006_a_confirmation_after_a_reverify_is_kept() {
    let mut h = fixture_e();
    ten_out(&mut h);
    h.arrive(seg(5001).ack(2461));
    for _ in 0..3 {
        let at = h.tcp.next_deadline().expect("the retransmission timer");
        let t = h.spec_t(at);
        assert_eq!(h.instant(t), at);
        h.start(t);
        h.tcp.fire(at);
        h.tcp.transmit(at, usize::MAX, |_| {});
    }
    h.arrive(seg(5001).ack(3921));
    let events: Vec<Event> = h.tcp.drain_events().collect();
    assert_eq!(events, [Event::Reachable(B), Event::Reverify(B), Event::Reachable(B)]);
}

/// [ip] reads each address's own advice: B's repeated confirmation never crowds out C's, and a
/// stale confirmation for B already pending is not repeated once C's is newer.
#[test]
fn s_gu_006_b_advice_is_kept_per_address() {
    const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
    let mut h = fixture_e();
    h.send(0, 300);

    let now = h.now();
    let conn_c = h.tcp.connect(now, A, Some(port(49153)), ep(C, 80)).unwrap();
    h.transmit();
    h.input(0, seg(5000).ack(1001).syn().wnd(65_535).mss(1460).from(C, 80).to(A, 49153));
    h.tcp.send(h.now(), conn_c, &[0u8; 300]).unwrap();
    h.transmit();

    h.arrive(seg(5001).ack(1101));
    h.arrive(seg(5001).ack(1101).from(C, 80).to(A, 49153));
    h.arrive(seg(5001).ack(1201));
    let events: Vec<Event> = h.tcp.drain_events().collect();
    assert_eq!(events, [Event::Reachable(B), Event::Reachable(C)]);
}
