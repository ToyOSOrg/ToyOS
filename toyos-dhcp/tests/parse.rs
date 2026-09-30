//! §D3 and §D4: reading and validating server messages. Each input reaches a client in
//! SELECTING after DH-01 unless it says otherwise.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_dhcp::{Config, Counter, Output, Phase};

fn selecting() -> D {
    D::ds(&[XID, 1_000]).0
}

/// Delivers `message` to a selecting client: the refusal it counted and whether it logged one.
fn refused(message: &[u8], rule: Counter) {
    let mut d = selecting();
    let out = d.receive(10, message);
    assert_eq!(out, Output::default(), "{rule:?}");
    assert_eq!(d.count(rule), 1, "{rule:?}");
    assert_eq!(d.logged(rule), usize::from(rule.logged()), "{rule:?}");
    assert_eq!(d.client.phase(), Phase::Selecting);
}

/// Delivers `message`: a REQUEST must follow.
fn accepted(message: &[u8]) -> D {
    let mut d = selecting();
    let out = d.receive(10, message);
    assert_eq!(message_type(sent(&out)), 3, "a REQUEST follows");
    d
}

fn offer_with(code: u8, data: Option<Vec<u8>>) -> Vec<u8> {
    server(XID, A, &with(offer_options(), code, data))
}

fn edited(change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut m = offer();
    change(&mut m);
    m
}

#[test]
fn s_dhcp_dh_038_truncated() {
    refused(&offer()[..239], Counter::Truncated);
}

#[test]
fn s_dhcp_dh_039_not_a_reply() {
    refused(&edited(|m| m[0] = 1), Counter::NotReply);
    refused(
        &edited(|m| {
            m[0] = 1;
            m[28..34].copy_from_slice(&MAC_B.0);
        }),
        Counter::NotReply,
    );
}

#[test]
fn s_dhcp_dh_040_the_hardware_type() {
    refused(&edited(|m| m[1] = 6), Counter::HardwareType);
    refused(&edited(|m| m[2] = 16), Counter::HardwareType);
}

#[test]
fn s_dhcp_dh_041_another_clients_reply() {
    refused(&edited(|m| m[28..34].copy_from_slice(&MAC_B.0)), Counter::ChaddrMismatch);
    accepted(&edited(|m| m[34..44].fill(0xff)));
}

#[test]
fn s_dhcp_dh_042_another_transaction() {
    refused(&edited(|m| m[7] = 0xf8), Counter::XidMismatch);
}

#[test]
fn s_dhcp_dh_043_bootp_is_refused() {
    refused(&edited(|m| m[236..240].fill(0)), Counter::BootpReply);
    refused(
        &edited(|m| {
            m[236..240].fill(0);
            m[7] = 0xf8;
        }),
        Counter::BootpReply,
    );
}

#[test]
fn s_dhcp_dh_044_past_end_and_padding() {
    assert_eq!(offer(), hex(V_DHCP_OFFER), "the harness builds the vector");
    accepted(&[offer(), vec![0xee; 8]].concat());
    let mut padded = offer();
    padded.pop();
    padded.resize(1_471, 0);
    padded.push(255);
    assert_eq!(padded.len(), 1_472);
    accepted(&padded);
}

#[test]
fn s_dhcp_dh_045_an_option_past_its_field() {
    let mut m = offer();
    let n = m.len();
    m.truncate(n - 7);
    m.extend_from_slice(&[6, 5, 192, 0, 2, 0x35]);
    refused(&m, Counter::OptionTruncated);
}

#[test]
fn s_dhcp_dh_046_no_end() {
    let mut m = offer();
    m.pop();
    assert_eq!(m.len(), 273);
    let d = accepted(&m);
    assert_eq!(d.count(Counter::NoEnd), 1);
}

#[test]
fn s_dhcp_dh_047_a_repeated_message_type() {
    let mut m = offer();
    m.pop();
    m.extend_from_slice(&[53, 1, 2, 255]);
    refused(&m, Counter::OptionLength);
}

#[test]
fn s_dhcp_dh_048_option_lengths() {
    for (code, data) in [(54, vec![192, 0, 2, 254, 0]), (1, vec![255, 255, 255]), (3, vec![0; 6]), (6, vec![]), (51, vec![0, 1])] {
        refused(&offer_with(code, Some(data)), Counter::OptionLength);
    }
}

