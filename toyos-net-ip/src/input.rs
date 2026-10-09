//! Frame dispatch and IPv4 input. The first failing step names the
//! refusal, so counters are deterministic, and a transport is handed only what passed them all.

use core::net::Ipv4Addr;

use toyos_net_wire::addr::{is_host, is_martian};
use toyos_net_wire::arp::Arp;
use toyos_net_wire::ethernet::{EtherType, Frame, MacAddr, MacClass, TagProtocol, Tags};
use toyos_net_wire::icmp::HostUnreachable;
use toyos_net_wire::igmp::IgmpPacket;
use toyos_net_wire::ipv4::{Ipv4Option, Ipv4Packet, MulticastAddr, Protocol};
use toyos_net_wire::tcp::TcpSegment;
use toyos_net_wire::udp::UdpDatagram;
use toyos_net_wire::Instant;

use crate::counters::Counter;
use crate::iface::{link_local_edge, Interface};
use crate::{arp, igmp, Arrival, Cast, Delivery, IfIndex, Ip, Peer};

const IPV6: u16 = 0x86DD;
const LOOSE_SOURCE_ROUTE: u8 = 0x83;
const STRICT_SOURCE_ROUTE: u8 = 0x89;
/// The DHCP client's port, which the acquisition exception admits (RFC 2131 §2).
const ACQUISITION_PORT: u16 = 68;

/// Frame dispatch, in order: EtherType, destination, tags, own source.
fn dispatch(i: &Interface, frame: &Frame<'_>) -> Option<Counter> {
    let destination = frame.destination();
    let for_us = destination == i.mac.get()
        || destination == MacAddr::BROADCAST
        || destination == MacAddr::multicast(MulticastAddr::ALL_HOSTS)
        || i.igmp.accepts(destination);
    let untagged = match frame.tags() {
        Tags::Untagged => true,
        Tags::Single(tag) => tag.protocol() == TagProtocol::Customer && tag.vlan_id() == 0,
        Tags::Double { .. } => false,
    };
    match frame.ether_type() {
        EtherType::Other(other) if other.value() == IPV6 => Some(Counter::EthIpv6),
        EtherType::Other(_) => Some(Counter::EthUnknownType),
        EtherType::Ipv4 | EtherType::Arp if !for_us => Some(Counter::EthNotForUs),
        EtherType::Ipv4 | EtherType::Arp if !untagged => Some(Counter::EthVlan),
        EtherType::Ipv4 | EtherType::Arp if frame.source() == i.mac => Some(Counter::EthOwnSource),
        EtherType::Ipv4 | EtherType::Arp => None,
    }
}

/// The acquisition exception: before the interface holds a usable address, a datagram to
/// UDP port 68 whose destination names one host, and so came in a frame to our MAC, is
/// admitted for the DHCP client alone.
fn acquisition(i: &Interface, packet: &Ipv4Packet<'_>) -> bool {
    is_host(packet.destination())
        && i.usable().next().is_none()
        && packet.protocol() == Protocol::Udp
        && !packet.is_fragment()
        && UdpDatagram::parse(packet).is_ok_and(|d| d.destination_port().get() == ACQUISITION_PORT)
}

