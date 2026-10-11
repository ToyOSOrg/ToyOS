//! Receiving, ICMP errors, close, and the properties.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Cast, Delivery};
use toyos_net_udp::{limits, Binding, Counter, Error, Received, SocketError, SocketId, Verdict};
use toyos_net_wire::ethernet::MacAddr;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn hi() -> Vec<u8> {
    hex(V_UDP_HI)
}

fn received(u: &mut U, id: SocketId) -> (Vec<u8>, Received) {
    u.recv(id).unwrap().expect("a datagram waits")
}

fn frames(ip: &[u8]) -> Vec<u8> {
    eth(MAC_A, MAC_B, 0x0800, ip)
}

#[test]
fn s_udp_us_030_a_bound_any_socket_receives() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    assert_eq!(u.datagram(&hi()), Some(Verdict::Delivered));
    let (payload, r) = received(&mut u, id);
    assert_eq!((payload.as_slice(), r.source, r.source_port, r.destination, r.ttl), (&b"hi"[..], B, Some(port(5_000)), A, 64));
    assert_eq!(u.count(Counter::RxDelivered), 1);
}

#[test]
fn s_udp_us_031_a_specific_socket_hears_no_broadcast() {
    let mut u = U::uf();
    let id = u.bind(A, 5_001).unwrap();
    assert_eq!(u.datagram(&hi()), Some(Verdict::Delivered));
    assert_eq!(received(&mut u, id).0, b"hi");
    assert_eq!(u.frame(&hex(V_UDP_BCAST)), Some(Verdict::Ignored));
    assert_eq!(u.count(Counter::RxNoSocketGroup), 1);
    assert!(u.out().is_empty());
}

#[test]
fn s_udp_us_032_bound_any_hears_both_broadcasts() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.frame(&hex(V_UDP_BCAST));
    u.frame(&hex(V_UDP_SUBNET_BCAST));
    let first = received(&mut u, id).1;
    let second = received(&mut u, id).1;
    assert_eq!((first.destination, first.link_broadcast), (LIMITED, true));
    assert_eq!((second.destination, second.link_broadcast), (ip4(192, 0, 2, 255), true));
}

#[test]
fn s_udp_us_033_mdns_arrives_with_its_group() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_353).unwrap();
    u.frame(&hex(V_UDP_MDNS));
    let (payload, r) = received(&mut u, id);
    assert_eq!(payload, hex(V_UDP_MDNS)[42..]);
    assert_eq!((r.source, r.source_port, r.destination, r.ttl), (B, Some(port(5_353)), MDNS, 255));
}

#[test]
fn s_udp_us_034_no_socket_for_a_group_is_no_error() {
    let mut u = U::uf();
    assert_eq!(u.frame(&hex(V_UDP_MDNS)), Some(Verdict::Ignored));
    assert_eq!(u.count(Counter::RxNoSocketGroup), 1);
    assert!(u.out().is_empty());
}

#[test]
fn s_udp_us_035_no_socket_is_a_port_unreachable() {
    let mut u = U::uf();
    assert_eq!(u.datagram(&hi()), Some(Verdict::NoSocket));
    assert_eq!(u.count(Counter::RxNoSocket), 1);
    assert_eq!(u.out().iter().map(|f| ip_bytes(f)).collect::<Vec<_>>(), [hex(V_ICMP_PORT_UNREACH_GEN)]);
}

#[test]
fn s_udp_us_036_an_empty_datagram() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.frame(&hex(V_UDP_EMPTY));
    let (payload, r) = received(&mut u, id);
    assert!(payload.is_empty() && r.len == 0);
}

#[test]
fn s_udp_us_037_source_port_zero() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    let from_zero = edit(hi(), |ip| set_ports(ip, 0, 5_001));
    u.datagram(&from_zero);
    let r = received(&mut u, id).1;
    assert_eq!(r.source_port, None);
    assert_eq!(u.count(Counter::RxSrcPortZero), 1);
    assert_eq!(u.udp.send_to(&mut u.ip, id, B, 0, b"x"), Err(Error::Refused(Counter::SendPortZero)));

    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.udp.connect(&mut u.ip, id, B, 5_000).unwrap();
    assert_eq!(u.datagram(&from_zero), Some(Verdict::NoSocket));
    assert_eq!(u.out().len(), 1, "a port unreachable");
}

#[test]
fn s_udp_us_038_no_checksum_is_delivered() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    let mut unchecked = hi();
    unchecked[26] = 0;
    unchecked[27] = 0;
    u.datagram(&unchecked);
    assert_eq!(received(&mut u, id).0, b"hi");
    assert_eq!(u.count(Counter::RxNoChecksum), 1);
}