#[test]
fn s_dhcp_dh_049_split_options_are_joined() {
    accepted(&hex(V_DHCP_OFFER_SPLIT));
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &hex(V_DHCP_OFFER_SPLIT));
    let mut split = hex(V_DHCP_OFFER_SPLIT);
    split[242] = 5;
    d.receive(20, &split);
    let Some(Config::Configured(lease)) = d.verified(5_020).config else { panic!() };
    assert_eq!(lease.dns, [DNS, DNS2]);
}

#[test]
fn s_dhcp_dh_050_an_overloaded_file_field() {
    let mut d = selecting();
    let out = d.receive(10, &hex(V_DHCP_OFFER_OVERLOAD));
    assert_eq!(sent(&out).payload, payload_of(V_DHCP_REQUEST));
}

#[test]
fn s_dhcp_dh_051_overload_misused() {
    let overload = hex(V_DHCP_OFFER_OVERLOAD);
    let mut value_4 = overload.clone();
    value_4[245] = 4;
    refused(&value_4, Counter::OverloadInvalid);
    let n = overload.len();
    let mut length_2 = overload[..n - 1].to_vec();
    length_2[244] = 2;
    length_2.extend_from_slice(&[0, 255]);
    refused(&length_2, Counter::OverloadInvalid);
    let mut nested = overload;
    nested[126..130].copy_from_slice(&[52, 1, 1, 255]);
    refused(&nested, Counter::OverloadInvalid);
}

#[test]
fn s_dhcp_dh_052_message_types() {
    refused(&offer_with(53, None), Counter::MessageTypeMissing);
    refused(&offer_with(53, Some(vec![1])), Counter::WrongDirection);
    refused(&offer_with(53, Some(vec![3])), Counter::WrongDirection);
    refused(&offer_with(53, Some(vec![10])), Counter::MessageTypeUnsupported);
    let mut d = D::db();
    d.receive(100_000, &server(0x1234_5678, A, &with(offer_options(), 53, Some(vec![9]))));
    assert_eq!((d.count(Counter::Forcerenew), d.count(Counter::XidMismatch)), (1, 0));
    assert_eq!(d.logged(Counter::Forcerenew), 1);
}

#[test]
fn s_dhcp_dh_053_a_foreign_client_identifier() {
    let mut id = CLIENT_ID.to_vec();
    id[14] = 0x0b;
    refused(&offer_with(61, Some(id)), Counter::ClientIdMismatch);
}

#[test]
fn s_dhcp_dh_054_the_server_identifier() {
    refused(&offer_with(54, None), Counter::NoServerId);
    for bad in [Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, Ipv4Addr::new(224, 0, 0, 1), Ipv4Addr::new(127, 0, 0, 1)] {
        refused(&offer_with(54, Some(ip(bad))), Counter::ServerIdInvalid);
    }
}

#[test]
fn s_dhcp_dh_055_yiaddr_must_be_a_host() {
    for bad in [
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::new(0, 1, 2, 3),
        Ipv4Addr::new(127, 0, 0, 1),
        Ipv4Addr::new(169, 254, 1, 1),
        Ipv4Addr::new(224, 0, 0, 251),
        Ipv4Addr::new(240, 0, 0, 1),
        Ipv4Addr::BROADCAST,
    ] {
        refused(&server(XID, bad, &offer_options()), Counter::YiaddrInvalid);
    }
}

#[test]
fn s_dhcp_dh_056_the_lease_time() {
    refused(&offer_with(51, None), Counter::NoLeaseTime);
    refused(&offer_with(51, Some(seconds(0))), Counter::LeaseZero);
}

#[test]
fn s_dhcp_dh_057_the_mask() {
    refused(&offer_with(1, None), Counter::NoSubnetMask);
    for bad in [[255, 0, 255, 0], [0, 0, 0, 0], [255, 255, 255, 255]] {
        refused(&offer_with(1, Some(bad.to_vec())), Counter::MaskInvalid);
    }
    let slash_31 = with(with(offer_options(), 1, Some(vec![255, 255, 255, 254])), 3, None);
    accepted(&server(XID, Ipv4Addr::new(192, 0, 2, 0), &slash_31));
    accepted(&offer_with(1, Some(vec![128, 0, 0, 0])));
}

