//! What `wlan` decides, sans-IO: the radio it drives, the networks a scan heard, the BSS a
//! connect chooses, the join and its handshakes, and the answers to the settings programs.
//!
//! The program hands [`Station`] each frame from the radio ([`Station::radio`]) and from a
//! settings program ([`Station::request`]), and the clock ([`Station::tick`]); what the station
//! has to send is [`Station::take`]'s. Every wait has a bound, so every request is answered.
//!
//! - **The radio is distrusted.** A frame of the radio protocol out of its order, or a bound
//!   it misses, ends the radio by name ([`Out::EndRadio`]). What a radio frame carries off the
//!   air is the air's: a malformed one is counted and dropped.
//! - **The BSS chosen** offers WPA2-Personal as `toyos-wlan-handshake` keys it ([`choice`]);
//!   among those of the network, one at least [`STRONG`] beats any weaker; among those, the
//!   higher band, then the stronger signal.
//! - **A join or association abandoned** by a connect or a disconnect is left; what the radio
//!   still says of it until it answers the leave is dropped.
//! - **A wrong passphrase is refused on what the station saw**: an access point that dropped
//!   every message 2 sent ([`Refusal::NoAnswerToMessage2`]), which a lossy link can also
//!   produce.
//! - **A join that was connected and lost is joined once more**, by a new scan, without a
//!   client waiting; so is one whose radio ended, once a radio says hello again.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod choice;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use toyos_crypto::{Secret, wpa2};
use toyos_ieee80211::Address;
use toyos_ieee80211::frame::{Frame, ManagementSubtype};
use toyos_ieee80211::mgmt::Body;
use toyos_radio::{Bands, Channel, Command, Event, JoinCause, JoinStep, Left, Signal, Violation};
use toyos_wifi::{Answer, MAX_PASSPHRASE, Network, Refusal, Request, Ssid, State};
use toyos_wlan_handshake::{Join as Keyed, Supplicant};

pub use choice::{Bss, STRONG, Unjoinable};

/// How long a radio has to say hello once it connects.
pub const HELLO_BOUND: Duration = Duration::from_secs(5);
/// How long a scan may take: a passive dwell of some 150 ms on each of the about 97 20 MHz
/// channels of 2.4, 5 and 6 GHz (13, 25 and 59) is about 15 s. Policy, as the dwell is the
/// driver's.
pub const SCAN_BOUND: Duration = Duration::from_secs(20);
/// How long the radio may take to authenticate and associate, its own retries included.
pub const JOIN_BOUND: Duration = Duration::from_secs(5);
/// How long the 4-way handshake may take once associated, the authenticator's resends of
/// message 1 and message 3 among it. Policy.
pub const HANDSHAKE_BOUND: Duration = Duration::from_secs(10);
/// How long the radio may take to answer a leave.
pub const LEAVE_BOUND: Duration = Duration::from_secs(5);

/// The most BSSes one scan keeps: past it, a BSS not yet heard is dropped and counted.
pub const MAX_BSS: usize = 256;

/// A settings program's connection, as the program numbers them.
pub type Client = u64;

/// The most octets of a key command's payload: an integrity key's ID, key and packet number.
const KEY_PAYLOAD: usize = 2 + 16 + 8;

/// A frame's payload. A key command's is held in a [`Secret`], wiped when it is dropped; no
/// other payload carries a secret.
pub enum Payload {
    Plain(Vec<u8>),
    Key { bytes: Secret<KEY_PAYLOAD>, len: usize },
}

impl Payload {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Plain(bytes) => bytes,
            Self::Key { bytes, len } => &bytes.expose()[..*len],
        }
    }
}

/// What the station has to send.
pub enum Out {
    Radio { msg_type: u32, payload: Payload },
    /// End the radio's connection, for this.
    EndRadio(String),
    Answer { client: Client, msg_type: u32, payload: Payload },
    /// End this client's connection: it broke the wifi protocol.
    EndClient(Client),
    /// A line for the log. It names no network and no address.
    Log(String),
}