#[test]
fn s_udp_us_039_a_bad_checksum_is_malformed() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    let mut bad = hi();
    bad[26..28].copy_from_slice(&[0xec, 0x5c]);
    assert_eq!(u.datagram(&bad), None);
    assert_eq!(u.ip.wire_counters().find(|(name, _)| *name == "udp.checksum"), Some(("udp.checksum", 1)));
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
    assert!(u.out().is_empty());
}

#[test]
fn s_udp_us_040_a_full_queue_drops_and_says_nothing() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    for _ in 0..limits::RX_DATAGRAMS {
        u.datagram(&hi());
    }
    assert_eq!(u.datagram(&hi()), Some(Verdict::Delivered));
    assert_eq!(u.count(Counter::RxQueueFull), 1);
    assert_eq!(u.udp.dropped(id), Ok(1));
    assert!(u.out().is_empty());
    received(&mut u, id);
    u.datagram(&hi());
    assert_eq!(u.count(Counter::RxQueueFull), 1);
    assert_eq!(u.count(Counter::RxDelivered), 17);
}

#[test]
fn s_udp_us_041_arrival_order() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    for data in [&b"a"[..], b"bb", b"ccc"] {
        u.datagram(&udp(B, 5_000, A, 5_001, data));
    }
    let got: Vec<Vec<u8>> = (0..3).map(|_| received(&mut u, id).0).collect();
    assert_eq!(got, [b"a".to_vec(), b"bb".to_vec(), b"ccc".to_vec()]);
}

#[test]
fn s_udp_us_042_a_short_buffer_truncates() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.datagram(&udp(B, 5_000, A, 5_001, b"0123456789"));
    u.datagram(&udp(B, 5_000, A, 5_001, b"next"));
    let mut buf = [0u8; 4];
    let r = u.udp.recv(id, &mut buf).unwrap().unwrap();
    assert_eq!((r.len, &buf), (4, b"0123"));
    assert_eq!(u.count(Counter::RxTruncated), 1);
    assert_eq!(received(&mut u, id).0, b"next");
}

#[test]
fn s_udp_us_043_a_group_not_joined_never_reaches_udp() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_353).unwrap();
    let other = ip4(239, 1, 2, 3);
    let ip = edit(hex(V_UDP_MDNS)[14..].to_vec(), |ip| set_destination(ip, other));
    let group_mac = MacAddr::multicast(toyos_net_wire::ipv4::MulticastAddr::new(other).unwrap());
    assert_eq!(u.frame(&eth(group_mac, MAC_B, 0x0800, &ip)), None);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::EthNotForUs), 1, "the frame filter comes first");
    assert_eq!(u.frame(&frames(&ip)), None);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpNotForUs), 1);
    assert_eq!(u.count(Counter::Rx), 0);
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

#[test]
fn s_udp_us_044_the_acquisition_exception() {
    let mut u = U::bare();
    let id = u.bind(ANY, 68).unwrap();
    u.udp.set_acquisition(id).unwrap();
    let offer = hex(V_DHCP_OFFER_FRAME);
    let leaked: &'static [u8] = Box::leak(offer.clone().into_boxed_slice());
    let Some(Delivery::Udp(arrival, datagram)) = u.ip.receive(u.clock(), u.if0, leaked) else { panic!("not admitted") };
    assert_eq!(arrival.cast, Cast::Acquisition);
    assert_eq!(u.udp.receive(u.clock(), &arrival, &datagram), Verdict::Delivered);
    let (payload, r) = received(&mut u, id);
    assert_eq!((payload.as_slice(), r.destination), (&offer[42..], A));
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpAcquisitionAdmitted), 1);
    u.ip.add_address(u.clock(), u.if0, A, 24).unwrap();
    let elsewhere = edit(offer[14..].to_vec(), |ip| set_destination(ip, ip4(192, 0, 2, 7)));
    assert_eq!(u.frame(&eth(MAC_A, MAC_R, 0x0800, &elsewhere)), Some(Verdict::Delivered), "a probing address is not usable");
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpAcquisitionAdmitted), 2);

    let mut u = U::bare();
    u.bind(ANY, 5_000).unwrap();
    let mut ip = offer[14..].to_vec();
    set_ports(&mut ip, 67, 5_000);
    fix(&mut ip);
    assert_eq!(u.frame(&eth(MAC_A, MAC_R, 0x0800, &ip)), None);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpNotForUs), 1);

    let mut u = U::bare();
    let id = u.bind(ANY, 68).unwrap();
    u.udp.set_acquisition(id).unwrap();
    let group = edit(offer[14..].to_vec(), |ip| set_destination(ip, ip4(239, 9, 9, 9)));
    assert_eq!(u.frame(&eth(MAC_A, MAC_R, 0x0800, &group)), None, "C-3 admits a unicast destination only");
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpNotForUs), 1);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::IpAcquisitionAdmitted), 0);
    assert_eq!(u.udp.recv(id, &mut [0; 1_500]), Ok(None));

    // A socket on 68 without the acquisition mark never hears it.
    let mut u = U::bare();
    let id = u.bind(ANY, 68).unwrap();
    assert_eq!(u.frame(&offer), Some(Verdict::Ignored));
    assert_eq!(u.count(Counter::RxNoSocket), 1);
    assert_eq!(u.udp.recv(id, &mut [0; 1_500]), Ok(None));
}

