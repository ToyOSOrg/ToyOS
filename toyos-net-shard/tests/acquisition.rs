//! The DHCP client's socket and the address taken away, through the shard: what a lease's holder
//! calls. No scenario ids: the specifications' DH-07 and DH-75 are `toyos-dhcp`'s, on [ip] and
//! [udp] directly; these are the same rules reached through `Shard`.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_shard::{Config, Event, Secrets, Shard};
use toyos_net_udp::{Counter, Error};
use toyos_net_wire::arp::Arp;
use toyos_net_wire::ethernet::{EtherType, Frame, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::Ipv4Packet;
use toyos_net_wire::udp::UdpDatagram;
use toyos_net_wire::Instant;

/// A's shard an hour into the clock, its link up and no address on it.
fn unaddressed() -> (Shard, Instant) {
    let secrets = Secrets {
        ip: [1; 16],
        resets: [2; 16],
        tcp: toyos_net_tcp::Secrets { isn: [3; 16], timestamp: [4; 16], port_offset: [5; 16], port_index: [6; 16], port_table: [0; 16] },
    };
    let now = Instant::from_millis(3_600_000);
    let mac = IndividualMac::new(MAC_A).unwrap();
    let mut shard = Shard::new(now, Config { mac, receive_buffer: 65_535, send_buffer: 65_535, secrets }).unwrap();
    shard.link_up(now).unwrap();
    (shard, now)
}

fn sent(shard: &mut Shard, now: Instant) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    shard.transmit(now, usize::MAX, |frame| frames.push(frame.to_vec()));
    frames
}

/// A's address verified, and the time it was.
fn verified(shard: &mut Shard, mut now: Instant) -> Instant {
    shard.add_address(now, A, 24).unwrap();
    while !shard.drain_events().any(|e| e == Event::Verified(A)) {
        sent(shard, now);
        now = shard.next_deadline().expect("conflict detection is still running");
        shard.fire(now);
    }
    now
}

/// A UDP datagram from the router's port 67 to `destination`'s port 68 in a frame to `to`.
fn from_router(to: MacAddr, destination: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(8 + payload.len()).unwrap().to_be_bytes();
    let mut udp = vec![0, 67, 0, 68, length[0], length[1], 0, 0];
    udp.extend_from_slice(payload);
    let pseudo = [&R.octets()[..], &destination.octets(), &[0, 17, length[0], length[1]], &udp].concat();
    let check = sum(&pseudo);
    udp[6..8].copy_from_slice(&check.to_be_bytes());
    let total = u16::try_from(20 + udp.len()).unwrap().to_be_bytes();
    let mut ip = vec![0x45, 0, total[0], total[1], 0, 0, 0x40, 0, 64, 17, 0, 0];
    ip.extend_from_slice(&R.octets());
    ip.extend_from_slice(&destination.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    let mut frame = [to.0, MAC_R.0].concat();
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&udp);
    frame
}

/// An ARP request for `target` from the router.
fn who_has(target: Ipv4Addr) -> Vec<u8> {
    let mut frame = [MacAddr::BROADCAST.0, MAC_R.0].concat();
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0, 6, 4, 0, 1]);
    frame.extend_from_slice(&MAC_R.0);
    frame.extend_from_slice(&R.octets());
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&target.octets());
    frame
}

#[test]
fn the_acquisition_socket_sends_from_no_address_to_the_limited_broadcast() {
    let (mut shard, now) = unaddressed();
    let socket = shard.acquisition().unwrap();
    shard.send_from(now, socket, Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 67, b"discover").unwrap();
    let frames = sent(&mut shard, now);
    let [frame] = &frames[..] else { panic!("one frame, not {}", frames.len()) };
    let frame = Frame::parse(frame).unwrap();
    assert_eq!(frame.destination(), MacAddr::BROADCAST);
    let ip = Ipv4Packet::parse(frame.body()).unwrap();
    assert_eq!((ip.source(), ip.destination()), (Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST));
    let udp = UdpDatagram::parse(&ip).unwrap();
    assert_eq!((udp.source_port(), udp.destination_port(), udp.payload()), (Some(port(68)), port(67), &b"discover"[..]));
}

#[test]
fn the_acquisition_socket_hears_a_datagram_to_an_address_not_yet_held() {
    let (mut shard, now) = unaddressed();
    let socket = shard.acquisition().unwrap();
    shard.receive(now, &from_router(MAC_A, A, b"offer"));
    let mut buf = [0u8; 16];
    let received = shard.recv_from(socket, &mut buf).unwrap().expect("the acquisition exception admits it");
    assert_eq!((&buf[..received.len], received.source, received.source_port), (&b"offer"[..], R, Some(port(67))));
}

#[test]
fn only_the_acquisition_socket_names_its_source() {
    let (mut shard, now) = unaddressed();
    let now = verified(&mut shard, now);
    sent(&mut shard, now);
    let plain = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
    let acquisition = shard.acquisition().unwrap();
    // A source the acquisition socket may name: an assigned address, to one host.
    assert_eq!(shard.send_from(now, plain, A, R, 67, b"request"), Err(Error::Refused(Counter::SourceNotPermitted)));
    assert_eq!(shard.send_from(now, acquisition, A, R, 67, b"request"), Ok(()));
}

#[test]
fn a_second_acquisition_socket_is_refused_its_port() {
    let (mut shard, _) = unaddressed();
    shard.acquisition().unwrap();
    assert_eq!(shard.acquisition(), Err(Error::Refused(Counter::PortInUse)));
}

#[test]
fn a_removed_address_takes_its_gateway_and_answers_nothing() {
    let (mut shard, now) = unaddressed();
    let now = verified(&mut shard, now);
    shard.set_gateways(now, &[R]).unwrap();
    sent(&mut shard, now);
    shard.receive(now, &who_has(A));
    let answered = sent(&mut shard, now);
    let replies = |frames: &[Vec<u8>]| {
        let arp = |f: &Vec<u8>| Frame::parse(f).ok().filter(|f| f.ether_type() == EtherType::Arp).and_then(|f| Arp::parse(f.body()).ok());
        frames.iter().filter_map(arp).filter(|a| a.sender_ip == A && a.target_ip == R).count()
    };
    assert_eq!(replies(&answered), 1, "held, the address answers");

    shard.remove_address(now, A).unwrap();
    assert_eq!(shard.ip().address(shard.iface(), A), None);
    assert_eq!(shard.ip().gateways(shard.iface()), Some(&[] as &[Ipv4Addr]));
    shard.receive(now, &who_has(A));
    assert_eq!(replies(&sent(&mut shard, now)), 0, "removed, it answers nothing");
    assert_eq!(shard.remove_address(now, A), Err(toyos_net_ip::Counter::AddrUnknown));
}
