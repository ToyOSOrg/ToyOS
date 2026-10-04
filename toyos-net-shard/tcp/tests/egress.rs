//! Pull egress: segments are built at a transmit opportunity once their next hop is known, and
//! timers start at hand-off.

mod common;

use std::net::Ipv4Addr;

use std::time::Duration;

use common::*;
use toyos_net_tcp::{Counter, Failure, Hop, Keepalive, Options, SoftError, State};
use toyos_net_wire::icmp::UnreachableCode;

const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
const HOST_UNREACHABLE: Failure = Failure::Unreachable(SoftError::Unreachable(UnreachableCode::Host));

#[test]
fn s_pl_001_the_timer_starts_at_hand_off() {
    let mut h = fixture_e();
    h.credit = Some(0);
    nothing(&h.send(0, 1000));
    nothing(&h.at(499));
    h.credit = None;
    expect(&h.at(500), &["SEQ=1001 LEN=1000"]);
    nothing(&h.at(699));
    expect(&h.at(700), &["SEQ=1001 LEN=1000"]);
}

#[test]
fn s_pl_002_no_expiry_for_a_retransmission_that_never_left() {
    let mut h = fixture_e();
    expect(&h.send(0, 1000), &["LEN=1000"]);
    h.credit = Some(0);
    nothing(&h.at(4999));
    assert_eq!(h.count(Counter::Rto), 1);
    h.credit = None;
    expect(&h.at(5000), &["SEQ=1001 LEN=1000"]);
    nothing(&h.at(5399));
    expect(&h.at(5400), &["SEQ=1001 LEN=1000"]);
    assert_eq!(h.count(Counter::Rto), 2);
    assert_eq!(h.count(Counter::RtoUnsent), 0);
    let mut h = fixture_e();
    assert_eq!(h.send(0, 2920).len(), 2);
    h.credit = Some(0);
    h.at(200);
    h.input(250, seg(5001).ack(2461));
    nothing(&h.at(2000));
    assert_eq!((h.count(Counter::Rto), h.count(Counter::RtoUnsent)), (1, 0), "a partial ACK arms nothing for a retransmission not yet left");
    h.credit = None;
    expect(&h.at(2001), &["SEQ=2461 LEN=1460"]);
    nothing(&h.at(2400));
    expect(&h.at(2401), &["SEQ=2461 LEN=1460"]);
    assert_eq!((h.count(Counter::Rto), h.count(Counter::RtoUnsent)), (2, 0));
}

#[test]
fn s_pl_003_unsent_is_not_in_flight() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 30_000);
    let info = h.info();
    assert_eq!(info.snd_nxt, info.snd_una);
    h.credit = Some(4);
    assert_eq!(h.at(1).len(), 4);
    let info = h.info();
    assert_eq!(info.snd_nxt.get() - info.snd_una.get(), 5840);
}

