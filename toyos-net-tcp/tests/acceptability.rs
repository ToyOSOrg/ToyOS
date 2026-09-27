//! Acceptability, RST, SYN and ACK in the synchronized states (tcp.md §18.7). From E unless
//! stated.

mod common;

use common::*;
use toyos_net_tcp::{Counter, Error, Failure, State};

/// B fills A's whole window with bytes the user does not read: RCV.NXT 70,536, window 0.
fn zero_window() -> H {
    let mut h = fixture_e();
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    assert_eq!(h.info().rcv_nxt.get(), 70_536);
    assert_eq!(h.info().rcv_edge.get(), 70_536);
    h
}

fn reset(h: &mut H) {
    let status = h.status();
    assert_eq!((status.state, status.failure), (State::Closed, Some(Failure::Reset)));
    let (r, _) = h.call(h.t, |tcp, now, id| tcp.recv(now, id, &mut [0u8; 8]));
    assert_eq!(r, Err(Error::Failed(Failure::Reset)));
}

#[test]
fn s_ac_001_in_order_text_is_acknowledged_late() {
    let mut h = fixture_e();
    nothing(&h.input(0, seg(5001).ack(1001).len(100)));
    nothing(&h.at(39));
    expect(&h.at(40), &["SEQ=1001 ACK=5101 WND=65435"]);
}

#[test]
fn s_ac_002_at_the_right_edge() {
    let mut h = fixture_e();
    expect(&h.input(0, seg(70_536).ack(1001).len(10)), &["SEQ=1001 ACK=5001 WND=65535"]);
    nothing(&h.input(100, seg(70_536).ack(1001).len(10)));
    assert_eq!(h.count(Counter::UnsolicitedAckLimited), 1);
    expect(&h.input(600, seg(70_536).ack(1001).len(10)), &["ACK=5001"]);
}

#[test]
fn s_ac_003_zero_window_probe() {
    let mut h = zero_window();
    expect(&h.input(10, seg(70_536).ack(1001).len(1)), &["SEQ=1001 ACK=70536 WND=0"]);
    expect(&h.input(20, seg(70_536).ack(1001).len(1)), &["SEQ=1001 ACK=70536 WND=0"]);
}

#[test]
fn s_ac_004_zero_window_pure_ack() {
    let mut h = zero_window();
    nothing(&h.input(10, seg(70_536).ack(1001).wnd(30_000)));
    assert_eq!(h.info().snd_wnd, 30_000);
}

#[test]
fn s_ac_005_zero_window_rst() {
    let mut h = zero_window();
    nothing(&h.input(10, seg(70_536).rst()));
    reset(&mut h);
}

#[test]
fn s_ac_006_empty_segment_beyond_the_window() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(80_000).ack(1001)), &["SEQ=1001 ACK=5001"]);
}

#[test]
fn s_ac_007_front_trim() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(4951).ack(1001).len(100)));
    assert_eq!(h.info().rcv_nxt.get(), 5051);
    let bytes = h.read(100);
    assert_eq!(bytes, (5001..5051u32).map(pattern).collect::<Vec<_>>());
    expect(&h.at(41), &["ACK=5051"]);
}

#[test]
fn s_ac_008_back_trim() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(70_500).ack(1001).fin().len(100)), &["ACK=5001 CTL=ACK"]);
    assert_eq!(h.info().ooo_ranges, 1);
    h.arrive(seg(5001).ack(1001).len(40_000));
    h.input(2, seg(45_001).ack(1001).len(25_499));
    assert_eq!(h.info().rcv_nxt.get(), 70_536, "70,500 to 70,535 were kept; the FIN was not");
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ac_009_exact_rst() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(5001).rst()));
    reset(&mut h);
}

#[test]
fn s_ac_010_inexact_rst_is_challenged() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5002).rst()), &["SEQ=1001 ACK=5001 CTL=ACK"]);
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.count(Counter::RstChallenged), 1);
    nothing(&h.input(2, seg(5001).rst()));
    reset(&mut h);
}

#[test]
fn s_ac_011_rst_outside_the_window() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(70_536).rst()));
    nothing(&h.input(2, seg(5000).rst()));
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ac_012_challenge_acks_are_limited() {
    let mut h = fixture_e();
    expect(&h.input(0, seg(5002).rst()), &["CTL=ACK"]);
    nothing(&h.input(100, seg(5003).rst()));
    expect(&h.input(600, seg(5004).rst()), &["CTL=ACK"]);
}

