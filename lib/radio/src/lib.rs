//! The radio protocol: `wlan` serves [`PORT`], and each Wi-Fi driver process, one per radio
//! function, is a client of it.
//!
//! **The driver's first frame is [`Event::Hello`]**, its address and bands. From then on `wlan`
//! sends [`Command`]s and the driver answers with [`Event`]s, in order:
//!
//! - [`Command::Scan`] is answered by any number of [`Event::Seen`], each a Beacon or Probe
//!   Response heard, and then one [`Event::ScanDone`];
//! - [`Command::Join`] by [`Event::Associated`] or [`Event::JoinFailed`]; the driver
//!   authenticates by Open System and associates, its request carrying the SSID and the elements
//!   the command names. **The driver keeps no scan results**: a join carries the elements of the
//!   Beacon or Probe Response the BSS was chosen by, whose operation elements the driver
//!   configures the association from;
//! - once associated, EAPOL frames go both ways ([`Command::Eapol`], [`Event::Eapol`]), and the
//!   keys `wlan` derives are installed by [`Command::Pairwise`], [`Command::Group`] and
//!   [`Command::Integrity`]. **An install takes effect after every frame sent before it has
//!   left**, so message 4 goes out under the keys message 3 came under. [`Command::Authorize`]
//!   opens the link to data;
//! - every [`Command::Leave`] is answered by its own [`Left::Asked`], associated or not; any
//!   other [`Event::Left`] ends the association that is up. Before [`Event::Associated`] there
//!   is none, and a failed join is [`Event::JoinFailed`].
//!
//! `wlan` bounds each answer it waits for, and ends a driver that misses one.
//!
//! **Every frame is decoded here, once**: one that does not decode is the peer's protocol error,
//! and the reader ends the connection by name. What a frame carries off the air (a Seen's or an
//! EAPOL frame's bytes) is the air's, decoded by its reader.

#![no_std]

use toyos_ieee80211::Address;

/// The port `wlan` serves and every Wi-Fi driver connects to.
pub const PORT: &str = "radio";

/// The rows that may hold [`PORT`]: the drivers.
pub const HOLDERS: &[&str] = &["ax2xx"];

/// The most octets of one received or sent frame this protocol carries: a frame longer than
/// this is the driver's to drop.
pub const MAX_FRAME: usize = 4096;

/// The most octets of an SSID (IEEE Std 802.11-2024 §9.4.2.2).
pub const MAX_SSID: usize = 32;

/// The most octets of the elements a join adds to its association request: an RSNE and an
/// RSNXE, each at most an element's 257.
pub const MAX_REQUEST: usize = 2 * 257;

/// The widest payload either side sends: a join's.
pub const MAX_PAYLOAD: usize = 6 + 2 + 1 + MAX_SSID + 2 + MAX_REQUEST + MAX_FRAME;

// Driver to wlan.
pub const MSG_HELLO: u32 = 1;
pub const MSG_SEEN: u32 = 2;
pub const MSG_SCAN_DONE: u32 = 3;
pub const MSG_ASSOCIATED: u32 = 4;
pub const MSG_JOIN_FAILED: u32 = 5;
pub const MSG_EAPOL_RECEIVED: u32 = 6;
pub const MSG_LEFT: u32 = 7;
// wlan to driver.
pub const MSG_SCAN: u32 = 16;
pub const MSG_JOIN: u32 = 17;
pub const MSG_EAPOL_SEND: u32 = 18;
pub const MSG_PAIRWISE: u32 = 19;
pub const MSG_GROUP: u32 = 20;
pub const MSG_INTEGRITY: u32 = 21;
pub const MSG_AUTHORIZE: u32 = 22;
pub const MSG_LEAVE: u32 = 23;

macro_rules! reasons {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $why:literal;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(#[doc = $why] $variant,)*
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn reason(self) -> &'static str {
                match self {
                    $(Self::$variant => $why,)*
                }
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.reason())
            }
        }
    };
}

