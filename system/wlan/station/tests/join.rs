//! A whole join: against the handshakes recorded for W2a (`../handshake/tests/fixtures`), and
//! against the handshake crate's test authenticator, which shares no crypto code with the
//! station, for what no recording holds: a wrong passphrase, a lost message, a loss.

mod common;
#[path = "../../handshake/tests/support/mod.rs"]
mod support;

use std::time::Duration;

use common::*;
use toyos_crypto::wpa2;
use toyos_ieee80211::Address;
use toyos_ieee80211::data::{Ethernet, msdus};
use toyos_ieee80211::frame::Frame;
use toyos_ieee80211::mgmt::Body;
use toyos_radio::{Band, Event, Left};
use toyos_wifi::{Refusal, Request, State};
use toyos_wlan_station::HANDSHAKE_BOUND;

/// The MPDUs of a radiotap pcap, as the handshake's capture test reads them.
fn mpdus(pcap: &[u8]) -> Vec<Vec<u8>> {
    let (mut at, mut out) = (24, Vec::new());
    while at < pcap.len() {
        let len = u32::from_le_bytes(pcap[at + 8..at + 12].try_into().unwrap()) as usize;
        let record = &pcap[at + 16..at + 16 + len];
        let radiotap = usize::from(u16::from_le_bytes([record[2], record[3]]));
        out.push(record[radiotap..].to_vec());
        at += 16 + len;
    }
    out
}

struct Recorded {
    probe_response: Vec<u8>,
    authenticator: Address,
    supplicant: Address,
    eapol: Vec<Vec<u8>>,
}

fn recorded(pcap: &[u8]) -> Recorded {
    let (mut probe_response, mut supplicant, mut eapol, mut authenticator) = (None, None, Vec::new(), None);
    for mpdu in mpdus(pcap) {
        match Frame::parse(&mpdu).unwrap() {
            Frame::Management(m) => match Body::parse(m.subtype, m.body).unwrap() {
                Body::ProbeResponse(_) => {
                    authenticator = Some(m.bssid);
                    probe_response = Some(mpdu.clone());
                }
                Body::AssociationRequest(_) => supplicant = Some(m.sa),
                _ => unreachable!(),
            },
            Frame::Data(d) => {
                let msdu = msdus(&d, d.body).unwrap().next().unwrap().unwrap();
                eapol.push(Ethernet::from_msdu(msdu).unwrap().payload.to_vec());
            }
            _ => unreachable!(),
        }
    }
    Recorded {
        probe_response: probe_response.unwrap(),
        authenticator: authenticator.unwrap(),
        supplicant: supplicant.unwrap(),
        eapol,
    }
}

