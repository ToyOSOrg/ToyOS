//! Opening: CLOSED, LISTEN, SYN-SENT, SYN-RECEIVED (tcp.md §18.6).

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_tcp::{Counter, Error, Failure, Received, State};

/// HS-01's opening: A connects at `t`, the SYN leaves.
fn connecting(t: i64) -> H {
    let mut h = H::new(65_535);
    h.start(t);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    expect(&h.transmit(), &["SEQ=1000 CTL=SYN WND=65535 MSS=1460 SACKOK TS=1000/0 WS=0"]);
    h
}

fn listening() -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(0);
    h.listener = Some(h.tcp.listen(A, Some(port(80)), || 0).unwrap());
    h
}

/// HS-03 before the ACK: a pending child.
fn child() -> H {
    let mut h = listening();
    expect(&h.input(0, seg(5000).syn().mss(1460)), &["SEQ=1000 ACK=5001 CTL=SYN,ACK WND=65535 MSS=1460"]);
    h
}

/// HS-05 after t = 5: an active SYN-RECEIVED.
fn simultaneous() -> H {
    let mut h = connecting(0);
    let outs = h.input(5, seg(5000).syn().mss(1460));
    expect(&outs, &["SEQ=1000 ACK=5001 CTL=SYN,ACK WND=65535 MSS=1460 NOSACKOK TS=- WS=-"]);
    assert_eq!(h.status().state, State::SynReceived);
    h
}

fn accept(h: &mut H) -> Option<toyos_net_tcp::ConnId> {
    h.tcp.accept(h.listener.unwrap()).unwrap()
}

#[test]
fn s_hs_001_active_open_bare_peer() {
    let mut h = connecting(0);
    assert_eq!(h.status().state, State::SynSent);
    let outs = h.input(10, seg(5000).ack(1001).syn().mss(1460));
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=ACK WND=65535 NOOPT"]);
    let info = h.info();
    assert_eq!(info.state, State::Established);
    assert!(!info.sack && info.ts_recent.is_none() && info.snd_shift == 0 && info.rcv_shift == 0);
    assert_eq!((info.snd_wnd, info.smss, info.cwnd), (65_535, 1460, 14_600));
    assert_eq!(info.srtt, Some(ms(10)));
    assert_eq!(info.rto, ms(200));
}

#[test]
fn s_hs_002_active_open_full_peer() {
    let mut h = connecting(0);
    let outs = h.input(10, seg(5000).ack(1001).syn().mss(1460).sackok().ts(50_010, 1000).ws(7));
    expect(&outs, &["SEQ=1001 ACK=5001 WND=65535 TS=1010/50010 SACK=-"]);
    let info = h.info();
    assert!(info.sack);
    assert_eq!(info.ts_recent, Some(50_010));
    assert_eq!((info.snd_shift, info.rcv_shift, info.smss, info.cwnd), (7, 0, 1448, 14_480));
    assert_eq!(info.srtt, Some(ms(10)));
}

#[test]
fn s_hs_003_passive_open_bare_peer() {
    let mut h = child();
    nothing(&h.input(20, seg(5001).ack(1001)));
    let id = accept(&mut h).expect("ready");
    h.conn = Some(id);
    assert_eq!(h.tcp.tuple(id).unwrap().remote, ep(B, 40_000));
    let info = h.info();
    assert_eq!(info.state, State::Established);
    assert_eq!(info.srtt, Some(ms(20)));
}

#[test]
fn s_hs_004_passive_open_full_peer() {
    let mut h = listening();
    let outs = h.input(0, seg(5000).syn().mss(1460).sackok().ts(49_980, 0).ws(7));
    expect(&outs, &["SEQ=1000 ACK=5001 CTL=SYN,ACK WND=65535 MSS=1460 SACKOK TS=1000/49980 WS=0"]);
    h.input(20, seg(5001).ack(1001).wnd(512).ts(50_000, 1000));
    h.conn = accept(&mut h);
    assert_eq!(h.info().srtt, Some(ms(20)));
}

#[test]
fn s_hs_005_simultaneous_open() {
    let mut h = simultaneous();
    let outs = h.input(10, seg(5000).ack(1001).syn().mss(1460));
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=ACK NOOPT"]);
    let info = h.info();
    assert_eq!(info.state, State::Established);
    assert!(!info.sack && info.ts_recent.is_none() && info.snd_shift == 0);
}

