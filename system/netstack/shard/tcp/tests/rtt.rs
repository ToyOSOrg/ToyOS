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

// ---- the least round trip of the connections whose data moved ----

/// Text that arrives in order reports the receiver's own estimate: an echo of the ACK sent at 0,
/// aged 15 ms. It is reported once, and the handshake that came before it moved no data.
#[test]
fn text_arriving_in_order_reports_the_receivers_round_trip_once() {
    let mut h = fixture_ef();
    assert_eq!(h.tcp.take_least_rtt(), None, "the handshake moved no data");
    h.input(15, seg(5001).ack(1001).wnd(512).ts(50_015, 1000).len(1448));
    assert_eq!(h.tcp.take_least_rtt(), Some(ms(15)));
    assert_eq!(h.tcp.take_least_rtt(), None, "taken");
}

/// Text above a hole moves nothing in order and reports nothing; the segment that fills the hole
/// does, with the estimate its own echo moved: the dup ACK of 20, aged 5 ms, an eighth of the way.
#[test]
fn text_above_a_hole_reports_nothing_until_the_hole_fills() {
    let mut h = fixture_ef();
    h.input(15, seg(5001).ack(1001).wnd(512).ts(50_015, 1000).len(1448));
    h.tcp.take_least_rtt();
    h.input(20, seg(5001 + 2 * 1448).ack(1001).wnd(512).ts(50_020, 1015).len(1448));
    assert_eq!(h.tcp.take_least_rtt(), None, "nothing arrived in order");
    h.input(25, seg(5001 + 1448).ack(1001).wnd(512).ts(50_025, 1020).len(1448));
    assert_eq!(h.tcp.take_least_rtt(), Some(std::time::Duration::from_micros(13_750)));
}

/// An acknowledgment that moves SND.UNA reports the sender's SRTT, and one segment that moves
/// both directions reports the lesser: text echoing 0 aged 30 ms, beside an SRTT of 12.5 ms.
#[test]
fn an_acknowledgment_reports_the_senders_round_trip_and_the_least_wins() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input(30, seg(5001).ack(1101).wnd(512).ts(50_030, 1000));
    let srtt = h.info().srtt;
    assert_eq!(srtt, Some(std::time::Duration::from_micros(12_500)));
    assert_eq!(h.tcp.take_least_rtt(), srtt);

    h.send(30, 100);
    h.input(60, seg(5001).ack(1201).wnd(512).ts(50_060, 1030).len(1448));
    let srtt = h.info().srtt.expect("an estimate");
    assert!(srtt < ms(30), "the sender's estimate is the lesser: {srtt:?}");
    assert_eq!(h.tcp.take_least_rtt(), Some(srtt));
}
