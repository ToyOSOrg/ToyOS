//! Round-trip time and the retransmission timeout (tcp.md §18.10). RT-01 to RT-04 and RT-12 feed
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

#[test]
fn s_rt_008_one_segment_timed() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    h.send(0, 1460);
    h.send(5, 1460);
    h.input(20, seg(5001).ack(3921));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(11_250)), "one 20 ms sample");
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

#[test]
fn s_rt_011_an_echo_from_the_future() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input(30, seg(5001).ack(1101).wnd(512).ts(50_030, 5000));
    let info = h.info();
    assert_eq!(info.srtt, Some(ms(10)));
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
