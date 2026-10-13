//! One test per row of the bridge's table. The oracle for QEMU's side is its
//! 11.1.1 behaviour, measured black-box; the transmit's bytes are read back
//! with the codec's own decoder, whose layouts `vd_agent.h` gives.

use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

use toyos_vdagent::encode::{message_bytes, CAPABILITIES_DATA, GRAB_DATA, RELEASE_DATA, REQUEST_DATA};
use toyos_vdagent::stream::{Decoder, Message};
use toyos_vdagent::{wire, LARGEST_TRANSMIT, MAX_TEXT};

use super::*;

/// What QEMU 11.1.1 announces: `CLIPBOARD_BY_DEMAND | CLIPBOARD_SELECTION |
/// CLIPBOARD_GRAB_SERIAL`.
const HOST_CAPS: u32 = 0x0002_0060;

fn opening() -> Message {
    Message::Capabilities { request: true, caps: HOST_CAPS }
}

/// `GRAB [serial][UTF8_TEXT]`, either way.
fn grab(serial: u32) -> Message {
    Message::Grab { serial, types: vec![wire::UTF8_TEXT] }
}

/// `CLIPBOARD [UTF8_TEXT, data]`, either way.
fn clipboard(data: &str) -> Message {
    Message::Clipboard { kind: wire::UTF8_TEXT, data: data.as_bytes().to_vec() }
}

/// What the bridge owes, encoded into a [`LARGEST_TRANSMIT`] buffer, and its
/// length.
fn transmit(bridge: &mut Bridge) -> (Vec<Message>, usize) {
    let mut out = vec![0; LARGEST_TRANSMIT];
    let len = bridge.transmit(&mut out);
    let mut messages = Vec::new();
    Decoder::default()
        .feed(&out[..len], |m| messages.push(m.expect("the bridge writes only messages the codec decodes")))
        .expect("the bridge's transmit is framed");
    (messages, len)
}

fn capabilities() -> Message {
    Message::Capabilities { request: false, caps: toyos_vdagent::encode::GUEST_CAPS }
}

/// A bridge whose session is open with nothing owed and nobody owning.
fn settled() -> Bridge {
    let mut bridge = Bridge::default();
    bridge.host(opening());
    transmit(&mut bridge);
    assert_eq!(bridge.host(clipboard("")), Effect::Guest(String::new()));
    assert_eq!(bridge.owner(), &Owner::Host);
    bridge
}

/// The opening asks for the host's text, and QEMU answers a request nobody
/// grabbed with no data: that answer is the guest's clipboard.
#[test]
fn the_answer_to_the_opening_request_lands() {
    let mut bridge = Bridge::default();
    assert_eq!(bridge.host(opening()), Effect::None);
    assert_eq!(transmit(&mut bridge).0, [capabilities(), Message::Request { kind: wire::UTF8_TEXT }]);
    assert_eq!(bridge.host(clipboard("host text")), Effect::Guest("host text".to_string()));
}

#[test]
fn the_opening_grabs_a_copy_made_before_the_port_opened() {
    let mut bridge = Bridge::default();
    assert_eq!(bridge.copy("early".to_string()), Effect::None);
    assert!(!bridge.owes(), "a copy while the port is closed owes nothing");
    bridge.host(opening());
    assert_eq!(transmit(&mut bridge).0, [capabilities(), grab(0), clipboard("early")]);
    // The session's next grab carries 1.
    bridge.copy("later".to_string());
    assert_eq!(transmit(&mut bridge).0, [grab(1), clipboard("later")]);
}

#[test]
fn capabilities_are_answered_once_per_session() {
    let mut bridge = settled();
    assert_eq!(bridge.host(Message::Capabilities { request: false, caps: HOST_CAPS }), Effect::Named(Named::Unasked));
    assert!(!bridge.owes(), "a host announcement without request was answered");
}

/// Refused, with no answer: QEMU 11.1.1 closes the session on a second guest
/// announcement.
#[test]
fn a_host_announcement_without_request_is_refused() {
    let mut bridge = Bridge::default();
    assert_eq!(bridge.host(Message::Capabilities { request: false, caps: HOST_CAPS }), Effect::Named(Named::Unasked));
    assert!(!bridge.owes());
    assert_eq!(bridge.owner(), &Owner::None);
}

#[test]
fn a_host_without_by_demand_or_grab_serial_gets_nothing_flowing() {
    for caps in [HOST_CAPS & !wire::CAP_CLIPBOARD_BY_DEMAND, HOST_CAPS & !wire::CAP_CLIPBOARD_GRAB_SERIAL] {
        let mut bridge = Bridge::default();
        assert_eq!(bridge.host(Message::Capabilities { request: true, caps }), Effect::Named(Named::HostLacks { caps }));
        assert_eq!(transmit(&mut bridge).0, [capabilities()]);
        assert_eq!(bridge.copy("text".to_string()), Effect::None);
        assert!(!bridge.owes(), "a copy flowed to a host that lacks {caps:#x}");
        assert_eq!(bridge.host(grab(0)), Effect::Named(Named::NoSession));
    }
}

