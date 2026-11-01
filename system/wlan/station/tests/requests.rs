//! Connect, disconnect, status and scan as the settings programs ask them.

mod common;

use common::*;
use toyos_radio::Event;
use toyos_wifi::{MSG_CONNECT, MSG_STATUS, Refusal, Request, Ssid, State};

#[test]
fn status_follows_the_radio_and_the_join() {
    let mut h = Harness::new();
    assert_eq!(h.status(), State::NoRadio);
    h.station.radio_connected(h.now);
    assert_eq!(h.status(), State::NoRadio, "until its hello");
    let mut h = Harness::with_radio(address(1));
    assert_eq!(h.status(), State::Idle);
    h.ask(1, Request::Scan);
    assert_eq!(h.status(), State::Scanning);
    h.event(Event::ScanDone);
    h.connect(2, SSID, PASSPHRASE);
    assert_eq!(h.status(), State::Joining { ssid: Ssid::new(SSID).unwrap() });
}

#[test]
fn nothing_is_asked_of_a_radio_that_is_not_there() {
    let mut h = Harness::new();
    h.connect(1, SSID, PASSPHRASE);
    h.ask(2, Request::Scan);
    assert_eq!(
        h.answered(),
        [(1, Answered::Refused(Refusal::NoRadio, String::new())), (2, Answered::Refused(Refusal::NoRadio, String::new()))]
    );
}

#[test]
fn a_passphrase_that_is_not_8_to_63_printable_characters_is_refused() {
    for passphrase in [&b"seven77"[..], &[b'a'; 64], b"tab\tinside it", "naïve passphrase".as_bytes()] {
        let mut h = Harness::with_radio(address(1));
        h.connect(1, SSID, passphrase);
        assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Passphrase, String::new()))]);
        assert_eq!(h.sent(), []);
    }
    for passphrase in [&b"eight888"[..], &[b'~'; 63], b"   spaces count   "] {
        let mut h = Harness::with_radio(address(1));
        h.connect(1, SSID, passphrase);
        assert_eq!(h.sent(), [Sent::Scan]);
    }
}

#[test]
fn a_second_connect_supersedes_the_first() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    h.connect(2, b"other", PASSPHRASE);
    assert_eq!(h.sent(), [Sent::Scan], "the scan running serves both");
    assert_eq!(h.answered(), [(1, Answered::Refused(Refusal::Superseded, "another connect".into()))]);
    assert_eq!(h.status(), State::Joining { ssid: Ssid::new(b"other").unwrap() });
}

#[test]
fn a_disconnect_while_scanning_cancels_the_join() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    h.sent();
    h.ask(2, Request::Disconnect);
    assert_eq!(h.sent(), [], "nothing was associated");
    assert_eq!(
        h.answered(),
        [(1, Answered::Refused(Refusal::Superseded, "a disconnect".into())), (2, Answered::Done)]
    );
    h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.event(Event::ScanDone);
    assert_eq!(h.sent(), [], "nothing is joined");
    assert_eq!(h.status(), State::Idle);
}

#[test]
fn a_client_that_goes_leaves_its_join_running_unanswered() {
    let mut h = Harness::with_radio(address(1));
    h.connect(1, SSID, PASSPHRASE);
    h.station.client_gone(1);
    h.seen(channel(2412), -40, &bss_frame(address(10), SSID, channel(2412), &wpa2()));
    h.event(Event::ScanDone);
    h.event(Event::JoinFailed { step: toyos_radio::JoinStep::Authentication, cause: toyos_radio::JoinCause::NoAnswer });
    assert_eq!(h.answered(), []);
}

#[test]
fn a_client_that_breaks_the_wifi_protocol_is_ended() {
    let mut h = Harness::with_radio(address(1));
    h.ask_raw(1, 99, &[]);
    h.ask_raw(2, MSG_STATUS, &[0]);
    h.ask_raw(3, MSG_CONNECT, &[33; 40]);
    h.ask_raw(4, MSG_CONNECT, &[]);
    // A second request before the first is answered.
    h.ask(5, Request::Scan);
    h.ask(5, Request::Status);
    assert_eq!(h.ended_clients, [1, 2, 3, 4, 5]);
    // What an ended client sends before its connection is gone is not acted on.
    h.answered();
    h.sent();
    h.ask(1, Request::Status);
    h.connect(4, SSID, PASSPHRASE);
    assert_eq!((h.answered(), h.sent()), (vec![], vec![]));
}

#[test]
fn a_client_may_ask_again_once_answered() {
    let mut h = Harness::with_radio(address(1));
    h.ask(1, Request::Status);
    h.ask(1, Request::Status);
    h.ask(1, Request::Scan);
    h.event(Event::ScanDone);
    h.ask(1, Request::Scan);
    assert!(h.ended_clients.is_empty());
    assert_eq!(h.answered().len(), 3);
}
