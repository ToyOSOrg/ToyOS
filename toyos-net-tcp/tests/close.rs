//! Closing (tcp.md §18.14). From E unless stated.

mod common;

use common::*;
use toyos_net_tcp::{limits, reuse_iss, Counter, Error, Failure, Received, State};

fn shutdown(h: &mut H, t: i64) -> Vec<O> {
    let (r, outs) = h.call(t, |tcp, now, id| tcp.shutdown_write(now, id));
    r.unwrap();
    outs
}

fn abort(h: &mut H, t: i64) -> Vec<O> {
    let (r, outs) = h.call(t, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    outs
}

fn recv(h: &mut H) -> Result<Received, Error> {
    h.call(h.t, |tcp, now, id| tcp.recv(now, id, &mut [0u8; 1000])).0
}

/// Whether A still holds a connection for the fixture's 4-tuple.
fn held(h: &mut H) -> bool {
    let id = h.id();
    h.tcp.info(id).is_some()
}

/// CL-01 to TIME-WAIT: A closed at 0, B acknowledged at 10 and sent its FIN at 20.
fn active_close() -> H {
    let mut h = fixture_e();
    expect(&h.close(0), &["SEQ=1001 ACK=5001 CTL=FIN,ACK"]);
    assert_eq!(h.info().state, State::FinWait1);
    nothing(&h.input(10, seg(5001).ack(1002)));
    assert_eq!(h.info().state, State::FinWait2);
    expect(&h.input(20, seg(5001).ack(1002).fin()), &["SEQ=1002 ACK=5002 CTL=ACK"]);
    assert!(!held(&mut h));
    assert_eq!(h.tcp.time_wait_count(), 1);
    h
}

/// CL-02 to LAST-ACK.
fn passive_close() -> H {
    let mut h = fixture_e();
    expect(&h.input(0, seg(5001).ack(1001).fin()), &["SEQ=1001 ACK=5002 CTL=ACK"]);
    assert_eq!(h.info().state, State::CloseWait);
    assert_eq!(recv(&mut h), Ok(Received::End));
    expect(&h.close(10), &["SEQ=1001 ACK=5002 CTL=FIN,ACK"]);
    assert_eq!(h.info().state, State::LastAck);
    h
}

/// CL-03 to CLOSING.
fn simultaneous_close() -> H {
    let mut h = fixture_e();
    h.close(0);
    expect(&h.input(5, seg(5001).ack(1001).fin()), &["SEQ=1002 ACK=5002"]);
    assert_eq!(h.info().state, State::Closing);
    h
}

#[test]
fn s_cl_001_active_close() {
    let mut h = active_close();
    nothing(&h.at(60_019));
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_020);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_002_passive_close() {
    let mut h = passive_close();
    nothing(&h.input(20, seg(5002).ack(1002)));
    assert!(!held(&mut h));
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_003_simultaneous_close() {
    let mut h = simultaneous_close();
    nothing(&h.input(10, seg(5002).ack(1002)));
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_009);
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_010);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_004_fin_acknowledging_ours() {
    let mut h = fixture_e();
    h.close(0);
    expect(&h.input(10, seg(5001).ack(1002).fin()), &["SEQ=1002 ACK=5002"]);
    assert_eq!(h.tcp.time_wait_count(), 1);
}

#[test]
fn s_cl_005_the_fin_rides_on_data() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| {
        tcp.send(now, id, &[7; 100]).unwrap();
        tcp.close(now, id)
    });
    expect(&outs, &["SEQ=1001 CTL=FIN,PSH,ACK LEN=100"]);
}

#[test]
fn s_cl_006_the_fin_is_retransmitted() {
    let mut h = fixture_e();
    h.close(0);
    nothing(&h.at(199));
    expect(&h.at(200), &["SEQ=1001 CTL=FIN,ACK LEN=0"]);
}

#[test]
fn s_cl_007_half_close() {
    let mut h = fixture_e();
    expect(&shutdown(&mut h, 0), &["CTL=FIN,ACK"]);
    h.input(10, seg(5001).ack(1002));
    h.input(20, seg(5001).ack(1002).len(500));
    expect(&h.input(30, seg(5501).ack(1002).fin()), &["ACK=5502"]);
    assert_eq!(h.status().state, State::TimeWait);
    assert_eq!(recv(&mut h), Ok(Received::Data(500)));
    assert_eq!(recv(&mut h), Ok(Received::End));
}