/// What the station dropped of what the air sent: the scan's counts since it began, logged when
/// it is done, and the join's since it was asked, logged when it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Seen frames that were no Beacon or Probe Response, or did not parse.
    pub malformed_seen: u64,
    /// Seen frames whose own channel is not the one they were heard on.
    pub off_channel: u64,
    /// BSSes dropped past [`MAX_BSS`].
    pub dropped_bss: u64,
    /// EAPOL frames from another address than the access point joined.
    pub stray_eapol: u64,
    /// EAPOL frames the supplicant refused and dropped.
    pub refused_eapol: u64,
}

/// A passphrase as typed: 8 to 63 printable ASCII characters (IEEE Std 802.11-2024 Annex J.4.1).
struct Passphrase {
    octets: Secret<MAX_PASSPHRASE>,
    len: usize,
}

impl Passphrase {
    fn new(typed: &[u8]) -> Option<Self> {
        if !(8..=MAX_PASSPHRASE).contains(&typed.len()) || !typed.iter().all(|b| (32..=126).contains(b)) {
            return None;
        }
        let mut octets = Secret::new([0; MAX_PASSPHRASE]);
        octets.expose_mut()[..typed.len()].copy_from_slice(typed);
        Some(Self { octets, len: typed.len() })
    }

    fn as_bytes(&self) -> &[u8] {
        &self.octets.expose()[..self.len]
    }
}

/// The BSS a join chose, with what its keys are derived from.
struct Chosen {
    bssid: Address,
    channel: Channel,
    signal: Signal,
}

enum Phase {
    /// Waiting for the scan the join asked.
    Scanning,
    /// Join sent; waiting for Associated.
    Associating { bss: Chosen, supplicant: Supplicant, since: Duration },
    /// Associated; the 4-way handshake runs.
    Handshaking { bss: Chosen, supplicant: Supplicant, since: Duration, message2s: u32 },
    Connected { bss: Chosen, supplicant: Supplicant },
}

struct Join {
    ssid: Ssid,
    passphrase: Passphrase,
    /// Who waits for its answer: none for a join made again after a loss. A join that fails
    /// is not made again.
    client: Option<Client>,
    phase: Phase,
}

struct Radio {
    address: Address,
    bands: Bands,
}

enum Link {
    /// No radio is connected.
    None,
    /// Connected at this time, and not yet said hello.
    Connecting(Duration),
    Ready(Radio),
}

pub struct Station {
    link: Link,
    /// The BSSes the last scan heard, or the one running.
    heard: BTreeMap<Address, Bss>,
    /// When the scan running was asked.
    scanning: Option<Duration>,
    /// The clients waiting for the scan's networks.
    scan_waiters: Vec<Client>,
    /// When a leave not yet answered was sent.
    leaving: Option<Duration>,
    /// A join abandoned before the radio answered it, whose answer is still due.
    abandoned: bool,
    join: Option<Join>,
    /// A join a radio's end interrupted while connected, made again at the next hello.
    resume: Option<Join>,
    /// The clients whose request is not answered yet.
    asking: Vec<Client>,
    /// The clients ended for what they sent, whose later frames are not acted on.
    ended: Vec<Client>,
    counters: Counters,
    out: Vec<Out>,
}

impl Default for Station {
    fn default() -> Self {
        Self::new()
    }
}

/// Which protocol rule the radio broke.
fn broke(why: &str) -> String {
    format!("the radio {why}")
}

impl Station {
    pub fn new() -> Self {
        Self {
            link: Link::None,
            heard: BTreeMap::new(),
            scanning: None,
            scan_waiters: Vec::new(),
            leaving: None,
            abandoned: false,
            join: None,
            resume: None,
            asking: Vec::new(),
            ended: Vec::new(),
            counters: Counters::default(),
            out: Vec::new(),
        }
    }

