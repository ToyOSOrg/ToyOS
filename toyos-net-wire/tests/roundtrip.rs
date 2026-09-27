mod common;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::arp::{Arp, Operation};
use toyos_net_wire::checksum::PseudoHeader;
use toyos_net_wire::ethernet::{EtherType, Frame, FrameBody, FrameBuilder, IndividualMac, MacAddr, Tags};
use toyos_net_wire::icmp::{EchoBuilder, EchoKind, HostUnreachable, IcmpMessage, IcmpPacket, UnreachableBuilder, UnreachableCode};
use toyos_net_wire::igmp::{IgmpMessage, IgmpPacket, ReportGroup, V2Builder, V2Kind};
use toyos_net_wire::ipv4::{
    Form, FragmentOffset, Ipv4Builder, Ipv4Option, Ipv4Packet, Ipv4Payload, Ipv4Source, MulticastAddr, OptionKind, Protocol,
    RawPayload, TrafficClass, Ttl,
};
use toyos_net_wire::tcp::{Control, EstablishedOptions, RawWindow, SackBlock, SeqNum, SynOptions, TcpBuilder, TcpFlags, TcpSegment, Timestamps, WindowShift};
use toyos_net_wire::udp::{UdpBuilder, UdpDatagram};
use toyos_net_wire::{BuildError, Port};

const FUZZ_ITERATIONS: usize = 500_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.byte()).collect()
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let (outer, inner) = (outer.as_ptr_range(), inner.as_ptr_range());
    outer.start <= inner.start && inner.end <= outer.end
}

fn concat(pieces: &[&[u8]]) -> Vec<u8> {
    pieces.concat()
}

#[derive(Debug, PartialEq)]
struct Datagram {
    source: Ipv4Addr,
    destination: Ipv4Addr,
    ttl: u8,
    traffic_class: u8,
    protocol: u8,
    dont_fragment: bool,
    more_fragments: bool,
    offset: u16,
    identification: Option<u16>,
    options: Vec<String>,
    payload: Payload,
}

