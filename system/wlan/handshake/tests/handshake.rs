//! The supplicant against the test authenticator in `support`: the handshakes completing, and
//! every hostile frame refused by name.

mod support;

use support::*;
use toyos_crypto::wpa2;
use toyos_wlan_handshake::{Join, Refused, Step, Supplicant, Unjoinable};

const PASSPHRASE: &[u8] = b"ThisIsAPassword";
const SSID: &[u8] = b"ThisIsASSID";

struct Bench {
    ap: Authenticator,
    sta: Supplicant,
}

fn bench(ap_caps: u16, own_caps: u16, rsnxe: Option<&[u8]>) -> Bench {
    let pmk = wpa2::derive_pmk(PASSPHRASE, SSID).unwrap();
    let mut ap = Authenticator::new(*pmk.as_bytes(), rsne(ap_caps));
    ap.rsnxe = rsnxe.map(<[u8]>::to_vec);
    let protected = ap_caps & MFPC != 0 && own_caps & MFPC != 0;
    if protected {
        ap.igtk = Some((4, 0x0000_0a0b_0c0d_0e0f, [0x49; 16]));
    }
    let own = rsne(own_caps);
    let join = Join {
        authenticator: AA,
        supplicant: SPA,
        beacon_rsne: &ap.rsne,
        beacon_rsnxe: rsnxe,
        own_rsne: &own,
        own_rsnxe: None,
    };
    let sta = Supplicant::new(pmk, &join).unwrap();
    assert_eq!(sta.protected(), protected);
    Bench { ap, sta }
}

fn plain() -> Bench {
    bench(0, 0, None)
}

fn fixed() -> [u8; 32] {
    SNONCE
}

fn no_nonce() -> [u8; 32] {
    panic!("no handshake starts here")
}

impl Bench {
    fn send(&mut self, frame: &Frame) -> Result<Step, Refused> {
        self.sta.receive(&frame.seal(), no_nonce)
    }

    /// The authenticator's next frame, as `make` makes it.
    fn next(&mut self, make: fn(&mut Authenticator) -> Frame) -> Result<Step, Refused> {
        let frame = make(&mut self.ap);
        self.send(&frame)
    }

    /// Messages 1 and 2: the supplicant answers with [`SNONCE`].
    fn start(&mut self) -> Received {
        let m1 = self.ap.message_1().seal();
        let step = self.sta.receive(&m1, fixed).unwrap();
        assert!(step.pairwise.is_none() && step.group.is_none() && step.integrity.is_none());
        self.ap.message_2(&step.reply)
    }

    /// The whole 4-way handshake.
    fn join(&mut self) -> Step {
        self.start();
        let m3 = self.ap.message_3();
        let step = self.send(&m3).unwrap();
        let m4 = open(&step.reply, &self.ap.kck());
        assert_eq!(m4.replay, m3.replay);
        step
    }
}

#[test]
fn message_2_carries_the_snonce_and_the_stations_rsne() {
    let mut b = plain();
    let m2 = b.start();
    assert_eq!(m2.nonce, SNONCE);
    assert_eq!(m2.info, VERSION_2 | PAIRWISE | MIC);
    assert_eq!(m2.key_length, 0);
    assert_eq!(m2.replay, b.ap.replay);
    assert_eq!(m2.data, rsne(0));
    assert_eq!(
        m2.version, 2,
        "the reply echoes the authenticator's EAPOL version"
    );
}

#[test]
fn the_4_way_handshake_installs_the_ptks_tk_and_the_gtk() {
    let mut b = plain();
    let step = b.join();
    let m4 = open(&step.reply, &b.ap.kck());
    assert_eq!(
        (m4.info, m4.key_length, m4.nonce, m4.data.len()),
        (VERSION_2 | PAIRWISE | MIC | SECURE, 0, [0; 32], 0)
    );
    assert_eq!(step.pairwise.unwrap().key(), &b.ap.tk());
    let group = step.group.unwrap();
    assert_eq!(
        (group.id(), group.key(), group.rsc()),
        (1, &[0x61; 16], 0x0000_0102_0304_0506)
    );
    assert!(
        step.integrity.is_none(),
        "no IGTK without management frame protection"
    );
}

#[test]
fn management_frame_protection_installs_the_igtk_too() {
    let mut b = bench(MFPC, MFPC, Some(&RSNXE));
    let step = b.join();
    let igtk = step.integrity.unwrap();
    assert_eq!(
        (igtk.id(), igtk.key(), igtk.ipn()),
        (4, &[0x49; 16], 0x0000_0a0b_0c0d_0e0f)
    );
}

