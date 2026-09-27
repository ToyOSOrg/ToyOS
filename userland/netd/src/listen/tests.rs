//! A listener's socket on a wire: smoltcp's own `Interface` on an Ethernet
//! device whose far end is played here segment by segment, so what is judged
//! is what the port answers the next peer.

use super::*;
use std::collections::VecDeque;

use smoltcp::iface::{Config, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr,
    HardwareAddress, IpAddress, IpCidr, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, TcpControl, TcpPacket,
    TcpRepr, TcpSeqNumber,
};

const OUR_MAC: EthernetAddress = EthernetAddress([0x02, 0, 0, 0, 0, 0x01]);
const OURS: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
const PEER_MAC: EthernetAddress = EthernetAddress([0x02, 0, 0, 0, 0, 0x02]);
const PEER: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
const PORT: u16 = 22;
/// The peer's initial sequence number, on every connection it opens.
const PEER_ISN: u32 = 1000;

#[derive(Default)]
struct Wire {
    inbound: VecDeque<Vec<u8>>,
    outbound: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        self.0.push(frame);
        result
    }
}

impl Device for Wire {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.inbound.pop_front()?;
        Some((Rx(frame), Tx(&mut self.outbound)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.outbound))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// One TCP segment our side sent: its flags, sequence number and the peer
/// port it went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sent {
    control: TcpControl,
    ack: bool,
    seq: u32,
    to: u16,
}

struct Net {
    iface: Interface,
    wire: Wire,
    sockets: SocketSet<'static>,
    listener: SocketHandle,
    listening: Listening,
    sent: Vec<Sent>,
}