#[derive(Debug, PartialEq)]
enum Payload {
    Udp { source: u16, destination: u16, data: Vec<u8> },
    Tcp(Segment),
    Echo { reply: bool, identifier: u16, sequence: u16, data: Vec<u8> },
    Unreachable { code: u8, quote: Vec<u8> },
    Igmp(String),
    Raw(Vec<u8>),
    Refused(&'static str),
}

#[derive(Debug, PartialEq)]
struct Segment {
    ports: (u16, u16),
    sequence: u32,
    acknowledgment: Option<u32>,
    flags: u8,
    window: u16,
    mss: Option<u16>,
    window_scale: Option<u8>,
    sack_permitted: bool,
    timestamps: Option<Timestamps>,
    sack: Vec<SackBlock>,
    data: Vec<u8>,
}

fn segment_of(s: &TcpSegment<'_>) -> Segment {
    let o = s.options();
    Segment {
        ports: (s.source_port().get(), s.destination_port().get()),
        sequence: s.sequence().get(),
        acknowledgment: s.acknowledgment().map(SeqNum::get),
        flags: s.flags().bits(),
        window: s.window().0,
        mss: o.mss(),
        window_scale: o.window_scale().map(|w| w.raw()),
        sack_permitted: o.sack_permitted(),
        timestamps: o.timestamps(),
        sack: o.sack_blocks().collect(),
        data: s.payload().to_vec(),
    }
}

fn datagram_of(ip: &Ipv4Packet<'_>) -> Datagram {
    let payload = if ip.is_fragment() {
        Payload::Raw(ip.payload().to_vec())
    } else {
        match ip.protocol() {
            Protocol::Udp => match UdpDatagram::parse(ip) {
                Ok(u) => Payload::Udp {
                    source: u.source_port().map_or(0, Port::get),
                    destination: u.destination_port().get(),
                    data: u.payload().to_vec(),
                },
                Err(e) => Payload::Refused(e.name()),
            },
            Protocol::Tcp => match TcpSegment::parse(ip) {
                Ok(s) => Payload::Tcp(segment_of(&s)),
                Err(e) => Payload::Refused(e.name()),
            },
            Protocol::Icmp => match IcmpPacket::parse(ip.payload()) {
                Ok(packet) => match packet.message() {
                    IcmpMessage::EchoRequest(e) | IcmpMessage::EchoReply(e) => Payload::Echo {
                        reply: matches!(packet.message(), IcmpMessage::EchoReply(_)),
                        identifier: e.identifier,
                        sequence: e.sequence,
                        data: e.data.to_vec(),
                    },
                    IcmpMessage::DestinationUnreachable { code: UnreachableCode::Protocol, .. } => {
                        Payload::Unreachable { code: 2, quote: packet.bytes()[8..].to_vec() }
                    }
                    IcmpMessage::DestinationUnreachable { code: UnreachableCode::Port, .. } => {
                        Payload::Unreachable { code: 3, quote: packet.bytes()[8..].to_vec() }
                    }
                    other => Payload::Igmp(format!("{other:?}")),
                },
                Err(e) => Payload::Refused(e.name()),
            },
            Protocol::Igmp => match IgmpPacket::parse(ip.payload()) {
                Ok(packet) => Payload::Igmp(format!("{:?}", packet.message())),
                Err(e) => Payload::Refused(e.name()),
            },
            Protocol::Other(_) => Payload::Raw(ip.payload().to_vec()),
        }
    };
    Datagram {
        source: ip.source(),
        destination: ip.destination(),
        ttl: ip.ttl(),
        traffic_class: ip.traffic_class().byte(),
        protocol: ip.protocol().number(),
        dont_fragment: ip.dont_fragment(),
        more_fragments: ip.more_fragments(),
        offset: ip.fragment_offset().units(),
        identification: (!ip.dont_fragment()).then_some(ip.identification()),
        options: ip.options().iter().map(|o| format!("{o:?}")).collect(),
        payload,
    }
}

enum AnyPayload<'a> {
    Udp(UdpBuilder<'a>),
    Tcp(TcpBuilder<'a>),
    Echo(EchoBuilder<'a>),
    Unreachable(UnreachableBuilder<'a>),
    Igmp(V2Builder),
    Raw(RawPayload<'a>),
}

impl AnyPayload<'_> {
    fn inner(&self) -> &dyn Ipv4Payload {
        match self {
            Self::Udp(p) => p,
            Self::Tcp(p) => p,
            Self::Echo(p) => p,
            Self::Unreachable(p) => p,
            Self::Igmp(p) => p,
            Self::Raw(p) => p,
        }
    }
}

impl Ipv4Payload for AnyPayload<'_> {
    fn protocol(&self) -> Protocol {
        self.inner().protocol()
    }

    fn length(&self, room: usize) -> Result<usize, BuildError> {
        self.inner().length(room)
    }

    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        self.inner().write(pseudo, out)
    }
}

fn emit<P: Ipv4Payload>(builder: &Ipv4Builder<'_, P>) -> Result<Vec<u8>, BuildError> {
    let mut out = junk(70_000);
    builder.emit(&mut out).map(<[u8]>::to_vec)
}

fn control_of<'a>(s: &TcpSegment<'_>, sack: &'a [SackBlock]) -> Option<Control<'a>> {
    let o = s.options();
    let syn = || -> Option<SynOptions> {
        (sack.is_empty()).then_some(())?;
        let window_scale = match o.window_scale() {
            Some(w) => Some(WindowShift::new(w.raw()).ok()?),
            None => None,
        };
        Some(SynOptions { mss: o.mss(), sack_permitted: o.sack_permitted(), timestamps: o.timestamps(), window_scale })
    };
    let established = || -> Option<EstablishedOptions<'a>> {
        (o.mss().is_none() && o.window_scale().is_none() && !o.sack_permitted()).then_some(())?;
        Some(EstablishedOptions { timestamps: o.timestamps(), sack })
    };
    let ack = s.acknowledgment();
    let (fin, psh) = (s.flags().contains(TcpFlags::FIN), s.flags().contains(TcpFlags::PSH));
    Some(match s.flags().bits() {
        0x02 => Control::Syn(syn()?),
        0x12 => Control::SynAck { acknowledgment: ack?, options: syn()? },
        0x04 | 0x14 => Control::Rst { acknowledgment: ack, options: established()? },
        0x10 | 0x11 | 0x18 | 0x19 => Control::Ack { acknowledgment: ack?, push: psh, fin, options: established()? },
        _ => return None,
    })
}

fn rebuild_ip(bytes: &[u8]) -> Option<Vec<u8>> {
    let ip = Ipv4Packet::parse(bytes).ok()?;
    let options: Vec<Ipv4Option<'_>> = ip.options().iter().collect();
    let form = match (ip.dont_fragment(), ip.more_fragments(), ip.fragment_offset().units()) {
        (true, false, 0) => Form::Atomic,
        (true, _, _) => return None,
        (false, more_fragments, _) => {
            Form::Fragmentable { identification: ip.identification(), more_fragments, offset: ip.fragment_offset() }
        }
    };
    let quoted;
    let sack: Vec<SackBlock>;
    let payload = if ip.is_fragment() {
        AnyPayload::Raw(RawPayload { protocol: ip.protocol(), bytes: ip.payload() })
    } else {
        match ip.protocol() {
            Protocol::Udp => {
                let u = UdpDatagram::parse(&ip).ok()?;
                AnyPayload::Udp(UdpBuilder { source: u.source_port()?, destination: u.destination_port(), data: u.payload() })
            }
            Protocol::Tcp => {
                let s = TcpSegment::parse(&ip).ok()?;
                sack = s.options().sack_blocks().collect();
                let control = control_of(&s, &sack)?;
                AnyPayload::Tcp(TcpBuilder {
                    source: s.source_port(),
                    destination: s.destination_port(),
                    sequence: s.sequence(),
                    control,
                    window: s.window(),
                    data: s.payload(),
                })
            }
            Protocol::Icmp => {
                let packet = IcmpPacket::parse(ip.payload()).ok()?;
                match packet.message() {
                    IcmpMessage::EchoRequest(e) => AnyPayload::Echo(EchoBuilder { kind: EchoKind::Request, ..EchoBuilder::reply_to(&e) }),
                    IcmpMessage::EchoReply(e) => AnyPayload::Echo(EchoBuilder::reply_to(&e)),
                    IcmpMessage::DestinationUnreachable { code, .. } => {
                        let code = match code {
                            UnreachableCode::Protocol => HostUnreachable::Protocol,
                            UnreachableCode::Port => HostUnreachable::Port,
                            _ => return None,
                        };
                        let quote = &packet.bytes()[8..];
                        quoted = Ipv4Packet::parse(quote).ok().filter(|d| d.bytes().len() == quote.len() && quote.len() <= 548)?;
                        AnyPayload::Unreachable(UnreachableBuilder { code, datagram: &quoted })
                    }
                    _ => return None,
                }
            }
            Protocol::Igmp => {
                let (kind, group) = match IgmpPacket::parse(ip.payload()).ok()?.message() {
                    IgmpMessage::V2Report(group) => (V2Kind::Report, group),
                    IgmpMessage::V1Report(group) => (V2Kind::V1Report, group),
                    IgmpMessage::Leave(group) => (V2Kind::Leave, group),
                    IgmpMessage::Query(_) => return None,
                };
                AnyPayload::Igmp(V2Builder { kind, group: ReportGroup::new(group).ok()? })
            }
            Protocol::Other(_) => AnyPayload::Raw(RawPayload { protocol: ip.protocol(), bytes: ip.payload() }),
        }
    };
    let builder = Ipv4Builder {
        source: Ipv4Source::new(ip.source()).ok()?,
        destination: ip.destination(),
        ttl: Ttl::new(ip.ttl()).ok()?,
        traffic_class: ip.traffic_class(),
        form,
        options: &options,
        payload,
    };
    emit(&builder).ok()
}

fn rebuild_frame(bytes: &[u8]) -> Option<Vec<u8>> {
    let frame = Frame::parse(bytes).ok()?;
    (frame.tags() == Tags::Untagged).then_some(())?;
    let builder = FrameBuilder { destination: frame.destination(), source: frame.source() };
    let mut out = junk(2000);
    match frame.ether_type() {
        EtherType::Arp => builder.emit(&Arp::parse(frame.body()).ok()?, &mut out).ok().map(<[u8]>::to_vec),
        EtherType::Ipv4 => {
            let ip = Ipv4Packet::parse(frame.body()).ok()?;
            let rebuilt = rebuild_ip(ip.bytes())?;
            let body = RawBody(&rebuilt);
            builder.emit(&body, &mut out).ok().map(<[u8]>::to_vec)
        }
        EtherType::Other(_) => None,
    }
}

struct RawBody<'a>(&'a [u8]);