#[test]
fn a_replayed_counter_is_refused() {
    let mut b = plain();
    // Message 1 went under counter 2 and message 3 under 3.
    b.join();
    let mut again = b.ap.message_3();
    again.replay = 3;
    assert_eq!(b.send(&again).err(), Some(Refused::Replayed));
    let mut m1 = b.ap.message_1();
    m1.replay = 2;
    assert_eq!(
        b.send(&m1).err(),
        Some(Refused::Replayed),
        "a message 1 under a verified counter"
    );
    let mut g1 = b.ap.group_1();
    g1.replay = 3;
    assert_eq!(b.send(&g1).err(), Some(Refused::Replayed));
}

#[test]
fn a_retransmitted_message_3_is_answered_and_reinstalls_nothing() {
    let mut b = plain();
    b.join();
    let m3 = b.ap.message_3();
    let step = b.send(&m3).unwrap();
    assert_eq!(open(&step.reply, &b.ap.kck()).replay, m3.replay);
    assert!(step.pairwise.is_none() && step.group.is_none() && step.integrity.is_none());
}

#[test]
fn a_resent_message_1_keeps_its_snonce() {
    let mut b = plain();
    b.start();
    // The access points measured keep their ANonce (#876 §1); one that does not still gets the
    // first SNonce.
    for anonce in [b.ap.anonce, [0x5a; 32]] {
        b.ap.anonce = anonce;
        let step = b.sta.receive(&b.ap.message_1().seal(), no_nonce).unwrap();
        assert_eq!(b.ap.message_2(&step.reply).nonce, SNONCE);
    }
    let step = b.next(Authenticator::message_3).unwrap();
    assert_eq!(
        step.pairwise.unwrap().key(),
        &b.ap.tk(),
        "the PTK of the last message 1's ANonce"
    );
}

#[test]
fn a_message_1_after_the_handshake_rekeys_under_a_fresh_snonce() {
    let mut b = plain();
    let first = b.join().pairwise.unwrap();
    b.ap.anonce = [0x3c; 32];
    let step = b
        .sta
        .receive(&b.ap.message_1().seal(), || [0x7e; 32])
        .unwrap();
    assert_eq!(b.ap.message_2(&step.reply).nonce, [0x7e; 32]);
    let step = b.next(Authenticator::message_3).unwrap();
    let second = step.pairwise.unwrap();
    assert_eq!(second.key(), &b.ap.tk());
    assert_ne!(second.key(), first.key());
    assert!(step.group.is_none(), "the GTK installed already");
}

#[test]
fn message_3s_rsne_must_be_the_beacons() {
    let mut b = plain();
    b.start();
    let group = b.ap.group_data();
    for (data, why) in [
        ([rsne(MFPC), group.clone()].concat(), Refused::RsneDiffers),
        (group.clone(), Refused::RsneDiffers),
        ([rsne(0), rsne(0), group].concat(), Refused::SecondRsne),
    ] {
        assert_eq!(m3_carrying(&mut b, data).err(), Some(why));
    }
}

#[test]
fn message_3s_rsnxe_must_be_the_beacons() {
    let mut b = bench(0, 0, Some(&RSNXE));
    b.start();
    let group = b.ap.group_data();
    let downgrade = [rsne(0), group.clone()].concat();
    assert_eq!(
        m3_carrying(&mut b, downgrade).err(),
        Some(Refused::RsnxeDiffers),
        "the downgrade from hash-to-element"
    );
    let other = [rsne(0), vec![244, 1, 0x00], group.clone()].concat();
    assert_eq!(
        m3_carrying(&mut b, other).err(),
        Some(Refused::RsnxeDiffers)
    );

    let mut b = plain();
    b.start();
    let added = [rsne(0), RSNXE.to_vec(), group].concat();
    assert_eq!(
        m3_carrying(&mut b, added).err(),
        Some(Refused::RsnxeDiffers),
        "one the beacon lacked"
    );
}

#[test]
fn group_key_id_0_is_refused() {
    let mut b = plain();
    b.start();
    b.ap.gtk.0 = 0;
    assert_eq!(
        b.next(Authenticator::message_3).err(),
        Some(Refused::GroupKeyIdZero)
    );
    b.ap.gtk.0 = 1;
    b.next(Authenticator::message_3).unwrap();
    b.ap.gtk.0 = 0;
    assert_eq!(
        b.next(Authenticator::group_1).err(),
        Some(Refused::GroupKeyIdZero)
    );
}