/// The host's reply crossed a guest copy: it does not land over the guest's
/// text.
#[test]
fn host_text_that_lost_to_a_guest_copy_is_refused() {
    let mut bridge = Bridge::default();
    bridge.host(opening());
    transmit(&mut bridge);
    bridge.copy("guest".to_string());
    assert_eq!(
        bridge.host(clipboard("host")),
        Effect::Named(Named::ClipboardRefused { kind: wire::UTF8_TEXT, len: 4 })
    );
    assert_eq!(bridge.owner(), &Owner::Guest("guest".to_string()));
}

#[test]
fn host_data_that_is_not_utf8_text_is_refused() {
    let mut bridge = settled();
    let png = Message::Clipboard { kind: 2, data: vec![1, 2, 3] };
    assert_eq!(bridge.host(png), Effect::Named(Named::ClipboardRefused { kind: 2, len: 3 }));
    let broken = Message::Clipboard { kind: wire::UTF8_TEXT, data: vec![0xff] };
    assert_eq!(bridge.host(broken), Effect::Named(Named::ClipboardRefused { kind: wire::UTF8_TEXT, len: 1 }));
}

/// The guest's grab 1 and the host's grab 0 cross: QEMU keeps 1, since 1 ≥
/// its counter after its own grab 0, so the bridge discards the host's.
#[test]
fn a_crossing_host_grab_older_than_the_guests_is_discarded() {
    let mut bridge = settled();
    bridge.copy("first".to_string());
    transmit(&mut bridge);
    bridge.copy("second".to_string());
    assert_eq!(transmit(&mut bridge).0, [grab(1), clipboard("second")]);
    assert_eq!(bridge.host(grab(0)), Effect::Named(Named::GrabDiscarded { serial: 0, last: 1 }));
    assert_eq!(bridge.owner(), &Owner::Guest("second".to_string()));
    assert!(!bridge.owes());
}

/// The guest's grab 0 and the host's grab 0 cross: QEMU discards the guest's,
/// since 0 is below its counter after its own grab 0, so the bridge keeps the
/// host's and both hold the host's text.
#[test]
fn a_crossing_host_grab_as_old_as_the_guests_is_kept() {
    let mut bridge = settled();
    bridge.copy("guest".to_string());
    assert_eq!(transmit(&mut bridge).0, [grab(0), clipboard("guest")]);
    assert_eq!(bridge.host(grab(0)), Effect::Guest(String::new()));
    assert_eq!(bridge.owner(), &Owner::Host);
    assert_eq!(transmit(&mut bridge).0, [Message::Request { kind: wire::UTF8_TEXT }]);
    // The bridge's next grab carries 1, the host's counter after its grab 0.
    bridge.copy("again".to_string());
    assert_eq!(transmit(&mut bridge).0, [grab(1), clipboard("again")]);
}

#[test]
fn a_host_grab_without_text_empties_the_guest_and_requests_nothing() {
    let mut bridge = settled();
    let png = Message::Grab { serial: 0, types: vec![2] };
    assert_eq!(bridge.host(png), Effect::Guest(String::new()));
    assert_eq!(bridge.owner(), &Owner::Host);
    assert!(!bridge.owes());
}

#[test]
fn a_guest_grab_carries_its_text_in_the_same_transmit() {
    let mut bridge = settled();
    bridge.copy("copied".to_string());
    assert_eq!(transmit(&mut bridge).0, [grab(0), clipboard("copied")]);
}

/// A copy of nothing grabs and releases whoever owned before: QEMU 11.1.1
/// empties its clipboard on a kept guest grab and its release, and an empty
/// `CLIPBOARD` pushed instead leaves a host paste with nothing.
#[test]
fn an_empty_copy_grabs_and_releases_whoever_owned() {
    for guest_owned in [false, true] {
        let mut bridge = settled();
        if guest_owned {
            bridge.copy("text".to_string());
            transmit(&mut bridge);
        }
        let serial = u32::from(guest_owned);
        assert_eq!(bridge.copy(String::new()), Effect::None);
        assert_eq!(bridge.owner(), &Owner::None);
        assert_eq!(transmit(&mut bridge).0, [grab(serial), Message::Release]);
    }
}

