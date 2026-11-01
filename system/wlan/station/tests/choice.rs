//! Which BSS a connect joins, and every one it refuses by name.

mod common;

use common::*;
use toyos_ieee80211::Address;
use toyos_radio::{Band, Event};
use toyos_wifi::{Refusal, Security};
use toyos_wlan_station::{STRONG, Unjoinable};

/// Connect to [`SSID`] over a scan that heard `heard` (address, MHz, dBm, frame): the BSSID
/// joined, or the refusal.
fn join_among(heard: &[(Address, u16, i8, Vec<u8>)]) -> Result<Address, (Refusal, String)> {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    assert_eq!(h.sent(), [Sent::Scan]);
    for (_, mhz, dbm, frame) in heard {
        h.seen(channel(*mhz), *dbm, frame);
    }
    h.event(Event::ScanDone);
    match (&h.sent()[..], &h.answered()[..]) {
        ([Sent::Join { bssid, .. }], []) => Ok(*bssid),
        ([], [(1, Answered::Refused(r, d))]) => Err((*r, d.clone())),
        other => panic!("{other:?}"),
    }
}

fn one(mhz: u16, dbm: i8, security: &[u8]) -> Vec<(Address, u16, i8, Vec<u8>)> {
    vec![(address(10), mhz, dbm, bss_frame(address(10), SSID, channel(mhz), security))]
}

fn refused(heard: &[(Address, u16, i8, Vec<u8>)], why: Unjoinable) {
    assert_eq!(join_among(heard), Err((Refusal::Unjoinable, why.reason().to_string())), "{why:?}");
}

#[test]
fn a_bss_without_what_this_station_joins_by_is_refused_by_name() {
    refused(&one(2412, -40, &[]), Unjoinable::Open);
    refused(&one(2412, -40, &rsne(&[8], &[4], 4, 0xc0, None)), Unjoinable::SaeOnly);
    refused(&one(2412, -40, &rsne(&[1], &[4], 4, 0, None)), Unjoinable::NoPsk);
    refused(&one(2412, -40, &rsne(&[2], &[2], 4, 0, None)), Unjoinable::Pairwise);
    refused(&one(2412, -40, &rsne(&[2], &[4], 2, 0, None)), Unjoinable::Group);
    refused(&one(2412, -40, &rsne(&[2], &[4], 4, 0xc0, Some(12))), Unjoinable::Management);
    // A basic SAE hash-to-element-only selector (123), and an unknown one (125).
    for selector in [0x80 | 123, 0x80 | 125] {
        let elements = elements(SSID, channel(2412), &[0x8c, selector], &wpa2());
        let frame = beacon_frame(address(10), &elements, false);
        refused(&[(address(10), 2412, -40, frame)], Unjoinable::Selector);
    }
}

#[test]
fn a_bss_in_a_band_the_radio_lacks_is_refused() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    // 6 GHz: no DS or HT Operation element names its channel, and this radio has no 6 GHz.
    h.seen(channel(5975), -40, &beacon_frame(address(10), &[&common::element(0, SSID)[..], &wpa2()].concat(), false));
    h.event(Event::ScanDone);
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Unjoinable, Unjoinable::Band.reason().into()))]);
}

#[test]
fn every_selector_this_station_meets_and_a_rate_are_joined() {
    let elements = elements(SSID, channel(2412), &[0x82, 0x80 | 127, 0x80 | 126, 0x80 | 122, 0x6c], &wpa2());
    let frame = beacon_frame(address(10), &elements, false);
    assert_eq!(join_among(&[(address(10), 2412, -40, frame)]), Ok(address(10)));
}

#[test]
fn a_transition_mode_network_is_joined_by_psk() {
    let heard = one(2412, -40, &rsne(&[2, 8], &[4], 4, 0x80, None));
    assert_eq!(join_among(&heard), Ok(address(10)));
}

#[test]
fn a_joinable_bss_is_chosen_over_a_stronger_unjoinable_one() {
    let heard = vec![
        (address(10), 5180, -30, bss_frame(address(10), SSID, channel(5180), &rsne(&[8], &[4], 4, 0xc0, None))),
        (address(11), 2437, -75, bss_frame(address(11), SSID, channel(2437), &wpa2())),
    ];
    assert_eq!(join_among(&heard), Ok(address(11)));
}

#[test]
fn a_strong_bss_is_chosen_by_band_and_a_weak_one_by_signal() {
    let bss = |n: u8, mhz: u16, dbm: i8| (address(n), mhz, dbm, bss_frame(address(n), SSID, channel(mhz), &wpa2()));
    let strong = STRONG.dbm();
    // A strong 5 GHz BSS over a stronger 2.4 GHz one.
    assert_eq!(join_among(&[bss(10, 2412, -35), bss(11, 5180, strong)]), Ok(address(11)));
    // A strong 2.4 GHz BSS over a weak 5 GHz one.
    assert_eq!(join_among(&[bss(10, 2412, strong), bss(11, 5180, strong - 1)]), Ok(address(10)));
    // Between strong ones of a band, the stronger; between weak ones, the stronger whatever the band.
    assert_eq!(join_among(&[bss(10, 5180, -50), bss(11, 5745, -45)]), Ok(address(11)));
    assert_eq!(join_among(&[bss(10, 5180, -85), bss(11, 2462, -80)]), Ok(address(11)));
}