impl Net {
    fn new() -> Self {
        let mut wire = Wire::default();
        let mut iface = Interface::new(Config::new(HardwareAddress::Ethernet(OUR_MAC)), &mut wire, Instant::from_millis(0));
        iface.update_ip_addrs(|addrs| addrs.push(IpCidr::new(IpAddress::Ipv4(OURS), 24)).unwrap());
        let mut socket = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; 4096]), tcp::SocketBuffer::new(vec![0; 4096]));
        socket.listen(PORT).expect("a fresh socket listens");
        let mut sockets = SocketSet::new(Vec::new());
        let listener = sockets.add(socket);
        Self { iface, wire, sockets, listener, listening: Listening::new(PORT), sent: Vec::new() }
    }

    fn socket(&mut self) -> &mut tcp::Socket<'static> {
        self.sockets.get_mut::<tcp::Socket>(self.listener)
    }

    /// One pass of netd's loop: everything the wire holds, in one batch, and
    /// the far end's ARP answered.
    fn pass(&mut self) {
        loop {
            while self.iface.poll(Instant::from_millis(0), &mut self.wire, &mut self.sockets) != PollResult::None {}
            if self.wire.outbound.is_empty() {
                return;
            }
            for frame in std::mem::take(&mut self.wire.outbound) {
                self.far_end(&frame);
            }
        }
    }

    fn far_end(&mut self, frame: &[u8]) {
        let eth = EthernetFrame::new_checked(frame).expect("the interface sent an Ethernet frame");
        match eth.ethertype() {
            EthernetProtocol::Arp => {
                let arp = ArpRepr::parse(&ArpPacket::new_checked(eth.payload()).unwrap()).unwrap();
                let ArpRepr::EthernetIpv4 { operation: ArpOperation::Request, .. } = arp else { return };
                let reply = ArpRepr::EthernetIpv4 {
                    operation: ArpOperation::Reply,
                    source_hardware_addr: PEER_MAC,
                    source_protocol_addr: PEER,
                    target_hardware_addr: OUR_MAC,
                    target_protocol_addr: OURS,
                };
                let mut out = vec![0u8; 14 + reply.buffer_len()];
                let mut e = EthernetFrame::new_unchecked(&mut out);
                EthernetRepr { src_addr: PEER_MAC, dst_addr: OUR_MAC, ethertype: EthernetProtocol::Arp }.emit(&mut e);
                reply.emit(&mut ArpPacket::new_unchecked(e.payload_mut()));
                self.wire.inbound.push_back(out);
            }
            EthernetProtocol::Ipv4 => {
                let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
                assert_eq!(ip.next_header(), IpProtocol::Tcp, "a listener sends only TCP");
                let tcp = TcpPacket::new_checked(ip.payload()).unwrap();
                let control = match (tcp.syn(), tcp.fin(), tcp.rst()) {
                    (true, _, _) => TcpControl::Syn,
                    (_, true, _) => TcpControl::Fin,
                    (_, _, true) => TcpControl::Rst,
                    _ => TcpControl::None,
                };
                self.sent.push(Sent { control, ack: tcp.ack(), seq: tcp.seq_number().0 as u32, to: tcp.dst_port() });
            }
            other => panic!("the interface sent a frame of type {other}"),
        }
    }

    /// A segment from the peer's `from` port onto the wire, delivered at the
    /// next [`Net::pass`].
    fn send(&mut self, from: u16, control: TcpControl, seq: u32, ack: Option<u32>) {
        let caps = ChecksumCapabilities::default();
        let tcp = TcpRepr {
            src_port: from,
            dst_port: PORT,
            control,
            seq_number: TcpSeqNumber(seq as i32),
            ack_number: ack.map(|a| TcpSeqNumber(a as i32)),
            window_len: 64000,
            window_scale: None,
            max_seg_size: None,
            sack_permitted: false,
            sack_ranges: [None, None, None],
            timestamp: None,
            payload: &[],
        };
        let ip = Ipv4Repr {
            src_addr: PEER,
            dst_addr: OURS,
            next_header: IpProtocol::Tcp,
            payload_len: tcp.buffer_len(),
            hop_limit: 64,
        };
        let mut out = vec![0u8; 14 + ip.buffer_len() + ip.payload_len];
        let mut e = EthernetFrame::new_unchecked(&mut out);
        EthernetRepr { src_addr: PEER_MAC, dst_addr: OUR_MAC, ethertype: EthernetProtocol::Ipv4 }.emit(&mut e);
        let mut packet = Ipv4Packet::new_unchecked(e.payload_mut());
        ip.emit(&mut packet, &caps);
        tcp.emit(&mut TcpPacket::new_unchecked(packet.payload_mut()), &IpAddress::Ipv4(PEER), &IpAddress::Ipv4(OURS), &caps);
        self.wire.inbound.push_back(out);
    }

    /// The peer's SYN from `from`, and the SYN-ACK's sequence number.
    fn syn(&mut self, from: u16) -> u32 {
        self.send(from, TcpControl::Syn, PEER_ISN, None);
        self.pass();
        self.sent
            .iter()
            .rev()
            .find(|s| s.control == TcpControl::Syn && s.ack && s.to == from)
            .unwrap_or_else(|| panic!("a SYN from {from} was answered {:?}", self.sent.last()))
            .seq
    }

    /// The peer's answer to the SYN-ACK and `then`, in one batch.
    fn ack_and(&mut self, from: u16, our_isn: u32, then: TcpControl) {
        self.send(from, TcpControl::None, PEER_ISN + 1, Some(our_isn + 1));
        self.send(from, then, PEER_ISN + 1, Some(our_isn + 1));
        self.pass();
    }

    /// Whether the port answers a new peer's SYN with a SYN-ACK: it listens.
    fn listens(&mut self, from: u16) -> bool {
        self.send(from, TcpControl::Syn, PEER_ISN, None);
        self.pass();
        let answer = *self.sent.iter().rev().find(|s| s.to == from).expect("a SYN is answered");
        answer.control == TcpControl::Syn
    }

    /// Whether the owner is woken on a pass with `room` or without.
    fn wakes(&mut self, room: bool) -> bool {
        match self.listening.wake(self.sockets.get_mut::<tcp::Socket>(self.listener), room) {
            [] => false,
            [1] => true,
            other => panic!("a wake of {other:?}"),
        }
    }

    fn accept(&mut self, room: bool, pipes: bool) -> Accept<()> {
        self.listening.accept(self.sockets.get_mut::<tcp::Socket>(self.listener), room, pipes.then_some(()))
    }
}

