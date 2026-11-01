//! The wifi protocol: `wlan` serves [`PORT`] to the network settings programs, the only rows
//! that may hold it ([`HOLDERS`]), so nothing else can scan, join or hand over a credential.
//!
//! **One request at a time per connection**, each answered once: [`Request::Scan`] by
//! [`Answer::Networks`], [`Request::Connect`] by [`Answer::Connected`] once the network's keys
//! are installed, [`Request::Disconnect`] by [`Answer::Done`], [`Request::Status`] by
//! [`Answer::Status`]; any of them by [`Answer::Refused`]. `wlan` bounds every wait of its own,
//! so each request is answered.
//!
//! **A passphrase travels to `wlan` and never back**: no answer carries one.
//!
//! Every frame is decoded here, once; one that does not decode is the peer's protocol error.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub use toyos_radio::{Band, Signal};

/// The port `wlan` serves.
pub const PORT: &str = "wifi";

/// The rows that may hold [`PORT`].
pub const HOLDERS: &[&str] = &["wifi"];

pub const MSG_SCAN: u32 = 1;
pub const MSG_CONNECT: u32 = 2;
pub const MSG_DISCONNECT: u32 = 3;
pub const MSG_STATUS: u32 = 4;

pub const MSG_NETWORKS: u32 = 16;
pub const MSG_CONNECTED: u32 = 17;
pub const MSG_DONE: u32 = 18;
pub const MSG_STATE: u32 = 19;
pub const MSG_REFUSED: u32 = 20;

/// The widest payload either side sends.
pub const MAX_PAYLOAD: usize = 8192;

/// The most networks one answer lists, strongest first.
pub const MAX_NETWORKS: usize = 200;

/// The longest refusal detail, in octets of UTF-8.
pub const MAX_DETAIL: usize = 256;

/// The most octets of an SSID (IEEE Std 802.11-2024 §9.4.2.2).
pub const MAX_SSID: usize = 32;

/// The longest passphrase, as typed: 63 characters (Annex J.4.1).
pub const MAX_PASSPHRASE: usize = 63;

/// A frame of this protocol that does not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

/// A network's name: 1 to 32 octets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct Ssid {
    octets: [u8; MAX_SSID],
    len: u8,
}

impl Ssid {
    pub fn new(octets: &[u8]) -> Option<Self> {
        if octets.is_empty() || octets.len() > MAX_SSID {
            return None;
        }
        let mut s = Self { octets: [0; MAX_SSID], len: octets.len() as u8 };
        s.octets[..octets.len()].copy_from_slice(octets);
        Some(s)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.octets[..usize::from(self.len)]
    }

    fn put(&self, out: &mut Vec<u8>) {
        out.push(self.len);
        out.extend_from_slice(self.as_bytes());
    }

    fn take(r: &mut &[u8]) -> Result<Self, Malformed> {
        let (&len, rest) = r.split_first().ok_or(Malformed)?;
        let (octets, rest) = rest.split_at_checked(usize::from(len)).ok_or(Malformed)?;
        *r = rest;
        Self::new(octets).ok_or(Malformed)
    }
}

/// What a network offers, as its strongest BSS's RSN element says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// No RSN element: open, or WEP.
    Open,
    /// WPA2-Personal: PSK.
    Wpa2,
    /// WPA3-Personal: SAE.
    Wpa3,
    /// Both, a transition-mode network.
    Wpa2Wpa3,
    /// An RSN element offering neither, such as 802.1X.
    Other,
}

impl Security {
    const ALL: [Self; 5] = [Self::Open, Self::Wpa2, Self::Wpa3, Self::Wpa2Wpa3, Self::Other];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Wpa2 => "WPA2-Personal",
            Self::Wpa3 => "WPA3-Personal",
            Self::Wpa2Wpa3 => "WPA2/WPA3-Personal",
            Self::Other => "other",
        }
    }
}

fn band_code(band: Band) -> u8 {
    match band {
        Band::TwoPointFour => 0,
        Band::Five => 1,
        Band::Six => 2,
    }
}

fn band_of(code: u8) -> Result<Band, Malformed> {
    Ok(match code {
        0 => Band::TwoPointFour,
        1 => Band::Five,
        2 => Band::Six,
        _ => return Err(Malformed),
    })
}