reasons! {
    /// Why a frame of this protocol was refused.
    Violation {
        UnknownMessage = "a message type this protocol does not have in that direction";
        Length = "a payload whose length its message does not have";
        Address = "an address that is all zeroes or a group address";
        Bands = "a band set that is empty or names a band this protocol does not";
        Frequency = "a frequency that is no channel's in 2.4, 5 or 6 GHz";
        Signal = "a signal above 0 dBm";
        FrameLength = "a frame that is empty or longer than this protocol carries";
        JoinStep = "a join step other than authentication or association";
        JoinCause = "a join failure that is neither a status code nor no answer";
        LeftCause = "a reason for leaving this protocol does not have";
        GroupKeyId = "a group key ID other than 1, 2 or 3";
        IntegrityKeyId = "an integrity key ID other than 4 or 5";
        PacketNumber = "a packet number past 48 bits";
        Ssid = "an SSID of 0 or more than 32 octets";
        Elements = "a join's elements that are empty, too long or not whole elements";
    }
}

/// A frequency band a radio operates in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Band {
    TwoPointFour,
    Five,
    Six,
}

impl Band {
    const fn bit(self) -> u8 {
        match self {
            Self::TwoPointFour => 1,
            Self::Five => 2,
            Self::Six => 4,
        }
    }
}

/// The bands a radio operates in: at least one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bands(u8);

impl Bands {
    const KNOWN: u8 = 0b111;

    pub fn of(bands: &[Band]) -> Option<Self> {
        let bits = bands.iter().fold(0, |all, b| all | b.bit());
        (bits != 0).then_some(Self(bits))
    }

    pub const fn has(self, band: Band) -> bool {
        self.0 & band.bit() != 0
    }

    fn decode(bits: u8) -> Result<Self, Violation> {
        if bits == 0 || bits & !Self::KNOWN != 0 {
            return Err(Violation::Bands);
        }
        Ok(Self(bits))
    }
}

/// A channel, by its band and its number there, as IEEE Std 802.11-2024 Annex E's operating
/// classes number them from each band's starting frequency: 2.4 GHz channels 1 to 14, 5 GHz
/// channels 32 to 177, and the 6 GHz 20 MHz channels 1, 5, ... 233 and 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Channel {
    band: Band,
    number: u8,
}

impl Channel {
    /// The channel centred on `mhz`, if one is.
    pub const fn from_mhz(mhz: u16) -> Option<Self> {
        let (band, number) = match mhz {
            2484 => (Band::TwoPointFour, 14),
            2412..=2472 if (mhz - 2407).is_multiple_of(5) => (Band::TwoPointFour, (mhz - 2407) / 5),
            5160..=5885 if mhz.is_multiple_of(5) => (Band::Five, (mhz - 5000) / 5),
            5935 => (Band::Six, 2),
            5955..=7115 if (mhz - 5950) % 20 == 5 => (Band::Six, (mhz - 5950) / 5),
            _ => return None,
        };
        // Each arm's `number` is at most 233.
        Some(Self { band, number: number as u8 })
    }

    pub const fn mhz(self) -> u16 {
        let n = self.number as u16;
        match (self.band, self.number) {
            (Band::TwoPointFour, 14) => 2484,
            (Band::TwoPointFour, _) => 2407 + 5 * n,
            (Band::Five, _) => 5000 + 5 * n,
            (Band::Six, 2) => 5935,
            (Band::Six, _) => 5950 + 5 * n,
        }
    }

    pub const fn band(self) -> Band {
        self.band
    }

    pub const fn number(self) -> u8 {
        self.number
    }

    fn decode(bytes: [u8; 2]) -> Result<Self, Violation> {
        Self::from_mhz(u16::from_le_bytes(bytes)).ok_or(Violation::Frequency)
    }
}

/// A received signal's power in dBm, at most 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Signal(i8);

impl Signal {
    pub const fn new(dbm: i8) -> Option<Self> {
        if dbm > 0 { None } else { Some(Self(dbm)) }
    }

    pub const fn dbm(self) -> i8 {
        self.0
    }
}

/// Which step of a join failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinStep {
    Authentication,
    Association,
}

