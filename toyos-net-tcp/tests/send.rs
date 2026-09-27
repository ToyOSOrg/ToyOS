//! The send side. From E unless stated.

mod common;

use common::*;
use toyos_net_tcp::{Error, Failure, Options, State};

fn nodelay(h: &mut H) {
    let (r, _) = h.call(h.t, |tcp, now, id| tcp.set_options(now, id, Options { nodelay: true, ..Options::default() }));
    r.unwrap();
}

#[test]
fn s_tx_001_one_segment_with_push() {
    let mut h = fixture_e();
    expect(&h.send(0, 100), &["SEQ=1001 ACK=5001 CTL=PSH,ACK LEN=100"]);
}

#[test]
fn s_tx_002_nagle() {
    let mut h = fixture_e();
    h.send(0, 100);
    nothing(&h.send(1, 50));
    expect(&h.input(10, seg(5001).ack(1101)), &["SEQ=1101 CTL=PSH,ACK LEN=50"]);
}

#[test]
fn s_tx_003_nodelay() {
    let mut h = fixture_e();
    nodelay(&mut h);
    expect(&h.send(0, 100), &["LEN=100"]);
    expect(&h.send(0, 50), &["SEQ=1101 LEN=50"]);
}

#[test]
fn s_tx_004_full_segments_then_wait() {
    let mut h = fixture_e();
    h.send(0, 100);
    expect(&h.send(1, 3000), &["SEQ=1101 LEN=1460", "SEQ=2561 LEN=1460"]);
    assert_eq!(h.info().queued, 3100);
}

#[test]
fn s_tx_005_push_only_where_the_queue_empties() {
    let mut h = fixture_e();
    nodelay(&mut h);
    expect(&h.send(0, 3000), &["LEN=1460 CTL=ACK", "LEN=1460 CTL=ACK", "LEN=80 CTL=PSH,ACK"]);
}

#[test]
fn s_tx_006_silly_window_override() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(1000));
    nothing(&h.send(0, 3000));
    nothing(&h.at(199));
    expect(&h.at(200), &["SEQ=1001 LEN=1000"]);
}

#[test]
fn s_tx_007_an_opened_window_cancels_the_override() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(1000));
    nothing(&h.send(0, 3000));
    expect(&h.input(50, seg(5001).ack(1001).wnd(65_535)), &["LEN=1460", "LEN=1460"]);
    nothing(&h.at(249));
}

#[test]
fn s_tx_008_half_the_largest_window() {
    // The specification's numbers: 2000 bytes of window go, but no segment exceeds the MSS.
    let mut h = client(65_535, seg(5000).ack(1001).syn().wnd(4000).mss(1460));
    h.input(1, seg(5001).ack(1001).wnd(2000));
    expect(&h.send(2, 5000), &["LEN=1460"]);
    nothing(&h.at(201));
    // Rule 3 alone: a window below the MSS but at least half the largest ever offered.
    let mut h = client(65_535, seg(5000).ack(1001).syn().wnd(2000).mss(1460));
    h.input(1, seg(5001).ack(1001).wnd(1000));
    expect(&h.send(2, 5000), &["LEN=1000"]);
}

#[test]
fn s_tx_009_a_shrunk_window() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 7300).len(), 5);
    nothing(&h.input(1, seg(5001).ack(1001).wnd(2000)));
    assert_eq!(h.info().state, State::Established);
    expect(&h.at(200), &["SEQ=1001 LEN=1460"]);
}

#[test]
fn s_tx_010_the_initial_window_bounds_a_burst() {
    let mut h = fixture_e();
    let outs = h.send(0, 30_000);
    assert_eq!(outs.len(), 10);
    assert_eq!(outs.iter().map(|o| o.payload.len()).sum::<usize>(), 14_600);
}

#[test]
fn s_tx_011_the_send_buffer_is_bounded() {
    let mut h = fixture_e();
    let data = vec![1u8; 70_000];
    let (r, _) = h.call(0, |tcp, now, id| tcp.send(now, id, &data));
    assert_eq!(r, Ok(65_535));
    let (r, _) = h.call(0, |tcp, now, id| tcp.send(now, id, b"x"));
    assert_eq!(r, Err(Error::WouldBlock));
}