#[test]
fn a_mic_mismatch_is_refused_and_changes_nothing() {
    let mut b = plain();
    b.start();
    let mut m3 = b.ap.message_3();
    m3.kck[0] ^= 1;
    assert_eq!(b.send(&m3).err(), Some(Refused::Mic));
    // A refused frame did not move the replay counter: the same counter is still accepted.
    let mut m3 = b.ap.message_3();
    m3.replay -= 1;
    b.send(&m3).unwrap();
    let mut g1 = b.ap.group_1();
    g1.kck[15] ^= 0x80;
    assert_eq!(b.send(&g1).err(), Some(Refused::Mic));
}

#[test]
fn every_single_octet_change_to_message_3_is_refused() {
    let mut b = plain();
    b.start();
    let m3 = b.ap.message_3().seal();
    for at in 0..m3.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut frame = m3.clone();
            frame[at] ^= flip;
            assert!(
                b.sta.receive(&frame, no_nonce).is_err(),
                "octet {at} ^ {flip:#x}"
            );
        }
    }
    b.sta.receive(&m3, no_nonce).unwrap();
}

#[test]
fn group_rekeying_installs_a_new_gtk_once() {
    let mut b = bench(MFPC, MFPC, None);
    b.join();
    b.ap.gtk = (2, [0x62; 16]);
    b.ap.igtk = Some((5, 7, [0x4a; 16]));
    let g1 = b.ap.group_1();
    let step = b.send(&g1).unwrap();
    let g2 = open(&step.reply, &b.ap.kck());
    assert_eq!(
        (g2.info, g2.replay, g2.data.len()),
        (VERSION_2 | MIC | SECURE, g1.replay, 0)
    );
    let group = step.group.unwrap();
    assert_eq!((group.id(), group.key()), (2, &[0x62; 16]));
    let igtk = step.integrity.unwrap();
    assert_eq!((igtk.id(), igtk.key(), igtk.ipn()), (5, &[0x4a; 16], 7));
    let step = b.next(Authenticator::group_1).unwrap();
    assert!(
        step.group.is_none() && step.integrity.is_none(),
        "the same keys again install nothing"
    );
}

#[test]
fn messages_out_of_their_order_are_refused() {
    let mut b = plain();
    let mut m3 = b.ap.message_3();
    m3.kck = [0; 16];
    assert_eq!(b.send(&m3).err(), Some(Refused::NoMessage1));
    assert_eq!(
        b.next(Authenticator::group_1).err(),
        Some(Refused::NoPairwiseKey)
    );
    b.start();
    assert_eq!(
        b.next(Authenticator::group_1).err(),
        Some(Refused::NoPairwiseKey),
        "before message 3"
    );
    let mut m3 = b.ap.message_3();
    m3.nonce = [0x11; 32];
    assert_eq!(b.send(&m3).err(), Some(Refused::AnonceChanged));
}

#[test]
fn key_information_no_authenticator_sends_is_refused() {
    let mut b = plain();
    for (set, clear, why) in [
        (0, ACK, Refused::NotFromAuthenticator),
        (REQUEST, 0, Refused::NotFromAuthenticator),
        (ERROR, 0, Refused::NotFromAuthenticator),
        (INSTALL, 0, Refused::UnknownMessage),
        (ENCRYPTED, 0, Refused::Plaintext),
    ] {
        let mut m1 = b.ap.message_1();
        m1.info = (m1.info | set) & !clear;
        assert_eq!(
            b.send(&m1).err(),
            Some(why),
            "message 1 with {set:#x} set, {clear:#x} clear"
        );
    }
    for version in [0, 1, 3, 7] {
        let mut m1 = b.ap.message_1();
        m1.info = (m1.info & !0b111) | version;
        assert_eq!(b.send(&m1).err(), Some(Refused::DescriptorVersion));
    }
    let mut m1 = b.ap.message_1();
    m1.key_length = 32;
    assert_eq!(b.send(&m1).err(), Some(Refused::KeyLength));
    b.start();
    for clear in [INSTALL, SECURE] {
        let mut m3 = b.ap.message_3();
        m3.info &= !clear;
        assert_eq!(b.send(&m3).err(), Some(Refused::UnknownMessage));
    }
    let mut m3 = b.ap.message_3();
    m3.info &= !ENCRYPTED;
    assert_eq!(b.send(&m3).err(), Some(Refused::Plaintext));
    let mut g = b.ap.group_1();
    g.info &= !MIC;
    assert_eq!(b.send(&g).err(), Some(Refused::UnknownMessage));
}

