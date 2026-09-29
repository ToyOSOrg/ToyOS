//! ICMPv4 (§9). An echo request to one of our unicast addresses is answered and routed like any
//! datagram. An error is generated only about a datagram to one of our unicast addresses, never
//! about an error, a group or link-group destination, a non-initial fragment or a source that is
//! not one host, and only when the destination's bucket and the global one both have a token.
//! An inbound error is handed on only once its quote names one of our datagrams.

use toyos_net_wire::ethernet::MacClass;
use toyos_net_wire::icmp::{Echo, EchoBuilder, HostUnreachable, IcmpError, IcmpMessage, IcmpPacket, Quote, UnreachableBuilder, UnreachableCode};
use toyos_net_wire::ipv4::{Ecn, Ipv4Builder, Ipv4Packet, Ipv4Source, Protocol, TrafficClass, Ttl};
use toyos_net_wire::{Instant, Port};

use crate::addr::is_host;
use crate::counters::Counter;
use crate::egress::FrameKind;
use crate::route::Source;
use crate::{Arrival, Cast, Delivery, ErrorKind, Flow, IfIndex, Ip, Peer, Transport, TransportError};

/// ICMP types that are errors: never answered with another (RFC 1122 §3.2.2).
const ERROR_TYPES: [u8; 5] = [3, 4, 5, 11, 12];