/// Why a join step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinCause {
    /// The access point answered with this status code (IEEE Std 802.11-2024 §9.4.1.9).
    Status(u16),
    /// It did not answer the driver's retries.
    NoAnswer,
}

/// Why an association ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    /// [`Command::Leave`] asked.
    Asked,
    /// The access point deauthenticated the station, with this reason code (§9.4.1.7).
    Deauthenticated(u16),
    /// It disassociated the station, with this reason code.
    Disassociated(u16),
    /// Its beacons stopped.
    BeaconLoss,
    /// The radio's firmware failed and was restarted.
    Firmware,
    /// The radio was switched off.
    RfKill,
}

/// A frame from the driver to `wlan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// The radio's own address, and its bands.
    Hello { address: Address, bands: Bands },
    /// A Beacon or Probe Response, one MPDU without its FCS, heard on `channel` at `signal`.
    Seen { channel: Channel, signal: Signal, frame: &'a [u8] },
    ScanDone,
    Associated,
    JoinFailed { step: JoinStep, cause: JoinCause },
    /// An EAPOL PDU from `source`, from its Protocol Version octet.
    Eapol { source: Address, frame: &'a [u8] },
    Left(Left),
}

/// A frame from `wlan` to the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// Scan every channel of the radio's bands.
    Scan,
    /// Authenticate with the BSS by Open System and associate with it, the request carrying
    /// `ssid` and `request` (the station's RSNE, and its RSNXE when it has one) whole. `heard`
    /// is the elements of the Beacon or Probe Response the BSS was chosen by.
    Join { bssid: Address, channel: Channel, ssid: &'a [u8], request: &'a [u8], heard: &'a [u8] },
    /// Send an EAPOL PDU to the access point joined.
    Eapol { frame: &'a [u8] },
    /// The pairwise temporal key, CCMP-128.
    Pairwise { key: &'a [u8; 16] },
    /// A CCMP-128 group key, and the last packet number sent under it.
    Group { id: u8, key: &'a [u8; 16], rsc: u64 },
    /// A BIP-CMAC-128 integrity group key, and the last packet number sent under it.
    Integrity { id: u16, key: &'a [u8; 16], ipn: u64 },
    /// The 4-way handshake completed: data may pass.
    Authorize,
    /// Leave the BSS.
    Leave,
}

fn address(bytes: [u8; 6]) -> Result<Address, Violation> {
    let a = Address(bytes);
    if a.is_group() || bytes == [0; 6] {
        return Err(Violation::Address);
    }
    Ok(a)
}

fn frame(bytes: &[u8]) -> Result<&[u8], Violation> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Violation::FrameLength);
    }
    Ok(bytes)
}

fn empty(payload: &[u8]) -> Result<(), Violation> {
    if payload.is_empty() { Ok(()) } else { Err(Violation::Length) }
}

fn exact<const N: usize>(payload: &[u8]) -> Result<[u8; N], Violation> {
    payload.try_into().map_err(|_| Violation::Length)
}

/// A 48-bit packet number, carried in eight octets.
fn packet_number(bytes: [u8; 8]) -> Result<u64, Violation> {
    let pn = u64::from_le_bytes(bytes);
    if pn >> 48 != 0 {
        return Err(Violation::PacketNumber);
    }
    Ok(pn)
}

/// A writer into a payload buffer; every message fits [`MAX_PAYLOAD`].
struct Out<'b> {
    buf: &'b mut [u8],
    len: usize,
}

impl Out<'_> {
    fn put(&mut self, bytes: &[u8]) {
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }
}