fn signal_of(octet: u8) -> Result<Signal, Malformed> {
    Signal::new(i8::from_le_bytes([octet])).ok_or(Malformed)
}

fn signal_octet(signal: Signal) -> u8 {
    signal.dbm().to_le_bytes()[0]
}

/// One network a scan heard: its strongest BSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: Ssid,
    pub security: Security,
    pub band: Band,
    pub signal: Signal,
    /// Whether this station can join it.
    pub joinable: bool,
}

/// Where the station is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No radio has said hello.
    NoRadio,
    Idle,
    Scanning,
    Joining { ssid: Ssid },
    Connected { ssid: Ssid, band: Band, channel: u8, signal: Signal },
}

macro_rules! refusals {
    ($($variant:ident = $code:literal, $why:literal;)*) => {
        /// Why a request was refused, or a join failed.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Refusal {
            $(#[doc = $why] $variant,)*
        }

        impl Refusal {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn reason(self) -> &'static str {
                match self {
                    $(Self::$variant => $why,)*
                }
            }

            const fn code(self) -> u8 {
                match self {
                    $(Self::$variant => $code,)*
                }
            }

            fn of(code: u8) -> Result<Self, Malformed> {
                match code {
                    $($code => Ok(Self::$variant),)*
                    _ => Err(Malformed),
                }
            }
        }
    };
}

refusals! {
    NoRadio = 1, "no radio is present";
    Passphrase = 2, "a passphrase that is not 8 to 63 printable ASCII characters";
    NotSeen = 3, "no access point of that network was heard";
    Unjoinable = 4, "no access point of that network offers what this station joins";
    Authentication = 5, "the access point refused authentication or did not answer it";
    Association = 6, "the access point refused association or did not answer it";
    NoMessage1 = 7, "the access point began no 4-way handshake";
    NoAnswerToMessage2 = 8, "the AP did not answer message 2: the passphrase is probably wrong, or the link lost it";
    Handshake = 9, "the access point's handshake broke a rule after its MIC verified";
    Lost = 10, "the association ended";
    RadioFailed = 11, "the radio broke the radio protocol, or did not answer it";
    Superseded = 12, "another connect or a disconnect replaced this one";
}

/// A request to `wlan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Scan,
    /// Join the network: `passphrase` as typed.
    Connect { ssid: Ssid, passphrase: &'a [u8] },
    Disconnect,
    Status,
}

/// An answer from `wlan`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<'a> {
    Networks(Vec<Network>),
    Connected { band: Band, channel: u8, signal: Signal },
    Done,
    State(State),
    /// `detail` says what was seen, such as a reason code or the handshake's rule.
    Refused { refusal: Refusal, detail: &'a str },
}

impl<'a> Request<'a> {
    pub fn decode(msg_type: u32, payload: &'a [u8]) -> Result<Self, Malformed> {
        let empty = |r| if payload.is_empty() { Ok(r) } else { Err(Malformed) };
        match msg_type {
            MSG_SCAN => empty(Self::Scan),
            MSG_DISCONNECT => empty(Self::Disconnect),
            MSG_STATUS => empty(Self::Status),
            MSG_CONNECT => {
                let mut r = payload;
                let ssid = Ssid::take(&mut r)?;
                Ok(Self::Connect { ssid, passphrase: r })
            }
            _ => Err(Malformed),
        }
    }

    /// The frame's message type and payload. A connect's payload holds the passphrase: the
    /// caller wipes it once sent.
    pub fn encode(&self) -> (u32, Vec<u8>) {
        let mut out = Vec::new();
        let msg_type = match self {
            Self::Scan => MSG_SCAN,
            Self::Disconnect => MSG_DISCONNECT,
            Self::Status => MSG_STATUS,
            Self::Connect { ssid, passphrase } => {
                ssid.put(&mut out);
                out.extend_from_slice(passphrase);
                MSG_CONNECT
            }
        };
        (msg_type, out)
    }
}