/// QEMU 11.1.1 sends a `RELEASE` back after the guest's own.
#[test]
fn the_release_after_our_own_is_ignored() {
    let mut bridge = settled();
    bridge.copy("text".to_string());
    bridge.copy(String::new());
    transmit(&mut bridge);
    assert_eq!(bridge.host(Message::Release), Effect::Named(Named::ReleaseIgnored));
    bridge.copy("again".to_string());
    assert_eq!(bridge.host(Message::Release), Effect::Named(Named::ReleaseIgnored));
    assert_eq!(bridge.owner(), &Owner::Guest("again".to_string()));
}

#[test]
fn a_host_release_empties_a_host_owned_clipboard() {
    let mut bridge = settled();
    assert_eq!(bridge.host(Message::Release), Effect::Guest(String::new()));
    assert_eq!(bridge.owner(), &Owner::None);
}

/// Host capabilities with nobody owning owe capabilities and a `REQUEST`; a
/// copy of [`MAX_TEXT`] before the encode withdraws the `REQUEST`.
#[test]
fn a_guest_copy_withdraws_the_owed_request() {
    let mut bridge = Bridge::default();
    bridge.host(opening());
    bridge.copy("x".repeat(MAX_TEXT));
    let (messages, len) = transmit(&mut bridge);
    assert_eq!(len, LARGEST_TRANSMIT);
    assert_eq!(messages, [capabilities(), grab(0), clipboard(&"x".repeat(MAX_TEXT))]);
}

/// QEMU never received the guest's grab and keeps its own, so both sides hold
/// the host's text.
#[test]
fn a_host_grab_kept_after_an_unsent_copy_wins_on_both_sides() {
    let mut bridge = Bridge::default();
    bridge.host(opening());
    bridge.copy("x".repeat(MAX_TEXT));
    assert_eq!(bridge.host(grab(0)), Effect::Guest(String::new()));
    assert_eq!(bridge.owner(), &Owner::Host);
    let (messages, len) = transmit(&mut bridge);
    assert_eq!(messages, [capabilities(), Message::Request { kind: wire::UTF8_TEXT }]);
    assert_eq!(len, message_bytes(CAPABILITIES_DATA) + message_bytes(REQUEST_DATA));
}

/// Every guest grab goes out with its `CLIPBOARD` or its `RELEASE`, so after it
/// QEMU serves a host paste itself, and an answer to a host `REQUEST` reaches
/// the host as nothing or, after two folded pastes, as stale text.
#[test]
fn a_host_request_owes_nothing() {
    let mut bridge = Bridge::default();
    bridge.copy("x".repeat(MAX_TEXT));
    bridge.host(opening());
    let (_, first) = transmit(&mut bridge);
    assert_eq!(first, 2_105_448);
    assert_eq!(bridge.host(Message::Request { kind: wire::UTF8_TEXT }), Effect::Named(Named::Request { kind: 1 }));
    assert_eq!(transmit(&mut bridge).1, 0);
    bridge.copy(String::new());
    assert_eq!(transmit(&mut bridge).1, message_bytes(GRAB_DATA) + message_bytes(RELEASE_DATA));
    assert_eq!(bridge.host(Message::Request { kind: wire::UTF8_TEXT }), Effect::Named(Named::Request { kind: 1 }));
    assert_eq!(transmit(&mut bridge).1, 0);
}

#[test]
fn the_hosts_close_clears_the_serials_and_keeps_the_owner() {
    let mut bridge = settled();
    bridge.copy("first".to_string());
    transmit(&mut bridge);
    bridge.copy("kept".to_string());
    bridge.closed();
    assert!(!bridge.owes(), "the close left a transmit owed");
    assert_eq!(bridge.owner(), &Owner::Guest("kept".to_string()));
    // The reopened session starts at 0, as QEMU's does.
    bridge.host(opening());
    assert_eq!(transmit(&mut bridge).0, [capabilities(), grab(0), clipboard("kept")]);
}

#[test]
fn a_copy_while_the_port_is_closed_only_sets_the_owner() {
    let mut bridge = settled();
    bridge.closed();
    assert_eq!(bridge.copy("closed".to_string()), Effect::None);
    assert!(!bridge.owes());
    assert_eq!(bridge.owner(), &Owner::Guest("closed".to_string()));
}

/// A host grab of `u32::MAX` leaves no serial for the guest's next grab, and
/// the session ends by name rather than wrapping.
#[test]
fn a_session_past_the_last_serial_ends() {
    let mut bridge = settled();
    bridge.host(grab(u32::MAX));
    transmit(&mut bridge);
    assert_eq!(bridge.copy("text".to_string()), Effect::Named(Named::SerialsExhausted));
    assert!(!bridge.owes());
    assert_eq!(bridge.host(grab(0)), Effect::Named(Named::NoSession));
}
