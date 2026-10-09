//! ICMPv4: echo, error generation under the limiter, and inbound errors.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Arrival, Cast, Counter, Delivery, ErrorClass, ErrorKind, Event, Instant, Limiter, Nud, Peer, Transport};
use toyos_net_wire::ethernet::{MacAddr, MacClass};
use toyos_net_wire::icmp::{IcmpError, ParameterProblemCode, TimeExceededCode, UnreachableCode};
use toyos_net_wire::ipv4::Ipv4Packet;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

/// V-ICMP-ECHO from B to A.
fn echo_to_a() -> Vec<u8> {
    edit(hex(V_ICMP_ECHO), |ip| {
        set_source(ip, B);
        set_destination(ip, A);
    })
}

fn icmp_types(out: &[Out]) -> Vec<(u8, u8)> {
    out.iter().filter_map(|o| o.ip()).filter(|ip| ip.protocol() == toyos_net_wire::ipv4::Protocol::Icmp).map(|ip| (ip.payload()[0], ip.payload()[1])).collect()
}

/// The frames that left for a port unreachable about `datagram` from B's MAC.
fn closed_port(h: &mut H, datagram: &[u8]) -> Vec<Out> {
    if let Some(Delivery::Udp(arrival, _)) = h.datagram(datagram) {
        h.ip.port_unreachable(h.clock(), &arrival);
    }
    h.out()
}

fn hi_from(source: Ipv4Addr) -> Vec<u8> {
    edit(hex(V_UDP_HI), |ip| set_source(ip, source))
}

/// Fixture I with B and R both REACHABLE.
fn b_and_r() -> H {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.reach(R, MAC_R);
    h.rebase();
    h
}

#[test]
fn s_ip_echo_001_the_mirror_of_the_request() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram(&echo_to_a());
    let out = h.out();
    assert_eq!(out.len(), 1);
    let mirror = edit(hex(V_ICMP_REPLY), |ip| {
        set_source(ip, A);
        set_destination(ip, B);
    });
    assert_eq!(ip_bytes(&out[0].frame), mirror);
    assert_eq!(out[0].to(), MAC_B);
    assert_eq!(h.count(Counter::IcmpEchoRepliesSent), 1);
}

#[test]
fn s_ip_echo_002_dscp_copied_ecn_cleared() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram(&edit(echo_to_a(), |ip| ip[1] = 46 << 2 | 3));
    let reply = h.out();
    let tc = reply[0].ip().unwrap().traffic_class();
    assert_eq!((tc.dscp(), tc.ecn()), (46, toyos_net_wire::ipv4::Ecn::NotEct));
}

#[test]
fn s_ip_echo_003_an_off_link_requester_is_answered_through_the_gateway() {
    let mut h = H::fixture_i();
    let request = edit(echo_to_a(), |ip| set_source(ip, REMOTE));
    h.frame(&eth(MAC_A, MAC_R, 0x0800, &request));
    let first = h.out();
    assert_eq!(first.len(), 1);
    assert!(first[0].requests(R), "an ARP request for R precedes the reply");
    h.reply_from(R, MAC_R);
    let reply = h.out();
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0].to(), MAC_R);
    assert_eq!(reply[0].ip().unwrap().destination(), REMOTE);
}

#[test]
fn s_ip_echo_004_the_reply_carries_no_options() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut request = vec![0x47, 0, 0, 0, 0, 0, 0x40, 0, 64, 1, 0, 0, 192, 0, 2, 2, 192, 0, 2, 1, 7, 7, 4, 0, 0, 0, 0, 0];
    request.extend_from_slice(&hex(V_ICMP_ECHO)[20..]);
    let total = request.len() as u16;
    request[2..4].copy_from_slice(&total.to_be_bytes());
    fix(&mut request);
    h.datagram(&request);
    let reply = h.out();
    let ip = reply[0].ip().unwrap();
    assert_eq!(ip.header_len(), 20);
    assert_eq!(ip.payload()[4..], hex(V_ICMP_ECHO)[24..]);
}

