mod common;

use common::*;
use toyos_net_wire::icmp::{
    Echo, EchoBuilder, EchoKind, HostUnreachable, IcmpError, IcmpMessage, IcmpPacket, ParameterProblemCode, Quote,
    RedirectCode, TimeExceededCode, Timestamp, UnreachableBuilder, UnreachableCode,
};
use toyos_net_wire::ipv4::{Form, Ipv4Builder, Ipv4Packet, Ipv4Payload, Ipv4Source, Protocol, TrafficClass, Ttl};
use toyos_net_wire::Class;

fn parse(bytes: &[u8]) -> Result<IcmpMessage<'_>, IcmpError> {
    IcmpPacket::parse(bytes).map(|packet| packet.message())
}

fn fixed(mut message: Vec<u8>, edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    edit(&mut message);
    fix_message(&mut message);
    message
}

fn echo_icmp() -> Vec<u8> {
    payload_of(&hex(V_ICMP_ECHO))
}

fn quote_of(message: IcmpMessage<'_>) -> Quote<'_> {
    match message {
        IcmpMessage::DestinationUnreachable { quote, .. }
        | IcmpMessage::Redirect { quote, .. }
        | IcmpMessage::TimeExceeded { quote, .. }
        | IcmpMessage::ParameterProblem { quote, .. } => quote,
        other => panic!("no quote in {other:?}"),
    }
}

fn unreachable_quoting(quote: &[u8]) -> Vec<u8> {
    let mut message = vec![3, 3, 0, 0, 0, 0, 0, 0];
    message.extend_from_slice(quote);
    fix_message(&mut message);
    message
}

#[test]
fn s_icmp_001_echo_request() {
    let bytes = echo_icmp();
    assert_eq!(parse(&bytes), Ok(IcmpMessage::EchoRequest(Echo { identifier: 1, sequence: 1, data: b"abcdefgh" })));
}

#[test]
fn s_icmp_002_echo_reply() {
    let bytes = payload_of(&hex(V_ICMP_REPLY));
    assert_eq!(parse(&bytes), Ok(IcmpMessage::EchoReply(Echo { identifier: 1, sequence: 1, data: b"abcdefgh" })));
}

#[test]
fn s_icmp_003_seven_bytes() {
    assert_eq!(parse(&echo_icmp()[..7]), Err(IcmpError::Truncated));
}

#[test]
fn s_icmp_004_echo_without_data() {
    let bytes = fixed(echo_icmp()[..8].to_vec(), |_| {});
    assert_eq!(parse(&bytes), Ok(IcmpMessage::EchoRequest(Echo { identifier: 1, sequence: 1, data: &[] })));
}

#[test]
fn s_icmp_005_checksum_changed() {
    let mut bytes = echo_icmp();
    bytes[3] ^= 0x01;
    assert_eq!(parse(&bytes), Err(IcmpError::Checksum));
}

#[test]
fn s_icmp_006_data_changed() {
    let mut bytes = echo_icmp();
    bytes[10] = b'X';
    assert_eq!(parse(&bytes), Err(IcmpError::Checksum));
}

#[test]
fn s_icmp_007_odd_data_length() {
    let bytes = fixed(echo_icmp()[..15].to_vec(), |_| {});
    assert_eq!(parse(&bytes), Ok(IcmpMessage::EchoRequest(Echo { identifier: 1, sequence: 1, data: b"abcdefg" })));
}

#[test]
fn s_icmp_008_echo_code_1() {
    assert_eq!(parse(&fixed(echo_icmp(), |b| b[1] = 1)), Err(IcmpError::Code));
}

#[test]
fn s_icmp_009_port_unreachable() {
    let bytes = payload_of(&hex(V_ICMP_PORT_UNREACH));
    let message = parse(&bytes).unwrap();
    let IcmpMessage::DestinationUnreachable { code, next_hop_mtu, quote } = message else { panic!("{message:?}") };
    assert_eq!((code, next_hop_mtu), (UnreachableCode::Port, None));
    assert_eq!((quote.source(), quote.destination(), quote.protocol()), (IP_A, IP_DNS, Protocol::Udp));
    assert_eq!(quote.total_length(), 57);
    let transport = quote.transport().unwrap();
    assert_eq!(transport, &hex("c0 00 00 35 00 25 d9 95")[..]);
    let [s0, s1, d0, d1, l0, l1, c0, c1] = *transport;
    assert_eq!(u16::from_be_bytes([s0, s1]), 49152);
    assert_eq!(u16::from_be_bytes([d0, d1]), 53);
    assert_eq!(u16::from_be_bytes([l0, l1]), 37);
    assert_eq!(u16::from_be_bytes([c0, c1]), 0xD995);
}

#[test]
fn s_icmp_010_fragmentation_needed() {
    let bytes = payload_of(&hex(V_ICMP_FRAG_NEEDED));
    let message = parse(&bytes).unwrap();
    let IcmpMessage::DestinationUnreachable { code, next_hop_mtu, quote } = message else { panic!("{message:?}") };
    assert_eq!(code, UnreachableCode::FragmentationNeeded);
    assert_eq!(next_hop_mtu.map(|mtu| mtu.get()), Some(1400));
    assert_eq!(quote.protocol(), Protocol::Tcp);
    assert_eq!((quote.source(), quote.destination()), (IP_A, IP_REMOTE));
    let [s0, s1, d0, d1, q0, q1, q2, q3] = *quote.transport().unwrap();
    assert_eq!((u16::from_be_bytes([s0, s1]), u16::from_be_bytes([d0, d1])), (49153, 443));
    assert_eq!(u32::from_be_bytes([q0, q1, q2, q3]), 0x1111_1111);
    assert_eq!(quote.total_length(), 1500);
    assert_eq!(quote.payload().len(), 8);
}

#[test]
fn s_icmp_011_fragmentation_needed_without_mtu() {
    let bytes = fixed(payload_of(&hex(V_ICMP_FRAG_NEEDED)), |b| b[6..8].copy_from_slice(&[0, 0]));
    assert!(matches!(parse(&bytes), Ok(IcmpMessage::DestinationUnreachable { next_hop_mtu: None, .. })));
}

#[test]
fn s_icmp_012_rfc4884_length_byte() {
    let original = payload_of(&hex(V_ICMP_PORT_UNREACH));
    let bytes = fixed(original.clone(), |b| b[5] = 0x07);
    let (IcmpMessage::DestinationUnreachable { code, quote, .. }, IcmpMessage::DestinationUnreachable { code: code2, quote: quote2, .. }) =
        (parse(&bytes).unwrap(), parse(&original).unwrap())
    else {
        panic!()
    };
    assert_eq!((code, quote), (code2, quote2));
}

#[test]
fn s_icmp_013_unreachable_codes() {
    let named = [
        UnreachableCode::Net,
        UnreachableCode::Host,
        UnreachableCode::Protocol,
        UnreachableCode::Port,
        UnreachableCode::FragmentationNeeded,
        UnreachableCode::SourceRouteFailed,
        UnreachableCode::NetUnknown,
        UnreachableCode::HostUnknown,
        UnreachableCode::SourceHostIsolated,
        UnreachableCode::NetProhibited,
        UnreachableCode::HostProhibited,
        UnreachableCode::NetUnreachableForTos,
        UnreachableCode::HostUnreachableForTos,
        UnreachableCode::CommunicationProhibited,
        UnreachableCode::HostPrecedenceViolation,
        UnreachableCode::PrecedenceCutoff,
    ];
    let base = payload_of(&hex(V_ICMP_PORT_UNREACH));
    for (number, expected) in named.into_iter().enumerate() {
        let bytes = fixed(base.clone(), |b| b[1] = number as u8);
        let Ok(IcmpMessage::DestinationUnreachable { code, .. }) = parse(&bytes) else { panic!("{number}") };
        assert_eq!(code, expected);
    }
    let bytes = fixed(base, |b| b[1] = 16);
    let Ok(IcmpMessage::DestinationUnreachable { code: UnreachableCode::Unassigned(code), .. }) = parse(&bytes) else {
        panic!()
    };
    assert_eq!(code.value(), 16);
}

#[test]
fn s_icmp_014_quote_of_19_bytes() {
    let quote = &payload_of(&hex(V_ICMP_PORT_UNREACH))[8..27];
    assert_eq!(parse(&unreachable_quoting(quote)), Err(IcmpError::QuoteTruncated));
}

#[test]
fn s_icmp_015_quote_short_of_8_payload_bytes() {
    let quote = &payload_of(&hex(V_ICMP_PORT_UNREACH))[8..35];
    assert_eq!(parse(&unreachable_quoting(quote)), Err(IcmpError::QuoteTruncated));
}

fn quote_with_options(payload: &[u8]) -> Vec<u8> {
    let ip = ip_of(&hex(V_UDP_DNS));
    let mut quote = ip[..20].to_vec();
    quote[0] = 0x46;
    quote.extend_from_slice(&[1, 1, 1, 0]);
    quote.extend_from_slice(payload);
    quote
}

#[test]
fn s_icmp_016_quote_with_options() {
    let transport = &ip_of(&hex(V_UDP_DNS))[20..28];
    let bytes = unreachable_quoting(&quote_with_options(transport));
    let quote = quote_of(parse(&bytes).unwrap());
    assert_eq!(quote.options(), [1, 1, 1, 0]);
    assert_eq!(quote.transport().unwrap(), transport);
}

#[test]
fn s_icmp_017_quote_with_options_short() {
    let transport = &ip_of(&hex(V_UDP_DNS))[20..24];
    assert_eq!(parse(&unreachable_quoting(&quote_with_options(transport))), Err(IcmpError::QuoteTruncated));
}

#[test]
fn s_icmp_018_quote_not_ipv4() {
    for first in [0x65, 0x44] {
        let bytes = fixed(payload_of(&hex(V_ICMP_PORT_UNREACH)), |b| b[8] = first);
        assert_eq!(parse(&bytes), Err(IcmpError::QuoteNotIpv4), "{first:#x}");
    }
}

#[test]
fn s_icmp_019_quoted_checksum_not_verified() {
    let bytes = fixed(payload_of(&hex(V_ICMP_PORT_UNREACH)), |b| b[18] ^= 0xFF);
    assert_eq!(quote_of(parse(&bytes).unwrap()).source(), IP_A);
}

#[test]
fn s_icmp_020_rfc4884_padding_beyond_the_quoted_length() {
    let mut quote = ip_of(&hex(V_UDP_DNS))[..28].to_vec();
    quote[2..4].copy_from_slice(&28u16.to_be_bytes());
    quote.resize(128, 0x5A);
    let bytes = unreachable_quoting(&quote);
    let quote = quote_of(parse(&bytes).unwrap());
    assert_eq!(quote.payload().len(), 8);
    assert_eq!(quote.beyond().len(), 100);
    assert!(quote.beyond().iter().all(|&b| b == 0x5A));
}

#[test]
fn s_icmp_021_time_exceeded() {
    let base = payload_of(&hex(V_ICMP_TIME_EXCEEDED));
    let code = |number| match parse(&fixed(base.clone(), |b| b[1] = number)).unwrap() {
        IcmpMessage::TimeExceeded { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert_eq!(code(0), TimeExceededCode::InTransit);
    assert_eq!(code(1), TimeExceededCode::Reassembly);
    assert!(matches!(code(2), TimeExceededCode::Unassigned(c) if c.value() == 2));
}

#[test]
fn s_icmp_022_parameter_problem() {
    let base = hex(V_ICMP_PARAM_PROBLEM);
    let message = parse(&base).unwrap();
    assert!(matches!(message, IcmpMessage::ParameterProblem { code: ParameterProblemCode::Pointer, pointer: 20, .. }));
    let code = |number| match parse(&fixed(base.clone(), |b| b[1] = number)).unwrap() {
        IcmpMessage::ParameterProblem { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert_eq!(code(1), ParameterProblemCode::MissingOption);
    assert_eq!(code(2), ParameterProblemCode::BadLength);
}

#[test]
fn s_icmp_023_redirect() {
    let bytes = hex(V_ICMP_REDIRECT);
    let message = parse(&bytes).unwrap();
    let IcmpMessage::Redirect { code, gateway, .. } = message else { panic!("{message:?}") };
    assert_eq!((code, gateway), (RedirectCode::Host, IP_ROUTER));
}

#[test]
fn s_icmp_024_timestamp_request() {
    let bytes = hex(V_ICMP_TIMESTAMP_REQ);
    let expected = Timestamp { identifier: 7, sequence: 1, originate: 0x0100_0000, receive: 0, transmit: 0 };
    assert_eq!(parse(&bytes), Ok(IcmpMessage::TimestampRequest(expected)));
}

#[test]
fn s_icmp_025_timestamp_length() {
    let base = hex(V_ICMP_TIMESTAMP_REQ);
    assert_eq!(parse(&fixed(base[..19].to_vec(), |_| {})), Err(IcmpError::TimestampLength));
    assert_eq!(parse(&fixed(base, |b| b.push(0))), Err(IcmpError::TimestampLength));
}

#[test]
fn s_icmp_026_timestamp_reply() {
    let bytes = fixed(hex(V_ICMP_TIMESTAMP_REQ), |b| b[0] = 14);
    assert!(matches!(parse(&bytes), Ok(IcmpMessage::TimestampReply(Timestamp { identifier: 7, .. }))));
}

#[test]
fn s_icmp_027_source_quench() {
    let bytes = fixed(hex(V_ICMP_PARAM_PROBLEM), |b| b[..8].copy_from_slice(&[4, 0, 0, 0, 0, 0, 0, 0]));
    assert_eq!(parse(&bytes), Err(IcmpError::SourceQuench));
    assert_eq!(IcmpError::SourceQuench.class(), Class::Unsupported);
    assert_eq!(IcmpError::SourceQuench.name(), "icmp.source-quench");
}

fn eight_byte_message(kind: u8) -> Vec<u8> {
    fixed(vec![kind, 0, 0, 0, 0, 0, 0, 0], |_| {})
}

#[test]
fn s_icmp_028_deprecated_types() {
    for kind in [6, 15, 16, 17, 18, 30, 31, 39] {
        assert_eq!(parse(&eight_byte_message(kind)), Err(IcmpError::DeprecatedType), "{kind}");
    }
}

#[test]
fn s_icmp_029_router_discovery() {
    for kind in [9, 10] {
        assert_eq!(parse(&eight_byte_message(kind)), Err(IcmpError::RouterDiscovery), "{kind}");
    }
}

#[test]
fn s_icmp_030_unknown_types() {
    for kind in [1, 2, 7, 19, 40, 42, 43, 253, 255] {
        assert_eq!(parse(&eight_byte_message(kind)), Err(IcmpError::UnknownType), "{kind}");
    }
}

#[test]
fn s_icmp_031_checksum_before_type() {
    let mut bytes = eight_byte_message(42);
    bytes[2] ^= 0x40;
    assert_eq!(parse(&bytes), Err(IcmpError::Checksum));
}

fn icmp_datagram<P: Ipv4Payload>(payload: P) -> Ipv4Builder<'static, P> {
    Ipv4Builder {
        source: Ipv4Source::new(IP_A).unwrap(),
        destination: IP_B,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        form: Form::Atomic,
        options: &[],
        payload,
    }
}

#[test]
fn s_icmp_047_echo_reply_to_odd_data() {
    let request = fixed(echo_icmp()[..15].to_vec(), |_| {});
    let IcmpMessage::EchoRequest(echo) = parse(&request).unwrap() else { panic!() };
    let reply = EchoBuilder::reply_to(&echo);
    assert_eq!(reply.kind, EchoKind::Reply);
    let mut out = junk(100);
    let built = icmp_datagram(reply).emit(&mut out).unwrap();
    let ip = Ipv4Packet::parse(built).unwrap();
    assert_eq!(parse(ip.payload()), Ok(IcmpMessage::EchoReply(Echo { identifier: 1, sequence: 1, data: b"abcdefg" })));
    assert_eq!(oracle_sum(ip.payload()), 0xFFFF);
}

#[test]
fn s_icmp_048_unreachable_zeroes_unused_bytes() {
    let offending = hex(V_UDP_HI);
    let offending = Ipv4Packet::parse(&offending).unwrap();
    let mut out = junk(100);
    let unreachable = UnreachableBuilder { code: HostUnreachable::Port, datagram: &offending };
    let built = icmp_datagram(unreachable).emit(&mut out).unwrap();
    assert_eq!(built[24..28], [0, 0, 0, 0]);
    assert_eq!(built, hex(V_ICMP_PORT_UNREACH_GEN));
    let offending = hex(V_IP_MIN);
    let offending = Ipv4Packet::parse(&offending).unwrap();
    let unreachable = UnreachableBuilder { code: HostUnreachable::Protocol, datagram: &offending };
    assert_eq!(icmp_datagram(unreachable).emit(&mut out).unwrap(), hex(V_ICMP_PROTO_UNREACH_GEN));
    // A datagram with no payload is quoted whole, and ToyOS parses what it sent.
    let sent = hex(V_ICMP_PROTO_UNREACH_GEN);
    let quote = quote_of(parse(&sent[20..]).unwrap());
    assert_eq!((quote.payload(), quote.transport()), (&[][..], None));
}
