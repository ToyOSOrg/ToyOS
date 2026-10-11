//! Loss recovery: NewReno on E, RFC 6675 on EF. LR-15's 64-range bound lives
//! beside the scoreboard in `src/tx.rs`.

mod common;

use common::*;
use toyos_net_tcp::{Counter, Seq};

/// B's duplicate ACK in E.
fn dup() -> S {
    seg(5001).ack(1001).wnd(65_535)
}

fn data_only(outs: &[O]) -> Vec<O> {
    outs.iter().filter(|o| !o.payload.is_empty()).cloned().collect()
}

#[test]
fn s_lr_001_newreno_one_loss() {
    let mut h = fixture_e();
    ten_out(&mut h);
    nothing(&h.input(20, dup()));
    nothing(&h.input(21, dup()));
    expect(&h.input(22, dup()), &["SEQ=1001 LEN=1460"]);
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd), (10_220, 14_600));
    assert_eq!(h.count(Counter::FastRecovery), 1);
    for t in 23..29 {
        nothing(&h.input(t, dup()));
    }
    assert_eq!(h.info().cwnd, 23_360);
    nothing(&h.input(40, seg(5001).ack(15_601)));
    let info = h.info();
    assert_eq!(info.cwnd, 2920);
    assert!(!info.in_recovery);
    assert_eq!(info.rtx_timer, None);
}

/// LR-02's opening: 30,000 bytes written, Limited Transmit, then the fast retransmit.
fn limited_transmit() -> H {
    let mut h = fixture_e();
    assert_eq!(h.send(0, 30_000).len(), 10);
    expect(&h.input(20, dup()), &["SEQ=15601 LEN=1460"]);
    expect(&h.input(21, dup()), &["SEQ=17061 LEN=1460"]);
    expect(&h.input(22, dup()), &["SEQ=1001 LEN=1460"]);
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd), (10_220, 14_600));
    assert_eq!(h.count(Counter::LimitedTransmit), 2);
    h
}

#[test]
fn s_lr_002_limited_transmit_and_inflation() {
    let mut h = limited_transmit();
    nothing(&h.input(23, dup()));
    nothing(&h.input(24, dup()));
    expect(&h.input(25, dup()), &["SEQ=18521"]);
    assert_eq!(h.info().cwnd, 18_980);
    expect(&h.input(26, dup()), &["SEQ=19981"]);
    assert_eq!(h.info().cwnd, 20_440);
}

#[test]
fn s_lr_003_newreno_partial_ack() {
    let mut h = fixture_e();
    ten_out(&mut h);
    for t in 20..22 {
        h.input(t, dup());
    }
    expect(&h.input(22, dup()), &["SEQ=1001"]);
    for t in 23..28 {
        h.input(t, dup());
    }
    assert_eq!(h.info().cwnd, 21_900);
    expect(&h.input(40, seg(5001).ack(3921)), &["SEQ=3921 LEN=1460"]);
    let info = h.info();
    assert_eq!(info.cwnd, 20_440);
    assert_eq!(info.rtx_timer, Some(h.instant(40 + info.rto.as_millis() as i64)));
    h.input(50, seg(5001).ack(15_601));
    assert_eq!(h.info().cwnd, 2920);
}

#[test]
fn s_lr_004_full_ack_after_limited_transmit() {
    let mut h = limited_transmit();
    for t in 23..27 {
        h.input(t, dup());
    }
    assert_eq!(h.info().snd_nxt.get(), 21_441);
    h.input(40, seg(5001).ack(15_601));
    assert_eq!(h.info().cwnd, 7300);
}

#[test]
fn s_lr_005_what_is_not_a_duplicate() {
    let mut h = fixture_e();
    ten_out(&mut h);
    h.input(20, seg(5001).ack(1001).len(10));
    h.input(21, seg(5011).ack(1001).wnd(60_000));
    h.input(22, seg(5011).ack(1001).wnd(65_535));
    nothing(&data_only(&h.input(23, seg(5011).ack(1001))));
    nothing(&data_only(&h.input(24, seg(5011).ack(1001))));
    expect(&data_only(&h.input(25, seg(5011).ack(1001))), &["SEQ=1001"]);
    let mut h = fixture_e();
    ten_out(&mut h);
    h.input(20, dup());
    h.input(21, dup());
    nothing(&data_only(&h.input(22, dup().fin())));
    assert_eq!(h.count(Counter::FastRecovery), 0);
}