impl Ip {
    pub(crate) fn icmp<'a>(&mut self, now: Instant, iface: IfIndex, cast: Cast, packet: Ipv4Packet<'a>) -> Option<Delivery<'a>> {
        let peer = Peer::Ip(packet.source());
        let message = match IcmpPacket::parse(packet.payload()) {
            Ok(parsed) => parsed.message(),
            Err(IcmpError::TimestampRequest) => {
                self.log.refuse(Counter::IcmpTimestampRequest, iface, peer);
                return None;
            }
            Err(IcmpError::SourceQuench) => {
                self.log.refuse(Counter::IcmpSourceQuench, iface, peer);
                return None;
            }
            Err(e) => {
                self.log.wire(e.name());
                return None;
            }
        };
        let unicast = cast == Cast::Unicast;
        let kind = match message {
            IcmpMessage::EchoRequest(echo) if unicast => {
                self.echo(now, iface, &packet, &echo);
                return None;
            }
            IcmpMessage::EchoRequest(_) => return self.drop(Counter::IcmpEchoToGroup),
            IcmpMessage::EchoReply(_) => return self.drop(Counter::IcmpEchoReply),
            IcmpMessage::Redirect { .. } => {
                self.log.refuse(Counter::IcmpRedirect, iface, peer);
                return None;
            }
            IcmpMessage::DestinationUnreachable { .. } | IcmpMessage::TimeExceeded { .. } | IcmpMessage::ParameterProblem { .. } if !unicast => {
                return self.drop(Counter::IcmpErrorToGroup);
            }
            IcmpMessage::DestinationUnreachable { code: UnreachableCode::FragmentationNeeded, next_hop_mtu, quote } => {
                (ErrorKind::FragmentationNeeded { next_hop_mtu, quoted_length: quote.total_length() }, quote)
            }
            IcmpMessage::DestinationUnreachable { code, quote, .. } => (ErrorKind::Unreachable(code), quote),
            IcmpMessage::TimeExceeded { code, quote } => (ErrorKind::TimeExceeded(code), quote),
            IcmpMessage::ParameterProblem { code, pointer, quote } => (ErrorKind::ParameterProblem { code, pointer }, quote),
        };
        self.attribute(iface, &packet, kind.0, &kind.1)
    }

    fn drop<'a>(&mut self, counter: Counter) -> Option<Delivery<'a>> {
        self.log.count(counter);
        None
    }

    /// §9.5: the quote names one of our usable addresses, is not a later fragment, and carries a
    /// TCP or UDP header from which the error's 4-tuple is read.
    fn attribute<'a>(&mut self, iface: IfIndex, packet: &Ipv4Packet<'_>, kind: ErrorKind, quote: &Quote<'_>) -> Option<Delivery<'a>> {
        let i = self.ifaces.get(iface.0)?;
        let refusal = if !i.is_usable(quote.source()) {
            Some(Counter::IcmpQuoteNotOurs)
        } else if quote.fragment_offset().units() != 0 {
            Some(Counter::IcmpQuoteNonInitialFragment)
        } else {
            match quote.protocol() {
                Protocol::Tcp | Protocol::Udp => None,
                Protocol::Icmp => Some(Counter::IcmpQuoteIcmp),
                Protocol::Igmp => Some(Counter::IcmpQuoteIgmp),
                Protocol::Other(_) => Some(Counter::IcmpQuoteOtherProtocol),
            }
        };
        if let Some(refusal) = refusal {
            return self.drop(refusal);
        }
        let transport = if quote.protocol() == Protocol::Tcp { Transport::Tcp } else { Transport::Udp };
        let Some(&[s0, s1, d0, d1, q0, q1, q2, q3]) = quote.transport() else { return self.drop(Counter::IcmpQuoteShort) };
        let ports = Port::new(u16::from_be_bytes([s0, s1])).zip(Port::new(u16::from_be_bytes([d0, d1])));
        let Some((source_port, destination_port)) = ports else { return self.drop(Counter::IcmpQuoteNotOurs) };
        let flow = Flow { source: quote.source(), source_port, destination: quote.destination(), destination_port };
        let sequence = (transport == Transport::Tcp).then_some(u32::from_be_bytes([q0, q1, q2, q3]));
        Some(Delivery::Error(TransportError { iface, transport, flow, sequence, kind, reporter: packet.source() }))
    }

    /// §9.2: the reply copies identifier, sequence, data and DSCP, carries no option, ECN 0.
    fn echo(&mut self, now: Instant, iface: IfIndex, packet: &Ipv4Packet<'_>, echo: &Echo<'_>) {
        let Ok(route) = self.route(packet.source(), Source::Bound(packet.destination()), Some(iface)) else { return };
        let Ok(source) = Ipv4Source::new(packet.destination()) else { return };
        let traffic_class = TrafficClass::new(packet.traffic_class().dscp(), Ecn::NotEct).unwrap_or(TrafficClass::ZERO);
        let builder = Ipv4Builder {
            source,
            destination: packet.source(),
            ttl: Ttl::DEFAULT,
            traffic_class,
            options: &[],
            payload: EchoBuilder::reply_to(echo),
        };
        self.own(now, &route, &builder, FrameKind::Echo);
    }

    /// [udp] found no socket for `arrival`: a port unreachable, if suppression and the limiter
    /// let one go (§9.3, §9.4).
    pub fn port_unreachable(&mut self, now: Instant, arrival: &Arrival<'_>) {
        let now = self.clock(now);
        if arrival.cast != Cast::Unicast {
            return self.log.count(Counter::IcmpErrorSuppressed);
        }
        self.error(now, arrival.iface, arrival.link, &arrival.packet, HostUnreachable::Port);
    }

    pub(crate) fn error(&mut self, now: Instant, iface: IfIndex, link: MacClass, packet: &Ipv4Packet<'_>, code: HostUnreachable) {
        let (source, destination) = (packet.source(), packet.destination());
        let about_error = packet.protocol() == Protocol::Icmp && packet.payload().first().is_some_and(|t| ERROR_TYPES.contains(t));
        let suppressed = about_error
            || link != MacClass::Individual
            || destination.is_broadcast()
            || destination.is_multicast()
            || self.is_directed_broadcast(destination)
            || packet.fragment_offset().units() != 0
            || !is_host(source);
        if suppressed {
            return self.log.count(Counter::IcmpErrorSuppressed);
        }
        if !self.limiter.allow(now, source) {
            return self.log.count(Counter::IcmpErrorRateLimited);
        }
        let Ok(route) = self.route(source, Source::Bound(destination), Some(iface)) else { return };
        let Ok(ours) = Ipv4Source::new(destination) else { return };
        let builder = Ipv4Builder {
            source: ours,
            destination: source,
            ttl: Ttl::DEFAULT,
            traffic_class: TrafficClass::ZERO,
            options: &[],
            payload: UnreachableBuilder { code, datagram: packet },
        };
        self.own(now, &route, &builder, FrameKind::Error);
    }
}