#[test]
fn s_ip_echo_005_sixteen_replies_wait() {
    let mut h = H::fixture_i_with(B, MAC_B);
    for _ in 0..20 {
        h.datagram(&echo_to_a());
    }
    assert_eq!(h.count(Counter::IcmpEchoReplyDropped), 4);
    assert_eq!(h.out().len(), 16);
}

#[test]
fn s_ip_echo_006_an_echo_reply_is_counted() {
    let mut h = H::fixture_i();
    h.datagram(&hex(V_ICMP_REPLY));
    assert_eq!(h.count(Counter::IcmpEchoReply), 1);
    assert!(h.out().is_empty());
    let others: u64 = Counter::ALL.iter().filter(|&&c| c != Counter::IcmpEchoReply).map(|&c| h.count(c)).sum();
    assert_eq!(others, 0);
}

#[test]
fn s_ip_echo_007_a_tentative_address_does_not_answer() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    h.datagram(&echo_to_a());
    assert_eq!(h.count(Counter::IpTentativeDestination), 1);
    assert!(h.out().iter().all(|o| o.ip().is_none()));
}

#[test]
fn s_ip_echo_008_echo_is_not_rate_limited() {
    let mut h = H::fixture_i_with(B, MAC_B);
    for _ in 0..10 {
        closed_port(&mut h, &hex(V_UDP_HI));
    }
    assert_eq!(closed_port(&mut h, &hex(V_UDP_HI)).len(), 0, "B's bucket is empty");
    h.datagram(&echo_to_a());
    assert_eq!(icmp_types(&h.out()), [(0, 0)]);
}

#[test]
fn s_ip_icg_001_protocol_unreachable() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram(&hex(V_IP_MIN));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(ip_bytes(&out[0].frame), hex(V_ICMP_PROTO_UNREACH_GEN));
}

#[test]
fn s_ip_icg_002_port_unreachable() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let out = closed_port(&mut h, &hex(V_UDP_HI));
    assert_eq!(out.len(), 1);
    assert_eq!(ip_bytes(&out[0].frame), hex(V_ICMP_PORT_UNREACH_GEN));
    assert_eq!(h.count(Counter::IcmpErrorsSent), 1);
}

/// ICG-03's four: to a group, a subnet broadcast, the limited broadcast, and about an ICMP error.
fn suppressed_requests(h: &mut H) {
    for (frame_to, destination) in [(MacAddr([1, 0, 0x5e, 0, 0, 0xfb]), MDNS), (MacAddr::BROADCAST, ip4(192, 0, 2, 255)), (MacAddr::BROADCAST, ip4(255, 255, 255, 255))] {
        let Some(Delivery::Udp(arrival, _)) = h.datagram_to(frame_to, &edit(hex(V_UDP_HI), |ip| set_destination(ip, destination))) else { panic!() };
        h.ip.port_unreachable(h.clock(), &arrival);
    }
    let about_error = edit(hex(V_ICMP_PORT_UNREACH), |ip| {
        set_source(ip, B);
        set_destination(ip, A);
    });
    let leaked: &'static [u8] = Box::leak(about_error.into_boxed_slice());
    let arrival = Arrival { iface: h.if0, packet: Ipv4Packet::parse(leaked).unwrap(), cast: Cast::Unicast, link: MacClass::Individual };
    h.ip.port_unreachable(h.clock(), &arrival);
}

#[test]
fn s_ip_icg_003_suppression_takes_no_token() {
    let mut h = H::fixture_i_with(B, MAC_B);
    suppressed_requests(&mut h);
    assert!(h.out().is_empty());
    assert_eq!(h.count(Counter::IcmpErrorSuppressed), 4);
    for _ in 0..10 {
        assert_eq!(closed_port(&mut h, &hex(V_UDP_HI)).len(), 1, "no token was taken");
    }
}