#[test]
fn s_udp_us_045_what_a_datagram_carries() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.now = 1_234;
    u.frame(&hex(V_UDP_SUBNET_BCAST));
    let r = received(&mut u, id).1;
    assert_eq!((r.destination, r.ttl, r.link_broadcast, r.at), (ip4(192, 0, 2, 255), 64, true, U::instant(1_234)));
}

fn connected_to_dns() -> (U, SocketId) {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.connect(&mut u.ip, id, DNS, 53).unwrap();
    (u, id)
}

fn with_code(code: u8) -> Vec<u8> {
    edit(hex(V_ICMP_PU_A_OUT), |ip| ip[21] = code)
}

#[test]
fn s_udp_us_046_refused_once() {
    let (mut u, id) = connected_to_dns();
    u.datagram(&hex(V_ICMP_PU_A_OUT));
    assert_eq!(u.count(Counter::IcmpErrorDelivered), 1);
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Err(Error::Failed(SocketError::Refused)));
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
}

#[test]
fn s_udp_us_047_an_unconnected_socket_hears_no_error() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.datagram(&hex(V_ICMP_PU_A_OUT));
    assert_eq!(u.count(Counter::IcmpErrorUnconnected), 1);
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
}

#[test]
fn s_udp_us_048_an_error_for_no_socket() {
    let mut u = U::uf();
    u.datagram(&hex(V_ICMP_PU_A_OUT));
    assert_eq!(u.count(Counter::IcmpErrorNoSocket), 1);
    assert!(u.out().is_empty());
}

#[test]
fn s_udp_us_049_hard_and_soft() {
    let (mut u, id) = connected_to_dns();
    u.datagram(&with_code(13));
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Err(Error::Failed(SocketError::Prohibited)));
    u.datagram(&with_code(1));
    let time_exceeded = edit(hex(V_ICMP_PU_A_OUT), |ip| {
        ip[20] = 11;
        ip[21] = 0;
    });
    u.datagram(&time_exceeded);
    assert_eq!(u.count(Counter::IcmpErrorSoft), 2);
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
}

#[test]
fn s_udp_us_050_the_latest_error_wins() {
    let (mut u, id) = connected_to_dns();
    u.datagram(&with_code(13));
    u.datagram(&with_code(3));
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Err(Error::Failed(SocketError::Refused)));
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
}

#[test]
fn s_udp_us_051_a_send_takes_the_error() {
    let (mut u, id) = connected_to_dns();
    u.datagram(&hex(V_ICMP_PU_A_OUT));
    assert_eq!(u.udp.send(&mut u.ip, id, b"x"), Err(Error::Failed(SocketError::Refused)));
    assert!(u.out().is_empty(), "nothing was queued");
    u.udp.send(&mut u.ip, id, b"x").unwrap();
    assert_eq!(u.out().len(), 1);
}

#[test]
fn s_udp_us_052_the_whole_4_tuple_must_match() {
    let (mut u, id) = connected_to_dns();
    u.datagram(&edit(hex(V_ICMP_PU_A_OUT), |ip| ip[51] = 54));
    assert_eq!(u.count(Counter::IcmpErrorNoSocket), 1);
    assert_eq!(u.udp.recv(id, &mut [0; 8]), Ok(None));
}