#[test]
fn s_lr_006_no_fast_retransmit_after_a_timeout() {
    let mut h = fixture_e();
    ten_out(&mut h);
    expect(&h.at(200), &["SEQ=1001"]);
    for t in 201..204 {
        nothing(&h.input(t, dup()));
    }
    assert_eq!(h.count(Counter::FastRecovery), 0);
}

#[test]
fn s_lr_007_inflation_is_capped() {
    let mut h = fixture_e();
    ten_out(&mut h);
    for t in 20..50 {
        h.input(t, dup());
    }
    assert_eq!(h.info().cwnd, 14_600 + 10 * 1460);
}

/// EF with 1001 lost: SACKs for 2449 onward, one segment per ACK.
fn sack_dup(h: &mut H, t: i64, blocks: &[(u32, u32)]) -> Vec<O> {
    h.input_full(t, seg(5001).ack(1001).sack(blocks))
}

fn sack_recovery() -> H {
    let mut h = fixture_ef();
    ten_out(&mut h);
    nothing(&sack_dup(&mut h, 20, &[(2449, 3897)]));
    nothing(&sack_dup(&mut h, 21, &[(2449, 5345)]));
    expect(&sack_dup(&mut h, 22, &[(2449, 6793)]), &["SEQ=1001 LEN=1448"]);
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd), (10_136, 10_136));
    assert!(info.in_recovery);
    assert_eq!(h.count(Counter::SackRecovery), 1);
    h
}

#[test]
fn s_lr_008_sack_recovery() {
    let mut h = sack_recovery();
    let info = h.info();
    assert_eq!((info.high_rxt, info.rescue_rxt, info.pipe), (Some(Seq::new(2449)), Some(Seq::new(2449)), 10_136));
    for (t, end) in [(23, 8241), (24, 9689), (25, 11_137), (26, 12_585), (27, 14_033), (28, 15_481)] {
        nothing(&sack_dup(&mut h, t, &[(2449, end)]));
    }
    h.input_full(40, seg(5001).ack(15_481));
    let info = h.info();
    assert!(!info.in_recovery);
    assert_eq!(info.cwnd, 10_136);
}

#[test]
fn s_lr_009_is_lost_on_the_first_ack() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&sack_dup(&mut h, 20, &[(2449, 6793)]), &["SEQ=1001"]);
}

/// Two holes: 5345 goes out as soon as NextSeg names it. RFC 6675 §4's rule 3 names a hole below
/// the highest SACKed byte even before IsLost holds for it, so that happens on the fourth ACK;
/// the `mutate-sack-ignored` control reds this.
#[test]
fn s_lr_010_a_second_hole() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    nothing(&sack_dup(&mut h, 20, &[(2449, 3897)]));
    nothing(&sack_dup(&mut h, 21, &[(2449, 5345)]));
    expect(&sack_dup(&mut h, 22, &[(6793, 8241), (2449, 5345)]), &["SEQ=1001"]);
    expect(&sack_dup(&mut h, 23, &[(6793, 9689), (2449, 5345)]), &["SEQ=5345 LEN=1448"]);
    nothing(&sack_dup(&mut h, 24, &[(6793, 11_137), (2449, 5345)]));
    assert_eq!(h.log.iter().filter(|o| o.seq == 5345 && !o.payload.is_empty()).count(), 2, "sent once, resent once");
}

