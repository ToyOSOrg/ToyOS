//! A station driven as `wlan` drives it, with the radio and the settings programs played by
//! the test.

#![allow(dead_code)]

use std::time::Duration;

use toyos_ieee80211::Address;
use toyos_radio::{Band, Bands, Channel, Command, Event, Left, Signal};
use toyos_wifi::{Answer, Refusal, Request, Ssid, State};
use toyos_wlan_station::{Client, Out, Station};

pub const SSID: &[u8] = b"toyos-test";
pub const PASSPHRASE: &[u8] = b"correct horse battery";

/// What the station sent the radio, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Scan,
    Join { bssid: Address, channel: Channel, elements: Vec<u8> },
    Eapol(Vec<u8>),
    Pairwise([u8; 16]),
    Group { id: u8, key: [u8; 16], rsc: u64 },
    Integrity { id: u16, key: [u8; 16], ipn: u64 },
    Authorize,
    Leave,
}

fn sent(msg_type: u32, payload: &[u8]) -> Sent {
    match Command::decode(msg_type, payload).expect("the station sends what decodes") {
        Command::Scan => Sent::Scan,
        Command::Join { bssid, channel, elements } => Sent::Join { bssid, channel, elements: elements.to_vec() },
        Command::Eapol { frame } => Sent::Eapol(frame.to_vec()),
        Command::Pairwise { key } => Sent::Pairwise(*key),
        Command::Group { id, key, rsc } => Sent::Group { id, key: *key, rsc },
        Command::Integrity { id, key, ipn } => Sent::Integrity { id, key: *key, ipn },
        Command::Authorize => Sent::Authorize,
        Command::Leave => Sent::Leave,
    }
}

/// What a settings program was answered, decoded and owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    Networks(Vec<toyos_wifi::Network>),
    Connected { band: Band, channel: u8, signal: Signal },
    Done,
    State(State),
    Refused(Refusal, String),
}

fn answered(msg_type: u32, payload: &[u8]) -> Answered {
    match Answer::decode(msg_type, payload).expect("the station answers what decodes") {
        Answer::Networks(n) => Answered::Networks(n),
        Answer::Connected { band, channel, signal } => Answered::Connected { band, channel, signal },
        Answer::Done => Answered::Done,
        Answer::State(s) => Answered::State(s),
        Answer::Refused { refusal, detail } => Answered::Refused(refusal, detail.to_string()),
    }
}

pub struct Harness {
    pub station: Station,
    pub now: Duration,
    pub radio: Vec<Sent>,
    pub answers: Vec<(Client, Answered)>,
    pub ended: Vec<String>,
    pub ended_clients: Vec<Client>,
    pub log: Vec<String>,
}

pub fn signal(dbm: i8) -> Signal {
    Signal::new(dbm).unwrap()
}

pub fn channel(mhz: u16) -> Channel {
    Channel::from_mhz(mhz).unwrap()
}

impl Harness {
    pub fn new() -> Self {
        Self {
            station: Station::new(),
            now: Duration::from_secs(1),
            radio: Vec::new(),
            answers: Vec::new(),
            ended: Vec::new(),
            ended_clients: Vec::new(),
            log: Vec::new(),
        }
    }

    /// A station with a radio of `address` that said hello, on 2.4 and 5 GHz.
    pub fn with_radio(address: Address) -> Self {
        let mut h = Self::new();
        h.station.radio_connected(h.now);
        h.event(Event::Hello { address, bands: Bands::of(&[Band::TwoPointFour, Band::Five]).unwrap() });
        h
    }

    fn collect(&mut self) {
        for out in self.station.take() {
            match out {
                Out::Radio { msg_type, payload } => self.radio.push(sent(msg_type, payload.bytes())),
                Out::EndRadio(why) => self.ended.push(why),
                Out::Answer { client, msg_type, payload } => {
                    self.answers.push((client, answered(msg_type, payload.bytes())))
                }
                Out::EndClient(client) => self.ended_clients.push(client),
                Out::Log(line) => self.log.push(line),
            }
        }
    }

    pub fn radio_gone(&mut self) {
        self.station.radio_gone();
        self.collect();
    }

    pub fn raw(&mut self, msg_type: u32, payload: &[u8]) {
        self.raw_with(msg_type, payload, || panic!("no handshake starts here"));
    }

    pub fn raw_with(&mut self, msg_type: u32, payload: &[u8], nonce: impl FnOnce() -> [u8; 32]) {
        if self.station.has_radio() {
            self.station.radio(msg_type, payload, self.now, nonce);
        }
        self.collect();
    }

    pub fn event(&mut self, event: Event<'_>) {
        let mut buf = [0; toyos_radio::MAX_PAYLOAD];
        let (msg_type, len) = event.encode(&mut buf);
        self.raw(msg_type, &buf[..len]);
    }