impl<'a> Event<'a> {
    pub fn decode(msg_type: u32, payload: &'a [u8]) -> Result<Self, Violation> {
        Ok(match msg_type {
            MSG_HELLO => {
                let [a0, a1, a2, a3, a4, a5, bands] = exact(payload)?;
                Self::Hello { address: address([a0, a1, a2, a3, a4, a5])?, bands: Bands::decode(bands)? }
            }
            MSG_SEEN => {
                let (&[f0, f1, signal], rest) = payload.split_first_chunk::<3>().ok_or(Violation::Length)?;
                Self::Seen {
                    channel: Channel::decode([f0, f1])?,
                    signal: Signal::new(i8::from_le_bytes([signal])).ok_or(Violation::Signal)?,
                    frame: frame(rest)?,
                }
            }
            MSG_SCAN_DONE => empty(payload).map(|()| Self::ScanDone)?,
            MSG_ASSOCIATED => empty(payload).map(|()| Self::Associated)?,
            MSG_JOIN_FAILED => {
                let [step, cause, s0, s1] = exact(payload)?;
                let step = match step {
                    1 => JoinStep::Authentication,
                    2 => JoinStep::Association,
                    _ => return Err(Violation::JoinStep),
                };
                let cause = match (cause, u16::from_le_bytes([s0, s1])) {
                    (0, status) => JoinCause::Status(status),
                    (1, 0) => JoinCause::NoAnswer,
                    _ => return Err(Violation::JoinCause),
                };
                Self::JoinFailed { step, cause }
            }
            MSG_EAPOL_RECEIVED => {
                let (&source, rest) = payload.split_first_chunk::<6>().ok_or(Violation::Length)?;
                Self::Eapol { source: address(source)?, frame: frame(rest)? }
            }
            MSG_LEFT => {
                let [kind, c0, c1] = exact(payload)?;
                let code = u16::from_le_bytes([c0, c1]);
                Self::Left(match (kind, code) {
                    (0, 0) => Left::Asked,
                    (1, reason) => Left::Deauthenticated(reason),
                    (2, reason) => Left::Disassociated(reason),
                    (3, 0) => Left::BeaconLoss,
                    (4, 0) => Left::Firmware,
                    (5, 0) => Left::RfKill,
                    _ => return Err(Violation::LeftCause),
                })
            }
            _ => return Err(Violation::UnknownMessage),
        })
    }

    /// The frame's message type, and its payload's length in `out`. A frame past [`MAX_FRAME`]
    /// panics: it is the sender's to drop.
    pub fn encode(&self, out: &mut [u8; MAX_PAYLOAD]) -> (u32, usize) {
        let mut o = Out { buf: out, len: 0 };
        let msg_type = match *self {
            Self::Hello { address, bands } => {
                o.put(&address.0);
                o.put(&[bands.0]);
                MSG_HELLO
            }
            Self::Seen { channel, signal, frame } => {
                o.put(&channel.mhz().to_le_bytes());
                o.put(&signal.0.to_le_bytes());
                o.put(frame);
                MSG_SEEN
            }
            Self::ScanDone => MSG_SCAN_DONE,
            Self::Associated => MSG_ASSOCIATED,
            Self::JoinFailed { step, cause } => {
                let step = match step {
                    JoinStep::Authentication => 1,
                    JoinStep::Association => 2,
                };
                let (cause, status) = match cause {
                    JoinCause::Status(s) => (0, s),
                    JoinCause::NoAnswer => (1, 0),
                };
                o.put(&[step, cause]);
                o.put(&status.to_le_bytes());
                MSG_JOIN_FAILED
            }
            Self::Eapol { source, frame } => {
                o.put(&source.0);
                o.put(frame);
                MSG_EAPOL_RECEIVED
            }
            Self::Left(left) => {
                let (kind, code) = match left {
                    Left::Asked => (0, 0),
                    Left::Deauthenticated(r) => (1, r),
                    Left::Disassociated(r) => (2, r),
                    Left::BeaconLoss => (3, 0),
                    Left::Firmware => (4, 0),
                    Left::RfKill => (5, 0),
                };
                o.put(&[kind]);
                o.put(&u16::to_le_bytes(code));
                MSG_LEFT
            }
        };
        (msg_type, o.len)
    }
}