/// LR-08 with 30,000 queued. The first two duplicates already sent new data by Limited Transmit
/// (LR-22), which ssthresh excludes; once SACKs bring pipe a segment below cwnd, each frees one
/// new segment, in order.
#[test]
fn s_lr_011_new_data_during_sack_recovery() {
    let mut h = fixture_ef();
    assert_eq!(h.send(0, 30_000).len(), 10);
    expect(&sack_dup(&mut h, 20, &[(2449, 3897)]), &["SEQ=15481"]);
    expect(&sack_dup(&mut h, 21, &[(2449, 5345)]), &["SEQ=16929"]);
    expect(&sack_dup(&mut h, 22, &[(2449, 6793)]), &["SEQ=1001"]);
    assert_eq!(h.info().ssthresh, 10_136);
    let mut next = h.info().snd_nxt.get();
    let mut sent = 0;
    for (t, end) in [(23, 8241), (24, 9689), (25, 11_137), (26, 12_585), (27, 14_033), (28, 15_481)] {
        let outs = sack_dup(&mut h, t, &[(2449, end)]);
        assert!(outs.len() <= 1, "{outs:?}");
        for o in outs {
            check(&o, &format!("SEQ={next} LEN=1448"));
            next += 1448;
            sent += 1;
        }
    }
    assert_eq!(sent, 4);
}

#[test]
fn s_lr_012_rescue_retransmission() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    sack_dup(&mut h, 20, &[(2449, 3897)]);
    sack_dup(&mut h, 21, &[(2449, 5345)]);
    expect(&sack_dup(&mut h, 22, &[(2449, 6793)]), &["SEQ=1001"]);
    for (t, end) in [(23, 8241), (24, 9689), (25, 11_137), (26, 12_585), (27, 14_033)] {
        nothing(&sack_dup(&mut h, t, &[(2449, end)]));
    }
    expect(&h.input_full(30, seg(5001).ack(14_033)), &["SEQ=14033 LEN=1448"]);
    nothing(&h.input_full(31, seg(5001).ack(14_033)));
}

#[test]
fn s_lr_013_the_recovery_point() {
    let mut h = sack_recovery();
    h.input_full(30, seg(5001).ack(6793));
    assert!(h.info().in_recovery);
    h.input_full(31, seg(5001).ack(15_481));
    assert!(!h.info().in_recovery);
}

#[test]
fn s_lr_014_invalid_blocks_and_dsack() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    sack_dup(&mut h, 20, &[(15_481, 16_929), (2449, 2449)]);
    assert_eq!(h.count(Counter::SackBlockInvalid), 2);
    assert_eq!(h.info().sacked_ranges, 0);
    sack_dup(&mut h, 21, &[(1, 1001)]);
    assert_eq!(h.count(Counter::DsackRcvd), 1);
    assert_eq!(h.info().sacked_ranges, 0);
    sack_dup(&mut h, 22, &[(2449, 3897)]);
    assert_eq!(h.info().sacked_ranges, 1);
    assert!(!h.info().in_recovery, "one duplicate, not three");
}

#[test]
fn s_lr_016_timeout() {
    let mut h = fixture_e();
    ten_out(&mut h);
    expect(&h.at(200), &["SEQ=1001 LEN=1460"]);
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd, info.rto), (10_220, 1460, ms(400)));
    expect(&h.input(210, seg(5001).ack(2461)), &["SEQ=2461 LEN=1460", "SEQ=3921 LEN=1460"]);
    assert_eq!(h.info().cwnd, 2920);
}

#[test]
fn s_lr_017_a_second_timeout() {
    let mut h = fixture_e();
    ten_out(&mut h);
    h.at(200);
    expect(&h.at(600), &["SEQ=1001 LEN=1460"]);
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd, info.rto), (10_220, 1460, ms(800)));
}