#[test]
fn s_dhcp_dh_058_network_and_broadcast_yiaddr() {
    for bad in [Ipv4Addr::new(192, 0, 2, 0), Ipv4Addr::new(192, 0, 2, 255)] {
        refused(&server(XID, bad, &offer_options()), Counter::YiaddrInvalid);
    }
    let slash_30 = with(offer_options(), 1, Some(vec![255, 255, 255, 252]));
    refused(&server(XID, Ipv4Addr::new(192, 0, 2, 3), &slash_30), Counter::YiaddrInvalid);
    accepted(&server(XID, A, &slash_30));
}

fn configured(ack_options: Vec<(u8, Vec<u8>)>) -> (D, toyos_dhcp::Lease) {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    d.receive(20, &server(XID, A, &ack_options));
    let Some(Config::Configured(lease)) = d.verified(5_020).config else { panic!() };
    (d, lease)
}

#[test]
fn s_dhcp_dh_059_the_first_usable_router() {
    let routers = [ip(Ipv4Addr::UNSPECIFIED), ip(Ipv4Addr::new(198, 51, 100, 1)), ip(A), ip(R)].concat();
    let (d, lease) = configured(with(ack_options(), 3, Some(routers)));
    assert_eq!(lease.router, Some(R));
    assert_eq!((d.count(Counter::RouterInvalid), d.logged(Counter::RouterInvalid)), (3, 3));
    let (_, lease) = configured(with(ack_options(), 3, Some(ip(Ipv4Addr::new(198, 51, 100, 1)))));
    assert_eq!(lease.router, None);
    assert_eq!(lease.address, A);
}

#[test]
fn s_dhcp_dh_060_at_most_three_resolvers() {
    let dns = [
        Ipv4Addr::UNSPECIFIED,
        DNS,
        Ipv4Addr::new(224, 0, 0, 1),
        Ipv4Addr::new(192, 0, 2, 255),
        DNS2,
        Ipv4Addr::new(192, 0, 2, 54),
        Ipv4Addr::new(192, 0, 2, 55),
    ];
    let (d, lease) = configured(with(ack_options(), 6, Some(dns.iter().flat_map(|a| a.octets()).collect())));
    assert_eq!(lease.dns, [DNS, DNS2, Ipv4Addr::new(192, 0, 2, 54)]);
    assert_eq!((d.count(Counter::DnsInvalid), d.count(Counter::DnsTruncated)), (3, 1));
}

#[test]
fn s_dhcp_dh_061_identity_while_requesting() {
    let requesting = || {
        let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
        d.receive(10, &offer());
        d
    };
    let mut d = requesting();
    d.receive(20, &server(XID, A, &with(ack_options(), 54, Some(ip(OTHER_SERVER)))));
    assert_eq!((d.logged(Counter::AckWrongServer), d.client.phase()), (1, Phase::Requesting));
    let mut d = requesting();
    d.receive(20, &server(XID, Ipv4Addr::new(192, 0, 2, 7), &ack_options()));
    assert_eq!((d.logged(Counter::AckAddressChanged), d.client.phase()), (1, Phase::Requesting));
    let mut d = requesting();
    d.receive(20, &nak(XID, OTHER_SERVER));
    assert_eq!((d.logged(Counter::NakWrongServer), d.client.phase()), (1, Phase::Requesting));
}

#[test]
fn s_dhcp_dh_062_offers_nobody_asked_for() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    d.receive(15, &server(XID, A, &with(offer_options(), 54, Some(ip(OTHER_SERVER)))));
    assert_eq!((d.count(Counter::UnexpectedOffer), d.client.phase()), (1, Phase::Requesting));
    let mut d = D::db();
    d.receive(100_000, &hex(V_DHCP_OFFER));
    assert_eq!(d.count(Counter::XidMismatch), 1);
}

#[test]
fn s_dhcp_dh_063_a_refused_offer_then_another() {
    let mut d = selecting();
    d.receive(10, &offer_with(51, Some(seconds(0))));
    let out = d.receive(20, &server(XID, A, &with(offer_options(), 54, Some(ip(OTHER_SERVER)))));
    let payload = &sent(&out).payload;
    assert_eq!(payload[243..249], [54, 4, 192, 0, 2, 253]);
}