impl FrameBody for RawBody<'_> {
    const ETHER_TYPE: toyos_net_wire::ethernet::TxEtherType = toyos_net_wire::ethernet::TxEtherType::Ipv4;

    fn length(&self) -> Result<usize, BuildError> {
        Ok(self.0.len())
    }

    fn write(&self, out: &mut [u8]) -> Result<(), BuildError> {
        out.copy_from_slice(self.0);
        Ok(())
    }
}

/// Holds R1 (the pieces re-emit the input) and RT-09 (every slice lies inside it) at each accepted layer.
fn walk_frame(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let frame = Frame::parse(bytes).map_err(|e| e.name())?;
    assert_eq!(concat(&[frame.header(), frame.body()]), bytes);
    assert!(inside(bytes, frame.header()) && inside(bytes, frame.body()));
    let mut layers = vec![format!("{frame:?}")];
    match frame.ether_type() {
        EtherType::Ipv4 => layers.extend(walk_ip(frame.body())?),
        EtherType::Arp => layers.extend(walk_arp(frame.body())?),
        EtherType::Other(_) => {}
    }
    Ok(layers)
}

fn walk_arp(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let arp = Arp::parse(bytes).map_err(|e| e.name())?;
    let mut out = [0xAA; 28];
    arp.write(&mut out).unwrap();
    assert_eq!(out, bytes[..28]);
    Ok(vec![format!("{arp:?}")])
}

fn walk_ip(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let ip = Ipv4Packet::parse(bytes).map_err(|e| e.name())?;
    let total = usize::from(ip.total_length());
    assert_eq!(ip.bytes(), &bytes[..total]);
    assert_eq!(concat(&[&ip.bytes()[..20], ip.options().bytes(), ip.payload()]), ip.bytes());
    assert!(inside(bytes, ip.bytes()) && inside(bytes, ip.payload()) && inside(bytes, ip.options().bytes()));
    for option in ip.options().iter() {
        if let Ipv4Option::Other { data, .. } = option {
            assert!(inside(bytes, data));
        }
    }
    let mut layers = vec![format!("{ip:?}"), format!("{:?}", ip.options().iter().collect::<Vec<_>>())];
    if ip.is_fragment() {
        return Ok(layers);
    }
    match ip.protocol() {
        Protocol::Udp => {
            let udp = UdpDatagram::parse(&ip).map_err(|e| e.name())?;
            let length = usize::from(u16::from_be_bytes([udp.header()[4], udp.header()[5]]));
            assert_eq!(concat(&[udp.header(), udp.payload()]), ip.payload()[..length]);
            assert!(inside(bytes, udp.payload()));
            layers.push(format!("{udp:?}"));
        }
        Protocol::Tcp => {
            let tcp = TcpSegment::parse(&ip).map_err(|e| e.name())?;
            assert_eq!(concat(&[tcp.header(), tcp.options_bytes(), tcp.payload()]), ip.payload());
            assert!(inside(bytes, tcp.payload()) && inside(bytes, tcp.options_bytes()));
            layers.push(format!("{tcp:?} {:?}", tcp.options().sack_blocks().collect::<Vec<_>>()));
        }
        Protocol::Icmp => layers.extend(walk_icmp(ip.payload())?),
        Protocol::Igmp => layers.extend(walk_igmp(ip.payload())?),
        Protocol::Other(_) => {}
    }
    Ok(layers)
}

