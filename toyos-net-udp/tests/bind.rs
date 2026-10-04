//! §U2: sockets and binding.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_udp::{Binding, Counter, Error, Sender};

fn refused(counter: Counter) -> Result<toyos_net_udp::SocketId, Error> {
    Err(Error::Refused(counter))
}

#[test]
fn s_udp_us_001_one_socket_per_port() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5001).unwrap();
    assert_eq!(u.udp.binding(id), Ok((Binding::Any, port(5001))));
    assert_eq!(u.bind(ANY, 5001), refused(Counter::PortInUse));
}

#[test]
fn s_udp_us_002_whatever_the_address() {
    let mut u = U::uf();
    u.bind(ANY, 5001).unwrap();
    assert_eq!(u.bind(A, 5001), refused(Counter::PortInUse));
    let mut u = U::uf();
    u.bind(A, 5001).unwrap();
    assert_eq!(u.bind(ANY, 5001), refused(Counter::PortInUse));
}

#[test]
fn s_udp_us_003_only_an_assigned_address_binds() {
    let mut u = U::uf();
    for (addr, p) in [
        (Ipv4Addr::new(192, 0, 2, 9), 5001),
        (MDNS, 5353),
        (LIMITED, 68),
        (Ipv4Addr::new(192, 0, 2, 255), 5001),
        (Ipv4Addr::new(127, 0, 0, 1), 5001),
    ] {
        assert_eq!(u.bind(addr, p), refused(Counter::BindAddressNotLocal), "{addr}");
    }
    assert_eq!(u.count(Counter::BindAddressNotLocal), 5);
}

#[test]
fn s_udp_us_004_any_needs_no_address() {
    let mut u = U::bare();
    assert_eq!(u.bind(A, 5001), refused(Counter::BindAddressNotLocal));
    assert!(u.bind(ANY, 68).is_ok());
}

#[test]
fn s_udp_us_005_algorithm_1() {
    let mut u = U::uf();
    u.draws.extend([10_000, 10_000]);
    let first = u.bind(ANY, 0).unwrap();
    let second = u.bind(ANY, 0).unwrap();
    assert_eq!(u.udp.binding(first).unwrap().1, port(59_152));
    assert_eq!(u.udp.binding(second).unwrap().1, port(59_153));
}

#[test]
fn s_udp_us_006_the_candidates_wrap() {
    let mut u = U::uf();
    u.draws.extend([16_383, 16_383]);
    assert_eq!(u.ephemeral(), port(65_535));
    assert_eq!(u.ephemeral(), port(49_152));
}

#[test]
fn s_udp_us_007_the_draw_is_reduced() {
    let mut u = U::uf();
    u.draws.extend([16_384, 0xffff_ffff]);
    assert_eq!(u.ephemeral(), port(49_152));
    assert_eq!(u.ephemeral(), port(65_535));
}

#[test]
fn s_udp_us_008_exhaustion_spends_one_draw() {
    let mut u = U::uf();
    for p in 49_152..=65_535 {
        u.bind(ANY, p).unwrap();
    }
    u.draws.extend([7, 8]);
    assert_eq!(u.bind(ANY, 0), refused(Counter::NoEphemeralPort));
    assert_eq!(u.draws, [8], "exactly one draw");
}

#[test]
fn s_udp_us_009_a_numbered_bind_is_skipped() {
    let mut u = U::uf();
    u.bind(ANY, 59_153).unwrap();
    u.draws.push_back(10_001);
    assert_eq!(u.ephemeral(), port(59_154));
}

#[test]
fn s_udp_us_010_no_privileged_ports() {
    let mut u = U::uf();
    let mdns = u.bind(ANY, 5353).unwrap();
    u.udp.set_ttl(mdns, toyos_net_wire::ipv4::Ttl::new(255).unwrap(), toyos_net_wire::ipv4::Ttl::new(255).unwrap()).unwrap();
    assert_eq!(u.bind(ANY, 5353), refused(Counter::PortInUse));
    assert!(u.bind(ANY, 1).is_ok());
}

#[test]
fn s_udp_us_011_close_frees_the_port_at_once() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5001).unwrap();
    u.udp.close(id).unwrap();
    assert!(u.bind(ANY, 5001).is_ok());
    assert_eq!(u.udp.binding(id), Err(Error::NoSuchSocket), "the old id names nothing");
}

// No id: a closed socket's turn goes with it (architecture §3.3 holds nothing without a bound).
// One the caller has not drained is never offered; one in the caller's round is named once, for
// the caller to take out.
#[test]
fn closing_leaves_no_turn_behind() {
    let mut u = U::uf();
    for _ in 0..1_000 {
        let id = u.bind(ANY, 5001).unwrap();
        u.udp.send_to(&mut u.ip, id, B, 9, b"x").unwrap();
        u.udp.close(id).unwrap();
    }
    assert_eq!(u.udp.drain_eligible().collect::<Vec<_>>(), [Sender::Closed]);
    assert_eq!(u.udp.drain_gone().count(), 0);

    let id = u.bind(ANY, 5002).unwrap();
    u.udp.send_to(&mut u.ip, id, B, 9, b"x").unwrap();
    assert_eq!(u.udp.drain_eligible().collect::<Vec<_>>(), [Sender::Socket(id)]);
    u.udp.close(id).unwrap();
    assert_eq!(u.udp.drain_gone().collect::<Vec<_>>(), [Sender::Socket(id)]);
    assert_eq!(u.udp.drain_eligible().count(), 0, "`closed` is already offered");
}