fn hex<const N: usize>(text: &str) -> [u8; N] {
    let text: String = text.split_whitespace().collect();
    core::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

/// The recorded run joined from its Probe Response: the keys the station installs are those the
/// recorded station logged, and its message 4 is the recorded one, which depends only on them.
fn join_recorded(pcap: &[u8], tk: &str, gtk: &str, igtk: Option<&str>) {
    let r = recorded(pcap);
    let mut h = Harness::with_radio(r.supplicant);
    h.connect(1, b"toyos-w2a", b"ThisIsAPassword");
    assert_eq!(h.sent(), [Sent::Scan]);
    let on = channel(2412);
    h.seen(on, -30, &r.probe_response);
    h.event(Event::ScanDone);
    let [Sent::Join { bssid, channel: joined, ssid, elements, heard }] = &h.sent()[..] else { panic!("a join") };
    assert_eq!((*bssid, *joined, &ssid[..]), (r.authenticator, on, &b"toyos-w2a"[..]));
    assert_eq!(heard[..], r.probe_response[24 + 12..], "the Probe Response's elements, whole");
    let own = [48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 2, 0x80, 0];
    assert_eq!(elements[..], own, "PSK and CCMP-128, management frame protection capable");
    h.event(Event::Associated);

    let snonce: [u8; 32] = r.eapol[1][17..49].try_into().unwrap();
    h.eapol(r.authenticator, &r.eapol[0], snonce);
    let [Sent::Eapol(m2)] = &h.sent()[..] else { panic!("message 2") };
    assert_eq!(m2[17..49], snonce, "message 2 carries the SNonce drawn");
    assert_eq!(m2[99..], own, "message 2 carries the RSNE the join sent");
    h.eapol(r.authenticator, &r.eapol[2], [0; 32]);
    let sent = h.sent();
    assert_eq!(sent[0], Sent::Eapol(r.eapol[3].clone()), "message 4, byte for byte");
    assert_eq!(sent[1], Sent::Pairwise(hex(tk)));
    assert!(matches!(sent[2], Sent::Group { id: 1, key, .. } if key == hex::<16>(gtk)));
    let rest: Vec<_> = sent[3..].to_vec();
    match igtk {
        Some(igtk) => {
            assert!(matches!(rest[0], Sent::Integrity { id: 4, key, .. } if key == hex::<16>(igtk)));
            assert_eq!(rest[1..], [Sent::Authorize]);
        }
        None => assert_eq!(rest, [Sent::Authorize]),
    }
    assert_eq!(
        h.answered(),
        [(1, Answered::Connected { band: Band::TwoPointFour, channel: 1, signal: signal(-30) })]
    );
    assert!(matches!(h.status(), State::Connected { channel: 1, .. }));
}

#[test]
fn a_recorded_psk_network_is_joined_with_the_recorded_keys() {
    join_recorded(
        include_bytes!("../../handshake/tests/fixtures/psk.pcap"),
        "f58cbd4e84fec5253f34669dac1e970f",
        "133b9638132e718795ee29cae83c51ec",
        None,
    );
}

#[test]
fn a_recorded_network_requiring_protection_is_joined_with_its_integrity_key() {
    join_recorded(
        include_bytes!("../../handshake/tests/fixtures/pmf.pcap"),
        "229711d21d0e3d2595299e6165854557",
        "5ec8b64ab99775a695f20cc6e21c13e0",
        Some("21101dad342098c10c771eedb7f9e53e"),
    );
}

use support::{AA, Authenticator, SNONCE, SPA};

/// A station associated with the tests' authenticator, which holds `ap_passphrase`'s PMK; the
/// station was given `passphrase`.
fn associated(passphrase: &[u8], ap_passphrase: &[u8]) -> (Harness, Authenticator) {
    let mut h = Harness::with_radio(SPA);
    h.connect(1, SSID, passphrase);
    h.sent();
    h.seen(channel(5180), -50, &bss_frame(AA, SSID, channel(5180), &wpa2()));
    h.event(Event::ScanDone);
    assert!(matches!(h.sent()[..], [Sent::Join { .. }]));
    h.event(Event::Associated);
    let pmk = *wpa2::derive_pmk(ap_passphrase, SSID).unwrap().as_bytes();
    (h, Authenticator::new(pmk, wpa2()))
}

/// The keys installed, then Authorize: a completed handshake.
fn completes(h: &mut Harness, ap: &mut Authenticator) {
    h.eapol(AA, &ap.message_1().seal(), SNONCE);
    let [Sent::Eapol(m2)] = &h.sent()[..] else { panic!("message 2") };
    ap.message_2(m2);
    completes_from_3(h, ap);
}

/// The handshake completed from message 3, message 2 answered.
fn completes_from_3(h: &mut Harness, ap: &mut Authenticator) {
    h.eapol(AA, &ap.message_3().seal(), [0; 32]);
    let sent = h.sent();
    assert_eq!(sent[1], Sent::Pairwise(ap.tk()));
    assert_eq!(sent.last(), Some(&Sent::Authorize));
}

#[test]
fn the_right_passphrase_connects() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    completes(&mut h, &mut ap);
    assert_eq!(h.answered(), [(1, Answered::Connected { band: Band::Five, channel: 36, signal: signal(-50) })]);
}

#[test]
fn a_wrong_passphrase_is_refused_on_message_2_going_unanswered() {
    let (mut h, mut ap) = associated(b"not the passphrase", PASSPHRASE);
    // The authenticator's MIC check of message 2 fails, so it sends message 1 again, and then
    // gives up.
    for _ in 0..4 {
        h.eapol(AA, &ap.message_1().seal(), SNONCE);
        let [Sent::Eapol(m2)] = &h.sent()[..] else { panic!("message 2") };
        assert_eq!(m2[17..49], SNONCE, "a resent message 1 keeps its SNonce");
    }
    h.left(Left::Deauthenticated(15));
    assert_eq!(
        h.answered(),
        [(
            1,
            Answered::Refused(
                Refusal::NoAnswerToMessage2,
                "message 2 sent 4 times; then deauthenticated, reason code 15".into()
            )
        )]
    );
    assert_eq!(h.status(), State::Idle);
    assert_eq!(h.sent(), [], "a refused join is not made again");
}

#[test]
fn a_handshake_that_does_not_finish_in_time_is_left_and_refused() {
    let (mut h, mut ap) = associated(b"not the passphrase", PASSPHRASE);
    h.eapol(AA, &ap.message_1().seal(), SNONCE);
    h.sent();
    h.advance(HANDSHAKE_BOUND.saturating_sub(Duration::from_millis(1)));
    assert_eq!(h.answered(), []);
    h.advance(Duration::from_millis(1));
    assert_eq!(h.sent(), [Sent::Leave]);
    assert_eq!(
        h.answered(),
        [(1, Answered::Refused(Refusal::NoAnswerToMessage2, "message 2 sent once; then no message 3 came in time".into()))]
    );
    h.left(Left::Asked);
    assert!(h.ended.is_empty());
}

#[test]
fn an_access_point_that_begins_no_handshake_is_refused_by_name() {
    let (mut h, _) = associated(PASSPHRASE, PASSPHRASE);
    h.advance(HANDSHAKE_BOUND);
    assert_eq!(h.sent(), [Sent::Leave]);
    assert!(matches!(&h.answered()[..], [(1, Answered::Refused(Refusal::NoMessage1, _))]));
}