fn walk_icmp(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let packet = IcmpPacket::parse(bytes).map_err(|e| e.name())?;
    assert_eq!(packet.bytes(), bytes);
    match packet.message() {
        IcmpMessage::EchoRequest(echo) | IcmpMessage::EchoReply(echo) => assert!(inside(bytes, echo.data)),
        IcmpMessage::DestinationUnreachable { quote, .. }
        | IcmpMessage::Redirect { quote, .. }
        | IcmpMessage::TimeExceeded { quote, .. }
        | IcmpMessage::ParameterProblem { quote, .. } => {
            assert_eq!(concat(&[quote.options(), quote.payload(), quote.beyond()]), bytes[28..]);
            let start = 28 + quote.options().len();
            match quote.transport() {
                Some(transport) => assert_eq!(&bytes[start..start + 8], transport),
                None => assert!(quote.payload().len() < 8),
            }
            assert!(inside(bytes, quote.options()) && inside(bytes, quote.payload()) && inside(bytes, quote.beyond()));
        }
        IcmpMessage::TimestampRequest(_) | IcmpMessage::TimestampReply(_) => {}
    }
    Ok(vec![format!("{packet:?}")])
}

fn walk_igmp(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let packet = IgmpPacket::parse(bytes).map_err(|e| e.name())?;
    assert_eq!(packet.bytes(), bytes);
    Ok(vec![format!("{packet:?}")])
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Layer {
    Frame,
    Datagram,
    Icmp,
    Igmp,
}

fn walk(layer: Layer, bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    match layer {
        Layer::Frame => walk_frame(bytes),
        Layer::Datagram => walk_ip(bytes),
        Layer::Icmp => walk_icmp(bytes),
        Layer::Igmp => walk_igmp(bytes),
    }
}

fn corpus() -> Vec<(Layer, Vec<u8>)> {
    let mut all: Vec<(Layer, Vec<u8>)> = FRAMES.iter().map(|v| (Layer::Frame, hex(v))).collect();
    all.extend(DATAGRAMS.iter().map(|v| (Layer::Datagram, hex(v))));
    all.push((Layer::Datagram, ipv4(IP_A, IP_B, 17, &hex(V_UDP_FFFF))));
    all.push((Layer::Datagram, ipv4(IP_A, IP_B, 6, &hex(V_TCP_ODD))));
    all.extend(ICMP_MESSAGES.iter().map(|v| (Layer::Icmp, hex(v))));
    all.extend(IGMP_MESSAGES.iter().map(|v| (Layer::Igmp, hex(v))));
    all
}

#[test]
fn s_rt_001_every_vector_reemits_raw() {
    for (layer, bytes) in corpus() {
        // An IGMPv3 report is the one layer this stage does not parse; R1 holds beneath it.
        if let Err(reason) = walk(layer, &bytes) {
            assert_eq!(reason, "igmp.v3-report", "{bytes:02x?}");
        }
        if layer == Layer::Frame {
            // Link padding is regenerated as zeros.
            let frame = Frame::parse(&bytes).unwrap();
            let inner = match frame.ether_type() {
                EtherType::Ipv4 => Ipv4Packet::parse(frame.body()).unwrap().bytes().to_vec(),
                _ => frame.body()[..28].to_vec(),
            };
            let mut body = inner;
            body.resize(body.len().max(46), 0);
            assert_eq!(concat(&[frame.header(), &body]), bytes);
        }
    }
}

#[test]
fn s_rt_002_canonical_vectors_rebuild() {
    let frames = [
        V_ARP_REQ, V_ARP_REPLY, V_ARP_PROBE, V_ARP_ANNOUNCE, V_UDP_DNS, V_TCP_SYN, V_TCP_SYNACK, V_TCP_DATA, V_TCP_SACK,
        V_TCP_RST, V_TCP_FIN, V_IGMP_REPORT,
    ];
    for vector in frames {
        assert_eq!(rebuild_frame(&hex(vector)), Some(hex(vector)), "{vector}");
    }
    let datagrams = [
        V_TCP_SYN_NOTS, V_TCP_SACK3, V_ICMP_ECHO, V_ICMP_REPLY, V_UDP_HI, V_ICMP_PORT_UNREACH_GEN, V_IP_FRAG_FIRST,
        V_IP_FRAG_LAST, V_IP_DSCP, V_IP_MIN,
    ];
    for vector in datagrams {
        assert_eq!(rebuild_ip(&hex(vector)), Some(hex(vector)), "{vector}");
    }
    for (protocol, segment) in [(17, V_UDP_FFFF), (6, V_TCP_ODD)] {
        let datagram = ipv4(IP_A, IP_B, protocol, &hex(segment));
        assert_eq!(rebuild_ip(&datagram), Some(datagram.clone()), "{segment}");
    }
    let leave = hex(V_IGMP_LEAVE);
    let IgmpMessage::Leave(group) = IgmpPacket::parse(&leave).unwrap().message() else { panic!() };
    let datagram = toyos_net_wire::igmp::datagram(
        Ipv4Source::new(IP_A).unwrap(),
        TrafficClass::ZERO,
        V2Builder { kind: V2Kind::Leave, group: ReportGroup::new(group).unwrap() },
    );
    assert_eq!(emit(&datagram).unwrap()[24..], leave);
}

fn random_datagram(rng: &mut Rng, scratch: &mut Vec<u8>) {
    // A datagram an unreachable may quote, built first so the builder can borrow its view.
    let data = rng.bytes(40);
    let quoted = if rng.chance(70) {
        AnyPayload::Udp(UdpBuilder { source: Port::new(5000).unwrap(), destination: Port::new(5001).unwrap(), data: &data })
    } else {
        AnyPayload::Raw(RawPayload { protocol: Protocol::from_number(253), bytes: &data[..data.len().min(12)] })
    };
    *scratch = emit(&Ipv4Builder {
        source: Ipv4Source::new(IP_B).unwrap(),
        destination: IP_A,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        form: Form::Atomic,
        options: &[],
        payload: quoted,
    })
    .unwrap();
}

fn random_multicast(rng: &mut Rng) -> MulticastAddr {
    let group = Ipv4Addr::new(224 + rng.below(16) as u8, rng.byte(), rng.byte(), rng.byte());
    MulticastAddr::new(group).unwrap()
}

fn random_options(rng: &mut Rng, store: &mut Vec<(u8, Vec<u8>)>) {
    store.clear();
    for _ in 0..rng.below(4) {
        let kind = loop {
            let kind = rng.byte();
            if OptionKind::new(kind).is_some() || kind == 0x94 {
                break kind;
            }
        };
        let max = if rng.chance(10) { 40 } else { 8 };
        store.push((kind, rng.bytes(max)));
    }
}

fn options_from(store: &[(u8, Vec<u8>)]) -> Vec<Ipv4Option<'_>> {
    store
        .iter()
        .map(|(kind, data)| match OptionKind::new(*kind) {
            Some(kind) => Ipv4Option::Other { kind, data },
            None => Ipv4Option::RouterAlert(u16::from_be_bytes([data.first().copied().unwrap_or(0), 0])),
        })
        .collect()
}