#[test]
fn s_pl_004_owed_acks_collapse() {
    let mut h = fixture_e();
    h.credit = Some(0);
    for k in 0..5u32 {
        h.input(1, seg(5001 + 100 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(2), &["ACK=5501"]);
}

#[test]
fn s_pl_005_at_most_three_duplicate_acks_are_held() {
    let mut h = fixture_e();
    h.credit = Some(0);
    for k in 0..3u32 {
        h.input(1, seg(6001 + 200 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(2), &["ACK=5001", "ACK=5001", "ACK=5001"]);
    h.credit = Some(0);
    for k in 3..8u32 {
        h.input(3, seg(6001 + 200 * k).ack(1001).len(100));
    }
    h.credit = None;
    expect(&h.at(4), &["ACK=5001", "ACK=5001", "ACK=5001"]);
}

#[test]
fn s_pl_006_a_segment_carries_the_state_of_its_moment() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 100);
    h.input(5, seg(5001).ack(1001).len(50));
    h.credit = None;
    expect(&h.at(10), &["SEQ=1001 ACK=5051 LEN=100"]);
    let mut h = fixture_ef();
    h.credit = Some(0);
    h.send(0, 100);
    h.input_full(5, seg(5001).ack(1001).len(50));
    h.credit = None;
    expect(&h.at(10), &["SEQ=1001 ACK=5051 LEN=100 TS=1010/*"]);
}

#[test]
fn s_pl_007_credit_bounds_an_opportunity() {
    let mut h = fixture_e();
    h.credit = Some(0);
    h.send(0, 5 * 1460);
    h.credit = Some(2);
    expect(&h.at(1), &["SEQ=1001", "SEQ=2461"]);
    h.credit = None;
    expect(&h.at(2), &["SEQ=3921", "SEQ=5381", "SEQ=6841"]);
}

#[test]
fn s_pl_008_resets_go_first() {
    let mut h = fixture_e();
    let other = h.tcp.connect(h.now(), A, Some(port(49153)), ep(B, 81)).unwrap();
    h.transmit();
    h.input(1, seg(7000).ack(1001).syn().mss(1460).from(B, 81).to(A, 49153));
    h.credit = Some(0);
    let now = h.now();
    h.tcp.send(now, other, &[1; 100]).unwrap();
    h.call(2, |tcp, now, id| tcp.abort(now, id)).0.unwrap();
    h.credit = Some(1);
    let outs = h.at(3);
    expect(&outs, &["CTL=RST,ACK"]);
    assert_eq!(outs[0].dst, (B, 80));
    h.credit = None;
    let outs = h.at(4);
    assert_eq!(outs.len(), 1);
    assert_eq!((outs[0].dst, outs[0].payload.len()), ((B, 81), 100));
}

#[test]
fn s_pl_009_give_up_without_credit() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.credit = Some(0);
    nothing(&h.at(900_000));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)));
    h.credit = None;
    expect(&h.at(900_001), &["SEQ=1101 ACK=5001 CTL=RST,ACK"]);
}

#[test]
fn s_pl_010_give_up_before_retransmission() {
    let mut h = fixture_e();
    h.send(0, 100);
    h.at(162_199);
    h.credit = Some(0);
    nothing(&h.at(179_999));
    h.credit = None;
    expect(&h.at(180_000), &["SEQ=1001 LEN=100"]);
    let times: Vec<i64> = h.at(899_999).iter().map(|o| o.t).collect();
    assert_eq!(times.first(), Some(&240_000));
    assert_eq!(times.last(), Some(&840_000), "an expiry every 60 s from 180 s");
    let rtos = h.count(Counter::Rto);
    expect(&h.at(900_000), &["CTL=RST,ACK"]);
    assert_eq!(h.count(Counter::Rto), rtos, "the give-up came first; no retransmission was made due");
}

#[test]
fn s_pl_009_a_handshake_gives_up_without_credit() {
    let mut h = H::new(65_535);
    h.credit = Some(0);
    connect_at_0(&mut h);
    nothing(&h.at(179_999));
    assert_eq!(h.status().state, State::SynSent);
    nothing(&h.at(180_000));
    assert_eq!(h.status().failure, Some(Failure::TimedOut), "no SYN ever left, yet the give-up ran");

    let mut h = listening();
    h.credit = Some(0);
    nothing(&h.input(0, seg(5000).syn().mss(1460)));
    nothing(&h.at(59_999));
    assert_eq!(h.count(Counter::SynAckGiveUp), 0);
    nothing(&h.at(60_000));
    assert_eq!(h.count(Counter::SynAckGiveUp), 1);
    assert_eq!(h.tcp.next_deadline(), None);
}

#[test]
fn s_pl_001_a_frame_the_sink_refuses_never_left() {
    let mut h = fixture_e();
    h.unframed = true;
    nothing(&h.send(0, 1000));
    let info = h.info();
    assert_eq!((info.snd_nxt, info.rtx_timer), (info.snd_una, None), "nothing of it counts as sent");
    assert_eq!(h.count(Counter::FrameRefused), 1, "offered once in the opportunity that refused it");
    nothing(&h.at(4_999));
    assert_eq!(h.count(Counter::FrameRefused), 2, "and once more at the next");
    h.unframed = false;
    expect(&h.at(5_000), &["SEQ=1001 ACK=5001 LEN=1000"]);
    assert_eq!(h.info().rtx_timer, Some(h.instant(5_200)), "timed from its hand-off");
}

/// Two in-order segments from B in one receive pass at t = 1: A owes an ACK at once.
fn two_in_order(h: &mut H) -> Vec<O> {
    let mut outs = h.at(1);
    h.arrive(seg(5001).ack(1001).len(100));
    h.arrive(seg(5101).ack(1001).len(100));
    outs.extend(h.transmit());
    outs
}