#[test]
fn a_message_3_whose_rsne_is_not_the_beacons_leaves_the_network() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    h.eapol(AA, &ap.message_1().seal(), SNONCE);
    let [Sent::Eapol(m2)] = &h.sent()[..] else { panic!() };
    ap.message_2(m2);
    ap.rsne = support::rsne(support::MFPC);
    h.eapol(AA, &ap.message_3().seal(), [0; 32]);
    assert_eq!(h.sent(), [Sent::Leave]);
    assert_eq!(
        h.answered(),
        [(1, Answered::Refused(Refusal::Handshake, "a message 3 whose RSNE is not the beacon's".into()))]
    );
}

#[test]
fn eapol_frames_the_air_spoils_are_dropped_and_counted() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    h.eapol(address(9), &ap.message_1().seal(), SNONCE);
    h.eapol(AA, &[1, 3, 0, 0], SNONCE);
    let mut m1 = ap.message_1();
    m1.key_length = 32;
    h.eapol(AA, &m1.seal(), SNONCE);
    h.eapol(AA, &ap.message_1().seal(), SNONCE);
    assert_eq!(h.sent().len(), 1, "the first message 1 alone was answered");
    // A message 3 anyone can forge: its MIC does not verify, so it is no reason to leave.
    let [Sent::Eapol(m2)] = &h.radio_after(|h| h.eapol(AA, &ap.message_1().seal(), SNONCE))[..] else { panic!() };
    ap.message_2(m2);
    let mut forged = ap.message_3();
    forged.kck = [0; 16];
    h.eapol(AA, &forged.seal(), [0; 32]);
    let c = h.station.counters();
    assert_eq!((c.stray_eapol, c.refused_eapol), (1, 3));
    assert!(h.ended.is_empty());
    assert_eq!(h.answered(), []);
    assert_eq!(h.sent(), [], "nothing is left or installed");
    completes_from_3(&mut h, &mut ap);
    h.ask(2, Request::Disconnect);
    assert!(h.log.iter().any(|l| l == "wlan: the join dropped 1 EAPOL frames from another address and refused 3"), "{:?}", h.log);
}

#[test]
fn a_connection_lost_is_joined_again_once_and_without_an_answer() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    completes(&mut h, &mut ap);
    h.answered();
    h.left(Left::Deauthenticated(7));
    assert_eq!(h.sent(), [Sent::Scan]);
    assert!(matches!(h.status(), State::Joining { .. }));
    h.seen(channel(5180), -50, &bss_frame(AA, SSID, channel(5180), &wpa2()));
    h.event(Event::ScanDone);
    assert!(matches!(h.sent()[..], [Sent::Join { .. }]));
    h.event(Event::Associated);
    let mut ap = Authenticator::new(ap.pmk, wpa2());
    completes(&mut h, &mut ap);
    assert_eq!(h.answered(), [], "nobody waits on a join made again");
    assert!(matches!(h.status(), State::Connected { .. }));

    // Lost again: joined again, and that join failing is the end of it.
    h.left(Left::BeaconLoss);
    h.sent();
    h.event(Event::ScanDone);
    assert_eq!(h.sent(), []);
    assert_eq!(h.status(), State::Idle);
    assert!(h.log.iter().any(|l| l.contains("no BSS of that name was heard")));
}

#[test]
fn a_radio_that_ends_while_connected_is_joined_through_again_after_its_hello() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    completes(&mut h, &mut ap);
    h.answered();
    h.radio_gone();
    assert_eq!(h.status(), State::NoRadio);
    h.station.radio_connected(h.now);
    h.event(Event::Hello { address: SPA, bands: toyos_radio::Bands::of(&[Band::Five]).unwrap() });
    assert_eq!(h.sent(), [Sent::Scan]);
    assert!(matches!(h.status(), State::Joining { .. }));
}

#[test]
fn a_disconnect_leaves_and_its_answer_comes_at_once() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    completes(&mut h, &mut ap);
    h.answered();
    h.ask(2, Request::Disconnect);
    assert_eq!(h.sent(), [Sent::Leave]);
    assert_eq!(h.answered(), [(2, Answered::Done)]);
    assert_eq!(h.status(), State::Idle);
    // What the association still says until the leave is answered is dropped.
    h.eapol(AA, &ap.group_1().seal(), [0; 32]);
    h.left(Left::Deauthenticated(3));
    h.left(Left::Asked);
    assert!(h.ended.is_empty(), "{:?}", h.ended);
    h.left(Left::Asked);
    assert_eq!(h.ended, ["the radio answered a leave nobody asked"]);
}

#[test]
fn a_group_rekey_installs_the_new_key_and_nothing_else() {
    let (mut h, mut ap) = associated(PASSPHRASE, PASSPHRASE);
    completes(&mut h, &mut ap);
    ap.gtk = (2, [0x62; 16]);
    h.eapol(AA, &ap.group_1().seal(), [0; 32]);
    let sent = h.sent();
    assert!(matches!(sent[..], [Sent::Eapol(_), Sent::Group { id: 2, key, .. }] if key == [0x62; 16]), "{sent:?}");
}