#[test]
fn s_ip_icg_004_refusals_are_silent() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram(&hex(V_IP_LSRR));
    h.datagram(&hex(V_IP_FRAG_FIRST));
    h.datagram(&edit(hex(V_UDP_HI), |ip| set_destination(ip, ip4(192, 0, 2, 7))));
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_icg_005_prop_only_echo_replies_and_unreachables() {
    let mut h = b_and_r();
    let mut rng = Rng(0x1c95);
    let inputs = [echo_to_a(), hex(V_IP_MIN), hex(V_UDP_HI), hex(V_IP_LSRR), hex(V_ICMP_PORT_UNREACH), hex(V_ICMP_REPLY), edit(hex(V_ICMP_TIME_EXCEEDED), |_| {})];
    let mut out = Vec::new();
    for step in 0..400u64 {
        h.at(step * 13);
        let input = &inputs[rng.below(inputs.len() as u64) as usize];
        if let Some(Delivery::Udp(arrival, _)) = h.datagram(input) {
            h.ip.port_unreachable(h.clock(), &arrival);
        }
        out.extend(h.out());
    }
    let types = icmp_types(&out);
    assert!(types.len() > 50);
    assert!(types.iter().all(|t| matches!(t, (0, 0) | (3, 2) | (3, 3))), "{types:?}");
}

#[test]
fn s_ip_icg_006_an_unknown_protocol_to_a_broadcast() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram_to(MacAddr::BROADCAST, &edit(hex(V_IP_MIN), |ip| set_destination(ip, ip4(192, 0, 2, 255))));
    assert_eq!(h.count(Counter::IpProtocol), 1);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_icg_007_short_datagrams_are_quoted_whole() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let out = closed_port(&mut h, &hex(V_UDP_HI));
    assert_eq!(out[0].ip().unwrap().payload()[8..], hex(V_UDP_HI)[..]);
    h.datagram(&hex(V_IP_MIN));
    let out = h.out();
    assert_eq!(out[0].ip().unwrap().payload()[8..], hex(V_IP_MIN)[..]);
}

#[test]
fn s_ip_icg_008_a_malformed_router_alert_is_dropped_silently() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut ip = vec![0x46, 0, 0, 0, 0, 0, 0x40, 0, 64, 17, 0, 0, 192, 0, 2, 2, 192, 0, 2, 1, 0x94, 3, 0, 0];
    ip.extend_from_slice(&hex(V_UDP_HI)[20..]);
    let total = ip.len() as u16;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    fix(&mut ip);
    assert!(h.datagram(&ip).is_none());
    assert_eq!(h.wire("ip.router-alert-length"), 1);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_rl_001_ten_then_limited() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let sent: usize = (0..15).map(|_| closed_port(&mut h, &hex(V_UDP_HI)).len()).sum();
    assert_eq!(sent, 10);
    assert_eq!(h.count(Counter::IcmpErrorRateLimited), 5);
}

#[test]
fn s_ip_rl_002_one_token_per_100_ms() {
    let mut h = H::fixture_i_with(B, MAC_B);
    for _ in 0..15 {
        closed_port(&mut h, &hex(V_UDP_HI));
    }
    let at = |h: &mut H, t: u64| {
        h.at(t);
        closed_port(h, &hex(V_UDP_HI)).len()
    };
    assert_eq!([at(&mut h, 99), at(&mut h, 100), at(&mut h, 150), at(&mut h, 200)], [0, 1, 0, 1]);
}

#[test]
fn s_ip_rl_003_another_destination_has_its_own_bucket() {
    let mut h = b_and_r();
    for _ in 0..15 {
        closed_port(&mut h, &hex(V_UDP_HI));
    }
    let remote = edit(hex(V_UDP_HI), |ip| set_source(ip, REMOTE));
    let out = closed_port(&mut h, &remote);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].to(), MAC_R);
}

fn two_hundred_sources() -> usize {
    let mut h = b_and_r();
    (1..=200u32).map(|n| closed_port(&mut h, &hi_from(Ipv4Addr::from(u32::from(ip4(198, 51, 100, 0)) + n))).len()).sum()
}

#[test]
fn s_ip_rl_004_the_global_burst_is_drawn() {
    let sent = two_hundred_sources();
    assert!((75..=100).contains(&sent), "{sent}");
    assert_eq!(two_hundred_sources(), sent);
}