fn random_timestamps(rng: &mut Rng) -> Option<Timestamps> {
    rng.chance(50).then(|| Timestamps { value: rng.next() as u32, echo: rng.next() as u32 })
}

fn generated(check: impl Fn(&[u8], &Datagram)) -> (usize, BTreeMap<&'static str, usize>) {
    let mut rng = Rng(0x5EED_0003);
    let mut refused = BTreeMap::new();
    let mut built = 0;
    let mut quoted = Vec::new();
    let mut option_store = Vec::new();
    for _ in 0..20_000 {
        random_datagram(&mut rng, &mut quoted);
        let quoted = Ipv4Packet::parse(&quoted).unwrap();
        random_options(&mut rng, &mut option_store);
        let options = options_from(&option_store);
        let max = if rng.chance(2) { 1400 } else { 48 };
        let data = rng.bytes(max);
        let sack: Vec<SackBlock> = (0..rng.below(6))
            .map(|_| SackBlock { left: SeqNum::new(rng.next() as u32), right: SeqNum::new(rng.next() as u32) })
            .collect();
        let port = |rng: &mut Rng| Port::new(rng.below(65_535) as u16 + 1).unwrap();
        let (payload, expected) = match rng.below(6) {
            0 => {
                let (source, destination) = (port(&mut rng), port(&mut rng));
                let expected = Payload::Udp { source: source.get(), destination: destination.get(), data: data.clone() };
                (AnyPayload::Udp(UdpBuilder { source, destination, data: &data }), expected)
            }
            1 => {
                let syn = SynOptions {
                    mss: rng.chance(50).then(|| rng.next() as u16),
                    sack_permitted: rng.chance(50),
                    timestamps: random_timestamps(&mut rng),
                    window_scale: rng.chance(50).then(|| WindowShift::new(rng.below(15) as u8).unwrap()),
                };
                let established = EstablishedOptions { timestamps: random_timestamps(&mut rng), sack: &sack };
                let acknowledgment = SeqNum::new(rng.next() as u32);
                let (push, fin) = (rng.chance(50), rng.chance(50));
                let (control, flags, ack) = match rng.below(5) {
                    0 => (Control::Syn(syn), 0x02, None),
                    1 => (Control::SynAck { acknowledgment, options: syn }, 0x12, Some(acknowledgment.get())),
                    2 => (Control::Rst { acknowledgment: None, options: established }, 0x04, None),
                    3 => (Control::Rst { acknowledgment: Some(acknowledgment), options: established }, 0x14, Some(acknowledgment.get())),
                    _ => (
                        Control::Ack { acknowledgment, push, fin, options: established },
                        0x10 | if push { 0x08 } else { 0 } | u8::from(fin),
                        Some(acknowledgment.get()),
                    ),
                };
                let is_syn = flags & 0x02 != 0;
                let builder = TcpBuilder {
                    source: port(&mut rng),
                    destination: port(&mut rng),
                    sequence: SeqNum::new(rng.next() as u32),
                    control,
                    window: RawWindow(rng.next() as u16),
                    data: &data,
                };
                let expected = Payload::Tcp(Segment {
                    ports: (builder.source.get(), builder.destination.get()),
                    sequence: builder.sequence.get(),
                    acknowledgment: ack,
                    flags,
                    window: builder.window.0,
                    mss: if is_syn { syn.mss } else { None },
                    window_scale: if is_syn { syn.window_scale.map(WindowShift::get) } else { None },
                    sack_permitted: is_syn && syn.sack_permitted,
                    timestamps: if is_syn { syn.timestamps } else { established.timestamps },
                    sack: if is_syn { Vec::new() } else { sack.clone() },
                    data: data.clone(),
                });
                (AnyPayload::Tcp(builder), expected)
            }
            2 => {
                let echo = EchoBuilder {
                    kind: if rng.chance(50) { EchoKind::Request } else { EchoKind::Reply },
                    identifier: rng.next() as u16,
                    sequence: rng.next() as u16,
                    data: &data,
                };
                let expected = Payload::Echo {
                    reply: echo.kind == EchoKind::Reply,
                    identifier: echo.identifier,
                    sequence: echo.sequence,
                    data: data.clone(),
                };
                (AnyPayload::Echo(echo), expected)
            }
            3 => {
                let (code, number) = if rng.chance(50) { (HostUnreachable::Port, 3) } else { (HostUnreachable::Protocol, 2) };
                let expected = Payload::Unreachable { code: number, quote: quoted.bytes().to_vec() };
                (AnyPayload::Unreachable(UnreachableBuilder { code, datagram: &quoted }), expected)
            }
            4 => {
                let Ok(group) = ReportGroup::new(random_multicast(&mut rng)) else { continue };
                let kind = *rng.pick(&[V2Kind::Report, V2Kind::V1Report, V2Kind::Leave]);
                let message = match kind {
                    V2Kind::Report => IgmpMessage::V2Report(group.get()),
                    V2Kind::V1Report => IgmpMessage::V1Report(group.get()),
                    V2Kind::Leave => IgmpMessage::Leave(group.get()),
                };
                (AnyPayload::Igmp(V2Builder { kind, group }), Payload::Igmp(format!("{message:?}")))
            }
            _ => {
                let protocol = Protocol::from_number(*rng.pick(&[0, 41, 50, 132, 253, 255]));
                (AnyPayload::Raw(RawPayload { protocol, bytes: &data }), Payload::Raw(data.clone()))
            }
        };
        let form = if rng.chance(70) {
            Form::Atomic
        } else {
            let offset = FragmentOffset::new(if rng.chance(50) { 0 } else { rng.below(8192) as u16 }).unwrap();
            Form::Fragmentable { identification: rng.next() as u16, more_fragments: rng.chance(50), offset }
        };
        let source = Ipv4Addr::new(rng.below(224) as u8, rng.byte(), rng.byte(), rng.byte());
        let builder = Ipv4Builder {
            source: Ipv4Source::new(source).unwrap(),
            destination: Ipv4Addr::from(rng.next() as u32),
            ttl: Ttl::new(rng.below(255) as u8 + 1).unwrap(),
            traffic_class: TrafficClass::from_byte(rng.byte()),
            form,
            options: &options,
            payload,
        };
        let bytes = match emit(&builder) {
            Ok(bytes) => bytes,
            Err(e) => {
                *refused.entry(e.name()).or_insert(0) += 1;
                continue;
            }
        };
        built += 1;
        let (dont_fragment, more_fragments, offset, identification) = match form {
            Form::Atomic => (true, false, 0, None),
            Form::Fragmentable { identification, more_fragments, offset } => (false, more_fragments, offset.units(), Some(identification)),
        };
        let fragment = more_fragments || offset != 0;
        let expected = Datagram {
            source,
            destination: builder.destination,
            ttl: builder.ttl.get(),
            traffic_class: builder.traffic_class.byte(),
            protocol: builder.payload.protocol().number(),
            dont_fragment,
            more_fragments,
            offset,
            identification,
            options: options.iter().map(|o| format!("{o:?}")).collect(),
            payload: if fragment { Payload::Raw(bytes[bytes.len() - builder.payload.length(65_535).unwrap()..].to_vec()) } else { expected },
        };
        check(&bytes, &expected);
    }
    (built, refused)
}