#[test]
fn s_hs_006_simultaneous_open_synack_lost() {
    let mut h = simultaneous();
    nothing(&h.input(10, seg(5001).ack(1001)));
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_hs_007_synsent_ack_at_iss() {
    let mut h = connecting(0);
    expect(&h.input(1, seg(5000).ack(1000).syn()), &["SEQ=1000 ACK=- CTL=RST"]);
    assert_eq!(h.status().state, State::SynSent);
    nothing(&h.at(999));
    expect(&h.at(1000), &["SEQ=1000 CTL=SYN"]);
}

#[test]
fn s_hs_008_synsent_ack_past_snd_nxt() {
    let mut h = connecting(0);
    expect(&h.input(1, seg(5000).ack(1002).syn()), &["SEQ=1002 CTL=RST"]);
    assert_eq!(h.status().state, State::SynSent);
}

#[test]
fn s_hs_009_synsent_refused() {
    let mut h = connecting(0);
    nothing(&h.input(1, seg(0).ack(1001).rst()));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::Refused)));
}

#[test]
fn s_hs_010_synsent_rst_without_ack() {
    let mut h = connecting(0);
    nothing(&h.input(1, seg(0).rst()));
    assert_eq!(h.status().state, State::SynSent);
    assert_eq!(h.count(Counter::SynSentRstNoAck), 1);
}

#[test]
fn s_hs_011_synsent_rst_with_a_bad_ack() {
    let mut h = connecting(0);
    nothing(&h.input(1, seg(0).ack(1000).rst()));
    assert_eq!(h.status().state, State::SynSent);
}

#[test]
fn s_hs_012_synsent_ack_without_syn() {
    let mut h = connecting(0);
    nothing(&h.input(1, seg(5001).ack(1001)));
    assert_eq!(h.status().state, State::SynSent);
}

#[test]
fn s_hs_013_synsent_bad_ack_without_syn() {
    let mut h = connecting(0);
    expect(&h.input(1, seg(5001).ack(2000)), &["SEQ=2000 CTL=RST"]);
}

#[test]
fn s_hs_014_synsent_fin() {
    let mut h = connecting(0);
    nothing(&h.input(1, seg(5000).fin()));
    assert_eq!(h.status().state, State::SynSent);
}

#[test]
fn s_hs_015_syn_give_up() {
    let mut h = connecting(0);
    let outs = h.at(182_999);
    let times: Vec<i64> = outs.iter().map(|o| o.t).collect();
    assert_eq!(times, [1000, 3000, 7000, 15_000, 31_000, 63_000, 123_000]);
    for o in &outs {
        check(o, "SEQ=1000 CTL=SYN MSS=1460 SACKOK WS=0");
        assert_eq!(o.ts, Some((1000 + o.t as u32, 0)));
    }
    nothing(&h.at(183_000));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::TimedOut)));
}

#[test]
fn s_hs_016_one_syn_retransmission() {
    let mut h = connecting(0);
    expect(&h.at(1000), &["CTL=SYN"]);
    h.input(1010, seg(5000).ack(1001).syn().mss(1460));
    let info = h.info();
    assert_eq!(info.srtt, None, "Karn: no sample");
    assert_eq!(info.rto, ms(3000));
    assert_eq!(info.cwnd, 14_600);
    expect(&h.send(1010, 100), &["SEQ=1001 LEN=100"]);
    nothing(&h.at(4009));
    expect(&h.at(4010), &["SEQ=1001 LEN=100"]);
}

#[test]
fn s_hs_017_two_syn_retransmissions() {
    let mut h = connecting(0);
    h.at(3000);
    h.input(3010, seg(5000).ack(1001).syn().mss(1460));
    assert_eq!(h.info().cwnd, 1460);
    expect(&h.send(3010, 14_600), &["LEN=1460"]);
}

#[test]
fn s_hs_018_syn_payload_is_discarded() {
    let mut h = listening();
    expect(&h.input(0, seg(5000).syn().len(10).mss(1460)), &["CTL=SYN,ACK ACK=5001"]);
    assert_eq!(h.count(Counter::SynDataDiscarded), 1);
    assert_eq!(h.refusals(Counter::SynDataDiscarded)[0].remote, ep(B, 40_000));
    h.input(10, seg(5001).ack(1001).len(10));
    h.conn = accept(&mut h);
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.read(100).len(), 10);
}

#[test]
fn s_hs_019_synack_payload_is_discarded() {
    let mut h = connecting(0);
    let outs = h.input(10, seg(5000).ack(1001).syn().len(10).mss(1460));
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=ACK"]);
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.count(Counter::SynDataDiscarded), 1);
    assert_eq!(h.refusals(Counter::SynDataDiscarded).len(), 1);
}