    pub fn eapol(&mut self, source: Address, frame: &[u8], nonce: [u8; 32]) {
        let mut buf = [0; toyos_radio::MAX_PAYLOAD];
        let (msg_type, len) = Event::Eapol { source, frame }.encode(&mut buf);
        self.raw_with(msg_type, &buf[..len], || nonce);
    }

    pub fn ask_raw(&mut self, client: Client, msg_type: u32, payload: &[u8]) {
        self.station.request(client, msg_type, payload, self.now);
        self.collect();
    }

    pub fn ask(&mut self, client: Client, request: Request<'_>) {
        let (msg_type, payload) = request.encode();
        self.station.request(client, msg_type, &payload, self.now);
        self.collect();
    }

    pub fn connect(&mut self, client: Client, ssid: &[u8], passphrase: &[u8]) {
        self.ask(client, Request::Connect { ssid: Ssid::new(ssid).unwrap(), passphrase });
    }

    pub fn status(&mut self) -> State {
        self.ask(999, Request::Status);
        match self.answers.pop() {
            Some((999, Answered::State(s))) => s,
            other => panic!("a status answer, not {other:?}"),
        }
    }

    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        self.station.tick(self.now);
        self.collect();
    }

    pub fn seen(&mut self, channel: Channel, dbm: i8, frame: &[u8]) {
        self.event(Event::Seen { channel, signal: signal(dbm), frame });
    }

    pub fn left(&mut self, left: Left) {
        self.event(Event::Left(left));
    }

    /// The radio commands sent since the last call.
    pub fn sent(&mut self) -> Vec<Sent> {
        std::mem::take(&mut self.radio)
    }

    pub fn answered(&mut self) -> Vec<(Client, Answered)> {
        std::mem::take(&mut self.answers)
    }
}

/// A Beacon (or with `probe`, a Probe Response) MPDU from `bssid` with `elements`.
pub fn beacon_frame(bssid: Address, elements: &[u8], probe: bool) -> Vec<u8> {
    let mut f = vec![if probe { 0x50 } else { 0x80 }, 0x00, 0, 0];
    f.extend_from_slice(&[0xff; 6]);
    f.extend_from_slice(&bssid.0);
    f.extend_from_slice(&bssid.0);
    f.extend_from_slice(&[0x10, 0x00]);
    f.extend_from_slice(&0u64.to_le_bytes());
    f.extend_from_slice(&100u16.to_le_bytes());
    f.extend_from_slice(&0x0411u16.to_le_bytes());
    f.extend_from_slice(elements);
    f
}

pub fn element(id: u8, body: &[u8]) -> Vec<u8> {
    let mut e = vec![id, u8::try_from(body.len()).unwrap()];
    e.extend_from_slice(body);
    e
}

/// An RSNE of `akms` (Table 9-190 types under 00-0F-AC), `pairwise` and `group` cipher types,
/// RSN Capabilities `caps` and, if given, a group management cipher type.
pub fn rsne(akms: &[u8], pairwise: &[u8], group: u8, caps: u16, management: Option<u8>) -> Vec<u8> {
    let mut b = vec![1, 0, 0x00, 0x0f, 0xac, group];
    b.extend_from_slice(&u16::try_from(pairwise.len()).unwrap().to_le_bytes());
    for &p in pairwise {
        b.extend_from_slice(&[0x00, 0x0f, 0xac, p]);
    }
    b.extend_from_slice(&u16::try_from(akms.len()).unwrap().to_le_bytes());
    for &a in akms {
        b.extend_from_slice(&[0x00, 0x0f, 0xac, a]);
    }
    b.extend_from_slice(&caps.to_le_bytes());
    if let Some(m) = management {
        b.extend_from_slice(&[0, 0, 0x00, 0x0f, 0xac, m]);
    }
    element(48, &b)
}

pub fn wpa2() -> Vec<u8> {
    rsne(&[2], &[4], 4, 0, None)
}

/// A BSS's elements: its SSID, basic rates `rates`, its channel's DS or HT Operation element,
/// and `security`.
pub fn elements(ssid: &[u8], channel: Channel, rates: &[u8], security: &[u8]) -> Vec<u8> {
    let mut e = element(0, ssid);
    e.extend(element(1, rates));
    if channel.band() == Band::TwoPointFour {
        e.extend(element(3, &[channel.number()]));
    } else {
        let mut ht = vec![0u8; 22];
        ht[0] = channel.number();
        e.extend(element(61, &ht));
    }
    e.extend_from_slice(security);
    e
}

/// The basic rates every test BSS has: 6, 12 and 24 Mb/s.
pub const RATES: &[u8] = &[0x8c, 0x98, 0xb0];

pub fn bss_frame(bssid: Address, ssid: &[u8], channel: Channel, security: &[u8]) -> Vec<u8> {
    beacon_frame(bssid, &elements(ssid, channel, RATES, security), false)
}

pub fn address(last: u8) -> Address {
    Address([0x02, 0x00, 0x5e, 0x10, 0x00, last])
}
