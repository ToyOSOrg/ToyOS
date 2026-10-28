//! Round-trip time and the retransmission timeout. RT-01 to RT-04 and RT-12 feed
//! a fresh estimator and live beside it in `src/rtt.rs`.

mod common;

use common::*;
use toyos_net_tcp::Counter;

#[test]
fn s_rt_005_the_minimum_is_200_ms() {
    let mut h = fixture_e();
    h.send(0, 100);
    nothing(&h.at(199));
    expect(&h.at(200), &["SEQ=1001 LEN=100"]);
}

#[test]
fn s_rt_006_three_seconds_after_a_lost_syn() {
    let mut h = H::new(65_535);
    h.start(0);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    h.transmit();
    h.at(1000);
    h.input(1010, seg(5000).ack(1001).syn().mss(1460));
    h.send(1010, 100);
    nothing(&h.at(4009));
    expect(&h.at(4010), &["SEQ=1001 LEN=100"]);
}

#[test]
fn s_rt_007_karn() {
    let mut h = fixture_e();
    h.send(0, 100);
    expect(&h.at(200), &["SEQ=1001"]);
    assert_eq!(h.info().rto, ms(400));
    h.input(250, seg(5001).ack(1101));
    let info = h.info();
    assert_eq!((info.rto, info.srtt), (ms(400), Some(ms(10))), "no sample from a retransmitted segment");
    h.send(250, 100);
    h.input(260, seg(5001).ack(1201));
    let info = h.info();
    assert_eq!((info.srtt, info.rttvar, info.rto), (Some(ms(10)), Some(std::time::Duration::from_micros(3_750)), ms(200)));
}

/// RFC 8985 §6.1: an ACK is timed from the newest segment it newly acknowledges, which the
/// receiver did not hold waiting for a second.
#[test]
fn s_rt_008_the_newest_segment_acknowledged_is_timed() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    h.send(0, 1460);
    h.send(5, 1460);
    h.input(20, seg(5001).ack(3921));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(10_625)), "one 15 ms sample");
}

#[test]
fn s_rt_009_timestamp_samples() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input(30, seg(5001).ack(1101).wnd(512).ts(50_030, 1000));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(12_500)));
    h.input(40, seg(5001).ack(1101).wnd(512).ts(50_040, 1000));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(12_500)), "no sample without progress");
}

#[test]
fn s_rt_010_a_retransmission_echoed() {
    let mut h = fixture_ef();
    h.send(0, 100);
    expect(&h.at(200), &["SEQ=1001 TS=1200/*"]);
    h.input(230, seg(5001).ack(1101).wnd(512).ts(50_230, 1200));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(12_500)), "a 30 ms sample");
}

/// An echo of a TSval never sent is counted; the ACK is still timed by its segment's departure.
#[test]
fn s_rt_011_an_echo_from_the_future() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input(30, seg(5001).ack(1101).wnd(512).ts(50_030, 5000));
    let info = h.info();
    assert_eq!(info.srtt, Some(std::time::Duration::from_micros(12_500)));
    assert_eq!(info.snd_una.get(), 1101);
    assert_eq!(h.count(Counter::TsEcrInvalid), 1);
}

#[test]
fn s_rt_013_backoff_doubles() {
    let mut h = fixture_e();
    h.send(0, 100);
    let times: Vec<i64> = h.at(300_000).iter().map(|o| o.t).collect();
    assert_eq!(times, [200, 600, 1400, 3000, 6200, 12_600, 25_400, 51_000, 102_200, 162_200, 222_200, 282_200]);
}

#[test]
fn s_rt_014_three_expiries_forget_the_estimate() {
    let mut h = fixture_e();
    h.send(0, 100);
    assert_eq!(h.at(1400).len(), 3);
    h.input(1410, seg(5001).ack(1101));
    h.send(1410, 100);
    h.input(1460, seg(5001).ack(1201));
    let info = h.info();
    assert_eq!((info.srtt, info.rttvar, info.rto), (Some(ms(50)), Some(ms(25)), ms(200)));
}

#[test]
fn s_rt_015_the_timer_restarts_on_new_acks() {
    let mut h = fixture_e();
    let (_, _) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    h.send(0, 1460);
    h.send(100, 1460);
    h.input(150, seg(5001).ack(2461));
    nothing(&h.at(349));
    expect(&h.at(350), &["SEQ=2461"]);
}

#[test]
fn s_rt_016_no_timer_when_all_is_acknowledged() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.input(10, seg(5001).ack(1101));
    assert_eq!(h.info().rtx_timer, None);
}

/// A sample is the departure's own time, finer than the timestamp clock's millisecond: 1.5 ms
/// reads as 1.5 ms where an echoed TSval would say 1 or 2.
#[test]
fn s_rt_018_a_sample_below_the_timestamp_tick() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input_past(1, 500, seg(5001).ack(1101).wnd(512).ts(50_001, 1000));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(8_937)));
}

/// Karn (RFC 6298 §3): after a fast retransmission, the departures before it time nothing: the
/// ACK that repairs the hole covers segments held behind it.
#[test]
fn s_rt_017_a_retransmission_forgets_the_departures_before_it() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 5840).len(), 4);
    for t in 11..=13 {
        h.input(t, seg(5001).ack(1001));
    }
    assert_eq!(h.count(Counter::FastRecovery), 1);
    h.input(20, seg(5001).ack(6841));
    assert_eq!(h.info().srtt, Some(ms(10)), "a segment held behind the hole times nothing");
}

/// Without timestamps too, every ACK that newly acknowledges a timed segment is a sample, not one
/// a round trip: HyStart++ can collect its N_RTT_SAMPLE in a round.
#[test]
fn s_rt_019_each_ack_is_a_sample_without_timestamps() {
    let mut h = fixture_e();
    let (_, _) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    h.send(0, 1460);
    h.send(0, 1460);
    h.input(20, seg(5001).ack(2461));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(11_250)), "a 20 ms sample");
    h.input(28, seg(5001).ack(3921));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(13_343)), "and a 28 ms one");
}