#[test]
fn s_hs_020_listen_rst() {
    let mut h = listening();
    nothing(&h.input(0, seg(5000).rst()));
    assert_eq!(h.count(Counter::ListenRst), 1);
}

#[test]
fn s_hs_021_listen_ack() {
    let mut h = listening();
    expect(&h.input(0, seg(5000).ack(777)), &["SEQ=777 ACK=- CTL=RST TS=-"]);
    expect(&h.input(1, seg(5000).ack(777).ts(90, 80)), &["SEQ=777 CTL=RST TS=0/90"]);
}

#[test]
fn s_hs_022_listen_synack() {
    let mut h = listening();
    expect(&h.input(0, seg(5000).ack(777).syn()), &["SEQ=777 CTL=RST"]);
    assert_eq!(accept(&mut h), None);
    assert_eq!(h.tcp.time_wait_count(), 0);
}

#[test]
fn s_hs_023_listen_without_syn() {
    let mut h = listening();
    nothing(&h.input(0, seg(5000).fin()));
    nothing(&h.input(1, seg(5000)));
    assert_eq!(h.count(Counter::ListenNoSyn), 2);
}

#[test]
fn s_hs_024_duplicate_syn_in_syn_received() {
    let mut h = child();
    expect(&h.input(500, seg(5000).syn().mss(1460)), &["SEQ=1000 ACK=5001 CTL=SYN,ACK"]);
    assert_eq!(h.count(Counter::SynRcvdDupSyn), 1);
    nothing(&h.at(999));
    expect(&h.at(1000), &["SEQ=1000 CTL=SYN,ACK"]);
}

#[test]
fn s_hs_025_new_syn_in_syn_received() {
    let mut h = child();
    nothing(&h.input(1, seg(5100).syn()));
    assert_eq!(accept(&mut h), None);
    assert_eq!(h.tcp.next_deadline(), None, "the child and its timer are gone");
    let outs = h.input(2, seg(5100).syn());
    expect(&outs, &["CTL=SYN,ACK ACK=5101"]);
}

#[test]
fn s_hs_026_child_bad_ack() {
    let mut h = child();
    expect(&h.input(1, seg(5001).ack(1002)), &["SEQ=1002 ACK=- CTL=RST"]);
    assert_eq!(accept(&mut h), None);
    nothing(&h.input(2, seg(5001).ack(1001)));
    assert!(accept(&mut h).is_some(), "the child stayed in SYN-RECEIVED");
}

#[test]
fn s_hs_027_child_rst() {
    let mut h = child();
    nothing(&h.input(1, seg(5001).rst()));
    assert_eq!(accept(&mut h), None);
    nothing(&h.at(5000));
    expect(&h.input(5001, seg(7000).syn()), &["CTL=SYN,ACK ACK=7001"]);
}

#[test]
fn s_hs_028_active_syn_received_rst() {
    let mut h = simultaneous();
    nothing(&h.input(6, seg(5001).rst()));
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::Refused)));
}

#[test]
fn s_hs_029_child_fin() {
    let mut h = child();
    expect(&h.input(1, seg(5001).ack(1001).fin()), &["SEQ=1001 ACK=5002 CTL=ACK"]);
    h.conn = accept(&mut h);
    assert_eq!(h.info().state, State::CloseWait);
    let (r, _) = h.call(2, |tcp, now, id| tcp.recv(now, id, &mut [0u8; 10]));
    assert_eq!(r, Ok(Received::End));
}

#[test]
fn s_hs_030_fin_waits_for_established() {
    let mut h = simultaneous();
    let (r, outs) = h.call(5, |tcp, now, id| tcp.shutdown_write(now, id));
    assert_eq!(r, Ok(()));
    nothing(&outs);
    let outs = h.input(10, seg(5000).ack(1001).syn().mss(1460));
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=FIN,ACK"]);
    assert_eq!(h.info().state, State::FinWait1);
}

#[test]
fn s_hs_031_child_text_without_ack() {
    let mut h = child();
    nothing(&h.input(1, seg(5001).len(10)));
    assert_eq!(accept(&mut h), None);
}

#[test]
fn s_hs_032_child_inexact_rst() {
    let mut h = child();
    expect(&h.input(1, seg(5500).rst()), &["SEQ=1001 ACK=5001 CTL=ACK"]);
    nothing(&h.input(2, seg(5001).ack(1001)));
    assert!(accept(&mut h).is_some());
}

#[test]
fn s_hs_033_child_completes_with_data() {
    let mut h = child();
    nothing(&h.input(1, seg(5001).ack(1001).len(100)));
    h.conn = accept(&mut h);
    assert_eq!(h.info().unread, 100);
    nothing(&h.at(40));
    expect(&h.at(41), &["SEQ=1001 ACK=5101"]);
}