#[test]
fn s_ip_rl_005_the_global_bucket_refills_at_100_a_second() {
    let mut h = b_and_r();
    for n in 0..200u32 {
        closed_port(&mut h, &hi_from(Ipv4Addr::from(u32::from(ip4(198, 51, 100, 0)) + n + 1)));
    }
    h.at(10);
    assert_eq!(closed_port(&mut h, &hi_from(ip4(203, 0, 113, 1))).len(), 1);
    h.at(15);
    assert_eq!(closed_port(&mut h, &hi_from(ip4(203, 0, 113, 2))).len(), 0);
}

#[test]
fn s_ip_rl_006_prop_colliding_destinations_share_a_bucket() {
    let limiter = Limiter::new([1; 16]);
    let base = ip4(203, 0, 113, 0);
    let (first, second) = (0..4096u32)
        .map(|n| Ipv4Addr::from(u32::from(base) + n))
        .find_map(|a| (1..4096u32).map(|m| Ipv4Addr::from(u32::from(a) + m)).find(|b| limiter.slot(*b) == limiter.slot(a)).map(|b| (a, b)))
        .unwrap();
    let mut shared = Limiter::new([1; 16]);
    let t = Instant::from_millis(1);
    let passed = (0..20).filter(|n| shared.allow(t, if n % 2 == 0 { first } else { second })).count();
    assert_eq!(passed, 10, "one bucket of ten between them");
    let other = Limiter::new([2; 16]);
    let colliding_elsewhere = (0..64u32).all(|n| {
        let a = Ipv4Addr::from(u32::from(first) + n);
        let b = Ipv4Addr::from(u32::from(second) + n);
        (limiter.slot(a) == limiter.slot(b)) == (other.slot(a) == other.slot(b))
    });
    assert!(!colliding_elsewhere, "the collision set is the secret's");
}

#[test]
fn s_ip_rl_007_suppressed_requests_spend_nothing() {
    let mut h = H::fixture_i_with(B, MAC_B);
    for _ in 0..5 {
        suppressed_requests(&mut h);
    }
    let sent: usize = (0..10).map(|_| closed_port(&mut h, &hex(V_UDP_HI)).len()).sum();
    assert_eq!(sent, 10);
}

fn delivered_error(h: &mut H, datagram: &[u8]) -> toyos_net_ip::TransportError {
    match h.frame(&eth(MAC_A, MAC_R, 0x0800, datagram)) {
        Some(Delivery::Error(e)) => e,
        other => panic!("{other:?}"),
    }
}

#[test]
fn s_ip_icd_001_a_port_unreachable_for_udp() {
    let mut h = H::fixture_i();
    let e = delivered_error(&mut h, &hex(V_ICMP_PORT_UNREACH));
    assert_eq!(e.transport, Transport::Udp);
    assert_eq!(e.kind, ErrorKind::Unreachable(UnreachableCode::Port));
    assert_eq!((e.flow.source, e.flow.source_port.get(), e.flow.destination, e.flow.destination_port.get()), (A, 49152, DNS, 53));
    assert_eq!(e.reporter, DNS);
}

#[test]
fn s_ip_icd_002_fragmentation_needed_for_tcp() {
    let mut h = H::fixture_i();
    let generation = h.ip.generation();
    let e = delivered_error(&mut h, &hex(V_ICMP_FRAG_NEEDED));
    assert_eq!(e.transport, Transport::Tcp { sequence: 0x1111_1111 });
    assert_eq!(e.kind, ErrorKind::FragmentationNeeded { next_hop_mtu: std::num::NonZeroU16::new(1_400), quoted_length: 1_500 });
    assert_eq!(e.reporter, R);
    assert_eq!(h.ip.generation(), generation);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_icd_003_an_error_to_a_broadcast() {
    let mut h = H::fixture_i();
    h.datagram_to(MacAddr::BROADCAST, &edit(hex(V_ICMP_PORT_UNREACH), |ip| set_destination(ip, ip4(192, 0, 2, 255))));
    assert_eq!(h.count(Counter::IcmpErrorToGroup), 1);
}

#[test]
fn s_ip_icd_004_a_quote_from_a_tentative_address() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, ip4(192, 0, 2, 9), 24).unwrap();
    h.run(200);
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    let e = edit(hex(V_ICMP_PORT_UNREACH), |ip| set_destination(ip, ip4(192, 0, 2, 9)));
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &e));
    assert_eq!(h.count(Counter::IcmpQuoteNotOurs), 1);
}