#[test]
fn s_ac_013_syn_is_challenged() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5001).syn().wnd(65_535)), &["SEQ=1001 ACK=5001 CTL=ACK"]);
    assert_eq!(h.info().state, State::Established);
    assert_eq!(h.count(Counter::SynChallenged), 1);
    nothing(&h.input(2, seg(5001).rst()));
    reset(&mut h);
}

#[test]
fn s_ac_014_syn_beyond_the_window() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(90_000).syn()), &["ACK=5001 CTL=ACK"]);
    assert_eq!(h.info().state, State::Established);
}

#[test]
fn s_ac_015_retransmitted_synack() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5000).ack(1001).syn().mss(1460)), &["SEQ=1001 ACK=5001"]);
    expect(&h.input(11, seg(5000).ack(1001).syn().mss(1460)), &["SEQ=1001 ACK=5001"]);
}

#[test]
fn s_ac_016_ack_of_unsent_data() {
    let mut h = fixture_e();
    h.send(0, 100);
    expect(&h.input(1, seg(5001).ack(1200).len(50)), &["SEQ=1101 ACK=5001"]);
    let info = h.info();
    assert_eq!((info.rcv_nxt.get(), info.snd_una.get()), (5001, 1001));
    assert_eq!(h.count(Counter::AckOutOfRange), 1);
}

#[test]
fn s_ac_017_rfc5961_lower_bound() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5001).ack(4_294_902_761).len(10)), &["ACK=5001"]);
    assert_eq!(h.info().rcv_nxt.get(), 5001);
    h.input(2, seg(5001).ack(4_294_902_762).len(10));
    let info = h.info();
    assert_eq!((info.rcv_nxt.get(), info.snd_una.get()), (5011, 1001));
}

#[test]
fn s_ac_018_data_with_the_old_ack_is_no_duplicate() {
    let mut h = fixture_e();
    ten_out(&mut h);
    for t in 1..=3 {
        let outs = h.input(t, seg(5001 + 10 * (t as u32 - 1)).ack(1001).len(10));
        assert!(outs.iter().all(|o| o.payload.is_empty()), "no fast retransmit");
    }
    assert_eq!(h.info().rcv_nxt.get(), 5031);
    assert_eq!(h.count(Counter::FastRecovery), 0);
}

#[test]
fn s_ac_019_text_without_ack() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(5001).len(10)));
    assert_eq!(h.info().rcv_nxt.get(), 5001);
}

#[test]
fn s_ac_020_old_segments_do_not_update_the_window() {
    let mut h = fixture_e();
    h.input(1, seg(5101).ack(1001).wnd(40_000).len(100));
    let info = h.info();
    assert_eq!((info.snd_wnd, info.snd_wl1.get()), (40_000, 5101));
    h.input(2, seg(5001).ack(1001).wnd(30_000).len(100));
    let info = h.info();
    assert_eq!(info.rcv_nxt.get(), 5201);
    assert_eq!(info.snd_wnd, 40_000);
}

#[test]
fn s_ac_021_a_window_may_shrink() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).wnd(20_000));
    assert_eq!(h.info().snd_wnd, 20_000);
}

#[test]
fn s_ac_022_max_snd_wnd() {
    let mut h = client(65_535, seg(5000).ack(1001).syn().wnd(8000).mss(1460));
    assert_eq!(h.info().max_snd_wnd, 8000);
    for (t, w) in [(1, 10_000), (2, 30_000), (3, 5_000)] {
        h.input(t, seg(5001).ack(1001).wnd(w));
    }
    assert_eq!(h.info().max_snd_wnd, 30_000);
}

#[test]
fn s_ac_023_one_ack_per_receive_pass() {
    let mut h = fixture_e();
    h.at(1);
    h.arrive(seg(5001).ack(1001).len(100));
    h.arrive(seg(5101).ack(1001).len(100));
    expect(&h.transmit(), &["ACK=5201"]);
}

#[test]
fn s_ac_024_text_after_the_peer_fin() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).fin());
    assert_eq!(h.info().state, State::CloseWait);
    nothing(&h.input(2, seg(5002).ack(1001).len(10)));
    assert_eq!(h.count(Counter::DataAfterFin), 1);
}

#[test]
fn s_ac_025_ack_half_the_space_away() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5001).ack(2_147_484_649)), &["ACK=5001"]);
    assert_eq!(h.info().snd_una.get(), 1001);
}

#[test]
fn s_ac_026_rst_with_fin() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(5001).rst().fin()));
    reset(&mut h);
}

#[test]
fn s_ac_027_rst_payload_is_ignored() {
    let mut h = fixture_e();
    nothing(&h.input(1, seg(5001).rst().len(20)));
    reset(&mut h);
}
