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

/// A sample is the departure's own time, finer than the timestamp clock's millisecond: 1.5 ms
/// reads as 1.5 ms where an echoed TSval would say 1 or 2.
#[test]
fn s_rt_018_a_sample_below_the_timestamp_tick() {
    let mut h = fixture_ef();
    h.send(0, 100);
    h.input_past(1, 500, seg(5001).ack(1101).wnd(512).ts(50_001, 1000));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(8_937)));
}

/// Without timestamps too, every ACK that newly acknowledges a timed segment is a sample, not one
/// a round trip: HyStart++ can collect its N_RTT_SAMPLE in a round.
#[test]
fn s_rt_019_each_ack_is_a_sample_without_timestamps() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    h.send(0, 1460);
    h.send(0, 1460);
    h.input(20, seg(5001).ack(2461));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(11_250)), "a 20 ms sample");
    h.input(28, seg(5001).ack(3921));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(13_343)), "and a 28 ms one");
}

/// Fixture EF with a 256 KiB send buffer, and the peer's window at 512 KiB.
fn fixture_ef_deep() -> H {
    let mut h = H::with(toyos_net_tcp::Config { send_buffer: 262_144, ..config(65_535) });
    h.start(-10);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    h.transmit();
    h.input(0, seg(5000).ack(1001).syn().wnd(65_535).mss(1460).sackok().ts(50_000, 990).ws(7));
    h
}

/// RFC 8985 §6.1 for every segment in flight, however many: with more than 64 out, an ACK of
/// only the newest is timed from their departure, 0.5 ms finer than the echo's millisecond.
#[test]
fn s_rt_020_every_segment_in_flight_is_timed() {
    let mut h = fixture_ef_deep();
    let mut t = 0;
    h.send(t, 262_144);
    while h.info().snd_nxt.since(h.info().snd_una) <= 70 * 1448 {
        t += 1;
        let nxt = h.info().snd_nxt.get();
        h.input(t, seg(5001).ack(nxt).wnd(4096).ts(50_000 + t as u32, 1000 + t as u32 - 1));
        let room = 262_144 - h.info().queued;
        h.send(t, room);
    }
    let (una, nxt) = (h.info().snd_una.get(), h.info().snd_nxt.get());
    let ts = (50_001 + t as u32, 1000 + t as u32);
    h.input_past(t, 300, seg(5001).ack(una + 64 * 1448).wnd(4096).ts(ts.0, ts.1));
    let departures = h.count(Counter::RttFromDeparture);
    h.input_past(t, 500, seg(5001).ack(nxt).wnd(4096).ts(ts.0, ts.1));
    assert_eq!(h.count(Counter::RttFromDeparture), departures + 1, "the newest segment's departure");
    assert_eq!((h.count(Counter::RttFromEcho), h.count(Counter::TsEcrInvalid)), (0, 0), "every echo was valid, and none timed");
}

/// Small sends join one run, timed from its start, which bounds the record by the flight; a full
/// segment after them starts its own: the ACK of both is the segment's 15 ms, not the run's 20.
#[test]
fn s_rt_021_a_segment_after_small_sends_is_timed_from_its_own_departure() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    h.send(0, 100);
    h.send(2, 100);
    h.send(5, 1460);
    h.input(20, seg(5001).ack(2661));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(10_625)), "one 15 ms sample");
}

/// An app-limited sender of small writes: 100 bytes every 10 ms with Nagle off, each ACKed 20 ms
/// after it left. Its writes join a run only until an ACK lands in it, so every sample is the
/// path's 20 ms and srtt stays there, where timing each from its run's first send would grow it.
#[test]
fn s_rt_022_small_periodic_writes_are_each_timed_by_the_path() {
    let mut h = H::new(65_535);
    h.start(-20);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    h.transmit();
    h.input(0, seg(5000).ack(1001).syn().wnd(65_535).mss(1460));
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    for i in 0..30u32 {
        let t = i64::from(i) * 10;
        if i >= 2 {
            h.input(t, seg(5001).ack(1001 + (i - 1) * 100));
            assert_eq!(h.info().srtt, Some(ms(20)), "the ACK at {t} ms");
        }
        assert_eq!(h.send(t, 100).len(), 1);
    }
    assert_eq!(h.count(Counter::RttFromDeparture), 28);
}

/// A window that left at one instant is one run, and every ACK into it is timed exactly, however
/// many land partway.
#[test]
fn s_rt_023_each_ack_into_a_burst_is_timed() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 14_600).len(), 10);
    for (t, segments) in [(10, 3), (11, 6), (12, 10)] {
        h.input(t, seg(5001).ack(1001 + segments * 1460));
    }
    assert_eq!(h.count(Counter::RttFromDeparture), 3);
}

/// Small writes at 0, 5 and 8 ms join one run. The ACK of the first is timed from it; the ACK of
/// the second lands where the run no longer says when its newest byte left, and is not timed; the
/// ACK of the rest is timed from the last write: 20 ms each time it is.
#[test]
fn s_rt_024_a_later_ack_partway_into_a_run_of_writes_is_not_timed() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    for t in [0, 5, 8] {
        h.send(t, 100);
    }
    h.input(20, seg(5001).ack(1101));
    let srtt = h.info().srtt;
    assert_eq!(srtt, Some(std::time::Duration::from_micros(11_250)), "a 20 ms sample");
    h.input(25, seg(5001).ack(1201));
    assert_eq!((h.info().srtt, h.count(Counter::RttFromDeparture)), (srtt, 1), "no sample");
    h.input(28, seg(5001).ack(1301));
    assert_eq!(h.info().srtt, Some(std::time::Duration::from_micros(12_343)), "a 20 ms sample");
}

/// HTTP's shape: a short header, then a window of body a millisecond later. The body is its own
/// run, not joined to the header's, so every ACK into it is a sample: four ACKs, four samples.
#[test]
fn s_rt_025_a_body_after_a_header_is_timed_by_each_ack() {
    let mut h = fixture_e();
    let (_, outs) = h.call(0, |tcp, now, id| tcp.set_options(now, id, toyos_net_tcp::Options { nodelay: true, ..Default::default() }));
    nothing(&outs);
    assert_eq!(h.send(0, 200).len(), 1);
    assert_eq!(h.send(1, 4 * 1460).len(), 4);
    for (t, ack) in [(20, 1201), (21, 1201 + 1460), (22, 1201 + 3 * 1460), (23, 1201 + 4 * 1460)] {
        h.input(t, seg(5001).ack(ack));
    }
    assert_eq!(h.count(Counter::RttFromDeparture), 4);
}