#[test]
fn s_tx_012_no_send_after_shutdown() {
    let mut h = fixture_e();
    h.call(0, |tcp, now, id| tcp.shutdown_write(now, id)).0.unwrap();
    let (r, _) = h.call(1, |tcp, now, id| tcp.send(now, id, b"x"));
    assert_eq!(r, Err(Error::Closing));
}

/// TX-13's opening: a shut window, 100 bytes queued, the first probe at 200.
fn persist() -> H {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(0));
    nothing(&h.send(0, 100));
    nothing(&h.at(199));
    expect(&h.at(200), &["SEQ=1001 LEN=1"]);
    assert_eq!(h.info().snd_nxt.get(), 1002);
    h
}

#[test]
fn s_tx_013_persist_backs_off() {
    let mut h = persist();
    nothing(&h.input(210, seg(5001).ack(1001).wnd(0)));
    let outs = h.at(200_000);
    let times: Vec<i64> = outs.iter().map(|o| o.t).collect();
    assert_eq!(times, [600, 1400, 3000, 6200, 12_600, 25_400, 51_000, 102_200, 162_200]);
    for o in &outs {
        check(o, "SEQ=1001 LEN=1");
    }
}

#[test]
fn s_tx_014_an_opened_window_ends_persist() {
    let mut h = persist();
    h.input(210, seg(5001).ack(1001).wnd(0));
    expect(&h.input(250, seg(5001).ack(1001).wnd(65_535)), &["SEQ=1001 CTL=PSH,ACK LEN=100"]);
    assert_eq!(h.info().snd_nxt.get(), 1101);
    nothing(&h.at(449));
}

#[test]
fn s_tx_015_the_probe_byte_taken() {
    let mut h = persist();
    h.input(210, seg(5001).ack(1002).wnd(0));
    assert_eq!(h.info().snd_una.get(), 1002);
    expect(&h.at(600), &["SEQ=1002 LEN=1"]);
}

#[test]
fn s_tx_016_an_answered_persist_lives() {
    let mut h = persist();
    let mut probes = 1;
    let mut t = 200;
    while t < 2 * 3_600_000 {
        h.input(t + 10, seg(5001).ack(1001).wnd(0));
        let deadline = h.tcp.next_deadline().unwrap();
        let outs = h.at(h.spec_t(deadline));
        expect(&outs, &["SEQ=1001 LEN=1"]);
        t = outs[0].t;
        probes += 1;
    }
    assert!(probes > 100);
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_tx_017_an_unanswered_persist_gives_up() {
    let mut h = persist();
    let outs = h.at(900_000);
    expect(&outs[outs.len() - 1..], &["SEQ=1001 ACK=5001 CTL=RST,ACK"]);
    assert_eq!(outs.last().unwrap().t, 900_000);
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)));
}

#[test]
fn s_tx_018_the_fin_as_probe() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(0));
    nothing(&h.close(0));
    nothing(&h.at(199));
    expect(&h.at(200), &["SEQ=1001 CTL=FIN,ACK LEN=0"]);
}

#[test]
fn s_tx_019_a_shut_window_stops_the_retransmission_timer() {
    let mut h = fixture_e();
    nodelay(&mut h);
    h.send(0, 2920);
    nothing(&h.input(10, seg(5001).ack(1001).wnd(0)));
    nothing(&h.at(209));
    expect(&h.at(210), &["SEQ=1001 LEN=1"]);
}

#[test]
fn s_tx_020_cwnd_does_not_arm_the_override() {
    let mut h = fixture_e();
    nodelay(&mut h);
    assert_eq!(h.send(0, 14_100).len(), 10);
    nothing(&h.send(1, 1000));
    assert_eq!(h.tcp.next_deadline(), Some(h.instant(200)), "only the retransmission timer");
    nothing(&h.at(199));
    let outs = h.input(199, seg(5001).ack(2461));
    assert!(outs.iter().any(|o| o.seq == 15_101), "the data waited for an ACK");
}

#[test]
fn s_tx_021_close_pushes() {
    let mut h = fixture_e();
    h.send(0, 100);
    nothing(&h.send(0, 50));
    expect(&h.close(0), &["SEQ=1101 CTL=FIN,PSH,ACK LEN=50"]);
}
