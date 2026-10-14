//! Frames other implementations wrote, from tcpdump's test captures (`fixtures/tcpdump/`, pinned in
//! `NOTICE`), each read as the standard says its bytes read.

mod common;

use toyos_ieee80211::data::{msdus, CcmpHeader, Ethernet};
use toyos_ieee80211::element::{Elements, SecondaryChannel, Suite, VhtChannelWidth};
use toyos_ieee80211::frame::{Data, Frame, ManagementSubtype};
use toyos_ieee80211::mgmt::Body;
use toyos_ieee80211::Address;

fn frames(name: &str) -> Vec<Vec<u8>> {
    let path = format!("{}/tests/fixtures/tcpdump/{name}.pcap", env!("CARGO_MANIFEST_DIR"));
    let file = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    common::pcap(&file).iter().map(common::mpdu).collect()
}

/// A frame's outcome in a word: its kind, or its own or its body's or elements' refusal.
fn outcome(mpdu: &[u8]) -> String {
    match Frame::parse(mpdu) {
        Err(e) => format!("{e:?}"),
        Ok(Frame::Data(d)) => format!("{}Data", if d.null { "Null" } else { "" }),
        Ok(Frame::BlockAck(_) | Frame::BlockAckReq(_)) => "BlockAck".into(),
        Ok(Frame::Management(m)) => {
            let elements = match Body::parse(m.subtype, m.body) {
                Err(e) => return format!("{:?}:{e:?}", m.subtype),
                Ok(Body::Beacon(b) | Body::ProbeResponse(b)) => Some(b.elements),
                Ok(Body::ProbeRequest { elements }) => Some(elements),
                Ok(Body::AssociationRequest(a) | Body::ReassociationRequest(a)) => Some(a.elements),
                Ok(Body::AssociationResponse(a) | Body::ReassociationResponse(a)) => Some(a.elements),
                Ok(_) => None,
            };
            match elements.map(Elements::parse) {
                Some(Err(e)) => format!("{:?}:{e:?}", m.subtype),
                _ => format!("{:?}", m.subtype),
            }
        }
    }
}

fn data(mpdu: &[u8]) -> Data<'_> {
    match Frame::parse(mpdu) {
        Ok(Frame::Data(d)) => d,
        other => panic!("not a data frame: {other:?}"),
    }
}

#[test]
fn every_capture_frame_reads_as_its_bytes_say_or_is_refused_by_name() {
    let mut seen = Vec::new();
    for name in [
        "ieee802.11_exthdr",
        "ieee802.11_htc",
        "ieee802.11_meshid",
        "ieee802.11_rx-stbc",
        "ieee802.11_parse_elements_oobr",
        "ieee802.11_rates_oobr",
        "ieee802.11_tim_ie_oobr",
        "ieee802.11_meshhdr-oobr",
    ] {
        seen.push(format!("{name}: {}", frames(name).iter().map(|f| outcome(f)).collect::<Vec<_>>().join(" ")));
    }
    let exthdr_scan = "ProbeRequest UnhandledControl ProbeResponse ".repeat(6);
    let expected = [
        format!(
            "ieee802.11_exthdr: {exthdr_scan}Authentication UnhandledControl Authentication AssociationRequest \
             UnhandledControl AssociationResponse NullData NullData"
        ),
        "ieee802.11_htc: Data".into(),
        "ieee802.11_meshid: Beacon ProbeRequest ProbeResponse".into(),
        "ieee802.11_rx-stbc: Data Data Data".into(),
        "ieee802.11_parse_elements_oobr: Beacon:RsnVersion".into(),
        "ieee802.11_rates_oobr: ProtocolVersion".into(),
        "ieee802.11_tim_ie_oobr: ReassociationResponse:RsnVersion ReassociationResponse:ElementPastBody \
         Truncated ReassociationResponse:ElementPastBody"
            .into(),
        "ieee802.11_meshhdr-oobr: ProtocolVersion".into(),
    ];
    assert_eq!(seen, expected);
}