#[test]
fn eapol_framing_is_checked() {
    let mut b = plain();
    let m1 = b.ap.message_1();
    for (field, why) in [
        (0u8, Refused::ProtocolVersion),
        (1, Refused::PacketType),
        (2, Refused::DescriptorType),
    ] {
        let mut f = m1.clone();
        match field {
            0 => f.version = 4,
            1 => f.packet = 0,
            _ => f.descriptor = 254,
        }
        assert_eq!(b.send(&f).err(), Some(why));
    }
    let sealed = m1.seal();
    for len in 0..sealed.len() {
        assert!(
            b.sta.receive(&sealed[..len], no_nonce).is_err(),
            "cut to {len} octets"
        );
    }
    let mut long = sealed.clone();
    long[2..4].copy_from_slice(&u16::try_from(sealed.len() - 3).unwrap().to_be_bytes());
    assert_eq!(
        b.sta.receive(&long, no_nonce).err(),
        Some(Refused::BodyLength)
    );
    let mut data_len = sealed.clone();
    data_len[98] = 1;
    assert_eq!(
        b.sta.receive(&data_len, no_nonce).err(),
        Some(Refused::KeyDataLength)
    );
    let mut trailing = sealed;
    trailing.extend_from_slice(&[0xee; 5]);
    b.sta
        .receive(&trailing, fixed)
        .expect("octets past the Packet Body Length are not the PDU's");
}

/// Message 3 with Key Data `data`, wrapped.
fn m3_carrying(b: &mut Bench, data: Vec<u8>) -> Result<Step, Refused> {
    let mut m3 = b.ap.message_3();
    m3.data = data;
    b.send(&m3)
}

/// Message 3 with `wrapped` as its encrypted Key Data.
fn m3_with(b: &mut Bench, wrapped: Vec<u8>) -> Result<Step, Refused> {
    let mut m3 = b.ap.message_3();
    m3.data = wrapped;
    m3.raw = true;
    b.send(&m3)
}

#[test]
fn hostile_key_data_is_refused() {
    let mut b = plain();
    b.start();
    let kek = b.ap.kek();
    for len in [0, 8, 16, 23, 25, 31] {
        assert_eq!(
            m3_with(&mut b, vec![0x5c; len]).err(),
            Some(Refused::WrappedLength),
            "{len} octets"
        );
    }
    assert_eq!(
        m3_with(&mut b, wrap(&[0x13; 16], &rsne(0))).err(),
        Some(Refused::Unwrap),
        "another KEK"
    );
    let mut tampered = wrap(&kek, &rsne(0));
    tampered[9] ^= 4;
    assert_eq!(m3_with(&mut b, tampered).err(), Some(Refused::Unwrap));

    let gtk = gtk_kde(1, &[0x61; 16]);
    for (data, why) in [
        (
            [rsne(0), vec![0xdd, 40, 0, 0x0f, 0xac, 1]].concat(),
            Refused::KeyDataElement,
        ),
        ([rsne(0), vec![0x30]].concat(), Refused::KeyDataElement),
        (rsne(0), Refused::NoGtk),
        (
            [rsne(0), gtk.clone(), gtk.clone()].concat(),
            Refused::Duplicate,
        ),
        (
            [rsne(0), gtk_kde(1, &[0x61; 32])].concat(),
            Refused::GtkLength,
        ),
        ([rsne(0), kde(1, &[1])].concat(), Refused::GtkLength),
    ] {
        assert_eq!(
            m3_with(&mut b, wrap(&kek, &data)).err(),
            Some(why),
            "{data:02x?}"
        );
    }
    // Unknown elements and KDEs, and other vendors', are skipped.
    let skipped = [
        rsne(0),
        vec![221, 4, 0x00, 0x50, 0xf2, 1],
        kde(12, &[0; 4]),
        vec![7, 0],
        gtk,
    ]
    .concat();
    m3_with(&mut b, wrap(&kek, &skipped)).unwrap();
}

#[test]
fn an_igtk_is_required_and_checked_under_management_frame_protection() {
    let mut b = bench(MFPC | MFPR, MFPC, None);
    b.start();
    let kek = b.ap.kek();
    let base = [rsne(MFPC | MFPR), gtk_kde(1, &[0x61; 16])].concat();
    for (extra, why) in [
        (vec![], Refused::NoIgtk),
        (igtk_kde(3, 0, &[0x49; 16]), Refused::IgtkKeyId),
        (igtk_kde(6, 0, &[0x49; 16]), Refused::IgtkKeyId),
        (igtk_kde(4, 0, &[0x49; 32]), Refused::IgtkLength),
        (
            [igtk_kde(4, 0, &[0x49; 16]), igtk_kde(5, 0, &[0x49; 16])].concat(),
            Refused::Duplicate,
        ),
    ] {
        assert_eq!(
            m3_with(&mut b, wrap(&kek, &[base.clone(), extra].concat())).err(),
            Some(why)
        );
    }
}

