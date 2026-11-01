//! A radio that breaks the radio protocol, or misses one of its bounds, is ended by name, and
//! what waited on it is refused.

mod common;

use std::time::Duration;

use common::*;
use toyos_radio::{Band, Bands, Event, JoinCause, JoinStep, Left, MSG_ASSOCIATED, MSG_HELLO, MSG_JOIN_FAILED, MSG_LEFT, MSG_SCAN_DONE, MSG_SEEN};
use toyos_wifi::{Refusal, Request, State};
use toyos_wlan_station::{HELLO_BOUND, JOIN_BOUND, LEAVE_BOUND, SCAN_BOUND};

/// A station whose radio said hello and is scanning for a connect of client 1.
fn scanning() -> Harness {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    assert_eq!(h.sent(), [Sent::Scan]);
    h
}

/// The join of client 1 is past its scan, its Join sent.
fn joining() -> Harness {
    let mut h = scanning();
    h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.event(Event::ScanDone);
    assert!(matches!(h.sent()[..], [Sent::Join { .. }]));
    h
}

/// The radio was ended for `why`, and client 1's connect refused with it.
fn ended(mut h: Harness, why: &str) {
    assert_eq!(h.ended, [format!("the radio {why}")]);
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::RadioFailed, format!("the radio {why}")))]);
    assert_eq!(h.status(), State::NoRadio);
    assert!(!h.station.has_radio());
}

/// `payload` under `msg_type` ends a scanning station's radio, as a frame that does not decode.
fn undecodable(msg_type: u32, payload: &[u8], violation: toyos_radio::Violation) {
    let mut h = scanning();
    h.raw(msg_type, payload);
    ended(h, &format!("sent a frame that does not decode: {}", violation.reason()));
}

#[test]
fn frames_that_do_not_decode_end_the_radio() {
    use toyos_radio::Violation as V;
    let beacon = bss_frame(address(10), SSID, channel(2412), &wpa2());
    let seen = |mhz: u16, dbm: i8, frame: &[u8]| [&mhz.to_le_bytes()[..], &dbm.to_le_bytes(), frame].concat();
    undecodable(99, &[], V::UnknownMessage);
    undecodable(toyos_radio::MSG_SCAN, &[], V::UnknownMessage);
    undecodable(MSG_SCAN_DONE, &[0], V::Length);
    undecodable(MSG_HELLO, &[2, 0, 0, 0, 0, 1], V::Length);
    undecodable(MSG_SEEN, &seen(2413, -40, &beacon), V::Frequency);
    undecodable(MSG_SEEN, &seen(5170, -40, &beacon)[..0], V::Length);
    undecodable(MSG_SEEN, &seen(2412, 1, &beacon), V::Signal);
    undecodable(MSG_SEEN, &seen(2412, -40, &[]), V::FrameLength);
    undecodable(MSG_SEEN, &seen(2412, -40, &[0; toyos_radio::MAX_FRAME + 1]), V::FrameLength);
    undecodable(MSG_JOIN_FAILED, &[3, 0, 0, 0], V::JoinStep);
    undecodable(MSG_JOIN_FAILED, &[1, 1, 5, 0], V::JoinCause);
    undecodable(MSG_LEFT, &[3, 1, 0], V::LeftCause);
    undecodable(MSG_LEFT, &[6, 0, 0], V::LeftCause);
    undecodable(toyos_radio::MSG_EAPOL_RECEIVED, &[0xff; 10], V::Address);
}

#[test]
fn a_hello_that_does_not_decode_ends_the_radio() {
    use toyos_radio::Violation as V;
    for (hello, violation) in [
        ([0u8, 0, 0, 0, 0, 0, 1], V::Address),
        ([1, 0, 0, 0, 0, 0, 1], V::Address),
        ([2, 0, 0, 0, 0, 1, 0], V::Bands),
        ([2, 0, 0, 0, 0, 1, 8], V::Bands),
    ] {
        let mut h = Harness::new();
        h.station.radio_connected(h.now);
        h.raw(MSG_HELLO, &hello);
        assert_eq!(h.ended, [format!("the radio sent a frame that does not decode: {}", violation.reason())]);
    }
}

