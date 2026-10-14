//! Every refusal, reached by a hostile input of its own, and the boundaries beside them accepted.

mod common;

use std::collections::BTreeSet;

use common::{beacon, element, management, qos_from_ds, subframe, AP, PEER, STA};
use toyos_ieee80211::data::{llc_snap, msdus, CcmpHeader, Ethernet, Msdu};
use toyos_ieee80211::element::Elements;
use toyos_ieee80211::frame::{Frame, ManagementSubtype};
use toyos_ieee80211::mgmt::Body;
use toyos_ieee80211::reorder::Window;
use toyos_ieee80211::{Address, Refused, Seq};

fn refusal<T>(result: Result<T, Refused>) -> Refused {
    match result {
        Ok(_) => panic!("accepted"),
        Err(e) => e,
    }
}

fn frame(bytes: &[u8]) -> Refused {
    refusal(Frame::parse(bytes))
}

fn elements(list: &[u8]) -> Refused {
    refusal(Elements::parse(list))
}

fn country(body: &[u8]) -> Refused {
    elements(&element(7, body))
}

/// A header of `fc` and 22 octets of zeros: two addresses' worth past Duration and the third.
fn header(fc: [u8; 2]) -> Vec<u8> {
    let mut f = fc.to_vec();
    f.resize(24, 0);
    f
}

fn ethernet(payload: &[u8]) -> Refused {
    refusal(Ethernet::from_msdu(Msdu { da: STA, sa: PEER, payload }))
}

fn amsdu(body: &[u8]) -> Refused {
    let f = qos_from_ds(true, body);
    let Ok(Frame::Data(d)) = Frame::parse(&f) else { panic!("not data") };
    match msdus(&d, d.body) {
        Err(e) => e,
        Ok(mut m) => refusal(m.try_fold((), |(), r| r.map(drop))),
    }
}

/// A BlockAckReq (subtype 8) or BlockAck (9) after its RA and TA.
fn block_ack(subtype: u8, rest: &[u8]) -> Refused {
    let mut f = vec![subtype << 4 | 0x04, 0, 0, 0];
    f.extend(AP.0);
    f.extend(STA.0);
    f.extend(rest);
    frame(&f)
}

fn window() -> Window<u16> {
    Window::new(Seq::new(100), 64).unwrap()
}