#[test]
fn s_udp_us_053_accepted_datagrams_outlive_close() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"1").unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"2").unwrap();
    u.udp.close(id).unwrap();
    u.bind(ANY, 50_001).unwrap();
    let out = u.out();
    assert_eq!(out.len(), 2);
    for (frame, data) in out.iter().zip([b"1", b"2"]) {
        let ip = ip_of(frame).unwrap();
        assert_eq!((ip.source(), &ip.payload()[..2], &ip.payload()[8..]), (A, &[0xc3, 0x51][..], &data[..]));
    }

    // Closed sockets hold at most CLOSED_DATAGRAMS together; past it a close refuses the rest.
    let mut u = U::uf();
    let mut accepted = 0;
    for n in 0..100u16 {
        let id = u.bind(ANY, 50_001 + n).unwrap();
        for _ in 0..limits::TX_DATAGRAMS {
            u.udp.send_to(&mut u.ip, id, DNS, 53, &[7; 1_000]).unwrap();
            accepted += 1;
        }
        u.udp.close(id).unwrap();
    }
    assert_eq!(accepted, 1_600);
    assert_eq!(u.count(Counter::TxDiscardedOnClose), 1_600 - 16);
    let out = u.out();
    assert_eq!(out.len(), 16);
    assert!(out.iter().all(|f| ip_of(f).unwrap().payload()[..2] == 50_001u16.to_be_bytes()), "the first close's, in order");

    let mut u = U::uf();
    let first = u.bind(ANY, 50_001).unwrap();
    for _ in 0..10 {
        u.udp.send_to(&mut u.ip, first, DNS, 53, b"a").unwrap();
    }
    u.udp.close(first).unwrap();
    let second = u.bind(ANY, 50_002).unwrap();
    for _ in 0..limits::TX_DATAGRAMS {
        u.udp.send_to(&mut u.ip, second, DNS, 53, b"b").unwrap();
    }
    u.udp.close(second).unwrap();
    assert_eq!(u.count(Counter::TxDiscardedOnClose), 10);
    let order: String = u.out().iter().map(|f| char::from(ip_of(f).unwrap().payload()[8])).collect();
    assert_eq!(order, "aaaaaaaaaabbbbbb");

    // The slot a closed socket leaves takes no turn with it to the next socket in it, and closed
    // sockets' datagrams take one turn among the sockets.
    let mut u = U::uf();
    let x = u.bind(ANY, 50_001).unwrap();
    for _ in 0..2 {
        u.udp.send_to(&mut u.ip, x, DNS, 53, b"x").unwrap();
    }
    u.udp.close(x).unwrap();
    let y = u.bind(ANY, 50_002).unwrap();
    let z = u.bind(ANY, 50_003).unwrap();
    let send_yz = |u: &mut U| {
        for _ in 0..2 {
            u.udp.send_to(&mut u.ip, y, DNS, 53, b"y").unwrap();
            u.udp.send_to(&mut u.ip, z, DNS, 53, b"z").unwrap();
        }
    };
    let order = |u: &mut U| -> String { u.out().iter().map(|f| char::from(ip_of(f).unwrap().payload()[8])).collect() };
    send_yz(&mut u);
    assert_eq!(order(&mut u), "xyzxyz");
    send_yz(&mut u);
    let w = u.bind(ANY, 50_004).unwrap();
    for _ in 0..2 {
        u.udp.send_to(&mut u.ip, w, DNS, 53, b"w").unwrap();
    }
    u.udp.close(w).unwrap();
    assert_eq!(order(&mut u), "yzwyzw");
}

#[test]
fn s_udp_us_054_close_discards_what_was_received() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    for _ in 0..3 {
        u.datagram(&hi());
    }
    u.udp.close(id).unwrap();
    assert_eq!(u.count(Counter::RxDiscardedOnClose), 3);
    assert_eq!(u.datagram(&hi()), Some(Verdict::NoSocket));
    assert_eq!(u.out().len(), 1);
}

