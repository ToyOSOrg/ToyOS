//! Every radio frame decodes to what was encoded, and the driver's side refuses what `wlan`
//! never sends. The refusals `wlan` meets are `toyos-wlan-station`'s tests.

use toyos_ieee80211::Address;
use toyos_radio::*;

const A: Address = Address([0x02, 0, 0, 0, 0, 1]);

#[test]
fn every_event_and_command_round_trips() {
    let ch = Channel::from_mhz(5180).unwrap();
    let events = [
        Event::Hello { address: A, bands: Bands::of(&[Band::TwoPointFour, Band::Six]).unwrap() },
        Event::Seen { channel: ch, signal: Signal::new(-61).unwrap(), frame: &[1, 2, 3] },
        Event::ScanDone,
        Event::Associated,
        Event::JoinFailed { step: JoinStep::Association, cause: JoinCause::Status(17) },
        Event::JoinFailed { step: JoinStep::Authentication, cause: JoinCause::NoAnswer },
        Event::Eapol { source: A, frame: &[1, 3, 0, 0] },
        Event::Left(Left::Asked),
        Event::Left(Left::Deauthenticated(15)),
        Event::Left(Left::Disassociated(8)),
        Event::Left(Left::BeaconLoss),
        Event::Left(Left::Firmware),
        Event::Left(Left::RfKill),
    ];
    let mut buf = [0; MAX_PAYLOAD];
    for e in events {
        let (t, n) = e.encode(&mut buf);
        assert_eq!(Event::decode(t, &buf[..n]), Ok(e));
        assert_eq!(Command::decode(t, &buf[..n]), Err(Violation::UnknownMessage));
    }
    let key = [7; 16];
    let commands = [
        Command::Scan,
        Command::Join { bssid: A, channel: ch, elements: &[48, 2, 1, 0] },
        Command::Eapol { frame: &[1, 3, 0, 0] },
        Command::Pairwise { key: &key },
        Command::Group { id: 3, key: &key, rsc: (1 << 48) - 1 },
        Command::Integrity { id: 5, key: &key, ipn: 9 },
        Command::Authorize,
        Command::Leave,
    ];
    for c in commands {
        let (t, n) = c.encode(&mut buf);
        assert_eq!(Command::decode(t, &buf[..n]), Ok(c));
        assert_eq!(Event::decode(t, &buf[..n]), Err(Violation::UnknownMessage));
    }
}

#[test]
fn commands_wlan_never_sends_are_refused() {
    let key = [7u8; 16];
    let join = |elements: &[u8]| [&A.0[..], &5180u16.to_le_bytes(), elements].concat();
    for (t, payload, why) in [
        (MSG_SCAN, vec![0], Violation::Length),
        (MSG_JOIN, join(&[]), Violation::Elements),
        (MSG_JOIN, join(&[48, 5, 1]), Violation::Elements),
        (MSG_JOIN, [&[1u8, 0, 0, 0, 0, 1][..], &5180u16.to_le_bytes(), &[48, 0]].concat(), Violation::Address),
        (MSG_JOIN, [&A.0[..], &5181u16.to_le_bytes(), &[48, 0]].concat(), Violation::Frequency),
        (MSG_PAIRWISE, key[..15].to_vec(), Violation::Length),
        (MSG_GROUP, [&[0u8][..], &key, &0u64.to_le_bytes()].concat(), Violation::GroupKeyId),
        (MSG_GROUP, [&[1u8][..], &key, &(1u64 << 48).to_le_bytes()].concat(), Violation::PacketNumber),
        (MSG_INTEGRITY, [&6u16.to_le_bytes()[..], &key, &0u64.to_le_bytes()].concat(), Violation::IntegrityKeyId),
        (MSG_EAPOL_SEND, vec![], Violation::FrameLength),
    ] {
        assert_eq!(Command::decode(t, &payload), Err(why), "{t} {payload:?}");
    }
}

#[test]
fn a_channel_is_each_band_s_numbering_and_nothing_between() {
    let numbered: Vec<(u16, Band, u8)> = (2400..7200)
        .filter_map(|mhz| Channel::from_mhz(mhz).map(|c| (mhz, c.band(), c.number())))
        .collect();
    for &(mhz, band, number) in &numbered {
        let c = Channel::from_mhz(mhz).unwrap();
        assert_eq!(c.mhz(), mhz);
        let ok = match band {
            Band::TwoPointFour => (1..=14).contains(&number),
            Band::Five => (32..=177).contains(&number),
            Band::Six => number == 2 || (number % 4 == 1 && number <= 233),
        };
        assert!(ok, "{mhz} MHz is {band:?} channel {number}");
    }
    let count = |b| numbered.iter().filter(|(_, band, _)| *band == b).count();
    assert_eq!((count(Band::TwoPointFour), count(Band::Five), count(Band::Six)), (14, 146, 60));
}