#[test]
fn s_cl_008_sending_in_close_wait() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    expect(&h.send(1, 100), &["SEQ=1001 ACK=5002 LEN=100"]);
    expect(&h.close(2), &["SEQ=1101 CTL=FIN,ACK"]);
    assert_eq!(h.info().state, State::LastAck);
}

#[test]
fn s_cl_009_a_retransmitted_fin_in_close_wait() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    expect(&h.input(1, seg(5001).ack(1001).fin()), &["SEQ=1001 ACK=5002"]);
    expect(&h.input(2, seg(5001).ack(1001).fin()), &["SEQ=1001 ACK=5002"]);
}

#[test]
fn s_cl_010_a_retransmitted_fin_restarts_time_wait() {
    let mut h = active_close();
    expect(&h.input(1000, seg(5001).ack(1002).fin()), &["SEQ=1002 ACK=5002"]);
    h.at(60_999);
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(61_000);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_011_an_orphan_in_fin_wait_2() {
    let mut h = fixture_e();
    shutdown(&mut h, 0);
    h.input(10, seg(5001).ack(1002));
    nothing(&h.close(100_000));
    nothing(&h.at(159_999));
    expect(&h.at(160_000), &["SEQ=1002 ACK=5001 CTL=RST,ACK"]);
    assert!(!held(&mut h));
}

#[test]
fn s_cl_012_the_orphan_bound() {
    let mut h = fixture_e();
    h.close(0);
    h.input(10, seg(5001).ack(1002));
    nothing(&h.at(60_009));
    expect(&h.at(60_010), &["SEQ=1002 ACK=5001 CTL=RST,ACK"]);
    assert!(!held(&mut h));
    assert_eq!(h.count(Counter::OrphanIdleAbort), 1);
}

/// CL-13's opening: data behind a shut window, the probes answered.
fn orphan_behind_a_shut_window(close: bool) -> H {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(0));
    h.send(0, 1000);
    if close {
        h.close(0);
    } else {
        shutdown(&mut h, 0);
    }
    h
}

fn answer_probes(h: &mut H, until: i64) -> Vec<O> {
    let mut all = Vec::new();
    while let Some(deadline) = h.tcp.next_deadline().filter(|&d| h.spec_t(d) <= until) {
        let outs = h.at(h.spec_t(deadline));
        let probe = outs.iter().any(|o| o.payload.len() == 1);
        all.extend(outs);
        if probe {
            all.extend(h.input(h.t + 10, seg(5001).ack(1001).wnd(0)));
        }
    }
    all.extend(h.at(until));
    all
}

#[test]
fn s_cl_013_an_orphan_behind_a_shut_window() {
    let mut h = orphan_behind_a_shut_window(true);
    let outs = answer_probes(&mut h, 60_000);
    let probes: Vec<i64> = outs.iter().filter(|o| o.payload.len() == 1).map(|o| o.t).collect();
    assert_eq!(&probes[..3], [200, 600, 1400]);
    let last = outs.last().unwrap();
    check(last, "SEQ=1001 ACK=5001 CTL=RST,ACK");
    assert_eq!(last.t, 60_000);
    assert!(!held(&mut h));
}

#[test]
fn s_cl_014_a_user_keeps_a_shut_window_open() {
    let mut h = orphan_behind_a_shut_window(false);
    let outs = answer_probes(&mut h, 7_200_000);
    assert!(outs.iter().all(|o| o.flags & RST == 0));
    assert_eq!(h.info().state, State::FinWait1);
    assert!(outs.iter().filter(|o| o.payload.len() == 1).count() > 100);
}

#[test]
fn s_cl_015_data_for_an_orphan() {
    let mut h = fixture_e();
    h.close(0);
    h.input(10, seg(5001).ack(1002));
    expect(&h.input(20, seg(5001).ack(1002).len(10)), &["SEQ=1002 ACK=5001 CTL=RST,ACK"]);
    assert!(!held(&mut h));
    assert_eq!(h.count(Counter::OrphanDataRst), 1);
}

