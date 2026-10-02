//! DH-07 and DH-75: server messages reaching the client through `toyos-net-ip` and
//! `toyos-net-udp`, as netd will compose them.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Cast, Delivery, Ip};
use toyos_net_udp::{SocketId, Udp, Verdict};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::{Instant, Port};

fn sum(data: &[u8]) -> u16 {
    let mut s: u32 = 0;
    for chunk in data.chunks(2) {
        s += if chunk.len() == 2 { u32::from(chunk[0]) << 8 | u32::from(chunk[1]) } else { u32::from(chunk[0]) << 8 };
        while s > 0xffff {
            s = (s & 0xffff) + (s >> 16);
        }
    }
    s as u16
}

/// A UDP datagram 67 → 68 around `payload`, checksums by the sum above, framed from the router.
fn from_server(to_mac: MacAddr, destination: Ipv4Addr, dport: u16, payload: &[u8]) -> Vec<u8> {
    let length = 8 + payload.len();
    let mut udp = vec![0, 67, (dport >> 8) as u8, dport as u8, (length >> 8) as u8, length as u8, 0, 0];
    udp.extend_from_slice(payload);
    let mut pseudo = R.octets().to_vec();
    pseudo.extend_from_slice(&destination.octets());
    pseudo.extend_from_slice(&[0, 17, (length >> 8) as u8, length as u8]);
    pseudo.extend_from_slice(&udp);
    udp[6..8].copy_from_slice(&(!sum(&pseudo)).to_be_bytes());
    let total = 20 + length;
    let mut ip = vec![0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, 17, 0, 0];
    ip.extend_from_slice(&R.octets());
    ip.extend_from_slice(&destination.octets());
    let c = !sum(&ip);
    ip[10..12].copy_from_slice(&c.to_be_bytes());
    ip.extend_from_slice(&udp);
    let mut frame = to_mac.0.to_vec();
    frame.extend_from_slice(&MAC_R.0);
    frame.extend_from_slice(&[8, 0]);
    frame.extend_from_slice(&ip);
    frame
}

struct Stack {
    ip: Ip,
    udp: Udp,
    socket: SocketId,
    if0: toyos_net_ip::IfIndex,
}

impl Stack {
    /// A's interface up with no address, netd's acquisition socket on port 68.
    fn new() -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), [0x5a; 16]);
        let if0 = ip.add_interface(Instant::from_nanos(0), IndividualMac::new(MAC_A).unwrap());
        ip.link_up(Instant::from_nanos(0), if0).unwrap();
        let mut udp = Udp::new();
        let socket = udp.bind(&ip, Ipv4Addr::UNSPECIFIED, Port::new(68), || 0).unwrap();
        udp.set_acquisition(socket).unwrap();
        Self { ip, udp, socket, if0 }
    }

    /// A frame arrives: what reached the socket, and how [ip] admitted it.
    fn deliver(&mut self, now: u64, frame: &[u8]) -> Option<(Vec<u8>, Cast)> {
        let Some(Delivery::Udp(arrival, datagram)) = self.ip.receive(at(now), self.if0, frame) else { return None };
        assert_eq!(self.udp.receive(at(now), &arrival, &datagram), Verdict::Delivered);
        let mut buf = [0u8; 1_500];
        let r = self.udp.recv(self.socket, &mut buf).unwrap().unwrap();
        Some((buf[..r.len].to_vec(), arrival.cast))
    }
}

#[test]
fn s_dhcp_dh_007_either_framing_of_the_offer() {
    let broadcast = from_server(MacAddr::BROADCAST, Ipv4Addr::BROADCAST, 68, &offer());
    for (frame, cast) in [(broadcast, Cast::LimitedBroadcast), (hex(V_DHCP_OFFER_FRAME), Cast::Acquisition)] {
        let mut stack = Stack::new();
        let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
        let (payload, admitted) = stack.deliver(10, &frame).expect("reaches the socket");
        assert_eq!(admitted, cast);
        let out = d.receive(10, &payload);
        assert_eq!(sent(&out).payload, payload_of(V_DHCP_REQUEST));
    }
}

#[test]
fn s_dhcp_dh_075_the_acquisition_exception_ends_with_an_address() {
    let mut stack = Stack::new();
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    let (payload, cast) = stack.deliver(10, &hex(V_DHCP_OFFER_FRAME)).expect("admitted");
    assert_eq!(cast, Cast::Acquisition);
    assert_eq!(message_type(sent(&d.receive(10, &payload))), 3);

    stack.ip.add_address(at(20), stack.if0, A, 24).unwrap();
    while let Some(deadline) = stack.ip.next_deadline().filter(|t| *t <= at(3_000)) {
        stack.ip.fire(deadline);
        stack.ip.transmit(deadline, usize::MAX, |_, _| {});
    }
    assert!(stack.ip.is_assigned(A));
    let before = stack.ip.counters().get(toyos_net_ip::Counter::IpNotForUs);
    let elsewhere = from_server(MAC_A, Ipv4Addr::new(192, 0, 2, 7), 68, &offer());
    assert!(stack.deliver(3_000, &elsewhere).is_none());
    assert_eq!(stack.ip.counters().get(toyos_net_ip::Counter::IpNotForUs), before + 1);
}