#[test]
fn a_captured_sae_beacon_reads_its_rsn_ht_and_vht_operation() {
    let beacon = &frames("ieee802.11_meshid")[0];
    let Ok(Frame::Management(m)) = Frame::parse(beacon) else { panic!("not management") };
    assert_eq!(m.subtype, ManagementSubtype::Beacon);
    let Ok(Body::Beacon(b)) = Body::parse(m.subtype, m.body) else { panic!("not a beacon body") };
    let e = Elements::parse(b.elements).unwrap();

    assert_eq!(e.dsss_channel, Some(149));
    let rsn = e.rsn.unwrap();
    assert_eq!(rsn.group_data, Some(Suite::CCMP_128));
    assert!(rsn.pairwise.unwrap().contains(Suite::CCMP_128));
    assert!(rsn.akm.unwrap().contains(Suite::AKM_SAE));
    assert!(!rsn.mfp_capable());
    let ht = e.ht_operation.unwrap();
    assert_eq!((ht.primary_channel, ht.secondary), (149, SecondaryChannel::Above));
    let vht = e.vht_operation.unwrap();
    // Channels 149 to 161 are one 80 MHz channel, centred on 155.
    assert_eq!((vht.width, vht.center_segment0, vht.center_segment1), (VhtChannelWidth::Eighty, 155, 0));
    let tim = e.tim.unwrap();
    assert_eq!((tim.dtim_count, tim.dtim_period, tim.group_buffered()), (1, 2, false));
}

#[test]
fn a_captured_association_response_carries_the_wmm_default_parameters() {
    use toyos_ieee80211::element::{AcParameters, Wmm};
    let response = &frames("ieee802.11_exthdr")[23];
    let Ok(Frame::Management(m)) = Frame::parse(response) else { panic!("not management") };
    let Ok(Body::AssociationResponse(r)) = Body::parse(m.subtype, m.body) else { panic!("not a response") };
    assert_eq!(r.aid, Some(1));
    let Some(Wmm::Parameter { access_categories: [be, bk, vi, vo], .. }) = Elements::parse(r.elements).unwrap().wmm else {
        panic!("no WMM Parameter element")
    };
    let ac = |aifsn, ecw_min, ecw_max, txop_limit| AcParameters { aifsn, admission_control: false, ecw_min, ecw_max, txop_limit };
    // AC_VI's TXOP is 3.008 ms and AC_VO's 1.504 ms: 94 and 47 units of 32 µs.
    assert_eq!([be, bk, vi, vo], [ac(3, 4, 10, 0), ac(7, 4, 10, 0), ac(2, 3, 4, 94), ac(2, 2, 3, 47)]);
}

#[test]
fn a_captured_ccmp_frame_keeps_its_header_and_carries_an_ipv4_msdu() {
    let frame = &frames("ieee802.11_rx-stbc")[0];
    let d = data(frame);
    assert!(d.flags.protected);
    let (ccmp, plaintext) = CcmpHeader::split(d.body).unwrap();
    assert_eq!((ccmp.pn, ccmp.key_id), (17, 0));

    let msdu = msdus(&d, plaintext).unwrap().next().unwrap().unwrap();
    let eth = Ethernet::from_msdu(msdu).unwrap();
    // From DS: Address 1 is the destination and Address 3 the source.
    assert_eq!((eth.dst, eth.src), (d.addr1, d.addr3));
    assert_eq!(eth.ethertype, 0x0800);
    assert_eq!(eth.payload[..2], [0x45, 0x00]);
}

#[test]
fn a_captured_to_ds_frame_with_ht_control_reaches_ethernet_as_a_broadcast_dhcp_request() {
    let frame = &frames("ieee802.11_htc")[0];
    let d = data(frame);
    assert!(d.ht_control.is_some());
    let msdu = msdus(&d, d.body).unwrap().next().unwrap().unwrap();
    let eth = Ethernet::from_msdu(msdu).unwrap();
    // To DS: Address 3 is the destination and Address 2 the source.
    assert_eq!((eth.dst, eth.src), (Address::BROADCAST, d.addr2));
    assert_eq!(eth.ethertype, 0x0800);
    let udp = &eth.payload[20..24];
    assert_eq!(udp, [0, 68, 0, 67]);
}