/// US-55's model: which sockets exist, how each is bound.
#[test]
fn s_udp_us_055_prop_the_demultiplexer() {
    for seed in 1..=40u64 {
        let mut u = U::uf();
        let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut model: BTreeMap<u16, (SocketId, Binding)> = BTreeMap::new();
        let ports = [5_001u16, 5_002, 5_003];
        let peers = [(B, 5_000u16), (DNS, 53)];
        for _ in 0..300 {
            match next() % 5 {
                0 => {
                    let p = ports[(next() % 3) as usize];
                    let addr = if next() % 2 == 0 { ANY } else { A };
                    let result = u.bind(addr, p);
                    match model.get(&p) {
                        Some(_) => assert_eq!(result.err(), Some(Error::Refused(Counter::PortInUse))),
                        None => {
                            let id = result.unwrap();
                            model.insert(p, (id, u.udp.binding(id).unwrap().0));
                        }
                    }
                }
                1 => {
                    u.draws.push_back(next() as u32);
                    let id = u.bind(ANY, 0).unwrap();
                    let p = u.udp.binding(id).unwrap().1.get();
                    assert!(p >= limits::EPHEMERAL_FIRST);
                    assert!(!model.contains_key(&p), "the port was free");
                    model.insert(p, (id, Binding::Any));
                }
                2 => {
                    if let Some((&p, &(id, _))) = model.iter().nth((next() % 4) as usize) {
                        let (peer, pp) = peers[(next() % 2) as usize];
                        u.udp.connect(&mut u.ip, id, peer, pp).unwrap();
                        model.insert(p, (id, u.udp.binding(id).unwrap().0));
                    }
                }
                3 => {
                    if let Some((&p, &(id, _))) = model.iter().nth((next() % 4) as usize) {
                        u.udp.close(id).unwrap();
                        model.remove(&p);
                    }
                }
                _ => {
                    let dport = ports[(next() % 3) as usize];
                    let (source, sport) = if next() % 4 == 0 { (B, 0) } else { peers[(next() % 2) as usize] };
                    let (frame_to, destination) = match next() % 3 {
                        0 => (MacAddr::BROADCAST, LIMITED),
                        1 => (MacAddr::multicast(toyos_net_wire::ipv4::MulticastAddr::new(MDNS).unwrap()), MDNS),
                        _ => (MAC_A, A),
                    };
                    let expected = model.get(&dport).map(|&(id, b)| {
                        let accepts = match (destination == A, b) {
                            (true, Binding::Any) => true,
                            (true, Binding::Specific(l)) => l == A,
                            (true, Binding::Connected { local, peer, peer_port }) => local == A && peer == source && Some(peer_port) == toyos_net_wire::Port::new(sport),
                            (false, binding) => binding == Binding::Any,
                        };
                        (id, accepts)
                    });
                    let verdict = u.frame(&eth(frame_to, MAC_B, 0x0800, &udp(source, sport, destination, dport, b"x")));
                    let delivered = matches!(verdict, Some(Verdict::Delivered));
                    assert_eq!(delivered, expected.is_some_and(|(_, a)| a), "seed {seed}: {destination}:{dport} from {source}:{sport}");
                    if let Some((id, true)) = expected {
                        let r = received(&mut u, id).1;
                        if let Some((_, Binding::Connected { peer, peer_port, .. })) = model.get(&dport) {
                            assert_eq!((r.source, r.source_port), (*peer, Some(*peer_port)), "a connected socket hears its peer alone");
                        }
                    }
                    u.out();
                }
            }
            let ports_held: BTreeSet<u16> = model.keys().copied().collect();
            assert_eq!(u.udp.socket_count(), ports_held.len(), "one socket per port");
        }
    }
}

#[test]
fn s_udp_us_056_prop_ip_options_never_surface() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_001).unwrap();
    u.datagram(&hi());
    let plain = received(&mut u, id);
    for with_options in [hex(V_IP_RR), hex(V_IP_NOPS)] {
        u.datagram(&with_options);
        assert_eq!(received(&mut u, id), plain);
    }
    let mut sent = Vec::new();
    for n in 0..64u16 {
        let destination = [B, DNS, MDNS, ip4(198, 51, 100, 7)][usize::from(n % 4)];
        u.udp.send_to(&mut u.ip, id, destination, 1 + n, &vec![7; usize::from(n) * 23]).unwrap();
        sent.extend(u.out());
    }
    assert_eq!(sent.len(), 64);
    assert!(sent.iter().all(|f| ip_of(f).unwrap().header_len() == 20));
}

#[test]
fn s_udp_017_a_closed_port_is_answered() {
    let mut u = U::uf();
    assert_eq!(u.datagram(&hi()), Some(Verdict::NoSocket));
    assert_eq!(ip_bytes(&u.out()[0]), hex(V_ICMP_PORT_UNREACH_GEN));
}

#[test]
fn s_udp_018_the_limited_broadcast_reaches_port_68() {
    let mut u = U::bare();
    let id = u.bind(ANY, 68).unwrap();
    u.frame(&eth(MacAddr::BROADCAST, MAC_R, 0x0800, &udp(R, 67, LIMITED, 68, b"offer")));
    assert_eq!(received(&mut u, id).0, b"offer");
}
