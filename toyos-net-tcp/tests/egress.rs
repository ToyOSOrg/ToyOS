//! Pull egress: segments are built at a transmit opportunity once their next hop is known, and
//! timers start at hand-off. PL-11, the `many_up` shape, runs on `toyos-net-testnet` in
//! `toyos-net-shard`'s tests.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_tcp::{Counter, Failure, Hop, SoftError, State};
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

/// B's next hop as `answer` gives it at spec time t; every other next hop is known.
fn hop_b(answer: impl Fn(i64) -> Hop<()> + 'static) -> Hops {
    Box::new(move |t, tuple| if tuple.remote.addr == B { answer(t) } else { Hop::Ready(()) })
}

fn connect_at_0(h: &mut H) {
    h.start(0);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
}

fn counters(h: &H) -> Vec<(&'static str, u64)> {
    h.tcp.counters().iter().collect()
}

#[test]
fn s_pl_012_a_flow_builds_nothing_while_its_next_hop_is_pending() {
    let mut h = H::new(65_535);
    h.hop = hop_b(|t| if t < 100 { Hop::Pending } else { Hop::Ready(()) });
    connect_at_0(&mut h);
    nothing(&h.transmit());
    nothing(&h.at(99));
    assert_eq!(h.tcp.next_deadline(), None, "no retransmission timer armed");
    assert_eq!(h.count(Counter::Rto), 0);
    expect(&h.at(100), &["SEQ=1000 CTL=SYN WND=65535 MSS=1460 SACKOK TS=1100/0 WS=0"]);
    assert_eq!(h.tcp.next_deadline(), Some(h.instant(1_100)), "timed from 100, when it was built");

    let mut h = fixture_e();
    let now = h.now();
    let second = h.tcp.connect(now, A, Some(port(49153)), ep(C, 80)).unwrap();
    h.transmit();
    h.deliver(seg(5000).ack(1001).syn().wnd(65_535).mss(1460).from(C, 80).to(A, 49153));
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
    let outs = h.at(500);
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
    nothing(&h.at(3_000));
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
    nothing(&h.at(4_000));
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
    nothing(&h.at(3_000));
    let status = h.status();
    assert_eq!((status.state, status.soft_error), (State::Established, Some(SoftError::Unreachable(UnreachableCode::Host))));
    assert_eq!((h.count(Counter::Rto), h.count(Counter::NextHopFailed), h.count(Counter::IcmpSoft)), (0, 1, 0));
    for t in [3_001, 3_002] {
        let before = counters(&h);
        nothing(&h.at(t));
        let after = counters(&h);
        let moved: Vec<_> = before.iter().zip(&after).filter(|(b, a)| b != a).map(|(b, a)| (b.0, a.1 - b.1)).collect();
        assert_eq!(moved, [("tcp.next-hop-failed", 1)], "an opportunity at {t} moves one counter by one");
    }
    expect(&h.at(30_000), &["SEQ=1001 ACK=5001 LEN=100"]);
    h.input(30_010, seg(5001).ack(1101));
    assert_eq!(h.status().soft_error, None, "forward progress clears it");

    let mut h = fixture_e();
    h.hop = hop_b(|t| if t < 3_000 { Hop::Pending } else { Hop::Unreachable });
    h.send(0, 100);
    nothing(&h.at(899_999));
    assert_eq!(h.status().state, State::Established);
    nothing(&h.at(900_000));
    assert_eq!(h.status().failure, Some(HOST_UNREACHABLE), "not \"timed out\"");

    // §16: only a segment not built counts, so a connection with nothing due is not asked.
    let mut h = fixture_e();
    h.hop = hop_b(|_| Hop::Unreachable);
    nothing(&h.input(1, seg(5001).ack(1001)));
    assert_eq!((h.status().soft_error, h.count(Counter::NextHopFailed)), (None, 0));
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
        let outs = h.at(50);
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
        let outs = h.at(50);
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
        nothing(&h.at(1_049));
        let outs = h.at(1_050);
        match left {
            true => expect(&outs, &["SEQ=1002 ACK=5002 CTL=ACK"]),
            false => nothing(&outs),
        }
        nothing(&h.at(1_051));
        assert_eq!((h.count(Counter::NextHopFailed), h.tcp.time_wait_count()), (failed, 1));
    }
}
