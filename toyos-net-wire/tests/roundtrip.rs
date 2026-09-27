mod common;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::arp::{Arp, ArpError, Operation};
use toyos_net_wire::ethernet::{EthError, EtherType, Frame, FrameBuilder, IndividualMac, MacAddr, Tags, HEADER_LEN, MIN_BODY};
use toyos_net_wire::icmp::{
    EchoBuilder, EchoKind, HostUnreachable, IcmpError, IcmpMessage, IcmpPacket, UnreachableBuilder, UnreachableCode, MAX_ERROR_LEN,
};
use toyos_net_wire::igmp::{IgmpError, IgmpMessage, IgmpPacket, ReportGroup, V2Builder, V2Kind};
use toyos_net_wire::ipv4::{
    Ecn, Ipv4Builder, Ipv4Error, Ipv4Option, Ipv4Packet, Ipv4Payload, Ipv4Source, MulticastAddr, Protocol, RawPayload,
    TrafficClass, Ttl, TxOption, TxOptionKind,
};
use toyos_net_wire::tcp::{
    Control, EstablishedOptions, RawWindow, SackBlock, SeqNum, SynOptions, TcpBuilder, TcpError, TcpFlags, TcpSegment, Timestamps,
    WindowShift,
};
use toyos_net_wire::udp::{UdpBuilder, UdpDatagram, UdpError};
use toyos_net_wire::{BuildError, Port};

const FUZZ_ITERATIONS: usize = 500_000;

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
    atomic: bool,
    options: Vec<(u8, Vec<u8>)>,
    payload: Payload,
}

#[derive(Debug, PartialEq)]
enum Payload {
    Udp { source: u16, destination: u16, data: Vec<u8> },
    Tcp(Segment),
    Echo { reply: bool, identifier: u16, sequence: u16, data: Vec<u8> },
    Unreachable { code: UnreachableCode, quote: Vec<u8> },
    Igmp(String),
    Raw(Vec<u8>),
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

fn option_bytes(option: Ipv4Option<'_>) -> (u8, Vec<u8>) {
    match option {
        Ipv4Option::RouterAlert(value) => (0x94, value.to_be_bytes().to_vec()),
        Ipv4Option::Other { kind, data } => (kind.value(), data.to_vec()),
    }
}

/// The model of a datagram the builders can express.
fn datagram_of(ip: &Ipv4Packet<'_>) -> Datagram {
    let payload = match ip.protocol() {
        Protocol::Udp => {
            let u = UdpDatagram::parse(ip).unwrap();
            Payload::Udp { source: u.source_port().map_or(0, Port::get), destination: u.destination_port().get(), data: u.payload().to_vec() }
        }
        Protocol::Tcp => Payload::Tcp(segment_of(&TcpSegment::parse(ip).unwrap())),
        Protocol::Icmp => {
            let packet = IcmpPacket::parse(ip.payload()).unwrap();
            match packet.message() {
                IcmpMessage::EchoRequest(e) | IcmpMessage::EchoReply(e) => Payload::Echo {
                    reply: matches!(packet.message(), IcmpMessage::EchoReply(_)),
                    identifier: e.identifier,
                    sequence: e.sequence,
                    data: e.data.to_vec(),
                },
                IcmpMessage::DestinationUnreachable { code, .. } => Payload::Unreachable { code, quote: packet.bytes()[8..].to_vec() },
                other => panic!("no builder sends {other:?}"),
            }
        }
        Protocol::Igmp => Payload::Igmp(format!("{:?}", IgmpPacket::parse(ip.payload()).unwrap().message())),
        Protocol::Other(_) => Payload::Raw(ip.payload().to_vec()),
    };
    Datagram {
        source: ip.source(),
        destination: ip.destination(),
        ttl: ip.ttl(),
        traffic_class: ip.traffic_class().byte(),
        protocol: ip.protocol().number(),
        atomic: ip.dont_fragment() && !ip.is_fragment(),
        options: ip.options().iter().map(option_bytes).collect(),
        payload,
    }
}

struct Header<'a> {
    source: Ipv4Source,
    destination: Ipv4Addr,
    ttl: Ttl,
    traffic_class: TrafficClass,
    options: &'a [TxOption<'a>],
}

impl Header<'_> {
    fn emit<P: Ipv4Payload>(&self, payload: P) -> Result<Vec<u8>, BuildError> {
        let Self { source, destination, ttl, traffic_class, options } = *self;
        emit(&Ipv4Builder { source, destination, ttl, traffic_class, options, payload })
    }

    /// Wraps `payload` in the same `Ipv4Builder` `emit` would, and hands it to `outer.emit` — the only path an Ethernet frame's bytes come from.
    fn frame<P: Ipv4Payload>(&self, payload: P, outer: &FrameBuilder, out: &mut [u8]) -> Result<Vec<u8>, BuildError> {
        let Self { source, destination, ttl, traffic_class, options } = *self;
        outer.emit(&Ipv4Builder { source, destination, ttl, traffic_class, options, payload }, out).map(<[u8]>::to_vec)
    }
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