impl<'a> Answer<'a> {
    pub fn decode(msg_type: u32, payload: &'a [u8]) -> Result<Self, Malformed> {
        let mut r = payload;
        let answer = match msg_type {
            MSG_NETWORKS => {
                let mut networks = Vec::new();
                while !r.is_empty() {
                    if networks.len() == MAX_NETWORKS {
                        return Err(Malformed);
                    }
                    let ssid = Ssid::take(&mut r)?;
                    let (&[security, band, signal, joinable], rest) = r.split_first_chunk::<4>().ok_or(Malformed)?;
                    r = rest;
                    networks.push(Network {
                        ssid,
                        security: *Security::ALL.get(usize::from(security)).ok_or(Malformed)?,
                        band: band_of(band)?,
                        signal: signal_of(signal)?,
                        joinable: match joinable {
                            0 => false,
                            1 => true,
                            _ => return Err(Malformed),
                        },
                    });
                }
                Self::Networks(networks)
            }
            MSG_CONNECTED => {
                let (&[band, channel, signal], rest) = r.split_first_chunk::<3>().ok_or(Malformed)?;
                r = rest;
                Self::Connected { band: band_of(band)?, channel, signal: signal_of(signal)? }
            }
            MSG_DONE => Self::Done,
            MSG_STATE => {
                let (&kind, rest) = r.split_first().ok_or(Malformed)?;
                r = rest;
                Self::State(match kind {
                    0 => State::NoRadio,
                    1 => State::Idle,
                    2 => State::Scanning,
                    3 => State::Joining { ssid: Ssid::take(&mut r)? },
                    4 => {
                        let ssid = Ssid::take(&mut r)?;
                        let (&[band, channel, signal], rest) = r.split_first_chunk::<3>().ok_or(Malformed)?;
                        r = rest;
                        State::Connected { ssid, band: band_of(band)?, channel, signal: signal_of(signal)? }
                    }
                    _ => return Err(Malformed),
                })
            }
            MSG_REFUSED => {
                let (&code, detail) = r.split_first().ok_or(Malformed)?;
                r = &[];
                if detail.len() > MAX_DETAIL {
                    return Err(Malformed);
                }
                Self::Refused {
                    refusal: Refusal::of(code)?,
                    detail: core::str::from_utf8(detail).map_err(|_| Malformed)?,
                }
            }
            _ => return Err(Malformed),
        };
        if !r.is_empty() {
            return Err(Malformed);
        }
        Ok(answer)
    }

    /// The frame's message type and payload. A list past [`MAX_NETWORKS`] keeps its first
    /// ones, and a detail past [`MAX_DETAIL`] is cut at a character boundary.
    pub fn encode(&self) -> (u32, Vec<u8>) {
        let mut out = Vec::new();
        let msg_type = match self {
            Self::Networks(networks) => {
                for n in networks.iter().take(MAX_NETWORKS) {
                    n.ssid.put(&mut out);
                    let security = Security::ALL.iter().position(|s| *s == n.security).unwrap_or(0) as u8;
                    out.extend_from_slice(&[security, band_code(n.band), signal_octet(n.signal), u8::from(n.joinable)]);
                }
                MSG_NETWORKS
            }
            Self::Connected { band, channel, signal } => {
                out.extend_from_slice(&[band_code(*band), *channel, signal_octet(*signal)]);
                MSG_CONNECTED
            }
            Self::Done => MSG_DONE,
            Self::State(state) => {
                match state {
                    State::NoRadio => out.push(0),
                    State::Idle => out.push(1),
                    State::Scanning => out.push(2),
                    State::Joining { ssid } => {
                        out.push(3);
                        ssid.put(&mut out);
                    }
                    State::Connected { ssid, band, channel, signal } => {
                        out.push(4);
                        ssid.put(&mut out);
                        out.extend_from_slice(&[band_code(*band), *channel, signal_octet(*signal)]);
                    }
                }
                MSG_STATE
            }
            Self::Refused { refusal, detail } => {
                out.push(refusal.code());
                let mut end = detail.len().min(MAX_DETAIL);
                while !detail.is_char_boundary(end) {
                    end -= 1;
                }
                out.extend_from_slice(&detail.as_bytes()[..end]);
                MSG_REFUSED
            }
        };
        (msg_type, out)
    }
}

const _: () = assert!(MAX_NETWORKS * (1 + MAX_SSID + 4) <= MAX_PAYLOAD);
const _: () = assert!(MAX_DETAIL < MAX_PAYLOAD);
