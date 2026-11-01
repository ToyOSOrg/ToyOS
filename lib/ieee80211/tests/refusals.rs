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

/// Frame Control `fc`, then 22 zero octets: Duration, three addresses, Sequence Control.
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

/// An HE Capabilities element whose PHY Capabilities octet 0 is `phy0`, with PPE Thresholds
/// Present (B55) or not, and then `rest`.
fn he_capabilities(phy0: u8, ppe: bool, rest: &[u8]) -> Vec<u8> {
    let mut phy = [0u8; 11];
    phy[0] = phy0;
    phy[6] = if ppe { 0x80 } else { 0 };
    element(255, &[&[35][..], &[0; 6], &phy, &[0xFA, 0xFF, 0xFA, 0xFF], rest].concat())
}

/// A vendor-specific element under WMM's OUI and type.
fn wmm(after_type: &[u8]) -> Vec<u8> {
    element(221, &[&[0x00, 0x50, 0xF2, 0x02][..], after_type].concat())
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
        (QosManagement, frame(&header([0xD0, 0x01]))),
        (ManagementFromDs, frame(&header([0x80, 0x02]))),
        (UnhandledManagement, frame(&header([0x90, 0]))),
        (UnhandledControl, frame(&[0xD4, 0, 0, 0, 2, 0, 0, 0, 0, 1])),
        (UnhandledData, frame(&header([0x18, 0]))),
        (HtcInNonQosData, frame(&header([0x08, 0x80]))),
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
        (RsnVersion, elements(&element(48, &[2, 0]))),
        (RsnCount, elements(&rsn_v1(&[0x00, 0x0F, 0xAC, 4, 2, 0, 0x00, 0x0F, 0xAC, 4]))),
        (
            MfpRequiredNotCapable,
            elements(&rsn_v1(&[0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 8, 0x40, 0])),
        ),
        (ExtendedRsnLength, elements(&element(244, &[0x02, 0x20]))),
        (SecondaryChannelOffset, elements(&ht_operation(2))),
        (VhtReserved, elements(&element(192, &[4, 42, 0, 0xFF, 0xFF]))),
        (PpeThresholds, elements(&he_capabilities(0, true, &[0b0001_1001, 0, 0]))),
        (WmmVersion, elements(&wmm(&[0, 2, 0]))),
        (WmmAifsn, elements(&wmm(&[[1, 1, 0, 0].as_slice(), &[0x01, 0xA4, 0, 0], &[0x27, 0xA4, 0, 0], &[0x42, 0x43, 94, 0], &[0x62, 0x32, 47, 0]].concat()))),
        (WmmAccessCategory, elements(&wmm(&[[1, 1, 0, 0].as_slice(), &[0x03, 0xA4, 0, 0], &[0x27, 0xA4, 0, 0], &[0x42, 0x43, 94, 0], &[0x42, 0x32, 47, 0]].concat()))),
        (ReservedBlockAckType, block_ack(8, &[0x00, 0x00, 0x00, 0x00])),
        (BlockAckVariant, block_ack(8, &[0x02, 0x00, 0x00, 0x00])),
        (BlockAckFragmentNumber, block_ack(8, &[0x04, 0x00, 0x01, 0x00])),
        (FragmentLevel, block_ack(9, &[[0x04, 0x00, 0x01, 0x00].as_slice(), &[0xFF; 8]].concat())),
        (ControlLength, block_ack(8, &[0x04, 0x00, 0x00, 0x00, 0x00])),
        (SubframeLength, amsdu(&[STA.0.as_slice(), &PEER.0, &[0, 100], &[0; 10]].concat())),
        (SubframePadding, amsdu(&subframe(STA, PEER, b"x", false))),
        (SnapHeaderAsAddress, amsdu(&[0xAA, 0xAA, 0x03, 0, 0, 0, 0x08, 0x00, 0x45, 0, 0, 0, 0, 0])),
        (NotSnap, ethernet(&[0xF0, 0xF0, 0x03, 0, 0, 0, 0x08, 0x00])),
        (UnknownOui, ethernet(&[0xAA, 0xAA, 0x03, 0x00, 0x10, 0x18, 0x08, 0x00])),
        (LengthField, ethernet(&[0xAA, 0xAA, 0x03, 0, 0, 0, 0x05, 0xFF])),
        (ExtendedIvClear, refusal(CcmpHeader::split(&[1, 0, 0, 0, 0, 0, 0, 0]))),
        (WindowSize, refusal(Window::<u16>::new(Seq::new(0), 257))),
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
fn an_he_capabilities_element_is_at_least_as_long_as_its_widths_and_ppe_thresholds_say() {
    let parse = |list: Vec<u8>| Elements::parse(&list).map(|e| e.he_capabilities.map(|h| (h.mcs_160.is_some(), h.mcs_80p80.is_some(), h.ppe_thresholds.map(<[u8]>::len))));
    assert_eq!(parse(he_capabilities(0, false, &[])), Ok(Some((false, false, None))));
    assert_eq!(parse(he_capabilities(0x18, false, &[0; 8])), Ok(Some((true, true, None))));
    assert_eq!(parse(he_capabilities(0x08, false, &[0; 3])), Err(Refused::ElementLength));
    assert_eq!(parse(he_capabilities(0x18, false, &[0; 7])), Err(Refused::ElementLength));
    // Two streams and two RU sizes: 7 + 2 × 2 × 6 = 31 bits, four octets; the fifth is ignored.
    assert_eq!(parse(he_capabilities(0, true, &[0b0001_1001, 0, 0, 0])), Ok(Some((false, false, Some(4)))));
    assert_eq!(parse(he_capabilities(0, true, &[0b0001_1001, 0, 0, 0, 0])), Ok(Some((false, false, Some(4)))));
    assert_eq!(parse(he_capabilities(0x08, false, &[0; 8])), Ok(Some((true, false, None))));
}

#[test]
fn ppe_thresholds_are_present_under_phy_capabilities_b55_and_not_b23() {
    // Four octets that would read as PPE Thresholds, after the MCS set.
    let mut b23 = he_capabilities(0, false, &[0xFF; 4]);
    // Element ID, length, extension ID and 6 MAC octets: PHY octet 2 is at 11.
    b23[11] = 0x80;
    assert_eq!(Elements::parse(&b23).unwrap().he_capabilities.unwrap().ppe_thresholds, None);
    assert_eq!(elements(&he_capabilities(0, true, &[])), Refused::PpeThresholds);
}

#[test]
fn an_he_operation_element_is_at_least_as_long_as_its_present_bits_say() {
    let op = |parameters: u32, rest: &[u8]| {
        let p = parameters.to_le_bytes();
        element(255, &[&[36, p[0], p[1], p[2], 0x05, 0xFC, 0xFF][..], rest].concat())
    };
    let list = op(1 << 17, &[37, 0x02, 39, 0, 6]);
    let six = Elements::parse(&list).unwrap().he_operation.unwrap();
    assert_eq!(six.six_ghz.map(|s| (s.primary_channel, s.center_segment0)), Some((37, 39)));
    assert_eq!(six.bss_color & 0x3F, 5);
    let list = op(1 << 14 | 1 << 15 | 1 << 17, &[1, 42, 0, 3, 37, 2, 39, 0, 6]);
    assert_eq!(Elements::parse(&list).unwrap().he_operation.unwrap().vht, Some([1, 42, 0]));
    assert_eq!(elements(&op(1 << 14, &[1, 42])), Refused::ElementLength);
    assert_eq!(Elements::parse(&op(0, &[0])).map(|e| e.he_operation.map(|o| o.vht)), Ok(Some(None)));
    assert_eq!(elements(&element(255, &[36, 0, 0, 0, 0x05, 0xFC])), Refused::ElementLength);
}

#[test]
fn ht_and_vht_elements_ignore_octets_past_their_fields_and_refuse_fewer() {
    let ht_operation = |len: usize| element(61, &vec![0; len]);
    assert!(Elements::parse(&ht_operation(23)).unwrap().ht_operation.is_some());
    assert_eq!(elements(&ht_operation(21)), Refused::ElementLength);
    let vht_operation = |len: usize| element(192, &[&[1, 42, 0, 0xFC, 0xFF][..], &[0; 2]].concat()[..len]);
    assert!(Elements::parse(&vht_operation(7)).unwrap().vht_operation.is_some());
    assert_eq!(elements(&vht_operation(4)), Refused::ElementLength);
}

#[test]
fn a_wmm_information_element_and_another_wmm_subtype_are_read_and_skipped() {
    use toyos_ieee80211::element::Wmm;
    let wmm_of = |list: Vec<u8>| Elements::parse(&list).unwrap().wmm;
    assert_eq!(wmm_of(wmm(&[0, 1, 0x80])), Some(Wmm::Information { qos_info: 0x80 }));
    assert_eq!(wmm_of(wmm(&[2, 1, 0xFF])), None);
    assert_eq!(elements(&wmm(&[0, 1])), Refused::ElementLength);
    assert_eq!(elements(&[wmm(&[0, 1, 0]), wmm(&[0, 1, 0])].concat()), Refused::DuplicateElement);
}

#[test]
fn an_he_block_ack_bitmap_is_as_long_as_its_fragment_number_says() {
    let ba = |fragment: u8, bitmap: &[u8]| {
        let mut f = vec![0x94, 0, 0, 0];
        f.extend(STA.0);
        f.extend(AP.0);
        f.extend([0x04, 0x00, fragment | 0x10, 0x00]);
        f.extend(bitmap);
        match Frame::parse(&f)? {
            Frame::BlockAck(b) => Ok(b),
            other => panic!("not a BlockAck: {other:?}"),
        }
    };
    for (fragment, octets) in [(0b0000, 8), (0b0100, 32)] {
        let mut bitmap = vec![0; octets];
        bitmap[octets - 1] = 0x80;
        let b = ba(fragment, &bitmap).unwrap();
        let last = Seq::new(1).add(u16::try_from(octets * 8 - 1).unwrap());
        assert!(b.acked(last) && !b.acked(last.add(1)) && !b.acked(Seq::new(1)), "{octets} octets");
        assert_eq!(ba(fragment, &bitmap[1..]).err(), Some(Refused::Truncated));
    }
    // Table 9-38 reserves B2–B1 of 1 and 3 and B3 set; B0 is a fragment level.
    for fragment in [0b0010, 0b0110, 0b1000, 0b1100, 0b0011] {
        assert_eq!(ba(fragment, &[0; 32]).err(), Some(Refused::BlockAckFragmentNumber), "{fragment:#06b}");
    }
    for fragment in [0b0001, 0b0101] {
        assert_eq!(ba(fragment, &[0; 32]).err(), Some(Refused::FragmentLevel), "{fragment:#06b}");
    }
}

#[test]
fn a_block_ack_type_is_reserved_or_another_variant_as_its_table_says() {
    let bar = |kind: u8| block_ack(8, &[kind << 1, 0x00, 0x00, 0x00]);
    let ba = |kind: u8| block_ack(9, &[[kind << 1, 0x00, 0x00, 0x00].as_slice(), &[0; 8]].concat());
    for kind in (0..16).filter(|&kind| kind != 2) {
        let bar_wanted = match kind {
            1 | 3 | 6 | 10 => Refused::BlockAckVariant,
            _ => Refused::ReservedBlockAckType,
        };
        let ba_wanted = match kind {
            1 | 6 | 7 | 8 | 10 | 11 => Refused::BlockAckVariant,
            _ => Refused::ReservedBlockAckType,
        };
        assert_eq!(bar(kind), bar_wanted, "BAR Type {kind}");
        assert_eq!(ba(kind), ba_wanted, "BA Type {kind}");
    }
}

#[test]
fn each_type_and_subtype_is_read_or_refused_as_table_9_1_says() {
    use Refused::*;
    for fc0 in (0..=0xFF_u8).step_by(4) {
        let subtype = fc0 >> 4;
        let wanted = match (fc0 >> 2 & 0b11, subtype) {
            (0, 7 | 15) | (1, 0 | 1 | 15) | (2, 2 | 3 | 5..=7 | 13) | (3, 2..) => Some(ReservedSubtype),
            (0, 6 | 9) => Some(UnhandledManagement),
            (0, _) | (1, 8 | 9) | (2, 0 | 4 | 8 | 12) => None,
            (1, _) => Some(UnhandledControl),
            (2, _) => Some(UnhandledData),
            (3, _) => Some(ExtensionFrame),
            _ => unreachable!(),
        };
        let mut f = header([fc0, 0]);
        f.extend([0; 32]);
        if fc0 >> 2 & 0b11 == 1 {
            // A BlockAckReq or BlockAck of the Compressed variant and its 64-bit bitmap.
            f.truncate(16);
            f.extend([0x04, 0x00, 0x00, 0x00]);
            if subtype == 9 {
                f.extend([0; 8]);
            }
        }
        assert_eq!(Frame::parse(&f).err(), wanted, "Frame Control {fc0:#04x}");
    }
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
    assert_eq!(country(b"US \x01\x0b\x14\x24\x04"), Refused::CountryTriplet);
    // One triplet makes the length even; a pad of 0 after it makes it odd.
    assert_eq!(country(b"US \x01\x0b\x14\x00"), Refused::CountryPad);
    // Two triplets make it odd, and they need the pad.
    assert_eq!(country(b"US \x01\x0b\x14\x24\x04\x17"), Refused::CountryPad);
}

#[test]
fn a_country_element_at_its_bounds_is_accepted() {
    use toyos_ieee80211::element::Triplet;
    // Three triplets: an even length without a pad. A first channel of 200 is the last a subband
    // triplet can name; its channels run on past it.
    let list = element(7, b"DE \x01\x0d\x14\xc8\x0b\xf6\xc9\x51\x00");
    let c = Elements::parse(&list).unwrap().country.unwrap();
    assert_eq!(&c.string, b"DE ");
    assert_eq!(
        c.triplets().collect::<Vec<_>>(),
        [
            Triplet::Subband { first_channel: 1, channels: 13, max_power_dbm: 20 },
            Triplet::Subband { first_channel: 200, channels: 11, max_power_dbm: -10 },
            Triplet::Operating { extension_id: 201, class: 81, coverage_class: 0 },
        ]
    );
    // 6 GHz channels 193 to 233 of operating class 131 (Table E-4), and the pad two triplets need.
    let list = element(7, b"US\x04\xc9\x83\x00\xc1\x0b\x00\x00");
    let c = Elements::parse(&list).unwrap().country.unwrap();
    assert_eq!(
        c.triplets().collect::<Vec<_>>(),
        [
            Triplet::Operating { extension_id: 201, class: 131, coverage_class: 0 },
            Triplet::Subband { first_channel: 193, channels: 11, max_power_dbm: 0 },
        ]
    );
    // Channel numbers are the operating class's to judge, a first channel or count of 0 included.
    assert!(Elements::parse(&element(7, b"US \x00\x00\x14")).unwrap().country.is_some());
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
    // Table 9-130 does not mark the RSNE extensible: nothing follows its last field.
    assert_eq!(elements(&element(48, &[&full[..], &[0]].concat())), Refused::ElementLength);
}

#[test]
fn an_rsnxe_ignores_octets_past_its_field() {
    let list = element(244, &[0x20, 0xFF]);
    assert!(Elements::parse(&list).unwrap().rsnx.unwrap().sae_h2e());
}

#[test]
fn llc_snap_refuses_a_length_and_is_rfc_1042_for_every_type() {
    assert_eq!(llc_snap(0x05FF), Err(Refused::LengthField));
    for ethertype in [0x0800, 0x8137, 0x80F3] {
        let [t0, t1] = u16::to_be_bytes(ethertype);
        assert_eq!(llc_snap(ethertype).unwrap(), [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, t0, t1]);
    }
}

#[test]
fn a_bridge_tunnel_msdu_is_refused_as_an_unknown_oui() {
    let msdu = Msdu { da: STA, sa: PEER, payload: &[0xAA, 0xAA, 0x03, 0, 0, 0xF8, 0x81, 0x37, 9] };
    assert_eq!(Ethernet::from_msdu(msdu), Err(Refused::UnknownOui));
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