fn options_of<'a>(ip: &Ipv4Packet<'a>) -> Option<Vec<TxOption<'a>>> {
    ip.options()
        .iter()
        .map(|option| match option {
            Ipv4Option::RouterAlert(value) => Some(TxOption::RouterAlert(value)),
            Ipv4Option::Other { kind, data } => Some(TxOption::Other { kind: TxOptionKind::new(kind.value())?, data }),
        })
        .collect()
}

fn header_of<'o>(ip: &Ipv4Packet<'_>, options: &'o [TxOption<'o>]) -> Option<Header<'o>> {
    Some(Header {
        source: Ipv4Source::new(ip.source()).ok()?,
        destination: ip.destination(),
        ttl: Ttl::new(ip.ttl()).ok()?,
        traffic_class: ip.traffic_class(),
        options,
    })
}

fn rebuild_ip(bytes: &[u8]) -> Option<Vec<u8>> {
    let ip = Ipv4Packet::parse(bytes).ok()?;
    (ip.dont_fragment() && !ip.is_fragment()).then_some(())?;
    let options = options_of(&ip)?;
    let header = header_of(&ip, &options)?;
    let built = match ip.protocol() {
        Protocol::Udp => {
            let u = UdpDatagram::parse(&ip).ok()?;
            header.emit(UdpBuilder { source: u.source_port()?, destination: u.destination_port(), data: u.payload() })
        }
        Protocol::Tcp => {
            let s = TcpSegment::parse(&ip).ok()?;
            let sack: Vec<SackBlock> = s.options().sack_blocks().collect();
            let control = control_of(&s, &sack)?;
            header.emit(TcpBuilder {
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
                IcmpMessage::EchoRequest(e) => header.emit(EchoBuilder { kind: EchoKind::Request, ..EchoBuilder::reply_to(&e) }),
                IcmpMessage::EchoReply(e) => header.emit(EchoBuilder::reply_to(&e)),
                IcmpMessage::DestinationUnreachable { code, .. } => {
                    let code = match code {
                        UnreachableCode::Protocol => HostUnreachable::Protocol,
                        UnreachableCode::Port => HostUnreachable::Port,
                        _ => return None,
                    };
                    // Only a whole quote within 576 bytes is what the builder would send.
                    let quote = &packet.bytes()[8..];
                    let quoted = Ipv4Packet::parse(quote).ok().filter(|d| d.bytes().len() == quote.len() && ip.bytes().len() <= MAX_ERROR_LEN)?;
                    header.emit(UnreachableBuilder { code, datagram: &quoted })
                }
                _ => return None,
            }
        }
        Protocol::Igmp => {
            let (kind, group) = match IgmpPacket::parse(ip.payload()).ok()?.message() {
                IgmpMessage::V2Report(group) => (V2Kind::Report, group),
                IgmpMessage::Leave(group) => (V2Kind::Leave, group),
                IgmpMessage::V1Report(_) | IgmpMessage::Query(_) => return None,
            };
            header.emit(V2Builder { kind, group: ReportGroup::new(group).ok()? })
        }
        Protocol::Other(protocol) => header.emit(RawPayload { protocol, bytes: ip.payload() }),
    };
    built.ok()
}

/// Mirrors `rebuild_ip`'s match, but hands each rebuilt payload to `outer` through `Header::frame` instead of emitting it alone:
/// an already-built datagram's bytes never reach an Ethernet frame except through a real `Ipv4Builder`, so no body here is forged.
fn rebuild_frame(bytes: &[u8]) -> Option<Vec<u8>> {
    let frame = Frame::parse(bytes).ok()?;
    (frame.tags() == Tags::Untagged).then_some(())?;
    let outer = FrameBuilder { destination: frame.destination(), source: frame.source() };
    let mut out = junk(2000);
    match frame.ether_type() {
        EtherType::Arp => outer.emit(&Arp::parse(frame.body()).ok()?, &mut out).ok().map(<[u8]>::to_vec),
        EtherType::Ipv4 => {
            let ip = Ipv4Packet::parse(frame.body()).ok()?;
            (ip.dont_fragment() && !ip.is_fragment()).then_some(())?;
            let options = options_of(&ip)?;
            let header = header_of(&ip, &options)?;
            let built = match ip.protocol() {
                Protocol::Udp => {
                    let u = UdpDatagram::parse(&ip).ok()?;
                    header.frame(UdpBuilder { source: u.source_port()?, destination: u.destination_port(), data: u.payload() }, &outer, &mut out)
                }
                Protocol::Tcp => {
                    let s = TcpSegment::parse(&ip).ok()?;
                    let sack: Vec<SackBlock> = s.options().sack_blocks().collect();
                    let control = control_of(&s, &sack)?;
                    header.frame(
                        TcpBuilder {
                            source: s.source_port(),
                            destination: s.destination_port(),
                            sequence: s.sequence(),
                            control,
                            window: s.window(),
                            data: s.payload(),
                        },
                        &outer,
                        &mut out,
                    )
                }
                Protocol::Icmp => {
                    let packet = IcmpPacket::parse(ip.payload()).ok()?;
                    match packet.message() {
                        IcmpMessage::EchoRequest(e) => {
                            header.frame(EchoBuilder { kind: EchoKind::Request, ..EchoBuilder::reply_to(&e) }, &outer, &mut out)
                        }
                        IcmpMessage::EchoReply(e) => header.frame(EchoBuilder::reply_to(&e), &outer, &mut out),
                        IcmpMessage::DestinationUnreachable { code, .. } => {
                            let code = match code {
                                UnreachableCode::Protocol => HostUnreachable::Protocol,
                                UnreachableCode::Port => HostUnreachable::Port,
                                _ => return None,
                            };
                            // Only a whole quote within 576 bytes is what the builder would send.
                            let quote = &packet.bytes()[8..];
                            let quoted =
                                Ipv4Packet::parse(quote).ok().filter(|d| d.bytes().len() == quote.len() && ip.bytes().len() <= MAX_ERROR_LEN)?;
                            header.frame(UnreachableBuilder { code, datagram: &quoted }, &outer, &mut out)
                        }
                        _ => return None,
                    }
                }
                Protocol::Igmp => {
                    let (kind, group) = match IgmpPacket::parse(ip.payload()).ok()?.message() {
                        IgmpMessage::V2Report(group) => (V2Kind::Report, group),
                        IgmpMessage::Leave(group) => (V2Kind::Leave, group),
                        IgmpMessage::V1Report(_) | IgmpMessage::Query(_) => return None,
                    };
                    header.frame(V2Builder { kind, group: ReportGroup::new(group).ok()? }, &outer, &mut out)
                }
                Protocol::Other(protocol) => header.frame(RawPayload { protocol, bytes: ip.payload() }, &outer, &mut out),
            };
            built.ok()
        }
        EtherType::Other(_) => None,
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
    let mut out = [0xAA; HEADER_LEN + MIN_BODY];
    let built = FrameBuilder { destination: MacAddr::BROADCAST, source: mac_a() }.emit(&arp, &mut out).unwrap();
    assert_eq!(&built[HEADER_LEN..HEADER_LEN + 28], &bytes[..28]);
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
        // An IGMPv3 report and an ICMP timestamp are refused by type; R1 holds beneath them.
        if let Err(reason) = walk(layer, &bytes) {
            assert!(["igmp.v3-report", "icmp.timestamp-request"].contains(&reason), "{bytes:02x?}: {reason}");
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
        V_TCP_SYN_NOTS, V_TCP_SACK3, V_ICMP_ECHO, V_ICMP_REPLY, V_UDP_HI, V_ICMP_PORT_UNREACH_GEN, V_IP_DSCP, V_IP_MIN,
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

/// A datagram an unreachable may quote.
fn random_datagram(rng: &mut Rng) -> Vec<u8> {
    let data = rng.bytes(40);
    if rng.chance(70) {
        emit(&datagram(IP_B, IP_A, UdpBuilder { source: Port::new(5000).unwrap(), destination: Port::new(5001).unwrap(), data: &data }))
    } else {
        emit(&datagram(IP_B, IP_A, RawPayload { protocol: unassigned(253), bytes: &data[..data.len().min(12)] }))
    }
    .unwrap()
}

fn random_multicast(rng: &mut Rng) -> MulticastAddr {
    let group = Ipv4Addr::new(224 + rng.below(16) as u8, rng.byte(), rng.byte(), rng.byte());
    MulticastAddr::new(group).unwrap()
}

fn random_options(rng: &mut Rng) -> Vec<(u8, Vec<u8>)> {
    (0..rng.below(4))
        .map(|_| {
            let kind = loop {
                let kind = rng.byte();
                if TxOptionKind::new(kind).is_some() || kind == 0x94 {
                    break kind;
                }
            };
            let max = if rng.chance(10) { 40 } else { 8 };
            let data = if kind == 0x94 { vec![rng.byte(), rng.byte()] } else { rng.bytes(max) };
            (kind, data)
        })
        .collect()
}

fn tx_options(store: &[(u8, Vec<u8>)]) -> Vec<TxOption<'_>> {
    store
        .iter()
        .map(|(kind, data)| match TxOptionKind::new(*kind) {
            Some(kind) => TxOption::Other { kind, data },
            None => TxOption::RouterAlert(u16::from_be_bytes([data[0], data[1]])),
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
    for _ in 0..20_000 {
        let quoted = random_datagram(&mut rng);
        let quoted = Ipv4Packet::parse(&quoted).unwrap();
        let option_store = random_options(&mut rng);
        let options = tx_options(&option_store);
        let max = if rng.chance(2) { 1400 } else { 48 };
        let data = rng.bytes(max);
        let sack: Vec<SackBlock> = (0..rng.below(6))
            .map(|_| SackBlock { left: SeqNum::new(rng.next() as u32), right: SeqNum::new(rng.next() as u32) })
            .collect();
        let port = |rng: &mut Rng| Port::new(rng.below(65_535) as u16 + 1).unwrap();
        let source = Ipv4Addr::new(rng.below(224) as u8, rng.byte(), rng.byte(), rng.byte());
        let ecn = *rng.pick(&[Ecn::NotEct, Ecn::Ect1, Ecn::Ect0, Ecn::Ce]);
        let header = Header {
            source: Ipv4Source::new(source).unwrap(),
            destination: Ipv4Addr::from(rng.next() as u32),
            ttl: Ttl::new(rng.below(255) as u8 + 1).unwrap(),
            traffic_class: TrafficClass::new(rng.below(64) as u8, ecn).unwrap(),
            options: &options,
        };
        let (result, protocol, expected) = match rng.below(6) {
            0 => {
                let (source, destination) = (port(&mut rng), port(&mut rng));
                let expected = Payload::Udp { source: source.get(), destination: destination.get(), data: data.clone() };
                (header.emit(UdpBuilder { source, destination, data: &data }), 17, expected)
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
                (header.emit(builder), 6, expected)
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
                (header.emit(echo), 1, expected)
            }
            3 => {
                let (code, expected) = *rng.pick(&[(HostUnreachable::Port, UnreachableCode::Port), (HostUnreachable::Protocol, UnreachableCode::Protocol)]);
                let expected = Payload::Unreachable { code: expected, quote: quoted.bytes().to_vec() };
                (header.emit(UnreachableBuilder { code, datagram: &quoted }), 1, expected)
            }
            4 => {
                let Ok(group) = ReportGroup::new(random_multicast(&mut rng)) else { continue };
                let (kind, message) = if rng.chance(50) {
                    (V2Kind::Report, IgmpMessage::V2Report(group.get()))
                } else {
                    (V2Kind::Leave, IgmpMessage::Leave(group.get()))
                };
                (header.emit(V2Builder { kind, group }), 2, Payload::Igmp(format!("{message:?}")))
            }
            _ => {
                let number = *rng.pick(&[0, 41, 50, 132, 253, 255]);
                (header.emit(RawPayload { protocol: unassigned(number), bytes: &data }), number, Payload::Raw(data.clone()))
            }
        };
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(e) => {
                *refused.entry(e.name()).or_insert(0) += 1;
                continue;
            }
        };
        built += 1;
        let expected = Datagram {
            source,
            destination: header.destination,
            ttl: header.ttl.get(),
            traffic_class: header.traffic_class.byte(),
            protocol,
            atomic: true,
            options: option_store.clone(),
            payload: expected,
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
    let unreached: Vec<_> = every_refusal().into_iter().filter(|name| !outcomes.contains_key(name)).collect();
    assert!(unreached.is_empty(), "never refused: {unreached:?}");
    assert!(outcomes["accepted"] > FUZZ_ITERATIONS / 10);
    assert!(rebuilt > FUZZ_ITERATIONS / 20);
}

fn every_refusal() -> Vec<&'static str> {
    let mut all: Vec<&str> = EthError::ALL.iter().map(|e| e.name()).collect();
    all.extend(ArpError::ALL.iter().map(|e| e.name()));
    all.extend(Ipv4Error::ALL.iter().map(|e| e.name()));
    all.extend(IcmpError::ALL.iter().map(|e| e.name()));
    all.extend(IgmpError::ALL.iter().map(|e| e.name()));
    all.extend(UdpError::ALL.iter().map(|e| e.name()));
    all.extend(TcpError::ALL.iter().map(|e| e.name()));
    all
}

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