/// One hostile input per refusal.
fn cases() -> Vec<(Refused, Refused)> {
    use Refused::*;
    let mut fragmented = management(8, &[]);
    fragmented[22] = 0x01;
    let mut group_sa = management(8, &[]);
    group_sa[10] |= 1;
    let rsn_v1 = |rest: &[u8]| element(48, &[&[1, 0][..], rest].concat());
    let ht_operation = |first: u8| element(61, &[&[6, first][..], &[0; 20]].concat());
    vec![
        (Truncated, frame(&[0x80, 0, 0])),
        (ProtocolVersion, frame(&header([0x81, 0]))),
        (ExtensionFrame, frame(&header([0x0C, 0]))),
        (ReservedSubtype, frame(&header([0x70, 0]))),
        (ManagementWithDs, frame(&header([0x80, 0x01]))),
        (UnhandledManagement, frame(&header([0x90, 0]))),
        (UnhandledControl, frame(&[0xD4, 0, 0, 0, 2, 0, 0, 0, 0, 1])),
        (ContentionFreeData, frame(&header([0x18, 0]))),
        (StrictlyOrdered, frame(&header([0x08, 0x80]))),
        (Fragmented, frame(&fragmented)),
        (GroupTransmitter, frame(&group_sa)),
        (ZeroBeaconInterval, refusal(Body::parse(ManagementSubtype::Beacon, &[0; 12]))),
        (AssociationId, refusal(Body::parse(ManagementSubtype::AssociationResponse, &[0, 0, 0, 0, 0, 0xC0]))),
        (ElementPastBody, elements(&[0, 5, b'a'])),
        (ExtensionWithoutId, elements(&[255, 0])),
        (DuplicateElement, elements(&[element(0, b"a"), element(0, b"b")].concat())),
        (ElementLength, elements(&element(3, &[1, 1]))),
        (SsidLength, elements(&element(0, &[b'a'; 33]))),
        (RateCount, elements(&element(1, &[2; 9]))),
        (DtimPeriod, elements(&element(5, &[0, 0, 0, 0]))),
        (TimBitmap, elements(&element(5, &[0, 1, 250, 0, 0]))),
        (CountryString, country(b"us \x01\x0b\x14")),
        (CountryPad, country(b"US \x01\x0b\x14\x01")),
        (CountryTriplet, country(b"US \x01\x0b")),
        (CountryChannels, country(b"US \x24\x00\x17")),
        (RsnVersion, elements(&element(48, &[2, 0]))),
        (RsnCount, elements(&rsn_v1(&[0x00, 0x0F, 0xAC, 4, 2, 0, 0x00, 0x0F, 0xAC, 4]))),
        (
            MfpRequiredNotCapable,
            elements(&rsn_v1(&[0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 8, 0x40, 0])),
        ),
        (ExtendedRsnLength, elements(&element(244, &[0x02, 0x20]))),
        (SecondaryChannelOffset, elements(&ht_operation(2))),
        (VhtReserved, elements(&element(192, &[4, 42, 0, 0xFF, 0xFF]))),
        (BlockAckVariant, block_ack(8, &[0x00, 0x00, 0x00, 0x00])),
        (BitmapSize, block_ack(9, &[[0x04, 0x00, 0x01, 0x00].as_slice(), &[0xFF; 8]].concat())),
        (ControlLength, block_ack(8, &[0x04, 0x00, 0x00, 0x00, 0x00])),
        (SubframeLength, amsdu(&[STA.0.as_slice(), &PEER.0, &[0, 100], &[0; 10]].concat())),
        (SubframePadding, amsdu(&subframe(STA, PEER, b"x", false))),
        (SnapHeaderAsAddress, amsdu(&[0xAA, 0xAA, 0x03, 0, 0, 0, 0x08, 0x00, 0x45, 0, 0, 0, 0, 0])),
        (NotSnap, ethernet(&[0xF0, 0xF0, 0x03, 0, 0, 0, 0x08, 0x00])),
        (UnknownOui, ethernet(&[0xAA, 0xAA, 0x03, 0x00, 0x10, 0x18, 0x08, 0x00])),
        (TranslatedType, ethernet(&[0xAA, 0xAA, 0x03, 0, 0, 0, 0x81, 0x37])),
        (LengthField, ethernet(&[0xAA, 0xAA, 0x03, 0, 0, 0, 0x05, 0xFF])),
        (ExtendedIvClear, refusal(CcmpHeader::split(&[1, 0, 0, 0, 0, 0, 0, 0]))),
        (WindowSize, refusal(Window::<u16>::new(Seq::new(0), 65))),
        (OldSequence, refusal(window().receive(Seq::new(100 + 2048), 0, &mut Vec::new()))),
        (DuplicateSequence, {
            let mut w = window();
            w.receive(Seq::new(105), 0, &mut Vec::new()).unwrap();
            refusal(w.receive(Seq::new(105), 1, &mut Vec::new()))
        }),
    ]
}

#[test]
fn every_refusal_has_a_hostile_input_that_produces_it() {
    let cases = cases();
    for (wanted, got) in &cases {
        assert_eq!(got, wanted);
    }
    let named: BTreeSet<Refused> = cases.iter().map(|(wanted, _)| *wanted).collect();
    let all: BTreeSet<Refused> = Refused::ALL.iter().copied().collect();
    assert_eq!(all.difference(&named).collect::<Vec<_>>(), Vec::<&Refused>::new());
}

#[test]
fn every_prefix_of_a_frame_is_refused_as_truncated_short_of_its_header() {
    let f = management(8, &beacon(&element(0, b"net")));
    for len in 0..24 {
        assert_eq!(frame(&f[..len]), Refused::Truncated, "{len} octets");
    }
    for len in 24..36 {
        let Ok(Frame::Management(m)) = Frame::parse(&f[..len]) else { panic!("{len} octets") };
        assert_eq!(refusal(Body::parse(m.subtype, m.body)), Refused::Truncated, "{len} octets");
    }
}

#[test]
fn an_element_whose_length_runs_one_past_the_body_is_refused() {
    let mut list = element(0, b"net");
    list[1] = 4;
    assert_eq!(elements(&list), Refused::ElementPastBody);
    assert_eq!(elements(&[0]), Refused::ElementPastBody);
}

#[test]
fn a_malformed_country_element_is_refused_whichever_rule_it_breaks() {
    assert_eq!(country(b"US"), Refused::ElementLength);
    assert_eq!(country(b"US "), Refused::ElementLength);
    assert_eq!(country(b"US \x01"), Refused::CountryPad);
    assert_eq!(country(b"U1 \x01\x0b\x14"), Refused::CountryString);
    assert_eq!(country(b"US \x00\x0b\x14"), Refused::CountryChannels);
    assert_eq!(country(b"US \xbe\x0c\x14"), Refused::CountryChannels);
    assert_eq!(country(b"US \x01\x0b\x14\x24\x04"), Refused::CountryTriplet);
}