#[test]
fn s_rt_003_build_then_parse() {
    let (built, refused) = generated(|bytes, expected| {
        let ip = Ipv4Packet::parse(bytes).unwrap();
        assert_eq!(&datagram_of(&ip), expected);
    });
    arp_and_frames_generated();
    println!("built {built}, refused {refused:?}");
    assert!(built > 15_000);
    assert!(refused.contains_key("ip.options-too-long") && refused.contains_key("tcp.too-many-sack-blocks"));
}

#[test]
fn s_rt_004_canonical_idempotence() {
    generated(|bytes, _| assert_eq!(rebuild_ip(bytes).as_deref(), Some(bytes)));
}

fn fix(layer: Layer, bytes: &mut [u8]) {
    match layer {
        Layer::Frame if bytes.len() >= 34 && bytes[12..14] == [0x08, 0x00] => fix_datagram(&mut bytes[14..]),
        Layer::Datagram if bytes.len() >= 20 => fix_datagram(bytes),
        Layer::Icmp | Layer::Igmp if bytes.len() >= 4 => fix_message(bytes),
        _ => {}
    }
}

fn fields(layer: Layer, bytes: &[u8]) -> Vec<usize> {
    let ip = match layer {
        Layer::Frame => 14,
        Layer::Datagram => 0,
        Layer::Icmp | Layer::Igmp => return vec![0, 1, 8, 10, 11],
    };
    let hlen = bytes.get(ip).map_or(20, |b| usize::from(b & 0x0F) * 4);
    let transport = ip + hlen;
    vec![ip, ip + 2, ip + 3, ip + 6, ip + 20, ip + 21, transport + 4, transport + 5, transport + 12, transport + 20, transport + 21, transport + 22, transport + 23]
}

