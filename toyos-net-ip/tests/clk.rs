//! The injected clock, deadlines, keyed draws and the moved clock.

mod common;

use common::*;
use toyos_net_ip::{limits, AddrState, Counter, Instant, Nud};

fn incomplete_b() -> H {
    let mut h = H::fixture_i();
    assert_eq!(h.udp_to(B), Ok(None));
    assert!(h.out()[0].requests(B));
    h
}

#[test]
fn s_ip_clk_001_a_send_arms_the_retransmission() {
    let h = incomplete_b();
    assert!(matches!(h.state(B), Some(Nud::Incomplete(_))));
    assert_eq!(h.ip.next_deadline(), Some(H::instant(1_000)));
}

#[test]
fn s_ip_clk_002_nothing_fires_before_its_deadline() {
    let mut h = incomplete_b();
    assert!(h.fire(999).is_empty());
    let out = h.fire(1_000);
    assert_eq!(out.len(), 1);
    assert!(out[0].requests(B));
}

#[test]
fn s_ip_clk_003_firing_more_often_changes_nothing() {
    let mut every_ms = incomplete_b();
    let mut sent_every_ms = vec![0];
    for t in 1..=3_000 {
        if !every_ms.fire(t).is_empty() {
            sent_every_ms.push(t);
        }
    }
    let mut at_deadlines = incomplete_b();
    let sent_at_deadlines: Vec<u64> = std::iter::once(0).chain(at_deadlines.run(3_000).iter().map(|o| o.at)).collect();
    assert_eq!(sent_every_ms, [0, 1_000, 2_000]);
    assert_eq!(sent_at_deadlines, sent_every_ms);
    for h in [&every_ms, &at_deadlines] {
        assert!(matches!(h.state(B), Some(Nud::Failed)));
        assert_eq!(h.count(Counter::NbFailed), 1);
    }
}

#[test]
fn s_ip_clk_004_a_jump_fires_a_reachable_entry_once() {
    let mut h = H::fixture_i_with(R, MAC_R);
    assert!(h.fire(3_600_000).is_empty());
    assert!(h.is_stale(R));
    assert!(matches!(h.udp_to(REMOTE), Ok(Some(frame)) if destination_of(&frame) == MAC_R));
    assert!(matches!(h.state(R), Some(Nud::Delay(_))));
}

/// R UNREACHABLE after PROBE's three unanswered polls, at the returned time.
fn unreachable_r() -> (H, u64) {
    let mut h = H::fixture_i();
    h.stale(R, MAC_R);
    h.udp_to(REMOTE).unwrap();
    h.run(8_000);
    assert!(matches!(h.state(R), Some(Nud::Unreachable(_))));
    h.rebase();
    (h, 8_000)
}

#[test]
fn s_ip_clk_005_a_jump_owes_one_request() {
    let (mut h, t0) = unreachable_r();
    h.at(t0 + 100);
    h.udp_to(REMOTE).unwrap();
    let first = h.out();
    assert_eq!(first.iter().filter(|o| o.requests(R)).count(), 1);
    h.at(t0 + 200);
    h.udp_to(REMOTE).unwrap();
    let jump = t0 + 200 + 600_000;
    let out = h.fire(jump);
    assert_eq!(out.iter().filter(|o| o.requests(R)).count(), 1, "one request at the jump");
    assert_eq!(h.ip.next_deadline(), Some(H::instant(jump + 9_000)), "RETRANS x 3^2 from the jump");
}

/// The trace of a seeded run: every frame, every event, every counter.
type Trace = (Vec<(u64, Vec<u8>)>, Vec<String>, Vec<u64>, std::time::Duration);

fn trace(secret: [u8; 16], seed: u64) -> Trace {
    let mut ip = toyos_net_ip::Ip::new(Instant::from_nanos(0), secret);
    let if0 = ip.add_interface(Instant::from_nanos(0), mac(MAC_A));
    ip.link_up(Instant::from_nanos(0), if0).unwrap();
    let mut rng = Rng(seed);
    let mut frames = Vec::new();
    let mut events = Vec::new();
    let inputs = [
        hex(V_ARP_REQ_B),
        hex(V_ARP_REPLY_R),
        hex(V_ARP_CONFLICT_B),
        to_a(&hex(V_UDP_HI)),
        to_a(&hex(V_ICMP_ECHO)),
        eth(MAC_A, MAC_R, 0x0800, &hex(V_IGMP3_QUERY_GEN)),
    ];
    let mut now = 0u64;
    ip.add_address(Instant::from_millis(0), if0, A, 24).unwrap();
    for _ in 0..400 {
        now += rng.below(700);
        let at = Instant::from_millis(now);
        match rng.below(6) {
            0 => {
                let input = &inputs[rng.below(inputs.len() as u64) as usize];
                let _ = ip.receive(at, if0, input);
            }
            1 => {
                let destination = std::net::Ipv4Addr::new(192, 0, 2, 2 + rng.below(4) as u8);
                let mut frame = [0u8; toyos_net_ip::FRAME];
                let datagram = toyos_net_wire::udp::UdpBuilder {
                    source: toyos_net_wire::Port::new(5001).unwrap(),
                    destination: toyos_net_wire::Port::new(5001).unwrap(),
                    data: b"x",
                };
                let out = toyos_net_ip::UdpOut { source: A, destination, ttl: toyos_net_wire::ipv4::Ttl::DEFAULT, broadcast: false, datagram };
                if let Ok(toyos_net_ip::Sent::Frame(n)) = ip.send_udp(at, &out, &mut frame) {
                    frames.push((now, frame[..n].to_vec()));
                }
            }
            2 => ip.fire(at),
            3 => {
                let _ = ip.join(at, if0, group(std::net::Ipv4Addr::new(239, 1, 1, rng.below(3) as u8)));
            }
            _ => {
                ip.transmit(at, rng.below(4) as usize, |_, f| frames.push((now, f.to_vec())));
            }
        }
        events.extend(ip.drain_events().map(|e| format!("{e:?}")));
    }
    let counters = Counter::ALL.iter().map(|&c| ip.counters().get(c)).collect();
    (frames, events, counters, ip.reachable_time(if0).unwrap())
}