fn error_quoting(protocol: u8) -> Vec<u8> {
    edit(hex(V_ICMP_PORT_UNREACH), |ip| {
        ip[28 + 9] = protocol;
        ip[28 + 10] = 0;
        ip[28 + 11] = 0;
    })
}

#[test]
fn s_ip_icd_005_a_quote_of_icmp() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &error_quoting(1)));
    assert_eq!(h.count(Counter::IcmpQuoteIcmp), 1);
}

#[test]
fn s_ip_icd_006_quotes_of_igmp_and_others() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &error_quoting(2)));
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &error_quoting(253)));
    assert_eq!((h.count(Counter::IcmpQuoteIgmp), h.count(Counter::IcmpQuoteOtherProtocol)), (1, 1));
}

#[test]
fn s_ip_icd_007_time_exceeded() {
    let mut h = H::fixture_i();
    let e = delivered_error(&mut h, &hex(V_ICMP_TIME_EXCEEDED));
    assert!(matches!(e.transport, Transport::Tcp { .. }));
    assert_eq!(e.kind, ErrorKind::TimeExceeded(TimeExceededCode::InTransit));
    assert_eq!((e.flow.source_port.get(), e.flow.destination, e.flow.destination_port.get()), (49153, REMOTE, 443));
}

#[test]
fn s_ip_icd_008_parameter_problem() {
    let mut h = H::fixture_i();
    let e = delivered_error(&mut h, &ipv4(R, A, 1, &hex(V_ICMP_PARAM_PROBLEM)));
    assert_eq!(e.kind, ErrorKind::ParameterProblem { code: ParameterProblemCode::Pointer, pointer: 20 });
}

#[test]
fn s_ip_icd_009_an_unassigned_code() {
    let mut h = H::fixture_i();
    let e = delivered_error(&mut h, &edit(hex(V_ICMP_FRAG_NEEDED), |ip| ip[21] = 16));
    let ErrorKind::Unreachable(code) = e.kind else { panic!("{:?}", e.kind) };
    assert!(matches!(code, UnreachableCode::Unassigned(c) if c.value() == 16));
}

#[test]
fn s_ip_icd_010_a_redirect_moves_no_route() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_R, 0x0800, &ipv4(R, A, 1, &hex(V_ICMP_REDIRECT))));
    assert_eq!(h.refusals(Counter::IcmpRedirect)[0].peer, Peer::Ip(R));
    assert_eq!(h.ip.route(REMOTE, toyos_net_ip::Source::Any, None).map(|r| r.next_hop), Ok(toyos_net_ip::NextHop::Neighbour(R)));
}

#[test]
fn s_ip_icd_011_prop_the_shared_classification() {
    for code in 0..=16u8 {
        let mut h = H::fixture_i();
        let e = delivered_error(&mut h, &edit(hex(V_ICMP_FRAG_NEEDED), |ip| ip[21] = code));
        let expected = match code {
            2 | 3 => ErrorClass::Refused,
            9 | 10 | 13 => ErrorClass::Prohibited,
            4 => ErrorClass::PathMtu,
            _ => ErrorClass::Soft,
        };
        assert_eq!(e.kind.class(), expected, "code {code}");
    }
}

#[test]
fn s_ip_icd_012_a_resolution_failure_is_local() {
    let mut h = H::fixture_i();
    h.udp_to(B).unwrap();
    h.out();
    let out = h.run(3_000);
    assert_eq!(out.len(), 2, "only the two retransmitted requests");
    assert!(h.events.contains(&Event::Failed { iface: h.if0, next_hop: B }), "the sender that waits for B is told by this");
    assert!(matches!(h.state(B), Some(Nud::Failed)));
}

#[test]
fn s_icmp_023_a_redirect_is_refused() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_R, 0x0800, &ipv4(R, A, 1, &hex(V_ICMP_REDIRECT))));
    assert_eq!(h.count(Counter::IcmpRedirect), 1);
}