    /// What the station has to send, in order.
    pub fn take(&mut self) -> Vec<Out> {
        core::mem::take(&mut self.out)
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Whether a radio is connected, said hello or not: the program accepts one at a time.
    pub fn has_radio(&self) -> bool {
        !matches!(self.link, Link::None)
    }

    /// The next time [`Self::tick`] has something to do.
    pub fn deadline(&self) -> Option<Duration> {
        let mut all = [None; 4];
        all[0] = match self.link {
            Link::Connecting(since) => Some(since + HELLO_BOUND),
            _ => None,
        };
        all[1] = self.scanning.map(|since| since + SCAN_BOUND);
        all[2] = self.leaving.map(|since| since + LEAVE_BOUND);
        all[3] = match self.join.as_ref().map(|j| &j.phase) {
            Some(Phase::Associating { since, .. }) => Some(*since + JOIN_BOUND),
            Some(Phase::Handshaking { since, .. }) => Some(*since + HANDSHAKE_BOUND),
            _ => None,
        };
        all.into_iter().flatten().min()
    }

    /// A radio's connection was accepted.
    pub fn radio_connected(&mut self, now: Duration) {
        assert!(!self.has_radio(), "the program accepts one radio at a time");
        self.link = Link::Connecting(now);
    }

    /// The radio's connection ended: what waited on it fails, and a connected join is made
    /// again at the next hello.
    pub fn radio_gone(&mut self) {
        self.end_radio(None);
    }

    fn end_radio(&mut self, violation: Option<String>) {
        let detail = match &violation {
            Some(why) => why.clone(),
            None => String::from("the radio's connection ended"),
        };
        if let Some(why) = violation {
            self.out.push(Out::EndRadio(why));
        }
        self.link = Link::None;
        self.scanning = None;
        self.leaving = None;
        self.abandoned = false;
        self.heard.clear();
        for client in core::mem::take(&mut self.scan_waiters) {
            self.refuse(client, Refusal::RadioFailed, &detail);
        }
        if let Some(mut join) = self.join.take() {
            if matches!(join.phase, Phase::Connected { .. }) {
                self.say("wlan: the radio ended while connected; joining again once a radio says hello");
                join.phase = Phase::Scanning;
                self.resume = Some(join);
            } else {
                self.fail(join, Refusal::RadioFailed, &detail);
            }
        }
    }

    fn violation(&mut self, why: String) {
        self.say(&format!("wlan: ending the radio: {why}"));
        self.end_radio(Some(why));
    }

    fn say(&mut self, line: &str) {
        self.out.push(Out::Log(String::from(line)));
    }

    fn command(&mut self, command: Command<'_>) {
        let mut buf = Secret::new([0u8; toyos_radio::MAX_PAYLOAD]);
        let (msg_type, len) = command.encode(buf.expose_mut());
        let encoded = &buf.expose()[..len];
        let payload = match command {
            Command::Pairwise { .. } | Command::Group { .. } | Command::Integrity { .. } => {
                let mut bytes = Secret::new([0; KEY_PAYLOAD]);
                bytes.expose_mut()[..len].copy_from_slice(encoded);
                Payload::Key { bytes, len }
            }
            _ => Payload::Plain(encoded.to_vec()),
        };
        self.out.push(Out::Radio { msg_type, payload });
    }

    fn answer(&mut self, client: Client, answer: Answer<'_>) {
        self.asking.retain(|c| *c != client);
        let (msg_type, payload) = answer.encode();
        self.out.push(Out::Answer { client, msg_type, payload: Payload::Plain(payload) });
    }

    fn refuse(&mut self, client: Client, refusal: Refusal, detail: &str) {
        self.answer(client, Answer::Refused { refusal, detail });
    }

    /// A join that failed: its client is told, and nothing is joined again.
    fn fail(&mut self, join: Join, refusal: Refusal, detail: &str) {
        self.say(&format!("wlan: a join failed: {}: {detail}", refusal.reason()));
        self.eapol_dropped();
        if let Some(client) = join.client {
            self.refuse(client, refusal, detail);
        }
    }

    /// What the join that ended dropped of its access point's EAPOL frames, if anything.
    fn eapol_dropped(&mut self) {
        let c = self.counters;
        if c.stray_eapol != 0 || c.refused_eapol != 0 {
            self.say(&format!(
                "wlan: the join dropped {} EAPOL frames from another address and refused {}",
                c.stray_eapol, c.refused_eapol
            ));
        }
        (self.counters.stray_eapol, self.counters.refused_eapol) = (0, 0);
    }

    /// Leave the BSS a join was associated with, if it was.
    fn leave(&mut self, join: &Join, now: Duration) {
        if matches!(join.phase, Phase::Scanning) {
            return;
        }
        self.abandoned |= matches!(join.phase, Phase::Associating { .. });
        self.command(Command::Leave);
        self.leaving = Some(now);
    }

    fn scan(&mut self, now: Duration) {
        if self.scanning.is_none() {
            self.heard.clear();
            let c = &mut self.counters;
            (c.malformed_seen, c.off_channel, c.dropped_bss) = (0, 0, 0);
            self.scanning = Some(now);
            self.command(Command::Scan);
        }
    }

    /// The bounds that passed by `now`.
    pub fn tick(&mut self, now: Duration) {
        if let Link::Connecting(since) = self.link
            && now >= since + HELLO_BOUND
        {
            return self.violation(broke("said no hello"));
        }
        if self.scanning.is_some_and(|since| now >= since + SCAN_BOUND) {
            return self.violation(broke("did not finish a scan in time"));
        }
        if self.leaving.is_some_and(|since| now >= since + LEAVE_BOUND) {
            return self.violation(broke("did not answer a leave in time"));
        }
        let Some(join) = &self.join else { return };
        match join.phase {
            Phase::Associating { since, .. } if now >= since + JOIN_BOUND => {
                self.violation(broke("did not answer a join in time"));
            }
            Phase::Handshaking { since, message2s, .. } if now >= since + HANDSHAKE_BOUND => {
                let join = self.join.take().expect("matched above");
                self.leave(&join, now);
                let (refusal, detail) = unanswered(message2s, "no message 3 came in time");
                self.fail(join, refusal, &detail);
            }
            _ => {}
        }
    }

    /// One frame from the radio. `nonce` is asked for 32 random octets when a handshake starts.
    pub fn radio(&mut self, msg_type: u32, payload: &[u8], now: Duration, nonce: impl FnOnce() -> [u8; 32]) {
        let event = match Event::decode(msg_type, payload) {
            Ok(event) => event,
            Err(v) => return self.violation(broke(&format!("sent a frame that does not decode: {}", Violation::reason(v)))),
        };
        match (&self.link, event) {
            (Link::None, _) => unreachable!("no radio frame arrives without a radio"),
            (Link::Connecting(_), Event::Hello { address, bands }) => self.hello(address, bands, now),
            (Link::Connecting(_), _) => self.violation(broke("sent a frame before its hello")),
            (Link::Ready(_), Event::Hello { .. }) => self.violation(broke("said hello twice")),
            (Link::Ready(_), Event::Seen { channel, signal, frame }) => self.seen(channel, signal, frame),
            (Link::Ready(_), Event::ScanDone) => self.scan_done(now),
            (Link::Ready(_), Event::Associated) => self.associated(now),
            (Link::Ready(_), Event::JoinFailed { step, cause }) => self.join_failed(step, cause),
            (Link::Ready(_), Event::Eapol { source, frame }) => self.eapol(source, frame, now, nonce),
            (Link::Ready(_), Event::Left(left)) => self.left(left, now),
        }
    }

    fn hello(&mut self, address: Address, bands: Bands, now: Duration) {
        self.link = Link::Ready(Radio { address, bands });
        self.say("wlan: a radio said hello");
        if let Some(join) = self.resume.take() {
            self.join = Some(join);
            self.scan(now);
        }
    }

    fn seen(&mut self, channel: Channel, signal: Signal, frame: &[u8]) {
        if self.scanning.is_none() {
            return self.violation(broke("reported a BSS outside a scan"));
        }
        let Some(bss) = heard(channel, signal, frame) else {
            self.counters.malformed_seen += 1;
            return;
        };
        let Some(bss) = bss else {
            self.counters.off_channel += 1;
            return;
        };
        if self.heard.len() >= MAX_BSS && !self.heard.contains_key(&bss.bssid) {
            self.counters.dropped_bss += 1;
            return;
        }
        self.heard.insert(bss.bssid, bss);
    }

    fn bands(&self) -> Bands {
        match &self.link {
            Link::Ready(radio) => radio.bands,
            _ => unreachable!("only a radio that said hello scans"),
        }
    }

    fn scan_done(&mut self, now: Duration) {
        if self.scanning.take().is_none() {
            return self.violation(broke("finished a scan nobody asked"));
        }
        let c = self.counters;
        self.say(&format!(
            "wlan: a scan heard {} BSSes, and dropped {} malformed frames, {} heard off their channel and {} past its bound",
            self.heard.len(),
            c.malformed_seen,
            c.off_channel,
            c.dropped_bss
        ));
        let networks = self.networks();
        for client in core::mem::take(&mut self.scan_waiters) {
            self.answer(client, Answer::Networks(networks.clone()));
        }
        if let Some(join) = self.join.take() {
            match join.phase {
                Phase::Scanning => self.choose(join, now),
                _ => self.join = Some(join),
            }
        }
    }

    /// Every network heard, by its strongest BSS, strongest first.
    fn networks(&self) -> Vec<Network> {
        let bands = self.bands();
        let mut by_ssid: BTreeMap<Ssid, &Bss> = BTreeMap::new();
        for bss in self.heard.values() {
            let Some(ssid) = bss.ssid else { continue };
            let best = by_ssid.entry(ssid).or_insert(bss);
            if bss.signal > best.signal {
                *best = bss;
            }
        }
        let mut networks: Vec<Network> = by_ssid
            .into_iter()
            .map(|(ssid, bss)| Network {
                ssid,
                security: bss.security,
                band: bss.channel.band(),
                signal: bss.signal,
                joinable: self.heard.values().any(|b| b.ssid == Some(ssid) && b.joinable(bands).is_ok()),
            })
            .collect();
        networks.sort_by_key(|n| core::cmp::Reverse(n.signal));
        networks
    }

    /// The scan a join asked is done: choose its BSS and join it.
    fn choose(&mut self, mut join: Join, now: Duration) {
        let radio_address = match &self.link {
            Link::Ready(radio) => radio.address,
            _ => unreachable!("only a radio that said hello scans"),
        };
        let bss = match choice::choose(self.heard.values(), join.ssid, self.bands()) {
            Ok(bss) => bss.clone(),
            Err(None) => return self.fail(join, Refusal::NotSeen, "no BSS of that name was heard"),
            Err(Some(why)) => return self.fail(join, Refusal::Unjoinable, why.reason()),
        };
        let own_rsne = bss.own_rsne();
        let keyed = Keyed {
            authenticator: bss.bssid,
            supplicant: radio_address,
            beacon_rsne: &bss.rsne,
            beacon_rsnxe: bss.rsnxe.as_deref(),
            own_rsne: &own_rsne,
            own_rsnxe: None,
        };
        let pmk = wpa2::derive_pmk(join.passphrase.as_bytes(), join.ssid.as_bytes())
            .expect("a checked passphrase and SSID derive a PMK");
        let supplicant = match Supplicant::new(pmk, &keyed) {
            Ok(s) => s,
            Err(why) => return self.fail(join, Refusal::Unjoinable, why.reason()),
        };
        self.command(Command::Join {
            bssid: bss.bssid,
            channel: bss.channel,
            ssid: join.ssid.as_bytes(),
            request: &own_rsne,
            heard: &bss.elements,
        });
        let chosen = Chosen { bssid: bss.bssid, channel: bss.channel, signal: bss.signal };
        join.phase = Phase::Associating { bss: chosen, supplicant, since: now };
        self.join = Some(join);
    }

    fn associated(&mut self, now: Duration) {
        if core::mem::take(&mut self.abandoned) {
            return;
        }
        let Some(Join { phase: phase @ Phase::Associating { .. }, .. }) = &mut self.join else {
            return self.violation(broke("associated with nothing it was asked to join"));
        };
        let Phase::Associating { bss, supplicant, .. } = core::mem::replace(phase, Phase::Scanning) else {
            unreachable!("matched above")
        };
        *phase = Phase::Handshaking { bss, supplicant, since: now, message2s: 0 };
    }

    fn join_failed(&mut self, step: JoinStep, cause: JoinCause) {
        if core::mem::take(&mut self.abandoned) {
            return;
        }
        if !matches!(self.join, Some(Join { phase: Phase::Associating { .. }, .. })) {
            return self.violation(broke("failed a join nobody asked"));
        }
        let join = self.join.take().expect("matched above");
        let refusal = match step {
            JoinStep::Authentication => Refusal::Authentication,
            JoinStep::Association => Refusal::Association,
        };
        let detail = match cause {
            JoinCause::Status(status) => format!("status code {status}"),
            JoinCause::NoAnswer => String::from("no answer"),
        };
        self.fail(join, refusal, &detail);
    }

    fn eapol(&mut self, source: Address, frame: &[u8], now: Duration, nonce: impl FnOnce() -> [u8; 32]) {
        let associated = matches!(self.join, Some(Join { phase: Phase::Handshaking { .. } | Phase::Connected { .. }, .. }));
        if !associated {
            // An association being left may still deliver one.
            if self.leaving.is_none() {
                self.violation(broke("delivered an EAPOL frame while nothing was associated"));
            }
            return;
        }
        let join = self.join.as_mut().expect("matched above");
        let (Phase::Handshaking { bss, supplicant, .. } | Phase::Connected { bss, supplicant }) = &mut join.phase else {
            unreachable!("matched above")
        };
        if source != bss.bssid {
            self.counters.stray_eapol += 1;
            return;
        }
        let step = match supplicant.receive(frame, nonce) {
            Ok(step) => step,
            Err(why) if why.verified() => {
                let join = self.join.take().expect("matched above");
                self.leave(&join, now);
                return self.fail(join, Refusal::Handshake, why.reason());
            }
            Err(_) => {
                self.counters.refused_eapol += 1;
                return;
            }
        };
        let (band, channel, signal) = (bss.channel.band(), bss.channel.number(), bss.signal);
        let first = step.pairwise.is_some() && matches!(join.phase, Phase::Handshaking { .. });
        if let Phase::Handshaking { message2s, .. } = &mut join.phase
            && step.pairwise.is_none()
        {
            *message2s += 1;
        }
        self.command(Command::Eapol { frame: &step.reply });
        if let Some(k) = &step.pairwise {
            self.command(Command::Pairwise { key: k.key() });
        }
        if let Some(k) = &step.group {
            self.command(Command::Group { id: k.id(), key: k.key(), rsc: k.rsc() });
        }
        if let Some(k) = &step.integrity {
            self.command(Command::Integrity { id: k.id(), key: k.key(), ipn: k.ipn() });
        }
        if first {
            self.command(Command::Authorize);
            let join = self.join.as_mut().expect("matched above");
            let Phase::Handshaking { bss, supplicant, .. } = core::mem::replace(&mut join.phase, Phase::Scanning) else {
                unreachable!("matched above")
            };
            join.phase = Phase::Connected { bss, supplicant };
            if let Some(client) = join.client.take() {
                self.answer(client, Answer::Connected { band, channel, signal });
            }
            self.say("wlan: connected");
        }
    }

    /// Each leave is answered by its own [`Left::Asked`]; any other [`Left`] is the end of the
    /// association that is up, or of the one being left.
    fn left(&mut self, left: Left, now: Duration) {
        let detail = match left {
            Left::Asked if self.leaving.take().is_some() => return,
            Left::Asked => return self.violation(broke("answered a leave nobody asked")),
            Left::Deauthenticated(reason) => format!("deauthenticated, reason code {reason}"),
            Left::Disassociated(reason) => format!("disassociated, reason code {reason}"),
            Left::BeaconLoss => String::from("the access point's beacons stopped"),
            Left::Firmware => String::from("the radio's firmware restarted"),
            Left::RfKill => String::from("the radio was switched off"),
        };
        let associated = matches!(self.join, Some(Join { phase: Phase::Handshaking { .. } | Phase::Connected { .. }, .. }));
        if !associated {
            if self.leaving.is_none() {
                self.violation(broke("left a BSS while nothing was associated"));
            }
            return;
        }
        let join = self.join.take().expect("matched above");
        match join.phase {
            Phase::Handshaking { message2s, .. } => {
                let (refusal, detail) = unanswered(message2s, &detail);
                self.fail(join, refusal, &detail);
            }
            Phase::Connected { .. } => {
                self.say(&format!("wlan: the association ended ({detail}); joining again"));
                self.eapol_dropped();
                self.join = Some(Join { phase: Phase::Scanning, ..join });
                self.scan(now);
            }
            Phase::Scanning | Phase::Associating { .. } => unreachable!("matched above"),
        }
    }

    /// A settings program's connection ended: nothing waits on it any more.
    pub fn client_gone(&mut self, client: Client) {
        self.asking.retain(|c| *c != client);
        self.ended.retain(|c| *c != client);
        self.scan_waiters.retain(|c| *c != client);
        if let Some(join) = &mut self.join
            && join.client == Some(client)
        {
            join.client = None;
        }
    }

    /// One frame from a settings program.
    pub fn request(&mut self, client: Client, msg_type: u32, payload: &[u8], now: Duration) {
        if self.ended.contains(&client) {
            return;
        }
        let request = match Request::decode(msg_type, payload) {
            Ok(r) if !self.asking.contains(&client) => r,
            _ => {
                self.ended.push(client);
                return self.out.push(Out::EndClient(client));
            }
        };
        self.asking.push(client);
        match request {
            Request::Status => {
                let state = self.state();
                self.answer(client, Answer::State(state));
            }
            Request::Scan => self.ask_scan(client, now),
            Request::Connect { ssid, passphrase } => self.connect(client, ssid, passphrase, now),
            Request::Disconnect => {
                self.resume = None;
                if let Some(join) = self.join.take() {
                    self.leave(&join, now);
                    self.eapol_dropped();
                    if let Some(waiting) = join.client {
                        self.refuse(waiting, Refusal::Superseded, "a disconnect");
                    }
                }
                self.answer(client, Answer::Done);
            }
        }
    }

    fn ask_scan(&mut self, client: Client, now: Duration) {
        if !matches!(self.link, Link::Ready(_)) {
            return self.refuse(client, Refusal::NoRadio, "");
        }
        // A join past its scan is answered with the networks that scan heard: the radio scans
        // nothing while it joins.
        if let Some(Join { phase: Phase::Associating { .. } | Phase::Handshaking { .. }, .. }) = &self.join {
            let networks = self.networks();
            return self.answer(client, Answer::Networks(networks));
        }
        self.scan_waiters.push(client);
        self.scan(now);
    }

    fn connect(&mut self, client: Client, ssid: Ssid, typed: &[u8], now: Duration) {
        if !matches!(self.link, Link::Ready(_)) {
            return self.refuse(client, Refusal::NoRadio, "");
        }
        let Some(passphrase) = Passphrase::new(typed) else {
            return self.refuse(client, Refusal::Passphrase, "");
        };
        self.resume = None;
        if let Some(old) = self.join.take() {
            self.leave(&old, now);
            self.eapol_dropped();
            if let Some(waiting) = old.client {
                self.refuse(waiting, Refusal::Superseded, "another connect");
            }
        }
        self.join = Some(Join { ssid, passphrase, client: Some(client), phase: Phase::Scanning });
        self.scan(now);
    }

    pub fn state(&self) -> State {
        if !matches!(self.link, Link::Ready(_)) {
            return State::NoRadio;
        }
        match &self.join {
            Some(Join { ssid, phase: Phase::Connected { bss, .. }, .. }) => State::Connected {
                ssid: *ssid,
                band: bss.channel.band(),
                channel: bss.channel.number(),
                signal: bss.signal,
            },
            Some(Join { ssid, .. }) => State::Joining { ssid: *ssid },
            None if self.scanning.is_some() => State::Scanning,
            None => State::Idle,
        }
    }
}

/// A handshake that ended unfinished: an access point that let every message 2 go unanswered,
/// or one that began none.
fn unanswered(message2s: u32, then: &str) -> (Refusal, String) {
    match message2s {
        0 => (Refusal::NoMessage1, String::from(then)),
        1 => (Refusal::NoAnswerToMessage2, format!("message 2 sent once; then {then}")),
        n => (Refusal::NoAnswerToMessage2, format!("message 2 sent {n} times; then {then}")),
    }
}

/// A Seen frame as a BSS: `None` for one that is no Beacon or Probe Response or does not parse,
/// `Some(None)` for one heard off its own channel.
fn heard(channel: Channel, signal: Signal, frame: &[u8]) -> Option<Option<Bss>> {
    let Ok(Frame::Management(m)) = Frame::parse(frame) else { return None };
    if !matches!(m.subtype, ManagementSubtype::Beacon | ManagementSubtype::ProbeResponse) || m.bssid.is_group() {
        return None;
    }
    let (Ok(Body::Beacon(b)) | Ok(Body::ProbeResponse(b))) = Body::parse(m.subtype, m.body) else { return None };
    Bss::parse(m.bssid, channel, signal, b.elements)
}