#[test]
fn s_pl_012_an_owed_ack_waits_for_its_next_hop() {
    let mut h = fixture_e();
    h.hop = hop_b(|t| if t < 100 { Hop::Pending } else { Hop::Ready(()) });
    nothing(&two_in_order(&mut h));
    nothing(&h.at(99));
    expect(&woken(&mut h, 100), &["SEQ=1001 ACK=5201 CTL=ACK LEN=0"]);
    nothing(&h.at(1_000));
}

#[test]
fn s_pl_014_an_owed_ack_outlasts_an_unreachable_next_hop() {
    let mut h = fixture_e();
    h.hop = hop_b(|t| if t < 100 { Hop::Unreachable } else { Hop::Ready(()) });
    nothing(&two_in_order(&mut h));
    assert_eq!((h.status().soft_error, h.count(Counter::NextHopFailed)), (Some(SoftError::Unreachable(UnreachableCode::Host)), 1));
    nothing(&h.at(99));
    expect(&woken(&mut h, 100), &["SEQ=1001 ACK=5201 CTL=ACK LEN=0"]);
    nothing(&h.at(1_000));
}

#[test]
fn s_pl_001_an_ack_the_sink_refuses_stays_owed() {
    let mut h = fixture_e();
    h.unframed = true;
    nothing(&two_in_order(&mut h));
    assert_eq!(h.count(Counter::FrameRefused), 1);
    h.unframed = false;
    expect(&h.at(2), &["SEQ=1001 ACK=5201 CTL=ACK LEN=0"]);
    nothing(&h.at(1_000));
}

#[test]
fn s_pl_012_a_window_update_offers_nothing_until_it_leaves() {
    // B fills A's buffer of two segments, and A's window is shut.
    let mut h = client(2_920, seg(5000).ack(1001).syn().wnd(65_535).mss(1460));
    nothing(&h.input(1, seg(5001).ack(1001).len(1460)));
    expect(&h.input(2, seg(6461).ack(1001).len(1460)), &["ACK=7921 WND=0"]);
    h.hop = hop_b(|t| if t < 100 { Hop::Pending } else { Hop::Ready(()) });
    assert_eq!(h.read(2_920).len(), 2_920);
    nothing(&h.transmit());
    assert_eq!(h.info().rcv_edge.get(), 7_921, "the edge the update will offer is not offered yet");
    nothing(&h.at(99));
    expect(&woken(&mut h, 100), &["SEQ=1001 ACK=7921 WND=2920 LEN=0"]);
    assert_eq!(h.info().rcv_edge.get(), 10_841);
}

#[test]
fn s_pl_012_an_ack_owed_in_syn_received_waits_for_its_next_hop() {
    // A segment outside the window is owed an ACK (RFC 9293 §3.10.7.4).
    let outside = || seg(75_001).ack(1001).len(10);

    let mut h = listening();
    expect(&h.input(0, seg(5000).syn().mss(1460)), &["CTL=SYN,ACK"]);
    h.hop = hop_b(|t| if t < 100 { Hop::Pending } else { Hop::Ready(()) });
    nothing(&h.input(10, outside()));
    nothing(&h.at(99));
    expect(&woken(&mut h, 100), &["SEQ=1001 ACK=5001 CTL=ACK LEN=0"]);
    nothing(&h.at(999));

    let mut h = listening();
    expect(&h.input(0, seg(5000).syn().mss(1460)), &["CTL=SYN,ACK"]);
    h.unframed = true;
    nothing(&h.input(10, outside()));
    assert_eq!(h.count(Counter::FrameRefused), 1);
    h.unframed = false;
    expect(&h.at(11), &["SEQ=1001 ACK=5001 CTL=ACK LEN=0"]);
    nothing(&h.at(999));
}

