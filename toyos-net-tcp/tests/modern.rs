//! Modern only and visible refusals, and the divergences.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_tcp::{limits, Counter, Event, Failure, Instant, Refusal, RefusalLog, State};

fn listening() -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(0);
    h.listener = Some(h.tcp.listen(A, Some(port(80)), || 0).unwrap());
    h
}

#[test]
fn s_mod_001_syn_with_fin() {
    let mut h = listening();
    nothing(&h.input(0, seg(5000).syn().fin()));
    assert_eq!(h.tcp.accept(h.listener.unwrap()).unwrap(), None);
    assert_eq!(h.tcp.next_deadline(), None, "no child");
    assert_eq!(h.count(Counter::SynFin), 1);
    let refusals = h.refusals(Counter::SynFin);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].remote, ep(B, 40_000));
}

#[test]
fn s_mod_002_syn_with_rst() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(5001).syn().rst()));
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.count(Counter::SynRst), 1);
    assert_eq!(h.refusals(Counter::SynRst).len(), 1);
    nothing(&h.input(2, seg(5001).syn().rst().to(A, 81)));
    assert_eq!(h.count(Counter::SynRst), 2);
}

#[test]
fn s_mod_003_urgent_data_inline() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).urg(5).data(b"0123456789"));
    assert_eq!(h.read(100), b"0123456789");
    assert_eq!(h.count(Counter::UrgentIgnored), 1);
    assert_eq!(h.refusals(Counter::UrgentIgnored).len(), 1);
    h.input(2, seg(5011).ack(1001).urg(1).data(b"x"));
    assert_eq!(h.count(Counter::UrgentIgnored), 2);
    assert_eq!(h.refusals(Counter::UrgentIgnored).len(), 1);
}

#[test]
fn s_mod_005_one_line_per_rule_per_10_s() {
    let mut h = listening();
    for i in 0..50u8 {
        h.input(i64::from(i), seg(5000).syn().fin().from(Ipv4Addr::new(192, 0, 2, 10 + i), 1234));
    }
    h.input(10_000, seg(5000).syn().fin().from(Ipv4Addr::new(192, 0, 2, 60), 1234));
    assert_eq!(h.count(Counter::SynFin), 51);
    let mut log = RefusalLog::default();
    let times = (0..50).chain([10_000]);
    let lines: Vec<(Refusal, u64)> = h
        .refusals(Counter::SynFin)
        .into_iter()
        .zip(times)
        .filter_map(|(r, t)| log.admit(Instant::from_millis(t), &r).map(|n| (r, n)))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!((lines[0].0.remote, lines[0].1), (ep(Ipv4Addr::new(192, 0, 2, 10), 1234), 0));
    assert_eq!((lines[1].0.remote, lines[1].1), (ep(Ipv4Addr::new(192, 0, 2, 60), 1234), 49));
    let mut h = listening();
    for i in 0..=limits::EVENTS as u16 {
        h.arrive(seg(5000).syn().fin().from(B, 1024 + i));
    }
    assert_eq!(h.tcp.drain_events().count(), limits::EVENTS, "an undrained shell holds a bounded list");
    assert_eq!(h.count(Counter::EventOverflow), 1);
}

#[test]
fn s_mod_005_a_bulk_peer_leaves_room_for_refusals() {
    let mut h = fixture_e();
    ten_out(&mut h);
    for k in 1..=limits::EVENTS as u32 + 1 {
        h.arrive(seg(5001).ack(1001 + k));
    }
    h.arrive(seg(7000).syn().rst().from(Ipv4Addr::new(192, 0, 2, 3), 1234).to(A, 81));
    h.transmit();
    assert_eq!(h.info().snd_una.get(), 1001 + limits::EVENTS as u32 + 1);
    assert_eq!(h.events.iter().filter(|e| **e == Event::Reachable(B)).count(), 1);
    assert_eq!(h.refusals(Counter::SynRst).len(), 1);
    assert_eq!(h.count(Counter::EventOverflow), 0);
}

#[test]
fn s_mod_006_tcp_md5() {
    let mut h = listening();
    let mut md5 = vec![19, 18];
    md5.extend_from_slice(&[0xaa; 16]);
    let outs = h.input(0, seg(5000).syn().mss(1460).opt(Opt::Raw(md5)));
    expect(&outs, &["CTL=SYN,ACK"]);
    assert!(!outs[0].options.contains(&19));
    assert_eq!(h.count(Counter::OptionMd5), 1);
    assert_eq!(h.refusals(Counter::OptionMd5).len(), 1);
}

#[test]
fn s_mod_007_fast_open() {
    let mut h = listening();
    let outs = h.input(0, seg(5000).syn().mss(1460).opt(Opt::Raw(vec![34, 2])).len(20));
    expect(&outs, &["CTL=SYN,ACK ACK=5001"]);
    assert_eq!(h.count(Counter::OptionFastOpen), 1);
    assert_eq!(h.count(Counter::SynDataDiscarded), 1);
    assert_eq!(h.refusals(Counter::OptionFastOpen).len(), 1);
    assert_eq!(h.refusals(Counter::SynDataDiscarded).len(), 1);
}

fn log_lines(h: &H, rule: Counter, times: &[i64]) -> usize {
    let mut log = RefusalLog::default();
    h.refusals(rule).into_iter().zip(times).filter(|(r, &t)| log.admit(Instant::from_millis(t as u64), r).is_some()).count()
}