/// EF's ten segments meet silence: two round trips and the slack on, a loss probe sends the last
/// one again (RFC 8985 §7.3), and the RTO runs from it.
fn probed_then_expired(h: &mut H) {
    ten_out(h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    assert_eq!(h.count(Counter::LossProbe), 1);
    nothing(&h.at(221));
    expect(&h.at(222), &["SEQ=1001"]);
}

#[test]
fn s_lr_018_no_sack_recovery_before_the_timeout_point() {
    let mut h = fixture_ef();
    probed_then_expired(&mut h);
    for (t, end) in [(232, 3897), (233, 5345), (234, 6793)] {
        sack_dup(&mut h, t, &[(2449, end)]);
    }
    assert!(!h.info().in_recovery);
    assert_eq!(h.count(Counter::SackRecovery), 0);
}

#[test]
fn s_lr_019_sacks_after_a_timeout_skip_held_ranges() {
    let mut h = fixture_ef();
    probed_then_expired(&mut h);
    let outs = h.input_full(232, seg(5001).ack(2449).sack(&[(1001, 2449), (3897, 15_481)]));
    expect(&outs, &["SEQ=2449 LEN=1448"]);
    assert_eq!(h.info().cwnd, 2896);
    assert_eq!(h.count(Counter::DsackRcvd), 1);
}

#[test]
fn s_lr_020_sacked_bytes_stay_queued() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    sack_dup(&mut h, 20, &[(2449, 6793)]);
    assert_eq!(h.info().queued, 14_480);
    h.input_full(30, seg(5001).ack(2449).sack(&[(2449, 6793)]));
    assert_eq!(h.info().queued, 14_480 - 1448);
    h.input_full(31, seg(5001).ack(6793));
    assert_eq!(h.info().queued, 14_480 - 5792);
}

#[test]
fn s_lr_021_pure_duplicates_without_blocks() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    for t in 20..23 {
        nothing(&h.input_full(t, seg(5001).ack(1001)));
    }
    assert!(!h.info().in_recovery);
    expect(&h.at(200), &["SEQ=1001"]);
}

#[test]
fn s_lr_022_limited_transmit_by_pipe() {
    let mut h = fixture_ef();
    assert_eq!(h.send(0, 30_000).len(), 10);
    expect(&sack_dup(&mut h, 20, &[(2449, 3897)]), &["SEQ=15481 LEN=1448"]);
}

#[test]
fn s_lr_023_a_lost_fast_retransmission() {
    let mut h = limited_transmit();
    let flight = h.info().snd_nxt.get() - h.info().snd_una.get();
    let rto = h.info().rto.as_millis() as i64;
    expect(&h.at(22 + rto), &["SEQ=1001"]);
    let info = h.info();
    assert_eq!(info.ssthresh, flight * 7 / 10);
    assert_eq!(info.cwnd, 1460);
}

/// RFC 8985 §7.3: with data queued past cwnd and the peer's window open, the probe is a segment
/// of new data, outside cwnd; its ACK infers no loss.
#[test]
fn rfc_8985_7_3_the_probe_is_new_data_where_the_window_takes_a_segment() {
    let mut h = fixture_ef();
    assert_eq!(h.send(0, 30_000).len(), 10);
    nothing(&h.at(21));
    expect(&h.at(22), &["SEQ=15481 LEN=1448"]);
    let cwnd = h.info().cwnd;
    h.input_full(30, seg(5001).ack(16_929));
    assert_eq!((h.count(Counter::LossProbe), h.count(Counter::LossProbeRecovery), h.count(Counter::RetransmitBytes)), (1, 0, 0));
    assert!(h.info().cwnd >= cwnd);
}

/// RFC 8985 §7.4.2: the probe sent the last segment again. The ACK that reaches its end without
/// a D-SACK leaves the episode open, since the original's ACK reads the same; the ACK past the
/// end with none says one copy was lost: cwnd is reduced as for a loss, once.
#[test]
fn rfc_8985_7_4_a_resent_probe_acknowledged_past_its_end_without_a_dsack_repaired_a_loss() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    nothing(&h.send(25, 1448));
    expect(&h.input_full(30, seg(5001).ack(15_481)), &["SEQ=15481 LEN=1448"]);
    assert_eq!(h.count(Counter::LossProbeRecovery), 0);
    h.input_full(40, seg(5001).ack(16_929));
    let info = h.info();
    assert_eq!((info.ssthresh, info.cwnd, info.in_recovery), (2896, 2896, false));
    assert_eq!(h.count(Counter::LossProbeRecovery), 1);
}

