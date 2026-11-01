//! Every wifi frame decodes to what was encoded, and one that does not decode is refused.

use toyos_wifi::*;

fn ssid(s: &[u8]) -> Ssid {
    Ssid::new(s).unwrap()
}

#[test]
fn requests_and_answers_round_trip() {
    for r in [
        Request::Scan,
        Request::Disconnect,
        Request::Status,
        Request::Connect { ssid: ssid(b"home"), passphrase: b"secret passphrase" },
    ] {
        let (t, p) = r.encode();
        assert_eq!(Request::decode(t, &p), Ok(r));
    }
    let s = Signal::new(-55).unwrap();
    let network = Network { ssid: ssid(&[0xff; 32]), security: Security::Wpa2Wpa3, band: Band::Six, signal: s, joinable: false };
    for a in [
        Answer::Networks(vec![]),
        Answer::Networks(vec![network; 3]),
        Answer::Connected { band: Band::Five, channel: 36, signal: s },
        Answer::Done,
        Answer::State(State::NoRadio),
        Answer::State(State::Idle),
        Answer::State(State::Scanning),
        Answer::State(State::Joining { ssid: ssid(b"x") }),
        Answer::State(State::Connected { ssid: ssid(b"x"), band: Band::TwoPointFour, channel: 11, signal: s }),
        Answer::Refused { refusal: Refusal::NoAnswerToMessage2, detail: "message 2 sent once" },
    ] {
        let (t, p) = a.encode();
        assert_eq!(Answer::decode(t, &p), Ok(a));
    }
    for r in Refusal::ALL {
        let (t, p) = Answer::Refused { refusal: *r, detail: "" }.encode();
        assert_eq!(Answer::decode(t, &p), Ok(Answer::Refused { refusal: *r, detail: "" }));
    }
}

#[test]
fn an_answer_past_its_bounds_is_cut_to_fit() {
    let s = Signal::new(-55).unwrap();
    let network = Network { ssid: ssid(&[b'n'; 32]), security: Security::Wpa2, band: Band::Five, signal: s, joinable: true };
    let (t, p) = Answer::Networks(vec![network; MAX_NETWORKS + 5]).encode();
    assert!(p.len() <= MAX_PAYLOAD);
    assert_eq!(Answer::decode(t, &p), Ok(Answer::Networks(vec![network; MAX_NETWORKS])));
    let long = "é".repeat(MAX_DETAIL);
    let (t, p) = Answer::Refused { refusal: Refusal::RadioFailed, detail: &long }.encode();
    let Ok(Answer::Refused { detail, .. }) = Answer::decode(t, &p) else { panic!() };
    assert_eq!(detail, &long[..MAX_DETAIL]);
}

#[test]
fn frames_that_do_not_decode_are_refused() {
    for (t, p) in [
        (MSG_SCAN, vec![0]),
        (MSG_CONNECT, vec![]),
        (MSG_CONNECT, vec![0]),
        (MSG_CONNECT, vec![33]),
        (99, vec![]),
    ] {
        assert_eq!(Request::decode(t, &p), Err(Malformed), "{t} {p:?}");
    }
    for (t, p) in [
        (MSG_NETWORKS, vec![1, b'a', 0, 0, 0xc0]),
        (MSG_NETWORKS, vec![1, b'a', 5, 0, 0xc0, 0]),
        (MSG_NETWORKS, vec![1, b'a', 0, 3, 0xc0, 0]),
        (MSG_NETWORKS, vec![1, b'a', 0, 0, 0x01, 0]),
        (MSG_NETWORKS, vec![1, b'a', 0, 0, 0xc0, 2]),
        (MSG_DONE, vec![0]),
        (MSG_STATE, vec![5]),
        (MSG_STATE, vec![1, 0]),
        (MSG_REFUSED, vec![0]),
        (MSG_REFUSED, vec![1, 0xff]),
        (MSG_CONNECTED, vec![0, 1]),
        (1, vec![]),
    ] {
        assert_eq!(Answer::decode(t, &p), Err(Malformed), "{t} {p:?}");
    }
}