#[test]
fn frames_out_of_the_protocol_s_order_end_the_radio() {
    let mut h = Harness::new();
    h.station.radio_connected(h.now);
    h.event(Event::ScanDone);
    assert_eq!(h.ended, ["the radio sent a frame before its hello"]);

    let hello = Event::Hello { address: address(1), bands: Bands::of(&[Band::Five]).unwrap() };
    let mut h = scanning();
    h.event(hello);
    ended(h, "said hello twice");

    let mut h = Harness::with_radio(address(1));
    h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    assert_eq!(h.ended, ["the radio reported a BSS outside a scan"]);

    let mut h = Harness::with_radio(address(1));
    h.event(Event::ScanDone);
    assert_eq!(h.ended, ["the radio finished a scan nobody asked"]);

    let mut h = scanning();
    h.event(Event::Associated);
    ended(h, "associated with nothing it was asked to join");

    let mut h = scanning();
    h.event(Event::JoinFailed { step: JoinStep::Association, cause: JoinCause::NoAnswer });
    ended(h, "failed a join nobody asked");

    let mut h = joining();
    h.eapol(address(10), &[1, 3, 0, 0], [0; 32]);
    ended(h, "delivered an EAPOL frame while nothing was associated");

    let mut h = joining();
    h.left(Left::Deauthenticated(1));
    ended(h, "left a BSS while nothing was associated");

    let mut h = scanning();
    h.left(Left::Asked);
    ended(h, "answered a leave nobody asked");
}

#[test]
fn a_radio_that_misses_a_bound_is_ended() {
    let mut h = Harness::new();
    h.station.radio_connected(h.now);
    h.advance(HELLO_BOUND.saturating_sub(Duration::from_millis(1)));
    assert!(h.ended.is_empty());
    h.advance(Duration::from_millis(1));
    assert_eq!(h.ended, ["the radio said no hello"]);

    let mut h = scanning();
    h.advance(SCAN_BOUND.saturating_sub(Duration::from_millis(1)));
    assert!(h.ended.is_empty());
    h.advance(Duration::from_millis(1));
    ended(h, "did not finish a scan in time");

    let mut h = joining();
    assert_eq!(h.station.deadline(), Some(h.now + JOIN_BOUND));
    h.advance(JOIN_BOUND);
    ended(h, "did not answer a join in time");

    let mut h = joining();
    h.ask(2, Request::Disconnect);
    assert_eq!(h.sent(), [Sent::Leave]);
    h.answered();
    h.advance(LEAVE_BOUND);
    assert_eq!(h.ended, ["the radio did not answer a leave in time"]);
}

#[test]
fn a_join_the_access_point_refuses_is_refused_with_its_status() {
    let mut h = joining();
    h.event(Event::JoinFailed { step: JoinStep::Authentication, cause: JoinCause::Status(17) });
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Authentication, "status code 17".into()))]);
    let mut h = joining();
    h.event(Event::JoinFailed { step: JoinStep::Association, cause: JoinCause::NoAnswer });
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Association, "no answer".into()))]);
    assert!(h.ended.is_empty());
}

#[test]
fn a_radio_whose_connection_ends_refuses_what_waited_on_it() {
    let mut h = joining();
    h.ask(2, Request::Scan);
    h.radio_gone();
    let mut answers = h.answered();
    answers.sort_by_key(|(c, _)| *c);
    let gone = "the radio's connection ended".to_string();
    assert_eq!(answers[0], (1, Answered::Refused(Refusal::RadioFailed, gone)));
    assert!(matches!(answers[1], (2, Answered::Networks(_))), "a scan asked mid-join is answered at once");
    assert!(h.ended.is_empty(), "nothing to end");
}

#[test]
fn a_join_abandoned_before_its_answer_drops_the_answer() {
    let mut h = joining();
    h.connect(2, SSID, PASSPHRASE);
    assert_eq!(h.sent(), [Sent::Leave, Sent::Scan]);
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Superseded, "another connect".into()))]);
    h.raw(MSG_ASSOCIATED, &[]);
    h.left(Left::Asked);
    h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.event(Event::ScanDone);
    assert!(matches!(h.sent()[..], [Sent::Join { .. }]));
    assert!(h.ended.is_empty(), "{:?}", h.ended);
}