#[test]
fn a_network_not_heard_is_refused_and_a_hidden_one_is_never_matched() {
    assert_eq!(join_among(&[]).unwrap_err().0, Refusal::NotSeen);
    let other = vec![(address(10), 2412, -40, bss_frame(address(10), b"another", channel(2412), &wpa2()))];
    assert_eq!(join_among(&other).unwrap_err().0, Refusal::NotSeen);
    let hidden = vec![(address(10), 2412, -40, bss_frame(address(10), &[0; 10], channel(2412), &wpa2()))];
    assert_eq!(join_among(&hidden).unwrap_err().0, Refusal::NotSeen);
}

#[test]
fn the_station_is_protection_capable_only_where_the_ap_s_cipher_is_bip_cmac_128() {
    let own = |security: &[u8]| {
        let mut h = Harness::with_radio(address(1));
        h.connect(1, SSID, PASSPHRASE);
        h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), security));
        h.event(Event::ScanDone);
        match &h.sent()[..] {
            [_, Sent::Join { elements, .. }] => elements[elements.len() - 2],
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(own(&wpa2()), 0x80);
    assert_eq!(own(&rsne(&[2], &[4], 4, 0x80, None)), 0x80);
    assert_eq!(own(&rsne(&[2], &[4], 4, 0x80, Some(6))), 0x80);
    assert_eq!(own(&rsne(&[2], &[4], 4, 0x80, Some(12))), 0);
}

#[test]
fn a_bss_heard_off_its_own_channel_is_dropped_and_counted() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    h.seen(channel(2417), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.seen(channel(5180), -40, &bss_frame(address(11), SSID, channel(5200), &wpa2()));
    assert_eq!(h.station.counters().off_channel, 2);
    h.event(Event::ScanDone);
    assert!(matches!(&h.answered()[..], [(1, Answered::Refused(Refusal::NotSeen, _))]));
}

#[test]
fn malformed_beacons_and_other_frames_are_dropped_and_counted() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    let good = bss_frame(address(10), SSID, channel(2412), &wpa2());
    let mut country_pad = good.clone();
    country_pad.extend(common::element(7, &[b'D', b'E', b' ', 1, 13, 20, 7]));
    let mut group_bssid = good.clone();
    group_bssid[16] = 0x01;
    let mut probe_request = good.clone();
    probe_request[0] = 0x40;
    for frame in [vec![0x80], good[..30].to_vec(), country_pad, group_bssid, probe_request] {
        h.seen(channel(2412), -40, &frame);
    }
    assert_eq!(h.station.counters().malformed_seen, 5);
    assert!(h.ended.is_empty());
    h.event(Event::ScanDone);
    assert!(h.log.iter().any(|l| l == "wlan: a scan heard 0 BSSes, and dropped 5 malformed frames, 0 heard off their channel and 0 past its bound"), "{:?}", h.log);
}

#[test]
fn a_scan_keeps_at_most_its_bound_of_bsses() {
    let mut h = Harness::with_radio(address(1));
    h.ask(1, toyos_wifi::Request::Scan);
    for n in 0..300u16 {
        let bssid = Address([0x02, 0, 0, 0, (n >> 8) as u8, n as u8]);
        h.seen(channel(2412), -40, &bss_frame(bssid, SSID, channel(2412), &wpa2()));
    }
    assert_eq!(h.station.counters().dropped_bss, 300 - toyos_wlan_station::MAX_BSS as u64);
}

#[test]
fn a_scan_lists_each_network_once_strongest_first() {
    let mut h = Harness::with_radio(address(1));
    h.ask(1, toyos_wifi::Request::Scan);
    assert_eq!(h.sent(), [Sent::Scan]);
    h.ask(2, toyos_wifi::Request::Scan);
    assert_eq!(h.sent(), [], "a scan running is shared");
    h.seen(channel(2412), -60, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.seen(channel(5180), -45, &bss_frame(address(11), SSID, channel(5180), &rsne(&[8], &[4], 4, 0xc0, None)));
    h.seen(channel(2437), -50, &bss_frame(address(12), b"open", channel(2437), &[]));
    h.event(Event::ScanDone);
    let answers = h.answered();
    assert_eq!(answers.len(), 2);
    let Answered::Networks(networks) = &answers[0].1 else { panic!() };
    let summary: Vec<_> = networks.iter().map(|n| (n.ssid.as_bytes().to_vec(), n.security, n.band, n.signal.dbm(), n.joinable)).collect();
    assert_eq!(
        summary,
        [
            (SSID.to_vec(), Security::Wpa3, Band::Five, -45, true),
            (b"open".to_vec(), Security::Open, Band::TwoPointFour, -50, false),
        ]
    );
}