#[test]
fn s_cl_016_close_with_unread_data() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    expect(&h.close(1), &["SEQ=1001 ACK=5101 CTL=RST,ACK"]);
    assert!(!held(&mut h));
    assert_eq!(h.count(Counter::CloseUnreadRst), 1);
}

#[test]
fn s_cl_017_time_wait_is_60_s() {
    let mut h = active_close();
    h.at(60_019);
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_020);
    assert_eq!(h.tcp.time_wait_count(), 0);
    assert!(limits::all().contains(&("time_wait_ms", 60_000)));
}

#[test]
fn s_cl_018_reopening_from_time_wait_by_timestamp() {
    let mut h = fixture_esf();
    expect(&h.close(0), &["CTL=FIN,ACK TS=1000/50000"]);
    h.input(10, seg(5001).ack(1002).wnd(512).ts(50_010, 1000));
    h.input(20, seg(5001).ack(1002).wnd(512).fin().ts(50_020, 1000));
    assert_eq!(h.tcp.time_wait_count(), 1);
    let fresh = toyos_net_tcp::isn(&secrets().isn, &h.tuple(), h.instant(1000));
    let expected = reuse_iss(toyos_net_tcp::Seq::new(h.real(1002)), fresh);
    let outs = h.input(1000, seg(9000).syn().mss(1460).sackok().ts(51_000, 0).ws(7));
    let want = format!("SEQ={} ACK=9001 CTL=SYN,ACK WND=65535 MSS=1460 SACKOK TS=2000/51000 WS=0", h.spec(expected.get()));
    expect(&outs, &[&want]);
    assert_eq!(h.count(Counter::TimeWaitReuse), 1);
    assert_eq!(reuse_iss(toyos_net_tcp::Seq::new(1002), toyos_net_tcp::Seq::new(1000)).get(), 67_538);
}

/// CL-19's opening: ES, A closed into TIME-WAIT with RCV.NXT 5002, no timestamps.
fn server_time_wait() -> H {
    let mut h = fixture_es();
    h.close(0);
    h.input(10, seg(5001).ack(1002).fin());
    assert_eq!(h.tcp.time_wait_count(), 1);
    h
}