#[test]
fn s_icmp_024_a_timestamp_request_is_refused() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram(&ipv4(B, A, 1, &hex(V_ICMP_TIMESTAMP_REQ)));
    assert_eq!(h.count(Counter::IcmpTimestampRequest), 1);
    assert_eq!(h.refusals(Counter::IcmpTimestampRequest)[0].peer, Peer::Ip(B));
    assert!(h.out().is_empty());
}

#[test]
fn s_icmp_026_a_timestamp_reply_is_unsupported() {
    let mut h = H::fixture_i();
    let mut reply = hex(V_ICMP_TIMESTAMP_REQ);
    reply[0] = 14;
    let mut ip = ipv4(B, A, 1, &reply);
    fix(&mut ip);
    h.datagram(&ip);
    assert_eq!(h.wire(IcmpError::TimestampReply.name()), 1);
}

#[test]
fn s_icmp_032_b_answers_a() {
    let mut h = H::as_b();
    h.frame(&eth(MAC_B, MAC_A, 0x0800, &hex(V_ICMP_ECHO)));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(ip_bytes(&out[0].frame), hex(V_ICMP_REPLY));
}

#[test]
fn s_icmp_033_all_the_data_comes_back() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut message = vec![8, 0, 0, 0, 0, 1, 0, 1];
    message.extend((0..1_472u32).map(|n| n as u8));
    h.datagram(&ipv4(B, A, 1, &message));
    let out = h.out();
    assert_eq!(out[0].ip().unwrap().payload()[8..], message[8..]);
}

#[test]
fn s_icmp_034_no_answer_to_a_group() {
    let mut h = H::fixture_i_with(B, MAC_B);
    h.datagram_to(MacAddr::BROADCAST, &edit(echo_to_a(), |ip| set_destination(ip, ip4(255, 255, 255, 255))));
    h.datagram_to(MacAddr([1, 0, 0x5e, 0, 0, 1]), &edit(echo_to_a(), |ip| set_destination(ip, ip4(224, 0, 0, 1))));
    assert_eq!(h.count(Counter::IcmpEchoToGroup), 2);
    assert!(h.out().is_empty());
}

#[test]
fn s_icmp_035_record_route_is_not_returned() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut request = vec![0x47, 0, 0, 0, 0, 0, 0x40, 0, 64, 1, 0, 0, 192, 0, 2, 2, 192, 0, 2, 1, 7, 7, 4, 0, 0, 0, 0, 0];
    request.extend_from_slice(&hex(V_ICMP_ECHO)[20..]);
    let total = request.len() as u16;
    request[2..4].copy_from_slice(&total.to_be_bytes());
    fix(&mut request);
    h.datagram(&request);
    assert!(h.out()[0].ip().unwrap().options().bytes().is_empty());
}

#[test]
fn s_icmp_036_a_quote_from_someone_else() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &edit(hex(V_ICMP_PORT_UNREACH), |ip| ip[40..44].copy_from_slice(&[192, 0, 2, 9]))));
    assert_eq!(h.count(Counter::IcmpQuoteNotOurs), 1);
}

#[test]
fn s_icmp_037_a_quote_of_a_later_fragment() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &edit(hex(V_ICMP_PORT_UNREACH), |ip| ip[34..36].copy_from_slice(&[0, 185]))));
    assert_eq!(h.count(Counter::IcmpQuoteNonInitialFragment), 1);
}

#[test]
fn s_icmp_038_each_error_goes_to_its_transport() {
    let mut h = H::fixture_i();
    let udp = delivered_error(&mut h, &hex(V_ICMP_PORT_UNREACH));
    assert_eq!((udp.transport, udp.flow.source_port.get()), (Transport::Udp, 49152));
    let tcp = delivered_error(&mut h, &hex(V_ICMP_FRAG_NEEDED));
    assert!(matches!(tcp.transport, Transport::Tcp { .. }));
    assert_eq!((tcp.flow.source_port.get(), tcp.flow.destination, tcp.flow.destination_port.get()), (49153, REMOTE, 443));
    assert!(matches!(tcp.kind, ErrorKind::FragmentationNeeded { next_hop_mtu: Some(m), .. } if m.get() == 1_400));
}