#[test]
fn a_country_element_at_its_bounds_is_accepted() {
    use toyos_ieee80211::element::Triplet;
    // Channel 190 onward, eleven of them, ends at channel 200; a pad of 0 evens the length.
    let list = element(7, b"DE \x01\x0d\x14\xbe\x0b\xf6\xc9\x51\x00\x00");
    let c = Elements::parse(&list).unwrap().country.unwrap();
    assert_eq!(&c.string, b"DE ");
    assert_eq!(
        c.triplets().collect::<Vec<_>>(),
        [
            Triplet::Subband { first_channel: 1, channels: 13, max_power_dbm: 20 },
            Triplet::Subband { first_channel: 190, channels: 11, max_power_dbm: -10 },
            Triplet::Operating { extension_id: 201, class: 81, coverage_class: 0 },
        ]
    );
}

#[test]
fn an_ssid_of_32_octets_and_a_tim_reaching_aid_2007_are_accepted() {
    let tim_last_octet = [&[0, 1, 250][..], &[0x80]].concat();
    let list = [element(0, &[b'a'; 32]), element(5, &tim_last_octet)].concat();
    let e = Elements::parse(&list).unwrap();
    assert_eq!(e.ssid.unwrap().len(), 32);
    let tim = e.tim.unwrap();
    assert!(tim.buffered(2007));
    assert!(!tim.buffered(2006));
    assert!(!tim.buffered(7));
}

#[test]
fn an_association_response_takes_aid_1_to_2007_with_its_two_high_bits_set() {
    let response = |aid: u16| {
        let body = [&0u16.to_le_bytes()[..], &0u16.to_le_bytes(), &(aid | 0xC000).to_le_bytes()].concat();
        match Body::parse(ManagementSubtype::AssociationResponse, &body) {
            Ok(Body::AssociationResponse(r)) => Ok(r.aid),
            Ok(_) => panic!("another body"),
            Err(e) => Err(e),
        }
    };
    assert_eq!(response(1), Ok(Some(1)));
    assert_eq!(response(2007), Ok(Some(2007)));
    assert_eq!(response(2008), Err(Refused::AssociationId));
}

#[test]
fn an_rsn_element_may_end_after_any_whole_field_and_not_inside_one() {
    let full = [
        &[1, 0][..],
        &[0x00, 0x0F, 0xAC, 4],
        &[1, 0, 0x00, 0x0F, 0xAC, 4],
        &[1, 0, 0x00, 0x0F, 0xAC, 8],
        &[0xC0, 0x00],
        &[0, 0],
        &[0x00, 0x0F, 0xAC, 6],
    ]
    .concat();
    let field_ends = [2, 6, 12, 18, 20, 22, 26];
    for len in 2..=full.len() {
        let list = element(48, &full[..len]);
        let result = Elements::parse(&list);
        assert_eq!(result.is_ok(), field_ends.contains(&len), "{len} octets: {:?}", result.err());
    }
    let list = element(48, &full);
    let rsn = Elements::parse(&list).unwrap().rsn.unwrap();
    assert!(rsn.mfp_required() && rsn.mfp_capable());
}

#[test]
fn llc_snap_refuses_a_length_and_tunnels_aarp_and_ipx() {
    assert_eq!(llc_snap(0x05FF), Err(Refused::LengthField));
    assert_eq!(llc_snap(0x8137).unwrap()[..6], [0xAA, 0xAA, 0x03, 0x00, 0x00, 0xF8]);
    assert_eq!(llc_snap(0x80F3).unwrap()[..6], [0xAA, 0xAA, 0x03, 0x00, 0x00, 0xF8]);
    assert_eq!(llc_snap(0x0800).unwrap(), [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00]);
}

#[test]
fn a_bridge_tunnel_msdu_is_its_ethertype_on_ethernet() {
    let eth = Ethernet::from_msdu(Msdu { da: STA, sa: PEER, payload: &[0xAA, 0xAA, 0x03, 0, 0, 0xF8, 0x81, 0x37, 9] }).unwrap();
    assert_eq!((eth.ethertype, eth.payload), (0x8137, &[9][..]));
}

#[test]
fn a_block_ack_transmitter_with_its_bandwidth_signaling_bit_is_the_individual_address() {
    let mut f = vec![0x84, 0, 0, 0];
    f.extend(STA.0);
    f.extend([AP.0[0] | 1, AP.0[1], AP.0[2], AP.0[3], AP.0[4], AP.0[5]]);
    f.extend([0x04, 0x30, 0x10, 0x00]);
    let Ok(Frame::BlockAckReq(bar)) = Frame::parse(&f) else { panic!("not a BlockAckReq") };
    assert_eq!((bar.ta, bar.tid, bar.ssn), (AP, 3, Seq::new(1)));
    assert_ne!(bar.ta, Address::BROADCAST);
}