#[test]
fn s_pl_015_what_is_owed_outside_a_connection_outlasts_a_refused_frame() {
    let mut h = fixture_e();
    h.unframed = true;
    nothing(&h.call(0, |tcp, now, id| tcp.abort(now, id).unwrap()).1);
    assert_eq!(h.count(Counter::FrameRefused), 1);
    h.unframed = false;
    expect(&h.at(1), &["SEQ=1001 ACK=5001 CTL=RST,ACK"]);
    nothing(&h.at(2));

    let mut h = H::new(65_535);
    h.unframed = true;
    nothing(&h.input(0, seg(5000).syn().from(B, 40_000).to(A, 81)));
    assert_eq!(h.count(Counter::FrameRefused), 1);
    h.unframed = false;
    expect(&h.at(1), &["SEQ=0 ACK=5001 CTL=RST,ACK"]);
    nothing(&h.at(2));

    let mut h = fixture_e();
    h.close(0);
    h.input(10, seg(5001).ack(1002));
    expect(&h.input(20, seg(5001).ack(1002).fin()), &["SEQ=1002 ACK=5002"]);
    h.unframed = true;
    nothing(&h.input(1_000, seg(5001).ack(1002).fin()));
    assert_eq!(h.count(Counter::FrameRefused), 1);
    h.unframed = false;
    expect(&h.at(1_001), &["SEQ=1002 ACK=5002 CTL=ACK"]);
    nothing(&h.at(1_002));
}

/// B's next hop as `answer` gives it at spec time t; every other next hop is known.
fn hop_b(answer: impl Fn(i64) -> Hop<()> + 'static) -> Hops {
    Box::new(move |t, tuple| if tuple.remote.addr == B { answer(t) } else { Hop::Ready(()) })
}

/// [ip] reported a change for B's next hop: what waits on it asks again at the opportunity at `t`.
fn woken(h: &mut H, t: i64) -> Vec<O> {
    h.tcp.wake(B);
    h.at(t)
}

fn connect_at_0(h: &mut H) {
    h.start(0);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
}

fn counters(h: &H) -> Vec<(&'static str, u64)> {
    h.tcp.counters().iter().collect()
}

/// Fixture E and a second connection, 49153 to 192.0.2.3:80, established with its hop ready.
fn with_second() -> (H, toyos_net_tcp::ConnId) {
    let mut h = fixture_e();
    let now = h.now();
    let second = h.tcp.connect(now, A, Some(port(49153)), ep(C, 80)).unwrap();
    h.transmit();
    h.deliver(seg(5000).ack(1001).syn().wnd(65_535).mss(1460).from(C, 80).to(A, 49153));
    (h, second)
}

#[test]
fn s_pl_012_a_flow_builds_nothing_while_its_next_hop_is_pending() {
    let mut h = H::new(65_535);
    h.hop = hop_b(|t| if t < 100 { Hop::Pending } else { Hop::Ready(()) });
    connect_at_0(&mut h);
    nothing(&h.transmit());
    nothing(&h.at(99));
    assert_eq!(h.asked, 1, "a waiting flow is not asked again until its next hop changes");
    assert_eq!(h.tcp.next_deadline(), Some(h.instant(180_000)), "no retransmission timer armed, only the give-up");
    assert_eq!(h.count(Counter::Rto), 0);
    expect(&woken(&mut h, 100), &["SEQ=1000 CTL=SYN WND=65535 MSS=1460 SACKOK TS=1100/0 WS=0"]);
    assert_eq!(h.tcp.next_deadline(), Some(h.instant(1_100)), "timed from 100, when it was built");

    let (mut h, second) = with_second();
    h.hop = hop_b(|t| if t < 500 { Hop::Pending } else { Hop::Ready(()) });
    h.credit = Some(0);
    h.send(0, 1000);
    let now = h.now();
    h.tcp.send(now, second, &[7; 1000]).unwrap();
    h.credit = Some(1);
    let outs = h.transmit();
    expect(&outs, &["SEQ=1001 LEN=1000"]);
    assert_eq!(outs[0].dst, (C, 80), "the one frame is the second connection's");
    let info = h.info();
    assert_eq!((info.snd_nxt.get(), info.snd_una.get()), (1001, 1001), "E's FlightSize is 0");
    assert_eq!(info.rtx_timer, None, "nothing of E's is timed");
    nothing(&h.at(499));
    h.credit = None;
    let outs = woken(&mut h, 500);
    expect(&outs, &["SEQ=1001 ACK=5001 LEN=1000", "SEQ=1001 LEN=1000"]);
    assert_eq!((outs[0].dst, outs[1].dst), ((B, 80), (C, 80)), "E's segment, then the second's retransmission");
    assert_eq!(h.info().rtx_timer, Some(h.instant(700)));
}