fn mutate(rng: &mut Rng, layer: Layer, bytes: &mut Vec<u8>, donor: &[u8]) {
    for _ in 0..=rng.below(4) {
        let len = bytes.len().max(1);
        match rng.below(11) {
            10 if layer == Layer::Frame && bytes.len() >= 12 => {
                let tag = if rng.chance(50) { [0x81, 0x00] } else { [0x88, 0xA8] };
                bytes.splice(12..12, [tag[0], tag[1], rng.byte() & 0x0F, rng.byte()]);
            }
            0 => {
                let at = rng.below(len);
                if let Some(b) = bytes.get_mut(at) {
                    *b ^= 1 << rng.below(8);
                }
            }
            1 => {
                let at = rng.below(len);
                if let Some(b) = bytes.get_mut(at) {
                    *b = rng.byte();
                }
            }
            2 | 3 => {
                let at = *rng.pick(&fields(layer, bytes));
                let value = *rng.pick(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x07, 0x08, 0x0a, 0x0f, 0x10, 0x3f, 0x40, 0x45, 0x46, 0x4f, 0x50, 0x7f, 0x80, 0x94, 0xf0, 0xff]);
                if let Some(b) = bytes.get_mut(at) {
                    *b = value;
                }
            }
            4 => bytes.truncate(rng.below(len)),
            5 => bytes.extend(rng.bytes(16)),
            6 => {
                let at = rng.below(len);
                if at < bytes.len() {
                    bytes.remove(at);
                }
            }
            7 => {
                let at = rng.below(bytes.len() + 1);
                bytes.insert(at, rng.byte());
            }
            8 => {
                let from = rng.below(donor.len());
                let at = rng.below(len);
                let take = rng.below(donor.len() - from + 1).min(bytes.len().saturating_sub(at));
                bytes[at..at + take].copy_from_slice(&donor[from..from + take]);
            }
            _ => {
                // An option-shaped run: a known kind, then a length, then data.
                let at = *rng.pick(&fields(layer, bytes));
                let kind = *rng.pick(&[0u8, 1, 2, 3, 4, 5, 7, 8, 19, 34, 68, 130, 131, 136, 137, 148, 254]);
                let run = [kind, rng.below(12) as u8, rng.byte(), rng.byte()];
                for (i, b) in run.into_iter().enumerate() {
                    if let Some(slot) = bytes.get_mut(at + i) {
                        *slot = b;
                    }
                }
            }
        }
    }
}

#[test]
fn s_rt_005_totality_structured_fuzz() {
    let corpus = corpus();
    let mut rng = Rng(0x5EED_0005);
    let mut outcomes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut rebuilt = 0;
    for _ in 0..FUZZ_ITERATIONS {
        let (layer, seed) = rng.pick(&corpus);
        let (_, donor) = rng.pick(&corpus);
        let mut bytes = seed.clone();
        mutate(&mut rng, *layer, &mut bytes, donor);
        if rng.chance(75) {
            fix(*layer, &mut bytes);
        }
        let result = walk(*layer, &bytes);
        *outcomes.entry(result.as_ref().err().copied().unwrap_or("accepted")).or_insert(0) += 1;
        // Parse, build, parse: the fields survive, and a second build is the first byte for byte.
        let datagram = match layer {
            Layer::Frame => Frame::parse(&bytes).ok().filter(|f| f.ether_type() == EtherType::Ipv4).map(|f| f.body()),
            Layer::Datagram => Some(&bytes[..]),
            Layer::Icmp | Layer::Igmp => None,
        };
        if let Some(first) = datagram.and_then(rebuild_ip) {
            let original = Ipv4Packet::parse(datagram.unwrap()).unwrap();
            assert_eq!(datagram_of(&Ipv4Packet::parse(&first).unwrap()), datagram_of(&original), "{bytes:02x?}");
            assert_eq!(rebuild_ip(&first).as_ref(), Some(&first), "{bytes:02x?}");
            rebuilt += 1;
        }
    }
    println!("{FUZZ_ITERATIONS} inputs, {rebuilt} rebuilt: {outcomes:#?}");
    // The mutations reach past the checksums into every rule every parser has.
    let unreached: Vec<_> = REFUSALS.iter().filter(|name| !outcomes.contains_key(*name)).collect();
    assert!(unreached.is_empty(), "never refused: {unreached:?}");
    assert!(outcomes["accepted"] > FUZZ_ITERATIONS / 10);
    assert!(rebuilt > FUZZ_ITERATIONS / 20);
}

const REFUSALS: &[&str] = &[
    "eth.truncated", "eth.group-source", "eth.too-many-tags", "eth.truncated-tag", "eth.length-frame", "eth.undefined-type-field",
    "arp.truncated", "arp.hardware-type", "arp.protocol-type", "arp.address-length", "arp.operation",
    "ip.truncated", "ip.version", "ip.header-length", "ip.header-overrun", "ip.total-length-below-header",
    "ip.total-length-overrun", "ip.header-checksum", "ip.option-length", "ip.option-overrun", "ip.router-alert-length",
    "icmp.truncated", "icmp.checksum", "icmp.code", "icmp.timestamp-length", "icmp.quote-truncated", "icmp.quote-not-ipv4",
    "icmp.source-quench", "icmp.router-discovery", "icmp.deprecated-type", "icmp.unknown-type",
    "igmp.truncated", "igmp.checksum", "igmp.query-length", "igmp.query-group", "igmp.query-sources-overrun", "igmp.group",
    "igmp.v3-report", "igmp.unknown-type",
    "udp.truncated", "udp.length-below-header", "udp.length-overrun", "udp.checksum", "udp.destination-port-zero",
    "tcp.truncated", "tcp.data-offset", "tcp.header-overrun", "tcp.checksum", "tcp.port-zero", "tcp.option-length",
    "tcp.option-overrun", "tcp.sack-length",
];