/// RFC 8985 §7.4.2: the ACK that reaches the resent probe's end carries no D-SACK, and the next
/// reports the probe as a duplicate: both copies arrived, nothing was lost, and cwnd stands through
/// the ACK of what is sent next.
#[test]
fn rfc_8985_7_4_a_dsack_after_the_ack_at_the_probes_end_infers_no_loss() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    h.input_full(30, seg(5001).ack(15_481));
    let cwnd = h.info().cwnd;
    h.input_full(31, seg(5001).ack(15_481).sack(&[(14_033, 15_481)]));
    nothing(&h.send(32, 1448).into_iter().filter(|o| o.payload.is_empty()).collect::<Vec<_>>());
    h.input_full(40, seg(5001).ack(16_929));
    assert_eq!((h.info().cwnd >= cwnd, h.count(Counter::LossProbeRecovery), h.count(Counter::DsackRcvd)), (true, 0, 1));
}

/// RFC 8985 §7.4: the same, but the ACK reports the probe's segment as a duplicate: nothing was
/// lost, and cwnd stands.
#[test]
fn rfc_8985_7_4_a_resent_probe_reported_as_a_duplicate_infers_no_loss() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    let cwnd = h.info().cwnd;
    h.input_full(30, seg(5001).ack(15_481).sack(&[(14_033, 15_481)]));
    assert_eq!((h.info().cwnd >= cwnd, h.count(Counter::LossProbeRecovery), h.count(Counter::DsackRcvd)), (true, 0, 1));
}

/// RFC 8985 §7.2: with one segment out, the probe waits WCDelAckT past two round trips, which is
/// past the RTO here, so it goes at the RTO's time in the RTO's place, and the RTO runs from it.
#[test]
fn rfc_8985_7_2_with_one_segment_out_the_probe_stands_in_for_the_first_rto() {
    let mut h = fixture_ef();
    h.send(0, 1448);
    let rto = h.info().rto.as_millis() as i64;
    nothing(&h.at(rto - 1));
    expect(&h.at(rto), &["SEQ=1001 LEN=1448"]);
    assert_eq!((h.count(Counter::LossProbe), h.count(Counter::Rto)), (1, 0));
    nothing(&h.at(2 * rto - 1));
    expect(&h.at(2 * rto), &["SEQ=1001 LEN=1448"]);
    assert_eq!(h.count(Counter::Rto), 1);
}

/// RFC 8985 §7.2: no probe is scheduled in RTO recovery. After the probe at 22 ms and the RTO at
/// 222 ms, a cumulative ACK without SACK moves SND.UNA while go-back-N has the rest still to send:
/// no second probe follows it, however long the next ACK takes.
#[test]
fn rfc_8985_7_2_no_probe_in_rto_recovery() {
    let mut h = fixture_ef();
    probed_then_expired(&mut h);
    h.input_full(232, seg(5001).ack(2449));
    let pto = 2 * h.info().srtt.unwrap().as_millis() as i64 + 2;
    let rto = h.info().rto.as_millis() as i64;
    h.at(232 + pto + 1);
    h.at(232 + rto - 1);
    assert_eq!((h.count(Counter::LossProbe), h.count(Counter::Rto)), (1, 1));
}

/// RFC 8985 §7.1: entering fast recovery ends the probe's episode. The resent probe is still
/// outstanding when SACK recovery begins, and that recovery's own reduction is the only one: the
/// ACK at the probe's end and the one past it reduce nothing more.
#[test]
fn rfc_8985_7_1_fast_recovery_ends_the_probes_episode() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    nothing(&sack_dup(&mut h, 30, &[(2449, 3897)]));
    nothing(&sack_dup(&mut h, 31, &[(2449, 5345)]));
    expect(&sack_dup(&mut h, 32, &[(2449, 6793)]), &["SEQ=1001 LEN=1448"]);
    let ssthresh = h.info().ssthresh;
    h.input_full(40, seg(5001).ack(15_481));
    assert!(!h.info().in_recovery);
    expect(&h.send(41, 1448), &["SEQ=15481 LEN=1448"]);
    h.input_full(50, seg(5001).ack(16_929));
    assert_eq!((h.info().ssthresh, h.count(Counter::SackRecovery), h.count(Counter::LossProbeRecovery)), (ssthresh, 1, 0));
}