#[test]
fn s_pl_013_a_local_host_unreachable_fails_a_connect_at_once() {
    let mut h = H::new(65_535);
    h.hop = hop_b(|t| if t < 3_000 { Hop::Pending } else { Hop::Unreachable });
    connect_at_0(&mut h);
    nothing(&h.transmit());
    nothing(&h.at(2_999));
    assert_eq!(h.status().state, State::SynSent);
    nothing(&woken(&mut h, 3_000));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(HOST_UNREACHABLE)));
    h.hop = hop_b(|_| Hop::Ready(()));
    nothing(&h.at(3_001));
    assert_eq!(h.tcp.next_deadline(), None, "the TCB is deleted and no RST is owed");
    assert_eq!((h.count(Counter::Rto), h.count(Counter::NextHopFailed)), (0, 1));

    let mut h = H::new(65_535);
    h.hop = hop_b(|t| match t {
        ..1_000 => Hop::Ready(()),
        1_000..4_000 => Hop::Pending,
        _ => Hop::Unreachable,
    });
    connect_at_0(&mut h);
    expect(&h.transmit(), &["CTL=SYN"]);
    nothing(&h.at(3_999));
    assert_eq!((h.status().state, h.count(Counter::Rto)), (State::SynSent, 1), "the retransmission due at 1,000 is never built");
    nothing(&woken(&mut h, 4_000));
    assert_eq!(h.status().failure, Some(HOST_UNREACHABLE));
    assert_eq!((h.count(Counter::Rto), h.count(Counter::NextHopFailed)), (1, 1));
}

#[test]
fn s_pl_014_a_synchronized_connection_records_host_unreachable_soft() {
    let mut h = fixture_e();
    h.hop = hop_b(|t| match t {
        ..3_000 => Hop::Pending,
        3_000..30_000 => Hop::Unreachable,
        _ => Hop::Ready(()),
    });
    nothing(&h.send(0, 100));
    nothing(&h.at(2_999));
    assert_eq!(h.status().soft_error, None);
    nothing(&woken(&mut h, 3_000));
    let status = h.status();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::Unreachable(UnreachableCode::Host))));
    assert_eq!((h.count(Counter::Rto), h.count(Counter::NextHopFailed), h.count(Counter::IcmpSoft)), (0, 1, 0));
    let (before, asked) = (counters(&h), h.asked);
    for t in [3_001, 3_002, 29_999] {
        nothing(&h.at(t));
    }
    assert_eq!((counters(&h), h.asked), (before, asked), "later opportunities ask nothing and count nothing");
    expect(&woken(&mut h, 30_000), &["SEQ=1001 ACK=5001 LEN=100"]);
    h.input(30_010, seg(5001).ack(1101));
    assert_eq!(h.status().soft_error, None, "forward progress clears it");

    let mut h = fixture_e();
    h.hop = hop_b(|t| if t < 3_000 { Hop::Pending } else { Hop::Unreachable });
    h.send(0, 100);
    woken(&mut h, 3_000);
    nothing(&h.at(899_999));
    assert_eq!(h.status().state, State::Established);
    nothing(&h.at(900_000));
    assert_eq!(h.status().failure, Some(HOST_UNREACHABLE), "not \"timed out\"");

    // §16: only a segment not built counts, so a connection with nothing due is not asked.
    let mut h = fixture_e();
    h.hop = hop_b(|_| Hop::Unreachable);
    nothing(&h.input(1, seg(5001).ack(1001)));
    assert_eq!((h.status().soft_error, h.count(Counter::NextHopFailed)), (None, 0));

    // The shard spends one frame of credit per opportunity: E is asked at the first and at none
    // of the three after it, while the second connection sends.
    let (mut h, second) = with_second();
    h.hop = hop_b(|_| Hop::Unreachable);
    h.credit = Some(0);
    h.send(0, 100);
    let now = h.now();
    h.tcp.send(now, second, &[7; 4 * 1460]).unwrap();
    let asked = h.asked;
    for _ in 0..4 {
        h.credit = Some(1);
        let outs = h.transmit();
        assert_eq!(outs.iter().map(|o| o.dst).collect::<Vec<_>>(), [(C, 80)]);
    }
    assert_eq!((h.count(Counter::NextHopFailed), h.asked - asked), (1, 5), "E once, the second's four");
}