#[test]
fn s_icmp_039_the_generated_port_unreachable() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(ip_bytes(&closed_port(&mut h, &hex(V_UDP_HI))[0].frame), hex(V_ICMP_PORT_UNREACH_GEN));
}

#[test]
fn s_icmp_040_576_bytes_at_most() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let big = udp(B, A, 5000, 5001, &[7; 972]);
    let out = closed_port(&mut h, &big);
    let ip = out[0].ip().unwrap();
    assert_eq!(ip.total_length(), 576);
    assert_eq!(ip.payload()[8..], big[..548]);
}

#[test]
fn s_icmp_041_the_quote_is_as_received() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let received = edit(hex(V_IP_RR), |ip| ip[8] = 3);
    let out = closed_port(&mut h, &received);
    assert_eq!(out[0].ip().unwrap().payload()[8..], received[..]);
}

#[test]
fn s_icmp_042_an_error_for_no_socket_is_not_answered() {
    let mut h = H::fixture_i();
    assert!(matches!(h.frame(&eth(MAC_A, MAC_DNS, 0x0800, &hex(V_ICMP_PORT_UNREACH))), Some(Delivery::Error(_))));
    assert!(h.out().is_empty());
}

#[test]
fn s_icmp_043_no_error_about_a_broadcast() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let broadcast = edit(hex(V_UDP_HI), |ip| set_destination(ip, ip4(255, 255, 255, 255)));
    let Some(Delivery::Udp(arrival, _)) = h.datagram_to(MacAddr::BROADCAST, &broadcast) else { panic!("not delivered") };
    h.ip.port_unreachable(h.clock(), &arrival);
    assert!(h.out().is_empty());
    assert_eq!(h.count(Counter::IcmpErrorSuppressed), 1);
}

#[test]
fn s_icmp_044_no_error_about_a_link_broadcast() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert!(h.datagram_to(MacAddr::BROADCAST, &hex(V_UDP_HI)).is_none());
    assert!(h.out().is_empty());
}

#[test]
fn s_icmp_045_no_error_about_a_fragment() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert!(h.datagram(&hex(V_IP_FRAG_LAST)).is_none());
    assert!(h.out().is_empty());
}

#[test]
fn s_icmp_046_no_error_to_a_source_that_is_no_host() {
    let mut h = H::fixture_i_with(B, MAC_B);
    for source in [ip4(0, 0, 0, 0), ip4(127, 0, 0, 1), ip4(224, 0, 0, 5), ip4(240, 0, 0, 1)] {
        assert!(closed_port(&mut h, &hi_from(source)).is_empty());
    }
    assert_eq!(h.count(Counter::IpInvalidSource), 4);
}

// An error of [ip]'s own is routed when it is generated, which is after its datagram was admitted:
// a prefix that becomes usable in between can make the datagram's source its directed broadcast.
// The error then leaves in no link broadcast: it is counted and logged against that address.
#[test]
fn an_error_whose_next_hop_became_a_broadcast_is_refused_and_counted() {
    let mut h = H::fixture_i();
    let source = ip4(192, 0, 2, 127);
    let Some(Delivery::Udp(arrival, _)) = h.datagram(&hi_from(source)) else { panic!("a host's datagram is admitted") };
    h.ip.add_address(h.clock(), h.if0, ip4(192, 0, 2, 3), 25).unwrap();
    h.run(3_000);
    assert_eq!(h.ip.address(h.if0, ip4(192, 0, 2, 3)), Some(toyos_net_ip::AddrState::Assigned));
    h.rebase();
    h.ip.port_unreachable(h.clock(), &arrival);
    let rule = Counter::IpBroadcastNotPermitted;
    assert_eq!(h.out().iter().map(Out::to).collect::<Vec<_>>(), [], "nothing left");
    assert_eq!((h.count(rule), h.count(Counter::IcmpErrorSuppressed), h.count(Counter::IcmpErrorsSent)), (1, 0, 0));
    assert_eq!(h.refusals(rule), [toyos_net_ip::Refusal { rule, iface: h.if0, peer: Peer::Ip(source) }]);
}