/// The IPv4 input policy, in order, after the header parsed.
fn admit(ifaces: &[Interface], i: &Interface, link: MacClass, packet: &Ipv4Packet<'_>) -> Result<Cast, Counter> {
    let (destination, source) = (packet.destination(), packet.source());
    let group = destination.is_broadcast() || destination.is_multicast() || i.is_directed_broadcast(destination);
    match link {
        MacClass::Broadcast if !group => return Err(Counter::IpUnicastInLinkBroadcast),
        MacClass::Group if !group => return Err(Counter::IpUnicastInLinkMulticast),
        MacClass::Individual | MacClass::Broadcast | MacClass::Group => {}
    }
    if is_martian(destination) {
        return Err(Counter::IpMartianDestination);
    }
    let cast = if i.is_usable(destination) {
        Cast::Unicast
    } else if destination.is_broadcast() {
        Cast::LimitedBroadcast
    } else if i.is_directed_broadcast(destination) {
        Cast::SubnetBroadcast
    } else if let Some(g) = MulticastAddr::new(destination).filter(|g| i.igmp.joined(*g)) {
        Cast::Multicast(g)
    } else if i.owns(destination) {
        return Err(Counter::IpTentativeDestination);
    } else if acquisition(i, packet) {
        Cast::Acquisition
    } else {
        return Err(Counter::IpNotForUs);
    };
    let unspecified_igmp = source == Ipv4Addr::UNSPECIFIED && packet.protocol() == Protocol::Igmp;
    if !unspecified_igmp && (!is_host(source) || link_local_edge(source) || i.usable().any(|a| a.cidr.is_edge(source))) {
        return Err(Counter::IpInvalidSource);
    }
    if ifaces.iter().any(|j| j.owns(source)) {
        return Err(Counter::IpOwnSource);
    }
    if packet.is_fragment() {
        return Err(Counter::IpFragment);
    }
    let source_routed = packet.options().iter().any(|o| {
        matches!(o, Ipv4Option::Other { kind, .. } if kind.value() == LOOSE_SOURCE_ROUTE || kind.value() == STRICT_SOURCE_ROUTE)
    });
    if source_routed {
        return Err(Counter::IpSourceRoute);
    }
    Ok(cast)
}

impl Ip {
    /// A frame the device received on `iface`. What a transport must act on comes back borrowing
    /// the frame; everything else — ARP, ICMP, IGMP, refusals — [ip] handles itself.
    pub fn receive<'a>(&mut self, now: Instant, iface: IfIndex, frame: &'a [u8]) -> Option<Delivery<'a>> {
        let now = self.clock(now);
        let frame = Frame::parse(frame).map_err(|e| self.log.wire(e.name())).ok()?;
        let Some(i) = self.ifaces.get(iface.0) else {
            self.log.count(Counter::UnknownInterface);
            return None;
        };
        if let Some(refusal) = dispatch(i, &frame) {
            self.log.count(refusal);
            return None;
        }
        if frame.ether_type() == EtherType::Arp {
            let parsed = Arp::parse(frame.body()).map_err(|e| self.log.wire(e.name())).ok()?;
            let (i, mut cx) = self.split(now, iface)?;
            arp::receive(i, &mut cx, &parsed);
            return None;
        }
        self.ipv4(now, iface, frame.destination().class(), frame.body())
    }

    fn ipv4<'a>(&mut self, now: Instant, iface: IfIndex, link: MacClass, body: &'a [u8]) -> Option<Delivery<'a>> {
        let packet = Ipv4Packet::parse(body).map_err(|e| self.log.wire(e.name())).ok()?;
        let i = self.ifaces.get(iface.0)?;
        let cast = match admit(&self.ifaces, i, link, &packet) {
            Ok(cast) => cast,
            Err(refusal) => {
                self.log.refuse(refusal, iface, Peer::Ip(packet.source()));
                return None;
            }
        };
        let arrival = Arrival { iface, packet, cast, link };
        match packet.protocol() {
            Protocol::Icmp => self.icmp(now, iface, cast, packet),
            Protocol::Igmp => {
                let message = IgmpPacket::parse(packet.payload()).map_err(|e| self.log.wire(e.name())).ok()?;
                let (i, mut cx) = self.split(now, iface)?;
                igmp::input(i, &mut cx, &packet, message.message());
                None
            }
            Protocol::Tcp if cast != Cast::Unicast => {
                self.log.count(Counter::IpTcpNotUnicast);
                None
            }
            Protocol::Tcp => {
                let segment = TcpSegment::parse(&packet).map_err(|e| self.log.wire(e.name())).ok()?;
                Some(Delivery::Tcp(arrival, segment))
            }
            Protocol::Udp => {
                let datagram = UdpDatagram::parse(&packet).map_err(|e| self.log.wire(e.name())).ok()?;
                if cast == Cast::Acquisition {
                    self.log.count(Counter::IpAcquisitionAdmitted);
                }
                Some(Delivery::Udp(arrival, datagram))
            }
            Protocol::Other(_) => {
                self.log.count(Counter::IpProtocol);
                if cast == Cast::Unicast {
                    self.error(now, iface, link, &packet, HostUnreachable::Protocol);
                }
                None
            }
        }
    }
}