#[test]
fn s_pl_015_what_is_owed_outside_a_connection_waits_for_its_next_hop() {
    for then in [Hop::Ready(()), Hop::Unreachable] {
        let left = then == Hop::Ready(());
        let failed = u64::from(!left);

        let mut h = fixture_e();
        h.hop = hop_b(move |t| if t < 50 { Hop::Pending } else { then });
        nothing(&h.call(0, |tcp, now, id| tcp.abort(now, id).unwrap()).1);
        nothing(&h.at(49));
        let outs = woken(&mut h, 50);
        match left {
            true => expect(&outs, &["SEQ=1001 ACK=5001 CTL=RST,ACK"]),
            false => nothing(&outs),
        }
        assert_eq!(h.count(Counter::NextHopFailed), failed);
        // The stub is freed: a segment for the 4-tuple is answered as one for no socket.
        let outs = h.input(60, seg(5001).ack(1001));
        match left {
            true => expect(&outs, &["SEQ=1001 ACK=- CTL=RST"]),
            false => nothing(&outs),
        }
        assert_eq!(h.count(Counter::NextHopFailed), 2 * failed);

        let mut h = H::new(65_535);
        h.hop = hop_b(move |t| if t < 50 { Hop::Pending } else { then });
        nothing(&h.input(0, seg(5000).syn().from(B, 40_000).to(A, 81)));
        nothing(&h.at(49));
        let outs = woken(&mut h, 50);
        match left {
            true => expect(&outs, &["SEQ=0 ACK=5001 CTL=RST,ACK"]),
            false => nothing(&outs),
        }
        nothing(&h.at(51));
        assert_eq!(h.count(Counter::NextHopFailed), failed);

        let mut h = fixture_e();
        h.close(0);
        h.input(10, seg(5001).ack(1002));
        expect(&h.input(20, seg(5001).ack(1002).fin()), &["SEQ=1002 ACK=5002"]);
        h.hop = hop_b(move |t| match t {
            ..1_000 => Hop::Ready(()),
            1_000..1_050 => Hop::Pending,
            _ => then,
        });
        nothing(&h.input(1_000, seg(5001).ack(1002).fin()));
        let asked = h.asked;
        nothing(&h.input(1_010, seg(5001).ack(1002).fin()));
        assert_eq!(h.asked, asked, "the waiting ACK is owed once and asked once");
        nothing(&h.at(1_049));
        let outs = woken(&mut h, 1_050);
        match left {
            true => expect(&outs, &["SEQ=1002 ACK=5002 CTL=ACK"]),
            false => nothing(&outs),
        }
        nothing(&h.at(1_051));
        assert_eq!((h.count(Counter::NextHopFailed), h.tcp.time_wait_count()), (failed, 1));
    }
}

#[test]
fn s_hs_034_a_child_whose_synack_cannot_leave_gives_up() {
    let mut h = listening();
    h.hop = hop_b(|t| if t < 3_000 { Hop::Pending } else { Hop::Unreachable });
    nothing(&h.input(0, seg(5000).syn().mss(1460)));
    nothing(&woken(&mut h, 3_000));
    assert_eq!(h.count(Counter::NextHopFailed), 1);
    // The hold-down's end: the child asks again, and is told again.
    nothing(&woken(&mut h, 23_000));
    assert_eq!(h.count(Counter::NextHopFailed), 2);
    nothing(&h.at(59_999));
    assert_eq!(h.count(Counter::SynAckGiveUp), 0);
    nothing(&h.at(60_000));
    assert_eq!(h.count(Counter::SynAckGiveUp), 1, "60 s from the SYN, though no SYN-ACK left");
    assert_eq!(h.tcp.next_deadline(), None);
    // Its slot is free: B's SYN again is a new child, answered once the hop is.
    h.hop = hop_b(|_| Hop::Ready(()));
    expect(&h.input(61_000, seg(5000).syn().mss(1460)), &["CTL=SYN,ACK"]);
}

#[test]
fn s_ka_003_probes_that_cannot_leave_still_give_up() {
    let mut h = fixture_e();
    let options = Options { keepalive: Some(Keepalive { idle: Duration::from_secs(7_200), ..Keepalive::default() }), ..Options::default() };
    h.call(0, |tcp, now, id| tcp.set_options(now, id, options)).0.unwrap();
    h.hop = hop_b(|t| if t < 7_200_000 { Hop::Ready(()) } else { Hop::Unreachable });
    nothing(&h.at(7_200_000));
    let status = h.status();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::Unreachable(UnreachableCode::Host))));
    nothing(&h.at(7_874_999));
    assert_eq!(h.status().state, State::Established);
    nothing(&h.at(7_875_000));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)), "as nine unanswered probes would");
    assert_eq!((h.count(Counter::KeepaliveProbe), h.count(Counter::NextHopFailed)), (0, 2), "the probe, then the reset");
}