#[test]
fn s_mod_008_challenges_are_logged_once() {
    let mut h = fixture_e();
    let times: Vec<i64> = (1..=11).map(|k| k * 100).collect();
    for (k, &t) in times.iter().enumerate() {
        h.input(t, seg(5002 + k as u32).rst());
    }
    assert_eq!(h.count(Counter::RstChallenged), 11);
    assert_eq!(log_lines(&h, Counter::RstChallenged, &times), 1);
}

#[test]
fn s_mod_009_out_of_range_acks_are_logged_per_10_s() {
    let mut h = fixture_e();
    h.send(0, 100);
    let times = [1, 5000, 10_001];
    for &t in &times {
        h.input(t, seg(5001).ack(1200).len(50));
    }
    assert_eq!(h.count(Counter::AckOutOfRange), 3);
    let refusals = h.refusals(Counter::AckOutOfRange);
    assert!(refusals.iter().all(|r| r.remote == ep(B, 80)));
    assert_eq!(log_lines(&h, Counter::AckOutOfRange, &times), 2);
}

/// TX-13 up to t = 210: SND.NXT 1002 past a shut window.
fn probed() -> H {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(0));
    h.send(0, 100);
    expect(&h.at(200), &["SEQ=1001 LEN=1"]);
    h.input(210, seg(5001).ack(1001).wnd(0));
    assert_eq!(h.info().snd_nxt.get(), 1002);
    h
}

/// A peer of ours whose window is shut at RCV.NXT: what an RFC 5961 receiver accepts.
fn shut_peer() -> (H, u32) {
    let mut h = fixture_e();
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    (h, 70_536)
}

#[test]
fn s_div_001_rst_at_the_right_edge() {
    let mut h = probed();
    let (r, outs) = h.call(300, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=RST,ACK"]);
    let (mut peer, rcv_nxt) = shut_peer();
    nothing(&peer.input(2, seg(rcv_nxt).rst()));
    assert_eq!(peer.status().failure, Some(Failure::Reset), "an RST at the peer's RCV.NXT lands");
    let (mut peer, rcv_nxt) = shut_peer();
    peer.input(2, seg(rcv_nxt + 1).rst());
    assert_eq!(peer.info().state, State::Established, "one at SND.NXT, a byte past, does not");
}

#[test]
fn s_div_002_rst_at_a_shrunk_edge() {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 7300).len(), 5);
    h.input(1, seg(5001).ack(1001).wnd(2000));
    let (r, outs) = h.call(2, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    expect(&outs, &["SEQ=3000 ACK=5001 CTL=RST,ACK"]);
    let mut peer = open_peer();
    nothing(&peer.input(1, seg(3001).rst()));
    assert_eq!(peer.info().state, State::Established, "an RST at the right edge is dropped");
    let mut peer = open_peer();
    expect(&peer.input(1, seg(3000).rst()), &["SEQ=5001 ACK=1001 CTL=ACK"]);
    expect(&h.input(3, seg(5001).ack(1001)), &["SEQ=1001 ACK=- CTL=RST"]);
    nothing(&peer.input(2, seg(1001).rst()));
    assert_eq!(peer.status().failure, Some(Failure::Reset), "the exact reset lands");
}

/// A peer of ours at RCV.NXT 1001 with a 2000-byte window, B of DIV-02.
fn open_peer() -> H {
    let mut h = H::new(2000);
    h.iss = 5000;
    h.local = (B, 80);
    h.peer = (A, 49152);
    h.start(-20);
    let listener = h.tcp.listen(B, Some(port(80)), || 0).unwrap();
    expect(&h.input(-20, seg(1000).syn().mss(1460)), &["SEQ=5000 ACK=1001 CTL=SYN,ACK WND=2000"]);
    h.input(0, seg(1001).ack(5001));
    h.conn = Some(h.tcp.accept(listener).unwrap().expect("a ready child"));
    h
}

#[test]
fn s_div_003_rst_at_snd_nxt_inside_the_window() {
    let mut h = fixture_e();
    h.send(0, 100);
    let (r, outs) = h.call(1, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    expect(&outs, &["SEQ=1101 ACK=5001 CTL=RST,ACK"]);
}

/// AC-20's second half with B's out-of-order segment offering `wnd`.
fn window_behind_snd_una(wnd: u16) -> H {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).wnd(2920));
    assert_eq!(h.send(0, 5840).len(), 2);
    h.input(1, seg(5101).ack(1001).wnd(wnd).len(100));
    h.input(2, seg(5001).ack(2461).wnd(1460).len(100));
    let info = h.info();
    assert_eq!((info.snd_una.get(), info.snd_nxt.get(), info.rcv_nxt.get()), (2461, 3921, 5201));
    assert_eq!((info.snd_wl1.get(), info.snd_wnd), (5101, u32::from(wnd)));
    h
}

#[test]
fn s_div_002_rst_below_the_offered_edge_behind_snd_una() {
    let mut h = window_behind_snd_una(2920);
    let (r, outs) = h.call(3, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    expect(&outs, &["SEQ=3920 ACK=5201 CTL=RST,ACK"]);
}

#[test]
fn s_div_001_rst_never_below_snd_una() {
    let mut h = window_behind_snd_una(1000);
    let (r, outs) = h.call(3, |tcp, now, id| tcp.abort(now, id));
    r.unwrap();
    expect(&outs, &["SEQ=2461 ACK=5201 CTL=RST,ACK"]);
}