#[test]
fn a_finished_handshake_is_owed_one_wake() {
    let mut net = Net::new();
    let isn = net.syn(5001);
    assert!(!net.wakes(true), "a SYN alone was taken for a connection");
    net.send(5001, TcpControl::None, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(net.wakes(true));
    assert!(!net.wakes(true), "a connection its owner holds a wake for was announced twice");
    assert_eq!(net.accept(true, true), Accept::Take(()));
}

/// **A peer that sends its FIN with the handshake's last ACK is still a
/// connection.** Both land in one pass, so the socket goes from `SynReceived`
/// to `CloseWait` with no pass ever seeing it `Established`.
#[test]
fn a_peer_that_closes_with_its_last_ack_is_a_connection() {
    let mut net = Net::new();
    let isn = net.syn(5001);
    net.ack_and(5001, isn, TcpControl::Fin);
    assert_eq!(net.socket().state(), tcp::State::CloseWait, "the premise: both in one pass");
    assert!(net.wakes(true), "a connection the peer half-closed was never announced");
    assert_eq!(net.accept(true, true), Accept::Take(()), "a connection the peer half-closed was not handed over");
}

/// A peer that resets before its owner took it frees the port: the next peer
/// is answered a SYN-ACK and not a reset.
#[test]
fn a_peer_that_resets_before_it_is_taken_frees_the_port() {
    let mut net = Net::new();
    let isn = net.syn(5001);
    net.ack_and(5001, isn, TcpControl::Rst);
    assert_eq!(net.socket().state(), tcp::State::Closed, "the premise: both in one pass");
    assert!(!net.wakes(true));
    assert!(net.listens(5002), "the port answered the next peer {:?}", net.sent.last());
}

/// A wake written for a connection its peer then reset is spent by the
/// accept that finds nothing, and the next connection is announced.
#[test]
fn a_wake_spent_on_a_reset_connection_announces_the_next() {
    let mut net = Net::new();
    let isn = net.syn(5001);
    net.send(5001, TcpControl::None, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(net.wakes(true));
    net.send(5001, TcpControl::Rst, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(!net.wakes(true), "a reset connection was announced");
    assert_eq!(net.accept(true, true), Accept::Nothing, "an accept took a connection its peer had reset");
    let isn = net.syn(5002);
    net.send(5002, TcpControl::None, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(net.wakes(true), "the connection after a reset one was never announced");
}

/// An accept that handed netd no pipes spends the owner's wake, and the
/// connection it left is announced again at once.
#[test]
fn an_accept_refused_for_its_pipes_is_woken_again() {
    let mut net = Net::new();
    let isn = net.syn(5001);
    net.send(5001, TcpControl::None, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(net.wakes(true));
    assert_eq!(net.accept(true, false), Accept::NoPipes);
    assert!(net.wakes(true), "an owner refused for its pipes was never woken again");
    assert_eq!(net.accept(true, true), Accept::Take(()));
}

/// An accept refused for room spends the owner's wake, and the connection it
/// left is announced again once room returns, and not before.
#[test]
fn an_accept_refused_for_room_is_woken_again_when_room_returns() {
    let mut net = Net::new();
    assert_eq!(net.accept(false, true), Accept::Nothing, "an accept with nothing waiting was refused for room");
    let isn = net.syn(5001);
    net.send(5001, TcpControl::None, PEER_ISN + 1, Some(isn + 1));
    net.pass();
    assert!(!net.wakes(false), "the owner was woken for a connection there is no room to take");
    assert!(net.wakes(true));
    assert_eq!(net.accept(false, true), Accept::NoRoom);
    assert!(!net.wakes(false), "an owner refused for room was woken again with room still gone");
    assert!(net.wakes(true), "an owner refused for room was never woken again");
    assert_eq!(net.accept(true, true), Accept::Take(()));
}