fn is_truncation(reason: &str) -> bool {
    reason.contains("truncated") || reason.contains("overrun")
}

#[test]
fn s_rt_006_truncation_sweep() {
    // ICMP, IGMP and TCP carry no length of their own: the datagram's total length is what a truncation of theirs breaks.
    let framed = corpus().into_iter().filter(|(layer, bytes)| matches!(layer, Layer::Frame | Layer::Datagram) && walk(*layer, bytes).is_ok());
    for (layer, bytes) in framed {
        for len in 0..bytes.len() {
            if let Err(reason) = walk(layer, &bytes[..len]) {
                assert!(is_truncation(reason), "{len} bytes of {bytes:02x?}: {reason}");
            }
        }
    }
}

fn covered(layer: Layer, bytes: &[u8]) -> Vec<(usize, usize)> {
    let ip = match layer {
        Layer::Frame if bytes[12..14] == [0x08, 0x00] => 14,
        Layer::Frame => return Vec::new(),
        Layer::Datagram => 0,
        Layer::Icmp | Layer::Igmp => return vec![(0, bytes.len())],
    };
    let hlen = usize::from(bytes[ip] & 0x0F) * 4;
    let total = usize::from(u16::from_be_bytes([bytes[ip + 2], bytes[ip + 3]]));
    let transport = ip + hlen..ip + total;
    match bytes[ip + 9] {
        _ if bytes[ip + 6] & 0x3F != 0 || bytes[ip + 7] != 0 => vec![(ip, ip + hlen)],
        17 => {
            let length = usize::from(u16::from_be_bytes([bytes[transport.start + 4], bytes[transport.start + 5]]));
            vec![(ip, ip + hlen), (transport.start, transport.start + length)]
        }
        1 | 2 | 6 => vec![(ip, ip + hlen), (transport.start, transport.end)],
        _ => vec![(ip, ip + hlen)],
    }
}

#[test]
fn s_rt_007_single_bit_flips() {
    for (layer, bytes) in corpus() {
        if walk(layer, &bytes).is_err() {
            continue;
        }
        for (start, end) in covered(layer, &bytes) {
            for bit in start * 8..end * 8 {
                let mut flipped = bytes.clone();
                flipped[bit / 8] ^= 0x80 >> (bit % 8);
                if let Ok(layers) = walk(layer, &flipped) {
                    // UDP-16: a checksum field of one set bit, flipped, reads as "none".
                    let udp = Ipv4Packet::parse(&flipped[if layer == Layer::Frame { 14 } else { 0 }..])
                        .ok()
                        .and_then(|ip| UdpDatagram::parse(&ip).ok().map(|u| u.checksum()));
                    assert_eq!(udp, Some(toyos_net_wire::udp::UdpChecksum::Absent), "bit {bit} of {bytes:02x?}: {layers:?}");
                }
            }
        }
    }
}

#[test]
fn s_rt_008_position_independence() {
    for (layer, bytes) in corpus() {
        let mut buffer = vec![0xC3; 7];
        buffer.extend_from_slice(&bytes);
        buffer.extend_from_slice(&[0x3C; 9]);
        assert_eq!(walk(layer, &buffer[7..7 + bytes.len()]), walk(layer, &bytes));
    }
}

#[test]
fn s_rt_009_zero_copy() {
    // `walk` holds every slice a view returns against the input's bounds.
    for (layer, bytes) in corpus() {
        let _ = walk(layer, &bytes);
    }
    let bytes = hex(V_UDP_DNS);
    let frame = Frame::parse(&bytes).unwrap();
    let ip = Ipv4Packet::parse(frame.body()).unwrap();
    let udp = UdpDatagram::parse(&ip).unwrap();
    assert_eq!(udp.payload().as_ptr(), bytes[42..].as_ptr());
}

fn arp_and_frames_generated() {
    let mut rng = Rng(0x005E_EDA4);
    for _ in 0..2000 {
        let mut mac = || MacAddr([rng.byte() & 0xFE, rng.byte(), rng.byte(), rng.byte(), rng.byte(), rng.byte()]);
        let (sender_mac, target_mac, destination) = (mac(), mac(), mac());
        let arp = Arp {
            operation: if rng.chance(50) { Operation::Request } else { Operation::Reply },
            sender_mac,
            sender_ip: Ipv4Addr::from(rng.next() as u32),
            target_mac,
            target_ip: Ipv4Addr::from(rng.next() as u32),
        };
        let source = IndividualMac::new(sender_mac).unwrap();
        let mut out = junk(100);
        let frame = FrameBuilder { destination, source }.emit(&arp, &mut out).unwrap().to_vec();
        let parsed = Frame::parse(&frame).unwrap();
        assert_eq!((parsed.destination(), parsed.source(), parsed.ether_type()), (destination, source, EtherType::Arp));
        assert_eq!(Arp::parse(parsed.body()), Ok(arp));
    }
}