/// A probe that came due while the next hop was not ready has not left when an ACK shuts the
/// window: persist takes over, and the last segment is not sent again into the shut window.
#[test]
fn a_probe_due_when_the_window_shuts_gives_way_to_persist() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    h.hop = Box::new(|t, _| if t < 100 { toyos_net_tcp::Hop::Pending } else { toyos_net_tcp::Hop::Ready(()) });
    nothing(&h.at(22));
    let latest = h.log.iter().rev().find_map(|o| o.ts.map(|(v, _)| v)).unwrap();
    nothing(&h.at(30));
    h.deliver(seg(5001).ack(1001).wnd(0).ts(50_030, latest));
    assert_eq!(h.info().snd_wnd, 0);
    h.tcp.wake(B);
    nothing(&h.at(100));
    assert_eq!(h.count(Counter::LossProbe), 0);
}

/// RFC 8985 §7.4.2: only a D-SACK matching the probe's end says the probe was a duplicate. The ACK
/// past the end reports an older segment as a duplicate, not the probe: one copy of the probe's
/// segment was still lost, and cwnd is reduced.
#[test]
fn rfc_8985_7_4_a_dsack_of_another_segment_still_infers_the_loss() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    nothing(&h.send(25, 1448));
    expect(&h.input_full(30, seg(5001).ack(15_481)), &["SEQ=15481 LEN=1448"]);
    h.input_full(40, seg(5001).ack(16_929).sack(&[(1001, 2449)]));
    assert_eq!((h.count(Counter::DsackRcvd), h.count(Counter::LossProbeRecovery), h.info().ssthresh), (1, 1, 2896));
}

/// RFC 8985 §7.4.2 and §7.1 on one ACK: it passes the resent probe's end and SACKs three
/// segments above a hole, so it both infers the probe's loss and enters SACK recovery, and cwnd is
/// cut twice: the probe's cut, then recovery's from the flight after the ACK. Linux v6.12 does the
/// same: `tcp_process_tlp_ack` reduces and leaves CWR through `tcp_try_keep_open`, so
/// `tcp_enter_recovery` finds no reduction in progress and reduces again.
#[test]
fn rfc_8985_7_4_an_ack_that_infers_the_probes_loss_and_enters_recovery_cuts_twice() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    nothing(&h.send(25, 5 * 1448));
    assert_eq!(h.input_full(30, seg(5001).ack(15_481)).len(), 5);
    h.input_full(40, seg(5001).ack(16_929).sack(&[(18_377, 22_721)]));
    let info = h.info();
    assert!(info.in_recovery);
    assert_eq!((h.count(Counter::LossProbeRecovery), h.count(Counter::SackRecovery)), (1, 1));
    assert_eq!((info.ssthresh, info.cwnd), (4 * 1448 * 7 / 10, 4 * 1448 * 7 / 10));
}

/// RFC 8985 §7.4.2, Case 2: after the ACK at the resent probe's end, a duplicate ACK without SACK
/// says both copies arrived; the ACK past the end that follows infers nothing.
#[test]
fn rfc_8985_7_4_a_duplicate_without_sack_at_the_probes_end_infers_no_loss() {
    let mut h = fixture_ef();
    ten_out(&mut h);
    expect(&h.at(22), &["SEQ=14033 LEN=1448"]);
    nothing(&h.send(25, 1448));
    expect(&h.input_full(30, seg(5001).ack(15_481)), &["SEQ=15481 LEN=1448"]);
    let cwnd = h.info().cwnd;
    nothing(&h.input_full(31, seg(5001).ack(15_481)));
    h.input_full(40, seg(5001).ack(16_929));
    assert_eq!((h.info().cwnd >= cwnd, h.count(Counter::LossProbeRecovery)), (true, 0));
}