#[test]
fn s_hs_034_synack_give_up() {
    let mut h = child();
    let outs = h.at(62_999);
    let times: Vec<i64> = outs.iter().map(|o| o.t).collect();
    assert_eq!(times, [1000, 3000, 7000, 15_000, 31_000]);
    nothing(&h.at(63_000));
    assert_eq!(h.count(Counter::SynAckGiveUp), 1);
    assert!(h.tcp.next_deadline().is_none());
}

#[test]
fn s_hs_035_connect_needs_a_unicast_remote() {
    let mut h = H::new(65_535);
    for addr in [Ipv4Addr::BROADCAST, Ipv4Addr::new(224, 0, 0, 1), Ipv4Addr::UNSPECIFIED] {
        assert_eq!(h.tcp.connect(h.now(), A, None, ep(addr, 80)), Err(Error::InvalidRemote));
    }
    nothing(&h.transmit());
}

#[test]
fn s_hs_037_the_initial_window_is_ten_smss() {
    let mut h = client(65_535, seg(5000).ack(1001).syn().mss(1400));
    assert_eq!(h.info().smss, 1400);
    let outs = h.send(1, 20_000);
    assert_eq!(outs.len(), 10);
    assert!(outs.iter().all(|o| o.payload.len() == 1400));
}

#[test]
fn s_hs_038_data_queued_in_syn_sent() {
    let mut h = connecting(0);
    nothing(&h.send(0, 100));
    let outs = h.input(10, seg(5000).ack(1001).syn().mss(1460));
    expect(&outs, &["SEQ=1001 ACK=5001 CTL=PSH,ACK LEN=100"]);
}

#[test]
fn s_hs_039_active_syn_received_bad_ack() {
    let mut h = simultaneous();
    expect(&h.input(6, seg(5000).ack(1005).syn()), &["SEQ=1005 CTL=RST"]);
    assert_eq!(h.status().state, State::SynReceived);
}

#[test]
fn s_hs_040_synack_retransmitted() {
    let mut h = child();
    expect(&h.at(1000), &["CTL=SYN,ACK"]);
    h.input(1020, seg(5001).ack(1001));
    h.conn = accept(&mut h);
    let info = h.info();
    assert_eq!(info.srtt, None);
    assert_eq!(info.rto, ms(3000));
}

#[test]
fn s_hs_041_no_socket() {
    let mut h = H::new(65_535);
    let to = (A, 81);
    expect(&h.input(0, seg(100).syn().to(to.0, to.1)), &["SEQ=0 ACK=101 CTL=RST,ACK"]);
    expect(&h.input(1, seg(100).ack(200).len(5).to(to.0, to.1)), &["SEQ=200 ACK=- CTL=RST"]);
    expect(&h.input(2, seg(100).len(5).to(to.0, to.1)), &["SEQ=0 ACK=105 CTL=RST,ACK"]);
    nothing(&h.input(3, seg(100).rst().to(to.0, to.1)));
}

#[test]
fn s_hs_042_no_socket_reset_limit() {
    let mut h = H::new(65_535);
    h.reset_budget = false;
    nothing(&h.input(0, seg(100).syn().to(A, 81)));
    assert_eq!(h.count(Counter::ClosedRstLimited), 1);
}

#[test]
fn s_hs_043_calls_in_the_wrong_state() {
    let mut h = connecting(0);
    let (r, outs) = h.call(1, |tcp, now, id| tcp.shutdown_write(now, id));
    assert_eq!(r, Err(Error::NotConnected));
    nothing(&outs);
    let (r, outs) = h.call(2, |tcp, now, id| tcp.recv(now, id, &mut [0u8; 4]));
    assert_eq!(r, Err(Error::WouldBlock));
    nothing(&outs);
    let (r, outs) = h.call(3, |tcp, now, id| tcp.abort(now, id));
    assert_eq!(r, Ok(()));
    nothing(&outs);
    assert_eq!(h.tcp.status(h.id()), Err(Error::NoSuchSocket));
    nothing(&h.at(10_000));
    let mut h = fixture_e();
    expect(&h.call(1, |tcp, now, id| tcp.shutdown_write(now, id)).1, &["CTL=FIN,ACK"]);
    let (r, outs) = h.call(2, |tcp, now, id| tcp.shutdown_write(now, id));
    assert_eq!(r, Ok(()));
    nothing(&outs);
    let (r, _) = h.call(3, |tcp, now, id| tcp.send(now, id, b"x"));
    assert_eq!(r, Err(Error::Closing));
}