#[test]
fn a_gtk_rsc_past_48_bits_is_refused() {
    let mut b = plain();
    b.start();
    b.ap.gtk_rsc = 1 << 48;
    assert_eq!(b.next(Authenticator::message_3).err(), Some(Refused::Rsc));
}

#[test]
fn associations_this_supplicant_cannot_key_are_refused() {
    let suites = |group: u8, pairwise: &[u8], akm: &[u8], caps: u16, management: Option<u8>| {
        let mut body = vec![
            1,
            0,
            0x00,
            0x0f,
            0xac,
            group,
            u8::try_from(pairwise.len()).unwrap(),
            0,
        ];
        for &p in pairwise {
            body.extend_from_slice(&[0x00, 0x0f, 0xac, p]);
        }
        body.extend_from_slice(&[u8::try_from(akm.len()).unwrap(), 0]);
        for &a in akm {
            body.extend_from_slice(&[0x00, 0x0f, 0xac, a]);
        }
        body.extend_from_slice(&caps.to_le_bytes());
        if let Some(m) = management {
            body.extend_from_slice(&[0, 0, 0x00, 0x0f, 0xac, m]);
        }
        [vec![48, u8::try_from(body.len()).unwrap()], body].concat()
    };
    let psk = suites(4, &[4], &[2], 0, None);
    let mixed_ap = suites(4, &[4], &[2, 8], MFPC, None);
    let cases: [(&[u8], &[u8], Unjoinable); 9] = [
        (&psk, &suites(4, &[4], &[8], 0, None), Unjoinable::Akm),
        (&suites(4, &[4], &[8], 0, None), &psk, Unjoinable::Akm),
        (&psk, &suites(4, &[4], &[2, 8], 0, None), Unjoinable::Akm),
        (&psk, &suites(4, &[2], &[2], 0, None), Unjoinable::Pairwise),
        (&suites(4, &[2], &[2], 0, None), &psk, Unjoinable::Pairwise),
        (
            &suites(2, &[4], &[2], 0, None),
            &suites(2, &[4], &[2], 0, None),
            Unjoinable::Group,
        ),
        (
            &suites(4, &[4], &[2], MFPC | MFPR, None),
            &psk,
            Unjoinable::Protection,
        ),
        (
            &mixed_ap,
            &suites(4, &[4], &[2], MFPC, Some(13)),
            Unjoinable::Management,
        ),
        (&psk, &psk[..psk.len() - 1], Unjoinable::Element),
    ];
    for (ap, own, why) in cases {
        let join = Join {
            authenticator: AA,
            supplicant: SPA,
            beacon_rsne: ap,
            beacon_rsnxe: None,
            own_rsne: own,
            own_rsnxe: None,
        };
        let pmk = wpa2::derive_pmk(PASSPHRASE, SSID).unwrap();
        assert_eq!(
            Supplicant::new(pmk, &join).err(),
            Some(why),
            "{ap:02x?} {own:02x?}"
        );
    }
    let join = Join {
        authenticator: AA,
        supplicant: SPA,
        beacon_rsne: &mixed_ap,
        beacon_rsnxe: Some(&[244, 0]),
        own_rsne: &psk,
        own_rsnxe: None,
    };
    let pmk = wpa2::derive_pmk(PASSPHRASE, SSID).unwrap();
    assert_eq!(
        Supplicant::new(pmk, &join).err(),
        Some(Unjoinable::Element),
        "an RSNXE of length 0"
    );
    let join = Join {
        beacon_rsnxe: None,
        ..join
    };
    let pmk = wpa2::derive_pmk(PASSPHRASE, SSID).unwrap();
    assert!(
        !Supplicant::new(pmk, &join).unwrap().protected(),
        "the station is not capable"
    );
}

#[test]
fn every_reason_is_its_own() {
    let mut reasons: Vec<&str> = Refused::ALL.iter().map(|r| r.reason()).collect();
    reasons.extend(Unjoinable::ALL.iter().map(|r| r.reason()));
    let count = reasons.len();
    reasons.sort();
    reasons.dedup();
    assert_eq!(reasons.len(), count);
}