#[test]
fn s_cl_019_reopening_by_sequence_number() {
    let mut h = server_time_wait();
    nothing(&h.input(100, seg(4000).syn()));
    assert_eq!(h.tcp.time_wait_count(), 1);
    expect(&h.input(200, seg(6000).syn()), &["CTL=SYN,ACK ACK=6001 WND=65535 NOSACKOK TS=-"]);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_020_rst_in_time_wait_is_ignored() {
    let mut h = active_close();
    nothing(&h.input(30, seg(5002).rst()));
    assert_eq!(h.count(Counter::TimeWaitRstIgnored), 1);
    assert_eq!(h.refusals(Counter::TimeWaitRstIgnored).len(), 1);
    h.at(60_019);
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_020);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_021_a_failed_reopening_returns_to_time_wait() {
    let mut h = server_time_wait();
    h.input(200, seg(6000).syn());
    nothing(&h.input(300, seg(6001).rst()));
    assert_eq!(h.tcp.time_wait_count(), 1);
    nothing(&h.input(400, seg(5000).syn()));
    h.at(60_009);
    assert_eq!(h.tcp.time_wait_count(), 1);
    h.at(60_010);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_022_syn_in_time_wait_without_a_listener() {
    let mut h = active_close();
    expect(&h.input(30, seg(6000).syn()), &["SEQ=1002 ACK=5002 CTL=ACK"]);
}

#[test]
fn s_cl_023_time_wait_is_bounded() {
    let mut h = H::new(65_535);
    h.start(0);
    for i in 0..=limits::TIME_WAIT_MAX as u16 {
        let local = 1024 + i;
        h.local = (A, local);
        let id = h.tcp.connect(h.now(), A, Some(port(local)), ep(B, 80)).unwrap();
        h.conn = Some(id);
        h.transmit();
        h.deliver(seg(5000).ack(1001).syn().mss(1460));
        h.close(0);
        h.deliver(seg(5001).ack(1002).fin());
    }
    assert_eq!(h.tcp.time_wait_count(), limits::TIME_WAIT_MAX);
    assert_eq!(h.count(Counter::TimeWaitEvicted), 1);
    h.local = (A, 1024);
    expect(&h.input(1, seg(5001).ack(1002).fin()), &["CTL=RST"]);
}

#[test]
fn s_cl_024_abort() {
    let mut h = fixture_e();
    h.send(0, 100);
    expect(&abort(&mut h, 1), &["SEQ=1101 ACK=5001 CTL=RST,ACK"]);
    assert!(!held(&mut h));
    let (r, _) = h.call(2, |tcp, now, id| tcp.send(now, id, b"x"));
    assert_eq!(r, Err(Error::NoSuchSocket));
}

#[test]
fn s_cl_025_abort_without_rst() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    shutdown(&mut h, 1);
    assert_eq!(h.info().state, State::LastAck);
    nothing(&abort(&mut h, 2));
    assert!(!held(&mut h));
    let mut h = fixture_e();
    shutdown(&mut h, 0);
    h.input(5, seg(5001).ack(1001).fin());
    assert_eq!(h.info().state, State::Closing);
    nothing(&abort(&mut h, 6));
    let mut h = fixture_e();
    shutdown(&mut h, 0);
    h.input(5, seg(5001).ack(1002).fin());
    assert_eq!(h.status().state, State::TimeWait);
    nothing(&abort(&mut h, 6));
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_cl_026_no_time_wait_after_a_reset() {
    let mut h = fixture_e();
    h.send(0, 100);
    abort(&mut h, 1);
    h.forget();
    expect(&h.input(2, seg(9000).syn()), &["SEQ=0 ACK=9001 CTL=RST,ACK"]);
    let now = h.now();
    assert!(h.tcp.connect(now, A, Some(port(49152)), ep(B, 80)).is_ok());
}

#[test]
fn s_cl_027_the_reset_stub() {
    let mut h = fixture_e();
    h.credit = Some(0);
    abort(&mut h, 0);
    nothing(&h.input(5, seg(5001).ack(1001).len(10)));
    h.credit = None;
    expect(&h.at(10), &["CTL=RST,ACK"]);
    expect(&h.input(11, seg(5001).ack(1001).len(10)), &["SEQ=1001 ACK=- CTL=RST"]);
}

#[test]
fn s_cl_028_close_in_syn_sent() {
    let mut h = H::new(65_535);
    h.start(0);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    expect(&h.transmit(), &["CTL=SYN"]);
    nothing(&h.close(5));
    expect(&h.input(10, seg(5000).ack(1001).syn().mss(1460)), &["SEQ=1001 ACK=- CTL=RST"]);
}

#[test]
fn s_cl_029_shutdown_read() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    h.call(1, |tcp, now, id| tcp.shutdown_read(now, id)).0.unwrap();
    assert_eq!(h.info().unread, 0);
    let outs = [h.input(2, seg(5101).ack(1001).len(100)), h.at(50)].concat();
    expect(&outs, &["ACK=5201 WND=65335"]);
    assert_eq!(h.info().unread, 0);
}

#[test]
fn s_cl_030_last_ack_waits_for_the_fin() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    h.send(1, 100);
    h.close(10);
    nothing(&h.input(20, seg(5002).ack(1101)));
    assert_eq!(h.info().state, State::LastAck);
    nothing(&h.input(30, seg(5002).ack(1102)));
    assert!(!held(&mut h));
}

#[test]
fn s_cl_031_rst_in_close_wait() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).fin());
    nothing(&h.input(1, seg(5002).rst()));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::Reset)));
}

#[test]
fn s_cl_032_rst_in_last_ack_and_closing() {
    let mut h = passive_close();
    nothing(&h.input(20, seg(5002).rst()));
    assert!(!held(&mut h));
    let mut h = simultaneous_close();
    nothing(&h.input(10, seg(5002).rst()));
    assert!(!held(&mut h));
}
