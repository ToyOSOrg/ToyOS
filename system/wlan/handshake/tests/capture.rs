//! The supplicant against a recorded handshake: wpa_supplicant v2.10 as the access point
//! (`mode=2`) and as the station, over two `mac80211_hwsim` radios, once with plain PSK and once
//! with management frame protection required. `tests/fixtures/{psk,pmf}.pcap` hold each run's
//! Probe Response, Association Request and four EAPOL-Key frames, as `NOTICE` describes.
//!
//! Given the captured message 1 and the SNonce of the captured message 2, the supplicant's
//! message 2 must be the captured one byte for byte; likewise its message 4 from message 3. The
//! keys it hands out must be those the station's own `-K` log printed.

use toyos_crypto::wpa2;
use toyos_ieee80211::Address;
use toyos_ieee80211::data::{Ethernet, msdus};
use toyos_ieee80211::element::{self, Id};
use toyos_ieee80211::frame::Frame;
use toyos_ieee80211::mgmt::Body;
use toyos_wlan_handshake::{Join, Supplicant};

const SSID: &[u8] = b"toyos-w2a";
const PASSPHRASE: &[u8] = b"ThisIsAPassword";
const EAPOL: u16 = 0x888e;

fn hex<const N: usize>(text: &str) -> [u8; N] {
    let text: String = text.split_whitespace().collect();
    core::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

/// The 802.11 frames of a pcap of link type 127, radiotap: each record's MPDU, without FCS.
fn mpdus(pcap: &[u8]) -> Vec<Vec<u8>> {
    assert_eq!(
        pcap[..4],
        0xa1b2c3d4u32.to_le_bytes(),
        "a little-endian pcap"
    );
    assert_eq!(
        u32::from_le_bytes(pcap[20..24].try_into().unwrap()),
        127,
        "radiotap"
    );
    let (mut at, mut out) = (24, Vec::new());
    while at < pcap.len() {
        let len = u32::from_le_bytes(pcap[at + 8..at + 12].try_into().unwrap()) as usize;
        let record = &pcap[at + 16..at + 16 + len];
        let radiotap = usize::from(u16::from_le_bytes([record[2], record[3]]));
        // mac80211_hwsim's radiotap: TSFT, Flags, Rate and Channel present, so Flags is the
        // octet after the 8-byte TSFT. Its bit 4 would say an FCS follows the MPDU.
        assert_eq!(
            record[4..8],
            0x0fu32.to_le_bytes(),
            "TSFT, Flags, Rate and Channel"
        );
        assert_eq!(record[16] & 0x10, 0, "an MPDU without its FCS");
        out.push(record[radiotap..].to_vec());
        at += 16 + len;
    }
    out
}

/// The element of `id` in `elements`, whole: Element ID and Length included.
fn whole(elements: &[u8], id: u8) -> Option<Vec<u8>> {
    element::iter(elements)
        .map(Result::unwrap)
        .find(|e| e.id == Id::Base(id))
        .map(|e| {
            let mut w = vec![id, u8::try_from(e.body.len()).unwrap()];
            w.extend_from_slice(e.body);
            w
        })
}

struct Recorded {
    authenticator: Address,
    supplicant: Address,
    beacon_rsne: Vec<u8>,
    beacon_rsnxe: Option<Vec<u8>>,
    own_rsne: Vec<u8>,
    own_rsnxe: Option<Vec<u8>>,
    /// Messages 1 to 4, each an EAPOL PDU.
    eapol: [Vec<u8>; 4],
}

fn recorded(pcap: &[u8]) -> Recorded {
    let mpdus = mpdus(pcap);
    let mut beacon = None;
    let mut association = None;
    let mut eapol = Vec::new();
    for mpdu in &mpdus {
        match Frame::parse(mpdu).unwrap() {
            Frame::Management(m) => match Body::parse(m.subtype, m.body).unwrap() {
                Body::ProbeResponse(b) => beacon = Some((m.sa, b.elements.to_vec())),
                Body::AssociationRequest(a) => association = Some((m.sa, a.elements.to_vec())),
                other => panic!("an unexpected frame {other:?}"),
            },
            Frame::Data(d) => {
                let msdu = msdus(&d, d.body).unwrap().next().unwrap().unwrap();
                let ethernet = Ethernet::from_msdu(msdu).unwrap();
                assert_eq!(ethernet.ethertype, EAPOL);
                eapol.push((ethernet.src, ethernet.payload.to_vec()));
            }
            _ => panic!("an unexpected control frame"),
        }
    }
    let (authenticator, beacon) = beacon.expect("a Probe Response");
    let (supplicant, association) = association.expect("an Association Request");
    let from = [authenticator, supplicant, authenticator, supplicant];
    assert_eq!(
        eapol.iter().map(|(src, _)| *src).collect::<Vec<_>>(),
        from,
        "messages 1 to 4 in turn"
    );
    Recorded {
        authenticator,
        supplicant,
        beacon_rsne: whole(&beacon, 48).expect("the access point's RSNE"),
        beacon_rsnxe: whole(&beacon, 244),
        own_rsne: whole(&association, 48).expect("the station's RSNE"),
        own_rsnxe: whole(&association, 244),
        eapol: eapol
            .into_iter()
            .map(|(_, pdu)| pdu)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
    }
}

/// The station's log: the PMK, the PTK (KCK, KEK, TK), the GTK and the IGTK it printed.
struct Logged {
    pmk: [u8; 32],
    ptk: [u8; 48],
    gtk: (u8, [u8; 16]),
    igtk: Option<(u16, [u8; 16])>,
}

fn replay(pcap: &[u8], logged: Logged) {
    let r = recorded(pcap);
    let pmk = wpa2::derive_pmk(PASSPHRASE, SSID).unwrap();
    assert_eq!(pmk.as_bytes(), &logged.pmk, "the PMK the station derived");
    let join = Join {
        authenticator: r.authenticator,
        supplicant: r.supplicant,
        beacon_rsne: &r.beacon_rsne,
        beacon_rsnxe: r.beacon_rsnxe.as_deref(),
        own_rsne: &r.own_rsne,
        own_rsnxe: r.own_rsnxe.as_deref(),
    };
    let mut sta = Supplicant::new(pmk, &join).unwrap();
    assert_eq!(sta.protected(), logged.igtk.is_some());
    let [m1, m2, m3, m4] = &r.eapol;
    let snonce: [u8; 32] = m2[17..49].try_into().unwrap();
    let step = sta.receive(m1, || snonce).unwrap();
    assert_eq!(&step.reply, m2, "message 2, byte for byte");
    let step = sta
        .receive(m3, || panic!("message 3 draws no nonce"))
        .unwrap();
    assert_eq!(&step.reply, m4, "message 4, byte for byte");
    assert_eq!(step.pairwise.unwrap().key()[..], logged.ptk[32..], "the TK");
    let group = step.group.unwrap();
    assert_eq!((group.id(), *group.key()), logged.gtk, "the GTK");
    let igtk = step.integrity.map(|k| (k.id(), *k.key()));
    assert_eq!(igtk, logged.igtk, "the IGTK");
}

const PMK: &str = "2722ca3de8582fd1a7c1cdce8585d1279c7770dd5a4bc0ddadabafe08b4132df";

#[test]
fn a_recorded_psk_handshake_is_answered_byte_for_byte() {
    replay(
        include_bytes!("fixtures/psk.pcap"),
        Logged {
            pmk: hex(PMK),
            ptk: hex(
                "1144d572e19e468c8146dc413a33b12e 6ce71d2d3996c3536427d52f0ce2593e
                 f58cbd4e84fec5253f34669dac1e970f",
            ),
            gtk: (1, hex("133b9638132e718795ee29cae83c51ec")),
            igtk: None,
        },
    );
}

#[test]
fn a_recorded_handshake_with_management_frame_protection_is_answered_byte_for_byte() {
    replay(
        include_bytes!("fixtures/pmf.pcap"),
        Logged {
            pmk: hex(PMK),
            ptk: hex(
                "cc079ba865edf73272bf0f4346606946 81bb9fc0f995f7330b5d5d29780499b1
                 229711d21d0e3d2595299e6165854557",
            ),
            gtk: (1, hex("5ec8b64ab99775a695f20cc6e21c13e0")),
            igtk: Some((4, hex("21101dad342098c10c771eedb7f9e53e"))),
        },
    );
}

/// A recorded join and two PTK rekeys the station asked for (its `wpa_ptk_rekey`), as
/// `tools/rekeyfixture.py` on `ToyOSOrg/evidence` wrote them from the second capture: the
/// addresses and RSNEs from its radio capture, then each EAPOL-Key frame and key the station's
/// `-dd -K` log printed, in order. The rekeys travel CCMP-protected on air, so the log is the
/// record of their bytes.
fn replay_rekeys(fixture: &str) {
    let lines: Vec<(&str, Vec<u8>)> = fixture
        .lines()
        .map(|l| {
            let (tag, value) = l.split_once(' ').unwrap();
            let bytes = (0..value.len() / 2)
                .map(|i| u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap());
            (tag, bytes.collect())
        })
        .collect();
    let field = |name: &str| {
        lines
            .iter()
            .find(|(t, _)| *t == name)
            .map(|(_, v)| v.clone())
    };
    let address = |name: &str| Address(field(name).unwrap().try_into().unwrap());
    let (beacon_rsne, own_rsne) = (field("beacon_rsne").unwrap(), field("own_rsne").unwrap());
    let (beacon_rsnxe, own_rsnxe) = (field("beacon_rsnxe"), field("own_rsnxe"));
    let join = Join {
        authenticator: address("aa"),
        supplicant: address("spa"),
        beacon_rsne: &beacon_rsne,
        beacon_rsnxe: beacon_rsnxe.as_deref(),
        own_rsne: &own_rsne,
        own_rsnxe: own_rsnxe.as_deref(),
    };
    let mut sta = Supplicant::new(wpa2::derive_pmk(PASSPHRASE, SSID).unwrap(), &join).unwrap();
    let (gtk, igtk) = (field("gtk").unwrap(), field("igtk"));
    let (mut ptk, mut completed) = (Vec::new(), 0);
    for (at, (tag, frame)) in lines.iter().enumerate() {
        match *tag {
            "ptk" => ptk = frame.clone(),
            "rx" => {
                // The station's next frame, past the EAPOL-Key Requests it sends on its own.
                let reply = lines[at..]
                    .iter()
                    .find(|(t, f)| *t == "tx" && u16::from_be_bytes([f[5], f[6]]) & (1 << 11) == 0)
                    .map(|(_, f)| f)
                    .expect("the station answered");
                let snonce: [u8; 32] = reply[17..49].try_into().unwrap();
                let step = sta.receive(frame, || snonce).unwrap();
                assert_eq!(&step.reply, reply, "the answer to line {at}, byte for byte");
                if let Some(tk) = step.pairwise {
                    assert_eq!(tk.key()[..], ptk[32..], "handshake {completed}'s TK");
                    // The station installed the group keys once, and reinstalled neither.
                    assert_eq!(
                        step.group.map(|g| g.key().to_vec()),
                        (completed == 0).then(|| gtk.clone())
                    );
                    let delivered = step.integrity.map(|k| k.key().to_vec());
                    assert_eq!(delivered, igtk.clone().filter(|_| completed == 0));
                    completed += 1;
                }
            }
            _ => {}
        }
    }
    assert_eq!(completed, 3, "the join and two rekeys");
}

#[test]
fn recorded_ptk_rekeys_are_answered_byte_for_byte_and_reinstall_no_group_key() {
    replay_rekeys(include_str!("fixtures/rekey-psk.txt"));
}

#[test]
fn recorded_ptk_rekeys_under_management_frame_protection_are_answered_byte_for_byte() {
    replay_rekeys(include_str!("fixtures/rekey-pmf.txt"));
}
