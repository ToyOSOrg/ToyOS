//! Ephemeral ports (tcp.md §18.4): RFC 6056 Algorithm 4 for connects, Algorithm 2 for listeners.

mod common;

use std::collections::HashSet;
use std::net::Ipv4Addr;

use common::*;
use toyos_net_tcp::{siphash24, Counter, Error, Instant, Tcp};

fn stack() -> Tcp {
    Tcp::new(config(65_535))
}

fn now() -> Instant {
    Instant::from_millis(1_000)
}

fn local_port(tcp: &mut Tcp, remote: (Ipv4Addr, u16)) -> u16 {
    let id = tcp.connect(now(), A, None, ep(remote.0, remote.1)).unwrap();
    tcp.tuple(id).unwrap().local.port.get()
}

#[test]
fn s_port_001_algorithm_4() {
    let mut tcp = stack();
    let ports: Vec<u16> = (0..3).map(|_| local_port(&mut tcp, (B, 80))).collect();
    assert_eq!(ports, [52289, 52290, 52291]);
}

#[test]
fn s_port_002_another_destination_another_counter() {
    let mut tcp = stack();
    for _ in 0..3 {
        local_port(&mut tcp, (B, 80));
    }
    assert_eq!(local_port(&mut tcp, (B, 443)), 64525);
    assert_eq!(local_port(&mut tcp, (B, 80)), 52292, "table[10] stood at 3");
}

#[test]
fn s_port_003_a_listening_port_is_skipped() {
    let mut tcp = stack();
    tcp.listen(Ipv4Addr::UNSPECIFIED, Some(port(52289)), || 0).unwrap();
    assert_eq!(local_port(&mut tcp, (B, 80)), 52290);
    assert_eq!(local_port(&mut tcp, (B, 80)), 52291, "table[10] ended at 2");
}

#[test]
fn s_port_004_time_wait_holds_its_4_tuple() {
    let mut h = H::new(65_535);
    h.local = (A, 52289);
    h.start(-10);
    let id = h.tcp.connect(h.now(), A, Some(port(52289)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    h.transmit();
    h.input(0, seg(5000).ack(1001).syn().mss(1460));
    h.close(0);
    h.input(10, seg(5001).ack(1002).fin());
    assert_eq!(h.tcp.time_wait_count(), 1);
    let now = h.now();
    let next = h.tcp.connect(now, A, None, ep(B, 80)).unwrap();
    assert_eq!(h.tcp.tuple(next).unwrap().local.port.get(), 52290);
}

#[test]
fn s_port_005_exhaustion() {
    let mut tcp = stack();
    for p in 49152..=65535 {
        tcp.listen(Ipv4Addr::UNSPECIFIED, Some(port(p)), || 0).unwrap();
    }
    assert_eq!(tcp.connect(now(), A, None, ep(B, 80)), Err(Error::AddrInUse));
    assert_eq!(tcp.counters().get(Counter::NoEphemeralPort), 1);
}

#[test]
fn s_port_006_algorithm_2_for_a_listener() {
    let mut tcp = stack();
    tcp.listen(Ipv4Addr::UNSPECIFIED, Some(port(60000)), || 0).unwrap();
    let mut draws = [60000u16, 50001].into_iter();
    let id = tcp.listen(Ipv4Addr::UNSPECIFIED, None, || draws.next().unwrap()).unwrap();
    assert_eq!(tcp.listener_port(id).unwrap().get(), 50001);
    let mut state = 7u16;
    for _ in 0..1000 {
        let id = tcp
            .listen(Ipv4Addr::UNSPECIFIED, None, || {
                state = state.wrapping_mul(31).wrapping_add(17);
                state
            })
            .unwrap();
        assert!((49152..=65535).contains(&tcp.listener_port(id).unwrap().get()));
    }
}

#[test]
fn s_port_007_never_a_self_connection() {
    let mut tcp = stack();
    assert_eq!(tcp.connect(now(), A, Some(port(52289)), ep(A, 52289)), Err(Error::InvalidRemote));
    assert_eq!(tcp.counters().get(Counter::SelfConnect), 1);
    let q = 50_000u16;
    let input = [192, 0, 2, 1, 192, 0, 2, 1, (q >> 8) as u8, q as u8];
    let offset = siphash24(&key(0x20), &input) as u32;
    let index = (siphash24(&key(0x30), &input) as u32 & 15) as usize;
    let mut secrets = secrets();
    secrets.port_table[index] = (u32::from(q - 49152).wrapping_sub(offset) % 16384) as u16;
    let mut tcp = Tcp::new(toyos_net_tcp::Config { secrets, ..config(65_535) });
    let id = tcp.connect(now(), A, None, ep(A, q)).unwrap();
    assert_ne!(tcp.tuple(id).unwrap().local.port.get(), q, "the first candidate named the destination itself");
}

#[test]
fn s_port_008_prop_ports() {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut tcp = stack();
    let mut listened = HashSet::new();
    for _ in 0..200 {
        let p = 49152 + (next() % 16384) as u16;
        if tcp.listen(Ipv4Addr::UNSPECIFIED, Some(port(p)), || 0).is_ok() {
            listened.insert(p);
        }
    }
    let mut held = HashSet::new();
    for _ in 0..10_000 {
        let remote = ep(Ipv4Addr::new(198, 51, 100, (next() % 4) as u8 + 1), 1 + (next() % 3) as u16);
        let id = tcp.connect(now(), A, None, remote).unwrap();
        let tuple = tcp.tuple(id).unwrap();
        let p = tuple.local.port.get();
        assert!((49152..=65535).contains(&p));
        assert!(!listened.contains(&p), "{p} is listened on");
        assert!(held.insert(tuple), "{tuple:?} repeats");
    }
}