#[test]
fn s_ka_002_a_segment_cancels_a_probe_still_owed() {
    let mut h = fixture_e();
    let options = Options { keepalive: Some(Keepalive { idle: Duration::from_secs(10), ..Keepalive::default() }), ..Options::default() };
    h.call(0, |tcp, now, id| tcp.set_options(now, id, options)).0.unwrap();
    h.hop = hop_b(|t| if (10_000..11_000).contains(&t) { Hop::Pending } else { Hop::Ready(()) });
    nothing(&h.at(10_000));
    nothing(&h.input(10_500, seg(5001).ack(1001)));
    nothing(&woken(&mut h, 11_000));
    nothing(&h.at(20_499));
    expect(&h.at(20_500), &["SEQ=1000 ACK=5001 CTL=ACK"]);
}

#[test]
fn s_pl_012_a_connection_freed_while_waiting_leaves_no_turn_behind() {
    let (mut h, second) = with_second();
    h.hop = hop_b(|_| Hop::Pending);
    let now = h.now();
    let waiting = h.tcp.connect(now, A, Some(port(49154)), ep(B, 81)).unwrap();
    nothing(&h.transmit());
    h.tcp.abort(now, waiting).unwrap();
    // A connection to 192.0.2.3 takes the freed slot, and sends beside the second in turn.
    let third = h.tcp.connect(now, A, Some(port(49155)), ep(C, 81)).unwrap();
    h.transmit();
    h.deliver(seg(9000).ack(1001).syn().wnd(65_535).mss(1460).from(C, 81).to(A, 49155));
    h.credit = Some(0);
    h.tcp.send(now, second, &[7; 3 * 1460]).unwrap();
    h.tcp.send(now, third, &[7; 3 * 1460]).unwrap();
    h.tcp.wake(B);
    h.credit = Some(3);
    let ports: Vec<u16> = h.transmit().iter().map(|o| o.src.1).collect();
    assert_eq!(ports, [49153, 49155, 49153], "one turn each");
}

#[test]
fn s_pl_015_resets_waiting_for_their_next_hop_stay_bounded() {
    let mut h = H::new(65_535);
    h.hop = hop_b(|t| if t < 50 { Hop::Pending } else { Hop::Ready(()) });
    for p in 0..70u16 {
        nothing(&h.input(0, seg(5000).syn().from(B, 40_000 + p).to(A, 81)));
    }
    assert_eq!(h.count(Counter::ClosedRstLimited), 6, "64 answers held");
    assert_eq!(woken(&mut h, 50).len(), 64);
    expect(&h.input(60, seg(5000).syn().from(B, 41_000).to(A, 81)), &["CTL=RST,ACK"]);

    // An answer whose frame was refused is held too: 63 waiting and it are 64.
    let mut h = H::new(65_535);
    h.hop = hop_b(|_| Hop::Pending);
    for p in 0..63u16 {
        nothing(&h.input(0, seg(5000).syn().from(B, 40_000 + p).to(A, 81)));
    }
    h.unframed = true;
    nothing(&h.input(0, seg(5000).syn().from(C, 40_000).to(A, 81)));
    assert_eq!((h.count(Counter::FrameRefused), h.count(Counter::ClosedRstLimited)), (1, 0));
    h.credit = Some(0);
    nothing(&h.input(0, seg(5000).syn().from(C, 40_001).to(A, 81)));
    assert_eq!(h.count(Counter::ClosedRstLimited), 1);
    // It leaves, and the 63 still waiting leave room for one, asked again or not.
    h.unframed = false;
    h.credit = None;
    expect(&h.at(1), &["CTL=RST,ACK"]);
    expect(&h.input(2, seg(5000).syn().from(C, 40_002).to(A, 81)), &["CTL=RST,ACK"]);
    h.tcp.wake_all();
    expect(&h.input(3, seg(5000).syn().from(C, 40_003).to(A, 81)), &["CTL=RST,ACK"]);
}