impl<'a> Command<'a> {
    pub fn decode(msg_type: u32, payload: &'a [u8]) -> Result<Self, Violation> {
        Ok(match msg_type {
            MSG_SCAN => empty(payload).map(|()| Self::Scan)?,
            MSG_JOIN => {
                let (&[b0, b1, b2, b3, b4, b5, f0, f1, ssid_len], rest) =
                    payload.split_first_chunk::<9>().ok_or(Violation::Length)?;
                let bssid = address([b0, b1, b2, b3, b4, b5])?;
                let channel = Channel::decode([f0, f1])?;
                let (ssid, rest) = rest.split_at_checked(usize::from(ssid_len)).ok_or(Violation::Length)?;
                if ssid.is_empty() || ssid.len() > MAX_SSID {
                    return Err(Violation::Ssid);
                }
                let (&request_len, rest) = rest.split_first_chunk::<2>().ok_or(Violation::Length)?;
                let (request, heard) =
                    rest.split_at_checked(usize::from(u16::from_le_bytes(request_len))).ok_or(Violation::Length)?;
                let whole = |e: &[u8], most: usize| {
                    !e.is_empty() && e.len() <= most && toyos_ieee80211::element::iter(e).all(|e| e.is_ok())
                };
                if !whole(request, MAX_REQUEST) || !whole(heard, MAX_FRAME) {
                    return Err(Violation::Elements);
                }
                Self::Join { bssid, channel, ssid, request, heard }
            }
            MSG_EAPOL_SEND => Self::Eapol { frame: frame(payload)? },
            MSG_PAIRWISE => Self::Pairwise { key: payload.try_into().map_err(|_| Violation::Length)? },
            MSG_GROUP => {
                let (&[id], rest) = payload.split_first_chunk::<1>().ok_or(Violation::Length)?;
                let (key, rsc) = rest.split_first_chunk::<16>().ok_or(Violation::Length)?;
                if !(1..=3).contains(&id) {
                    return Err(Violation::GroupKeyId);
                }
                Self::Group { id, key, rsc: packet_number(exact(rsc)?)? }
            }
            MSG_INTEGRITY => {
                let (&id, rest) = payload.split_first_chunk::<2>().ok_or(Violation::Length)?;
                let (key, ipn) = rest.split_first_chunk::<16>().ok_or(Violation::Length)?;
                let id = u16::from_le_bytes(id);
                if !matches!(id, 4 | 5) {
                    return Err(Violation::IntegrityKeyId);
                }
                Self::Integrity { id, key, ipn: packet_number(exact(ipn)?)? }
            }
            MSG_AUTHORIZE => empty(payload).map(|()| Self::Authorize)?,
            MSG_LEAVE => empty(payload).map(|()| Self::Leave)?,
            _ => return Err(Violation::UnknownMessage),
        })
    }

    /// The frame's message type, and its payload's length in `out`. A frame, SSID or elements
    /// past their bounds panic. A key command's payload holds the key: the caller wipes `out`
    /// once it is sent.
    pub fn encode(&self, out: &mut [u8; MAX_PAYLOAD]) -> (u32, usize) {
        let mut o = Out { buf: out, len: 0 };
        let msg_type = match *self {
            Self::Scan => MSG_SCAN,
            Self::Join { bssid, channel, ssid, request, heard } => {
                assert!(ssid.len() <= MAX_SSID && request.len() <= MAX_REQUEST && heard.len() <= MAX_FRAME);
                o.put(&bssid.0);
                o.put(&channel.mhz().to_le_bytes());
                o.put(&[ssid.len() as u8]);
                o.put(ssid);
                o.put(&(request.len() as u16).to_le_bytes());
                o.put(request);
                o.put(heard);
                MSG_JOIN
            }
            Self::Eapol { frame } => {
                o.put(frame);
                MSG_EAPOL_SEND
            }
            Self::Pairwise { key } => {
                o.put(key);
                MSG_PAIRWISE
            }
            Self::Group { id, key, rsc } => {
                o.put(&[id]);
                o.put(key);
                o.put(&rsc.to_le_bytes());
                MSG_GROUP
            }
            Self::Integrity { id, key, ipn } => {
                o.put(&id.to_le_bytes());
                o.put(key);
                o.put(&ipn.to_le_bytes());
                MSG_INTEGRITY
            }
            Self::Authorize => MSG_AUTHORIZE,
            Self::Leave => MSG_LEAVE,
        };
        (msg_type, o.len)
    }
}