#[test]
fn s_ip_clk_006_prop_one_secret_one_run() {
    for seed in 1..=24 {
        let first = trace(SECRET, seed);
        let second = trace(SECRET, seed);
        assert_eq!(first.0, second.0, "seed {seed}: frames");
        assert_eq!(first.1, second.1, "seed {seed}: events");
        assert_eq!(first.2, second.2, "seed {seed}: counters");
        let other = trace([0xa5; 16], seed);
        for reachable in [first.3, other.3] {
            assert!((limits::nud::REACHABLE_MIN..=limits::nud::REACHABLE_MAX).contains(&reachable));
        }
    }
    assert_ne!(trace(SECRET, 1).3, trace([0xa5; 16], 1).3, "the secret draws the reachable time");
}

#[test]
fn s_ip_clk_007_a_regressed_clock_is_the_latest() {
    let mut h = incomplete_b();
    h.fire(5_000);
    let deadline = h.ip.next_deadline();
    h.ip.fire(H::instant(4_000));
    assert_eq!(h.ip.transmit(H::instant(4_000), usize::MAX, |_, _| {}), 0);
    assert_eq!(h.count(Counter::ClockRegressed), 2);
    assert_eq!(h.ip.next_deadline(), deadline);
    let mut again = incomplete_b();
    again.fire(5_000);
    assert_eq!(h.fire(5_000).len(), again.fire(5_000).len());
    assert_eq!(format!("{:?}", h.state(B)), format!("{:?}", again.state(B)));
}

#[test]
fn s_ip_clk_008_announce_wait_counts_from_the_hand_off() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    let mut probes = 0;
    let mut third_left = 0;
    while probes < 3 {
        let deadline = h.ip.next_deadline().unwrap();
        let t = deadline.nanos() / 1_000_000 - BASE_MS;
        h.ip.fire(deadline);
        h.now = t;
        let late = if probes == 2 { t + 500 } else { t };
        h.at(late);
        let out = h.out();
        probes += out.iter().filter(|o| o.arp().is_some_and(|a| a.sender_ip.is_unspecified())).count();
        third_left = late;
    }
    let due = h.ip.next_deadline().unwrap();
    assert_eq!(due, H::instant(third_left).after(limits::acd::ANNOUNCE_WAIT));
    h.ip.fire(due);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
}

#[test]
fn s_ip_clk_009_a_jump_refills_a_bucket_to_its_cap() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let error_to_b = |h: &mut H| {
        let arrival = match h.datagram(&hex(V_UDP_HI)) {
            Some(toyos_net_ip::Delivery::Udp(arrival, _)) => arrival,
            other => panic!("{other:?}"),
        };
        h.ip.port_unreachable(h.clock(), &arrival);
    };
    for _ in 0..10 {
        error_to_b(&mut h);
    }
    assert_eq!(h.out().len(), 10);
    h.at(3_600_000);
    for _ in 0..11 {
        error_to_b(&mut h);
    }
    assert_eq!(h.out().iter().filter(|o| o.to() == MAC_B).count(), 10);
    assert_eq!(h.count(Counter::IcmpErrorRateLimited), 1);
}

#[test]
fn s_ip_clk_010_a_deadline_saturates() {
    let mut h = H::fixture_i();
    let late = Instant::from_nanos(u64::MAX - 500);
    let mut frame = [0u8; toyos_net_ip::FRAME];
    let datagram = toyos_net_wire::udp::UdpBuilder {
        source: toyos_net_wire::Port::new(5001).unwrap(),
        destination: toyos_net_wire::Port::new(5001).unwrap(),
        data: b"hi",
    };
    let out = toyos_net_ip::UdpOut { source: A, destination: B, ttl: toyos_net_wire::ipv4::Ttl::DEFAULT, broadcast: false, datagram };
    assert_eq!(h.ip.send_udp(late, &out, &mut frame), Ok(toyos_net_ip::Sent::Held));
    assert_eq!(h.ip.transmit(late, usize::MAX, |_, _| {}), 1);
    assert!(matches!(h.state(B), Some(Nud::Incomplete(_))));
    assert_eq!(h.ip.next_deadline(), Some(Instant::from_nanos(u64::MAX)));
}
